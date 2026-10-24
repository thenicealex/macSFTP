use std::sync::Arc;

use macsftp_core::{
    AppEvent, ErrorCode, TransferDirection, TransferId, TransferProgress, UserFacingError,
};
use russh_sftp::client::RawSftpSession;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
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

/// Bytes to request for a chunk of `remaining` bytes. Clamping has to happen in
/// `u64` before the cast: a `(remaining as u32).min(chunk)` would wrap past
/// 4 GiB and truncate the transfer.
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

pub(crate) async fn open_raw_sftp_session(
    channel: russh::Channel<russh::client::Msg>,
    direction: TransferDirection,
) -> Result<(Arc<RawSftpSession>, u32), UserFacingError> {
    let sftp_config = russh_sftp::client::Config {
        max_packet_len: 262144,
        max_concurrent_writes: 128,
        request_timeout_secs: 30,
    };
    let mut raw = RawSftpSession::new_with_config(channel.into_stream(), sftp_config);
    let version = raw.init().await.map_err(|_| {
        warn!(
            target: "macsftp_sftp::transfer",
            "failed to initialize raw SFTP session"
        );
        transfer_error(
            ErrorCode::ChannelClosed,
            "Transfer Couldn’t Start",
            "The file transfer connection couldn’t start.",
            "The file transfer session could not be initialized.",
        )
    })?;
    let mut chunk_size = DEFAULT_CHUNK_SIZE;
    if version
        .extensions
        .get("limits@openssh.com")
        .is_some_and(|v| v == "1")
    {
        let extension = raw.limits().await.map_err(|_| {
            warn!(
                target: "macsftp_sftp::transfer",
                "failed to negotiate SFTP limits"
            );
            transfer_error(
                ErrorCode::ChannelClosed,
                "Transfer Couldn’t Start",
                "Failed to negotiate SFTP limits.",
                "The server limits could not be read.",
            )
        })?;
        let limit = match direction {
            TransferDirection::Download => extension.max_read_len,
            TransferDirection::Upload => extension.max_write_len,
        };
        if limit > 0 {
            chunk_size = limit.min(DEFAULT_CHUNK_SIZE as u64) as u32;
        }
        raw.set_limits(russh_sftp::client::rawsession::Limits::from(extension));
    }
    Ok((Arc::new(raw), chunk_size))
}

pub(crate) async fn open_transfer_raw_sftp(
    handle: &russh::client::Handle<crate::physical_connection::ClientHandler>,
    direction: TransferDirection,
) -> Result<(Arc<RawSftpSession>, u32), UserFacingError> {
    let channel = handle.channel_open_session().await.map_err(|error| {
        warn!(
            target: "macsftp_sftp::transfer",
            failure = crate::physical_connection::TransportFailureKind::from_russh_error(&error).as_str(),
            "failed to open SSH session channel for raw transfer; technical detail redacted"
        );
        transfer_error(
            ErrorCode::ChannelClosed,
            "Transfer Couldn’t Start",
            "The transfer connection couldn’t open. Reconnect and try again.",
            "The SSH session channel could not be opened.",
        )
    })?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| {
            warn!(
                target: "macsftp_sftp::transfer",
                failure = crate::physical_connection::TransportFailureKind::from_russh_error(&error).as_str(),
                "server rejected SFTP subsystem request for raw transfer; technical detail redacted"
            );
            transfer_error(
                ErrorCode::ChannelClosed,
                "Transfer Couldn’t Start",
                "The server rejected the SFTP subsystem for this transfer.",
                "The server rejected the SFTP subsystem.",
            )
        })?;
    open_raw_sftp_session(channel, direction).await
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
        // Keep the negotiated value verbatim: servers that advertise a limit
        // below 1 KiB reject any read above it, so flooring here would make
        // every download from such a server fail.
        self.chunk_size = chunk_size.max(1);
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
        // Ranges the server answered with fewer bytes than requested. They are
        // retried before the cursor moves on so a short read cannot leave a
        // hole in the file.
        let mut retry_ranges: std::collections::VecDeque<(u64, u32)> =
            std::collections::VecDeque::new();
        let mut eof_reached = false;
        let mut in_flight = tokio::task::JoinSet::new();
        let mut bytes_done: u64 = 0;
        let mut current_offset: Option<u64> = None;
        let mut throttle = ProgressThrottle::new(10);

        loop {
            if cancel.is_cancelled() {
                in_flight.abort_all();
                return Err(cancelled_transfer_error());
            }

            while in_flight.len() < self.window_capacity {
                let (offset, read_len) = if let Some((offset, len)) = retry_ranges.pop_front() {
                    // Retried tails stay eligible even after an empty read
                    // marked EOF: with an unknown file size, EOF can arrive
                    // while a short read is still missing bytes.
                    (offset, len)
                } else if eof_reached {
                    break;
                } else if let Some(total) = self.bytes_total {
                    if next_request_offset >= total {
                        break;
                    }
                    // Clamp in u64 space: `(total - offset) as u32` wraps once
                    // more than 4 GiB remain and would end the transfer early.
                    let remaining = total - next_request_offset;
                    let len = next_chunk_len(remaining, self.chunk_size);
                    let offset = next_request_offset;
                    next_request_offset += u64::from(len);
                    (offset, len)
                } else {
                    let offset = next_request_offset;
                    next_request_offset += u64::from(self.chunk_size);
                    (offset, self.chunk_size)
                };

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
                        "in-flight pipelined read task failed"
                    );
                    return Err(UserFacingError::new(
                        ErrorCode::Unknown,
                        "Transfer Read Failed",
                        "The transfer read worker failed unexpectedly.",
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

            let received = data.len() as u64;
            let requested = u64::from(chunk.requested_len);
            if received == 0 {
                match self.bytes_total {
                    None => eof_reached = true,
                    Some(total) if chunk.offset < total => {
                        in_flight.abort_all();
                        return Err(transfer_error(
                            ErrorCode::Unknown,
                            "Transfer Incomplete",
                            "The server stopped sending data before the file was complete.",
                            format!("Expected {total} bytes, received {bytes_done} bytes"),
                        ));
                    }
                    Some(_) => {}
                }
            } else if received < requested {
                // SFTP permits a short read away from EOF. Only an empty
                // response ends an unknown-length file; otherwise re-request
                // the missing tail so nothing is skipped.
                let received_end = chunk.offset + received;
                let requested_end = chunk.offset + requested;
                let missing_end = self
                    .bytes_total
                    .map_or(requested_end, |total| requested_end.min(total));
                if received_end < missing_end {
                    retry_ranges.push_back((received_end, (missing_end - received_end) as u32));
                }
            }

            if !data.is_empty() {
                if current_offset != Some(chunk.offset) {
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

                current_offset = Some(chunk.offset + received);
                bytes_done += received;
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
        /// Cap on bytes returned per read, regardless of the requested length.
        max_read: Option<usize>,
        /// Delay the read at offset 0 so later chunks complete first.
        delay_first_read: bool,
    }

    #[derive(Default)]
    struct MockOptions {
        fail_read: bool,
        max_read: Option<usize>,
        delay_first_read: bool,
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
            if self.delay_first_read && offset == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let offset = offset as usize;
            if offset >= self.file_content.len() {
                return Err(MockError::Eof);
            }
            let mut end = (offset + len as usize).min(self.file_content.len());
            if let Some(max_read) = self.max_read {
                end = end.min(offset + max_read);
            }
            let data = self.file_content[offset..end].to_vec();
            Ok(Data { id, data })
        }
    }

    async fn spawn_mock_raw_session(content: Vec<u8>, fail_read: bool) -> Arc<RawSftpSession> {
        spawn_mock_raw_session_with(
            content,
            MockOptions {
                fail_read,
                ..MockOptions::default()
            },
        )
        .await
    }

    async fn spawn_mock_raw_session_with(
        content: Vec<u8>,
        options: MockOptions,
    ) -> Arc<RawSftpSession> {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let handler = MockSftpHandler {
                file_content: content,
                fail_read: options.fail_read,
                max_read: options.max_read,
                delay_first_read: options.delay_first_read,
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
                assert_eq!(
                    error.detail.as_deref(),
                    Some("The server reported an error processing this request.")
                );
            }
            Ok(_) => panic!("server error must return an error"),
        }
    }

    /// SFTP permits a short read away from EOF. The missing tail must be
    /// re-requested instead of being treated as the end of the file.
    #[tokio::test]
    async fn test_pipelined_download_refetches_short_reads() {
        let payload: Vec<u8> = (0..4096).map(|i| (i % 241) as u8).collect();
        let raw = spawn_mock_raw_session_with(
            payload.clone(),
            MockOptions {
                max_read: Some(300),
                ..MockOptions::default()
            },
        )
        .await;
        let (event_tx, _event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(vec![0u8; payload.len()]);

        let reader = PipelinedDownloadReader::new(raw, "mock-handle", Some(payload.len() as u64))
            .with_chunk_size(1024)
            .with_window_capacity(4);

        let transferred = reader
            .download_to(&mut destination, TransferId(11), &event_tx, &cancel)
            .await
            .expect("short reads must be retried until the file is complete");

        assert_eq!(transferred, payload.len() as u64);
        assert_eq!(
            destination.into_inner(),
            payload,
            "retried ranges must fill the same offsets"
        );
    }

    /// With an unknown file size an empty read marks EOF, but a short read
    /// that arrived earlier may still have missing bytes. Its retry must run
    /// even after EOF was observed, or the file is silently truncated.
    #[tokio::test]
    async fn test_pipelined_download_unknown_size_retries_after_eof() {
        let payload: Vec<u8> = (0..4096).map(|i| (i % 233) as u8).collect();
        let raw = spawn_mock_raw_session_with(
            payload.clone(),
            MockOptions {
                max_read: Some(300),
                ..MockOptions::default()
            },
        )
        .await;
        let (event_tx, _event_rx) = flume::bounded(200);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(vec![0u8; payload.len()]);

        let reader = PipelinedDownloadReader::new(raw, "mock-handle", None)
            .with_chunk_size(1024)
            .with_window_capacity(16);

        let transferred = reader
            .download_to(&mut destination, TransferId(13), &event_tx, &cancel)
            .await
            .expect("unknown-length download must retry short reads after EOF");

        assert_eq!(transferred, payload.len() as u64);
        assert_eq!(
            destination.into_inner(),
            payload,
            "every byte before EOF must be present"
        );
    }

    /// Responses may arrive in any order; each one carries its own offset, so
    /// a slow first read must not shift later chunks.
    #[tokio::test]
    async fn test_pipelined_download_out_of_order_completion() {
        let payload: Vec<u8> = (0..8192).map(|i| (i % 197) as u8).collect();
        let raw = spawn_mock_raw_session_with(
            payload.clone(),
            MockOptions {
                delay_first_read: true,
                ..MockOptions::default()
            },
        )
        .await;
        let (event_tx, _event_rx) = flume::bounded(100);
        let cancel = CancellationToken::new();
        let mut destination = Cursor::new(vec![0u8; payload.len()]);

        let reader = PipelinedDownloadReader::new(raw, "mock-handle", Some(payload.len() as u64))
            .with_chunk_size(1024)
            .with_window_capacity(4);

        let transferred = reader
            .download_to(&mut destination, TransferId(12), &event_tx, &cancel)
            .await
            .expect("out-of-order completion must still write every offset");

        assert_eq!(transferred, payload.len() as u64);
        assert_eq!(
            destination.into_inner(),
            payload,
            "out-of-order chunks must be written at their own offsets"
        );
    }

    /// Regression guard for the >4 GiB arithmetic: clamping the remaining
    /// length after casting to `u32` wraps and ends large transfers early.
    #[test]
    fn next_chunk_len_does_not_wrap_past_four_gib() {
        let chunk = DEFAULT_CHUNK_SIZE;
        let four_gib = 4 * 1024 * 1024 * 1024_u64;

        assert_eq!(next_chunk_len(four_gib, chunk), chunk);
        assert_eq!(next_chunk_len(four_gib + 17, chunk), chunk);
        assert_eq!(next_chunk_len(8 * 1024 * 1024 * 1024, chunk), chunk);
        assert_eq!(next_chunk_len(1234, chunk), 1234);
        assert_eq!(next_chunk_len(0, chunk), 0);
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
