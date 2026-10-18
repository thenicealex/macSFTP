use std::sync::Arc;

use macsftp_core::{AppEvent, ErrorCode, TransferId, TransferProgress, UserFacingError};
use russh_sftp::client::RawSftpSession;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::runtime::ProgressThrottle;

pub const DEFAULT_WINDOW_CAPACITY: usize = 16;
pub const DEFAULT_CHUNK_SIZE: u32 = 256 * 1024;

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

pub(crate) async fn open_raw_sftp_session(
    channel: russh::Channel<russh::client::Msg>,
) -> Result<(Arc<RawSftpSession>, u32), UserFacingError> {
    let sftp_config = russh_sftp::client::Config {
        max_packet_len: 262144,
        max_concurrent_writes: 16,
        request_timeout_secs: 30,
    };
    let mut raw = RawSftpSession::new_with_config(channel.into_stream(), sftp_config);
    let version = raw.init().await.map_err(|error| {
        transfer_error(
            ErrorCode::ChannelClosed,
            "Transfer Couldn’t Start",
            "The file transfer connection couldn’t start.",
            error.to_string(),
        )
    })?;
    let mut chunk_size = DEFAULT_CHUNK_SIZE;
    if version
        .extensions
        .get("limits@openssh.com")
        .is_some_and(|v| v == "1")
    {
        let extension = raw.limits().await.map_err(|error| {
            transfer_error(
                ErrorCode::ChannelClosed,
                "Transfer Couldn’t Start",
                "Failed to negotiate SFTP limits.",
                error.to_string(),
            )
        })?;
        if extension.max_read_len > 0 {
            chunk_size = (extension.max_read_len as u32).min(DEFAULT_CHUNK_SIZE);
        }
        raw.set_limits(russh_sftp::client::rawsession::Limits::from(extension));
    }
    Ok((Arc::new(raw), chunk_size))
}

pub(crate) async fn open_transfer_raw_sftp(
    handle: &russh::client::Handle<crate::physical_connection::ClientHandler>,
) -> Result<(Arc<RawSftpSession>, u32), UserFacingError> {
    let channel = handle.channel_open_session().await.map_err(|error| {
        transfer_error(
            ErrorCode::ChannelClosed,
            "Transfer Couldn’t Start",
            "The transfer connection couldn’t open. Reconnect and try again.",
            error.to_string(),
        )
    })?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| {
            transfer_error(
                ErrorCode::ChannelClosed,
                "Transfer Couldn’t Start",
                "The server rejected the SFTP subsystem for this transfer.",
                error.to_string(),
            )
        })?;
    open_raw_sftp_session(channel).await
}

struct ReadChunkResult {
    offset: u64,
    requested_len: u32,
    result: Result<Vec<u8>, UserFacingError>,
}

pub struct PipelinedDownloadReader {
    raw: Arc<RawSftpSession>,
    file_handle: String,
    chunk_size: u32,
    window_capacity: usize,
    bytes_total: Option<u64>,
}

impl PipelinedDownloadReader {
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
        self.chunk_size = chunk_size.max(1024);
        self
    }

    pub fn with_window_capacity(mut self, window_capacity: usize) -> Self {
        self.window_capacity = window_capacity.max(1);
        self
    }

    pub async fn download_to<W>(
        self,
        destination: &mut W,
        transfer_id: TransferId,
        event_tx: &flume::Sender<AppEvent>,
        cancel: &CancellationToken,
    ) -> Result<u64, UserFacingError>
    where
        W: tokio::io::AsyncWrite + tokio::io::AsyncSeek + Unpin,
    {
        if let Some(0) = self.bytes_total {
            if cancel.is_cancelled() {
                return Err(cancelled_transfer_error());
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
                result = destination.shutdown() => result.map_err(|error| {
                    local_transfer_error("Transfer Couldn’t Finish", &error)
                })?,
            }
            emit_transfer_progress(event_tx, transfer_id, 0, Some(0)).await?;
            return Ok(0);
        }

        let mut next_request_offset: u64 = 0;
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
                let offset = next_request_offset;
                let read_len = if let Some(total) = self.bytes_total {
                    if offset >= total {
                        eof_reached = true;
                        break;
                    }
                    ((total - offset) as u32).min(self.chunk_size)
                } else {
                    self.chunk_size
                };

                if read_len == 0 {
                    eof_reached = true;
                    break;
                }

                let raw = self.raw.clone();
                let handle = self.file_handle.clone();
                in_flight.spawn(async move {
                    let result = raw.read(handle, offset, read_len).await;
                    ReadChunkResult {
                        offset,
                        requested_len: read_len,
                        result: match result {
                            Ok(data) => Ok(data.data),
                            Err(russh_sftp::client::error::Error::Status(status))
                                if status.status_code == russh_sftp::protocol::StatusCode::Eof =>
                            {
                                Ok(Vec::new())
                            }
                            Err(error) => Err(transfer_error(
                                ErrorCode::Unknown,
                                "Transfer Read Failed",
                                "The remote file could not be read.",
                                error.to_string(),
                            )),
                        },
                    }
                });

                next_request_offset += read_len as u64;
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
                Err(join_err) => {
                    in_flight.abort_all();
                    return Err(UserFacingError::new(
                        ErrorCode::Unknown,
                        "Transfer Read Failed",
                        format!("In-flight read task failed: {join_err}"),
                    ));
                }
            };

            let data = match chunk.result {
                Ok(data) => data,
                Err(err) => {
                    in_flight.abort_all();
                    return Err(err);
                }
            };

            if data.is_empty() || data.len() < chunk.requested_len as usize {
                eof_reached = true;
            }

            if !data.is_empty() {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        in_flight.abort_all();
                        return Err(cancelled_transfer_error());
                    }
                    seek_res = destination.seek(std::io::SeekFrom::Start(chunk.offset)) => {
                        if let Err(error) = seek_res {
                            in_flight.abort_all();
                            return Err(local_transfer_error("Transfer Write Failed", &error));
                        }
                    }
                }

                tokio::select! {
                    _ = cancel.cancelled() => {
                        in_flight.abort_all();
                        return Err(cancelled_transfer_error());
                    }
                    write_res = destination.write_all(&data) => {
                        if let Err(error) = write_res {
                            in_flight.abort_all();
                            return Err(local_transfer_error("Transfer Write Failed", &error));
                        }
                    }
                }

                bytes_done += data.len() as u64;
                if throttle.should_send(std::time::Instant::now()) {
                    emit_transfer_progress(event_tx, transfer_id, bytes_done, self.bytes_total)
                        .await?;
                }
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

        tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = destination.shutdown() => result.map_err(|error| {
                local_transfer_error("Transfer Couldn’t Finish", &error)
            })?,
        }

        throttle.reset();
        emit_transfer_progress(event_tx, transfer_id, bytes_done, self.bytes_total).await?;

        Ok(bytes_done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::{Data, Handle, OpenFlags, StatusCode, Version};
    use russh_sftp::server::{self, Handler, StatusReply};
    use std::collections::HashMap;
    use std::io::Cursor;

    struct MockSftpHandler {
        file_content: Vec<u8>,
        fail_read: bool,
    }

    #[derive(Debug)]
    enum MockError {
        Eof,
        Failed,
    }

    impl From<MockError> for StatusReply {
        fn from(error: MockError) -> Self {
            match error {
                MockError::Eof => StatusReply::new(StatusCode::Eof),
                MockError::Failed => StatusReply::new(StatusCode::Failure),
            }
        }
    }

    impl Handler for MockSftpHandler {
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
                handle: "mock-handle".to_string(),
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

        async fn read(
            &mut self,
            id: u32,
            _handle: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, Self::Error> {
            if self.fail_read {
                return Err(MockError::Failed);
            }
            let offset = offset as usize;
            if offset >= self.file_content.len() {
                return Err(MockError::Eof);
            }
            let end = (offset + len as usize).min(self.file_content.len());
            let data = self.file_content[offset..end].to_vec();
            Ok(Data { id, data })
        }
    }

    async fn spawn_mock_raw_session(content: Vec<u8>, fail_read: bool) -> Arc<RawSftpSession> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let handler = MockSftpHandler {
                file_content: content,
                fail_read,
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
    async fn test_pipelined_download_roundtrip() {
        let payload: Vec<u8> = (0..32_768).map(|i| (i % 251) as u8).collect();
        let raw = spawn_mock_raw_session(payload.clone(), false).await;
        let (event_tx, event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(vec![0u8; payload.len()]);

        let reader = PipelinedDownloadReader::new(raw, "mock-handle", Some(payload.len() as u64))
            .with_chunk_size(1024);

        let transferred = reader
            .download_to(&mut destination, TransferId(42), &event_tx, &cancel)
            .await
            .expect("pipelined download should complete successfully");

        assert_eq!(
            transferred,
            payload.len() as u64,
            "transferred byte count should match payload length"
        );
        assert_eq!(
            destination.into_inner(),
            payload,
            "downloaded bytes should match payload"
        );

        let mut progress_events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let AppEvent::TransferProgress(progress) = event {
                progress_events.push(progress);
            }
        }
        assert!(
            !progress_events.is_empty(),
            "progress events should be emitted during transfer"
        );
        let last_progress = progress_events
            .last()
            .expect("must have final progress event");
        assert_eq!(
            last_progress.bytes_done,
            payload.len() as u64,
            "final progress must reflect total bytes"
        );
    }

    #[tokio::test]
    async fn test_pipelined_download_with_custom_window_capacity() {
        let payload: Vec<u8> = (0..16_384).map(|i| (i % 233) as u8).collect();

        // Test with window capacity 1 (sequential)
        let raw_seq = spawn_mock_raw_session(payload.clone(), false).await;
        let (event_tx_seq, _rx) = flume::bounded(100);
        let cancel = CancellationToken::new();
        let mut dest_seq = Cursor::new(vec![0u8; payload.len()]);
        let reader_seq =
            PipelinedDownloadReader::new(raw_seq, "mock-handle", Some(payload.len() as u64))
                .with_chunk_size(1024)
                .with_window_capacity(1);

        let bytes_seq = reader_seq
            .download_to(&mut dest_seq, TransferId(1), &event_tx_seq, &cancel)
            .await
            .expect("sequential window download should succeed");
        assert_eq!(bytes_seq, payload.len() as u64);
        assert_eq!(dest_seq.into_inner(), payload);

        // Test with window capacity 8 (deep pipeline)
        let raw_pipe = spawn_mock_raw_session(payload.clone(), false).await;
        let (event_tx_pipe, _rx) = flume::bounded(100);
        let mut dest_pipe = Cursor::new(vec![0u8; payload.len()]);
        let reader_pipe =
            PipelinedDownloadReader::new(raw_pipe, "mock-handle", Some(payload.len() as u64))
                .with_chunk_size(1024)
                .with_window_capacity(8);

        let bytes_pipe = reader_pipe
            .download_to(&mut dest_pipe, TransferId(2), &event_tx_pipe, &cancel)
            .await
            .expect("pipelined window download should succeed");
        assert_eq!(bytes_pipe, payload.len() as u64);
        assert_eq!(dest_pipe.into_inner(), payload);
    }

    #[tokio::test]
    async fn test_pipelined_download_empty_file() {
        let raw = spawn_mock_raw_session(Vec::new(), false).await;
        let (event_tx, _event_rx) = flume::bounded(10);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(Vec::new());

        let reader = PipelinedDownloadReader::new(raw, "mock-handle", Some(0));
        let transferred = reader
            .download_to(&mut destination, TransferId(10), &event_tx, &cancel)
            .await
            .expect("empty file download should succeed immediately");

        assert_eq!(transferred, 0, "empty download should transfer 0 bytes");
    }

    #[tokio::test]
    async fn test_pipelined_download_size_mismatch() {
        // Server only has 512 bytes, but reader expects 1024 bytes
        let payload: Vec<u8> = vec![42u8; 512];
        let raw = spawn_mock_raw_session(payload, false).await;
        let (event_tx, _event_rx) = flume::bounded(10);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(vec![0u8; 1024]);

        let reader =
            PipelinedDownloadReader::new(raw, "mock-handle", Some(1024)).with_chunk_size(256);

        let result = reader
            .download_to(&mut destination, TransferId(99), &event_tx, &cancel)
            .await;

        match result {
            Err(error) => {
                assert_eq!(
                    error.title, "Transfer Incomplete",
                    "error title should indicate size mismatch"
                );
            }
            Ok(_) => panic!("download with truncated source must return an error"),
        }
    }

    #[tokio::test]
    async fn test_pipelined_download_cancellation() {
        let payload = vec![1u8; 64 * 1024];
        let raw = spawn_mock_raw_session(payload, false).await;
        let (event_tx, _event_rx) = flume::bounded(10);
        let cancel = CancellationToken::new();
        cancel.cancel(); // Pre-cancel

        let mut destination = Cursor::new(vec![0u8; 64 * 1024]);
        let reader = PipelinedDownloadReader::new(raw, "mock-handle", Some(64 * 1024));

        let result = reader
            .download_to(&mut destination, TransferId(123), &event_tx, &cancel)
            .await;

        match result {
            Err(error) => {
                assert_eq!(
                    error.code,
                    ErrorCode::Cancelled,
                    "cancelled download must report cancelled error code"
                );
            }
            Ok(_) => panic!("pre-cancelled download must fail"),
        }
    }

    #[tokio::test]
    async fn test_pipelined_download_server_read_error() {
        let payload = vec![7u8; 1024];
        let raw = spawn_mock_raw_session(payload, true).await;
        let (event_tx, _event_rx) = flume::bounded(10);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(vec![0u8; 1024]);

        let reader = PipelinedDownloadReader::new(raw, "mock-handle", Some(1024));
        let result = reader
            .download_to(&mut destination, TransferId(777), &event_tx, &cancel)
            .await;

        match result {
            Err(error) => {
                assert_eq!(
                    error.title, "Transfer Read Failed",
                    "server read failure must be mapped to Transfer Read Failed"
                );
            }
            Ok(_) => panic!("server error must return an error"),
        }
    }
}
