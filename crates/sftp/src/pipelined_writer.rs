use std::sync::Arc;

use macsftp_core::{AppEvent, ErrorCode, TransferId, TransferProgress, UserFacingError};
use russh_sftp::client::RawSftpSession;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::runtime::ProgressThrottle;

pub const TARGET_PIPELINE_WINDOW_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_WINDOW_CAPACITY: usize = 32;
pub const DEFAULT_CHUNK_SIZE: u32 = 256 * 1024;

/// Calculates the optimal sliding window capacity (number of concurrent in-flight chunks)
/// to saturate the 8 MiB SSH transport channel window across high-latency networks.
///
/// Bounded between 16 (for 512 KiB+ chunks or default) and 128 (for small 32 KiB chunks)
/// to avoid excessive async task spawning while maintaining a full pipeline.
pub fn optimal_window_capacity(chunk_size: u32) -> usize {
    if chunk_size == 0 {
        return DEFAULT_WINDOW_CAPACITY;
    }
    let capacity = (TARGET_PIPELINE_WINDOW_BYTES / u64::from(chunk_size)) as usize;
    capacity.clamp(16, 128)
}

fn next_chunk_len(remaining: u64, chunk_size: u32) -> u32 {
    remaining.min(u64::from(chunk_size)) as u32
}

fn cancelled_transfer_error() -> UserFacingError {
    UserFacingError::new(
        ErrorCode::Cancelled,
        "Transfer cancelled",
        "The transfer was cancelled.",
    )
}

fn local_transfer_error(title: &str, error: &std::io::Error) -> UserFacingError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        _ => ErrorCode::Unknown,
    };
    transfer_error(
        code,
        title,
        "Check the local file and permissions, then try again.",
        error.to_string(),
    )
}

fn transfer_error(
    code: ErrorCode,
    title: &str,
    message: &str,
    detail: impl Into<String>,
) -> UserFacingError {
    let mut error = UserFacingError::new(code, title, message).with_retryable(true);
    error.detail = Some(detail.into());
    error
}

async fn emit_transfer_progress(
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    bytes_done: u64,
    bytes_total: Option<u64>,
) -> Result<(), UserFacingError> {
    event_tx
        .send_async(AppEvent::TransferProgress(TransferProgress {
            transfer_id,
            bytes_done,
            bytes_total,
        }))
        .await
        .map_err(|_| {
            UserFacingError::new(
                ErrorCode::ChannelClosed,
                "Transfer stopped",
                "The application stopped receiving transfer updates.",
            )
        })
}

struct WriteChunkResult {
    len: u32,
    result: Result<(), UserFacingError>,
}

pub struct PipelinedUploadWriter {
    raw: Arc<RawSftpSession>,
    file_handle: String,
    chunk_size: u32,
    window_capacity: usize,
    bytes_total: Option<u64>,
}

impl PipelinedUploadWriter {
    pub fn new(
        raw: Arc<RawSftpSession>,
        file_handle: impl Into<String>,
        bytes_total: Option<u64>,
    ) -> Self {
        Self {
            raw,
            file_handle: file_handle.into(),
            chunk_size: DEFAULT_CHUNK_SIZE,
            window_capacity: DEFAULT_WINDOW_CAPACITY,
            bytes_total,
        }
    }

    pub fn with_chunk_size(mut self, chunk_size: u32) -> Self {
        self.chunk_size = chunk_size.max(1);
        self
    }

    pub fn with_window_capacity(mut self, window_capacity: usize) -> Self {
        self.window_capacity = window_capacity.max(1);
        self
    }

    pub async fn upload_from<R>(
        self,
        source: &mut R,
        transfer_id: TransferId,
        event_tx: &flume::Sender<AppEvent>,
        cancel: &CancellationToken,
    ) -> Result<u64, UserFacingError>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        if let Some(0) = self.bytes_total {
            if cancel.is_cancelled() {
                return Err(cancelled_transfer_error());
            }
            emit_transfer_progress(event_tx, transfer_id, 0, Some(0)).await?;
            return Ok(0);
        }

        let mut current_offset: u64 = 0;
        let mut eof_reached = false;
        let mut in_flight = tokio::task::JoinSet::new();
        let mut bytes_done: u64 = 0;
        let mut throttle = ProgressThrottle::new(10);

        loop {
            if cancel.is_cancelled() {
                in_flight.abort_all();
                return Err(cancelled_transfer_error());
            }

            while in_flight.len() < self.window_capacity && !eof_reached {
                let to_read = if let Some(total) = self.bytes_total {
                    if current_offset >= total {
                        eof_reached = true;
                        break;
                    }
                    let remaining = total - current_offset;
                    next_chunk_len(remaining, self.chunk_size) as usize
                } else {
                    self.chunk_size as usize
                };

                let mut buffer = Vec::with_capacity(to_read);
                while buffer.len() < to_read {
                    let n = tokio::select! {
                        _ = cancel.cancelled() => {
                            in_flight.abort_all();
                            return Err(cancelled_transfer_error());
                        }
                        result = source.read_buf(&mut buffer) => {
                            result.map_err(|error| {
                                local_transfer_error("Transfer Read Failed", &error)
                            })?
                        }
                    };
                    if n == 0 {
                        eof_reached = true;
                        break;
                    }
                }

                if buffer.is_empty() {
                    eof_reached = true;
                    break;
                }

                let offset = current_offset;
                let len = buffer.len() as u32;
                current_offset += u64::from(len);

                let raw = self.raw.clone();
                let handle = self.file_handle.clone();
                in_flight.spawn(async move {
                    let result = raw.write(handle, offset, buffer).await;
                    WriteChunkResult {
                        len,
                        result: match result {
                            Ok(status)
                                if status.status_code == russh_sftp::protocol::StatusCode::Ok =>
                            {
                                Ok(())
                            }
                            Ok(status) => Err(transfer_error(
                                ErrorCode::Unknown,
                                "Transfer Write Failed",
                                "The remote file could not be written.",
                                format!("Server status code: {:?}", status.status_code),
                            )),
                            Err(error) => Err(transfer_error(
                                ErrorCode::Unknown,
                                "Transfer Write Failed",
                                "The remote file could not be written.",
                                crate::session_actor::sftp_status_guidance(&error),
                            )),
                        },
                    }
                });
            }

            if in_flight.is_empty() {
                break;
            }

            let next_result = tokio::select! {
                _ = cancel.cancelled() => {
                    in_flight.abort_all();
                    return Err(cancelled_transfer_error());
                }
                res = in_flight.join_next() => res,
            };

            let Some(join_result) = next_result else {
                break;
            };

            let chunk = match join_result {
                Ok(chunk) => chunk,
                Err(_join_err) => {
                    in_flight.abort_all();
                    warn!(
                        target: "macsftp_sftp::transfer",
                        "in-flight pipelined write task failed"
                    );
                    return Err(UserFacingError::new(
                        ErrorCode::Unknown,
                        "Transfer Write Failed",
                        "The transfer write worker failed unexpectedly.",
                    ));
                }
            };

            if let Err(err) = chunk.result {
                in_flight.abort_all();
                return Err(err);
            }

            bytes_done += u64::from(chunk.len);
            if throttle.should_send(std::time::Instant::now()) {
                emit_transfer_progress(event_tx, transfer_id, bytes_done, self.bytes_total).await?;
            }
        }

        if let Some(total) = self.bytes_total
            && bytes_done != total
        {
            return Err(transfer_error(
                ErrorCode::Unknown,
                "Transfer Incomplete",
                "The transferred file size did not match the expected size.",
                format!("Expected {total} bytes, wrote {bytes_done} bytes"),
            ));
        }

        throttle.reset();
        emit_transfer_progress(event_tx, transfer_id, bytes_done, self.bytes_total).await?;

        Ok(bytes_done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::{Handle, OpenFlags, StatusCode, Version};
    use russh_sftp::server::{self, Handler, StatusReply};
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MockUploadSftpHandler {
        received_data: Arc<Mutex<Vec<u8>>>,
        fail_write_at_offset: Option<u64>,
        delay_first_write: bool,
    }

    #[derive(Default)]
    struct MockOptions {
        fail_write_at_offset: Option<u64>,
        delay_first_write: bool,
    }

    #[derive(Debug)]
    enum MockError {
        Failed,
    }

    impl From<MockError> for StatusReply {
        fn from(error: MockError) -> Self {
            match error {
                MockError::Failed => StatusReply::new(StatusCode::Failure),
            }
        }
    }

    impl Handler for MockUploadSftpHandler {
        type Error = MockError;

        fn unimplemented(&self) -> Self::Error {
            MockError::Failed
        }

        async fn init(
            &mut self,
            _version: u32,
            _extensions: HashMap<String, String>,
        ) -> Result<Version, Self::Error> {
            Ok(Version::new())
        }

        async fn open(
            &mut self,
            id: u32,
            _filename: String,
            _pflags: OpenFlags,
            _attrs: russh_sftp::protocol::FileAttributes,
        ) -> Result<Handle, Self::Error> {
            Ok(Handle {
                id,
                handle: "mock-upload-handle".to_string(),
            })
        }

        async fn close(
            &mut self,
            id: u32,
            _handle: String,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            Ok(russh_sftp::protocol::Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            _handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> Result<russh_sftp::protocol::Status, Self::Error> {
            if let Some(fail_offset) = self.fail_write_at_offset
                && offset == fail_offset
            {
                return Err(MockError::Failed);
            }
            if self.delay_first_write && offset == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }

            let mut storage = self
                .received_data
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let end = offset as usize + data.len();
            if storage.len() < end {
                storage.resize(end, 0);
            }
            storage[offset as usize..end].copy_from_slice(&data);

            Ok(russh_sftp::protocol::Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    async fn spawn_mock_raw_upload_session(
        received_data: Arc<Mutex<Vec<u8>>>,
        options: MockOptions,
    ) -> Arc<RawSftpSession> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let handler = MockUploadSftpHandler {
                received_data,
                fail_write_at_offset: options.fail_write_at_offset,
                delay_first_write: options.delay_first_write,
            };
            server::run(server_io, handler).await;
        });

        let sftp_config = russh_sftp::client::Config {
            max_packet_len: 262144,
            max_concurrent_writes: 16,
            request_timeout_secs: 10,
        };
        let raw = RawSftpSession::new_with_config(client_io, sftp_config);
        raw.init()
            .await
            .expect("mock raw sftp session should initialize");
        Arc::new(raw)
    }

    #[tokio::test]
    async fn test_pipelined_upload_roundtrip() {
        let payload: Vec<u8> = (0..65_536).map(|i| (i % 251) as u8).collect();
        let received = Arc::new(Mutex::new(Vec::new()));
        let raw = spawn_mock_raw_upload_session(received.clone(), MockOptions::default()).await;
        let (event_tx, event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();

        let writer =
            PipelinedUploadWriter::new(raw, "mock-upload-handle", Some(payload.len() as u64))
                .with_chunk_size(4096)
                .with_window_capacity(4);

        let mut source = std::io::Cursor::new(payload.clone());
        let written = writer
            .upload_from(&mut source, TransferId(1), &event_tx, &cancel)
            .await
            .expect("upload should succeed");

        assert_eq!(written, payload.len() as u64);
        let received_bytes = received.lock().expect("lock mock buffer").clone();
        assert_eq!(received_bytes, payload);

        let events: Vec<_> = event_rx.drain().collect();
        assert!(!events.is_empty(), "progress events should be emitted");
    }

    #[tokio::test]
    async fn test_pipelined_upload_zero_byte_file() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let raw = spawn_mock_raw_upload_session(received.clone(), MockOptions::default()).await;
        let (event_tx, event_rx) = flume::bounded(10);
        let cancel = CancellationToken::new();

        let writer = PipelinedUploadWriter::new(raw, "mock-upload-handle", Some(0));
        let mut source = std::io::Cursor::new(Vec::new());
        let written = writer
            .upload_from(&mut source, TransferId(2), &event_tx, &cancel)
            .await
            .expect("0-byte upload should succeed");

        assert_eq!(written, 0);
        assert_eq!(received.lock().expect("lock mock buffer").len(), 0);

        let events: Vec<_> = event_rx.drain().collect();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AppEvent::TransferProgress(p) => {
                assert_eq!(p.bytes_done, 0);
                assert_eq!(p.bytes_total, Some(0));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_pipelined_upload_cancellation() {
        let payload: Vec<u8> = vec![42; 65_536];
        let received = Arc::new(Mutex::new(Vec::new()));
        let raw = spawn_mock_raw_upload_session(
            received.clone(),
            MockOptions {
                delay_first_write: true,
                ..MockOptions::default()
            },
        )
        .await;
        let (event_tx, _event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();

        let writer =
            PipelinedUploadWriter::new(raw, "mock-upload-handle", Some(payload.len() as u64))
                .with_chunk_size(4096)
                .with_window_capacity(4);

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            cancel_clone.cancel();
        });

        let mut source = std::io::Cursor::new(payload);
        let result = writer
            .upload_from(&mut source, TransferId(3), &event_tx, &cancel)
            .await;

        let error = result.expect_err("upload should fail upon cancellation");
        assert_eq!(error.code, ErrorCode::Cancelled);
    }

    #[tokio::test]
    async fn test_pipelined_upload_server_error() {
        let payload: Vec<u8> = vec![99; 32_768];
        let received = Arc::new(Mutex::new(Vec::new()));
        let raw = spawn_mock_raw_upload_session(
            received.clone(),
            MockOptions {
                fail_write_at_offset: Some(4096),
                ..MockOptions::default()
            },
        )
        .await;
        let (event_tx, _event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();

        let writer =
            PipelinedUploadWriter::new(raw, "mock-upload-handle", Some(payload.len() as u64))
                .with_chunk_size(4096)
                .with_window_capacity(4);

        let mut source = std::io::Cursor::new(payload);
        let result = writer
            .upload_from(&mut source, TransferId(4), &event_tx, &cancel)
            .await;

        let error = result.expect_err("upload should fail when server fails a write");
        assert_eq!(error.code, ErrorCode::Unknown);
    }

    #[tokio::test]
    async fn test_pipelined_upload_incomplete_fails() {
        let payload: Vec<u8> = vec![1; 1000];
        let received = Arc::new(Mutex::new(Vec::new()));
        let raw = spawn_mock_raw_upload_session(received.clone(), MockOptions::default()).await;
        let (event_tx, _event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();

        // Stated size is 2000, but cursor only has 1000 bytes
        let writer =
            PipelinedUploadWriter::new(raw, "mock-upload-handle", Some(2000)).with_chunk_size(500);

        let mut source = std::io::Cursor::new(payload);
        let result = writer
            .upload_from(&mut source, TransferId(5), &event_tx, &cancel)
            .await;

        let error = result.expect_err("incomplete file should error");
        assert_eq!(error.title, "Transfer Incomplete");
    }

    #[test]
    fn test_optimal_window_capacity_calculation() {
        assert_eq!(optimal_window_capacity(0), DEFAULT_WINDOW_CAPACITY);
        // 256 KiB chunks: 8 MiB / 256 KiB = 32 chunks
        assert_eq!(optimal_window_capacity(256 * 1024), 32);
        // 128 KiB chunks: 8 MiB / 128 KiB = 64 chunks
        assert_eq!(optimal_window_capacity(128 * 1024), 64);
        // 64 KiB chunks: 8 MiB / 64 KiB = 128 chunks
        assert_eq!(optimal_window_capacity(64 * 1024), 128);
        // 32 KiB chunks: clamped to upper bound of 128 chunks (4 MiB)
        assert_eq!(optimal_window_capacity(32 * 1024), 128);
        // Large chunks (e.g. 1 MiB): clamped to lower bound of 16 chunks
        assert_eq!(optimal_window_capacity(1024 * 1024), 16);
    }
}
