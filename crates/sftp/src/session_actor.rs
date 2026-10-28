use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use macsftp_core::{
    AppEvent, EditCheckId, EditSessionId, ErrorCode, FileKind, FsOp, FsPath, FsScope,
    RemoteDirLoading, RemoteDirSnapshot, RemoteEditSnapshotCheckFailed, RemoteEditSnapshotChecked,
    RemoteEntry, RemoteEntryResolution, RemoteEventScope, RemoteOperationFailure, RemotePath,
    RemoteScoped, RemoteSnapshot, SessionId, StartTransferCommand, TabId, Timestamp, TransferId,
    TransferJob, TransferPlanId, UserFacingError, is_bidi_or_control,
};
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::fs::DirEntry;
use russh_sftp::protocol::{FileType as SftpFileType, StatusCode};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::transfer_job::{
    emit_temporary_file_warning, open_transfer_sftp, plan_remote_download, remote_planning_error,
};

/// Host trust file locations and prompt policy (architecture §10).
#[derive(Debug, Clone)]
pub struct HostTrustConfig {
    /// App-owned known_hosts — the only file macSFTP writes.
    pub app_known_hosts_path: PathBuf,
    /// The user's `~/.ssh/known_hosts`, read-only.
    pub user_known_hosts_path: Option<PathBuf>,
    /// How long an unanswered trust prompt blocks the handshake before
    /// it is rejected (ADR-004: default 5 minutes).
    pub trust_prompt_timeout: Duration,
    /// Upper bound on one peer-facing setup phase: TCP connect, SSH
    /// banner/key exchange, a single authentication request (password,
    /// public key, certificate, or SSH agent socket/listing), and each SFTP
    /// subsystem setup step (channel open, subsystem request, protocol init,
    /// remote home resolution).
    ///
    /// Time spent waiting for the user to answer a trust or keyboard-
    /// interactive prompt is excluded, so a slow human never trips it. These
    /// are per-step bounds, not one budget for the whole connection: a very
    /// slow but progressing server stays connected, while a peer that accepts
    /// the socket and then stops talking fails in roughly one bound instead of
    /// pinning a pooled `Connecting` entry forever.
    pub handshake_timeout: Duration,
}

impl HostTrustConfig {
    pub fn new(app_known_hosts_path: PathBuf, user_known_hosts_path: Option<PathBuf>) -> Self {
        Self {
            app_known_hosts_path,
            user_known_hosts_path,
            trust_prompt_timeout: Duration::from_secs(300),
            handshake_timeout: Duration::from_secs(30),
        }
    }
}

/// The real per-tab browsing session actor (current architecture §7).
///
/// Establishes TCP + SSH handshake with host key verification through
/// `KnownHostsStore` and the `TrustRegistry` prompt flow, authenticates
/// with a password or private key, opens the SFTP subsystem, and holds
/// the connection until cancelled or the server disconnects.
///
/// Emits the shared app event contract consumed by the runtime bridge.
pub struct RemoteSessionActor {
    tab_id: TabId,
    session_id: SessionId,
    session_epoch: u64,
    shared_connection: std::sync::Arc<crate::pool::SharedConnection>,
    sftp: russh_sftp::client::SftpSession,
    event_tx: flume::Sender<AppEvent>,
}

/// A request the runtime routes to one connected real browsing actor.
///
/// The actor already owns the SFTP session, so the request only carries
/// the operation payload; its tab/session scope is attached by the actor.
#[derive(Debug)]
pub enum RemoteSessionRequest {
    ReadDir {
        path: RemotePath,
    },
    /// Resolve one listing entry that the user opened. Symlinks are followed
    /// here (never during a listing) so the app can decide between navigating
    /// into a link and editing the file it points at.
    ResolveEntry {
        path: RemotePath,
    },
    AcquireTransferConnection {
        responder: oneshot::Sender<Arc<crate::pool::SharedConnection>>,
    },
    PlanRemoteDownload {
        command: StartTransferCommand,
        plan_id: TransferPlanId,
        created_at: Timestamp,
        next_transfer_id: Arc<AtomicU64>,
        cancel: CancellationToken,
        responder: oneshot::Sender<Option<Vec<TransferJob>>>,
    },
    /// Delete a residual remote temp file left by a previous run. Used by
    /// the app to reconcile the recorded residual on reconnect.
    RemoveRemoteTempFile {
        transfer_id: TransferId,
        path: RemotePath,
    },
    /// Create / rename / delete against the live SFTP session. On success
    /// the actor re-lists `refresh_path` so the UI refreshes in place.
    Fs {
        scope: FsScope,
        op: FsOp,
        refresh_path: RemotePath,
    },
    /// Authoritative remote-metadata check for an in-flight edit upload. The
    /// actor reads `symlink_metadata(path)` and emits a scoped
    /// `RemoteEditSnapshotChecked`/`CheckFailed`; no responder is needed
    /// because the actor owns `event_tx` and attaches its own live scope.
    CheckRemoteEditSnapshot {
        edit_session_id: EditSessionId,
        check_id: EditCheckId,
        path: RemotePath,
    },
}

/// Upper bound when chasing a remote symlink chain at open time. Servers cap
/// `ELOOP` themselves; this only stops a misbehaving one from looping forever.
const MAX_LINK_HOPS: usize = 16;

impl RemoteSessionActor {
    pub fn new(
        tab_id: TabId,
        session_id: SessionId,
        session_epoch: u64,
        shared_connection: std::sync::Arc<crate::pool::SharedConnection>,
        sftp: russh_sftp::client::SftpSession,
        event_tx: flume::Sender<AppEvent>,
    ) -> Self {
        Self {
            tab_id,
            session_id,
            session_epoch,
            shared_connection,
            sftp,
            event_tx,
        }
    }

    fn scope(&self) -> RemoteEventScope {
        RemoteEventScope::new(self.tab_id, self.session_id, self.session_epoch)
    }

    /// Run the connect flow, then serve requests until cancellation or
    /// server disconnect. Cancellation exits silently — the dispatch
    /// loop owns cleanup, matching the mock actor's contract.
    pub async fn run(
        self,
        cancel: CancellationToken,
        request_rx: flume::Receiver<RemoteSessionRequest>,
    ) {
        let scope = self.scope();

        let _ = self
            .event_tx
            .send_async(AppEvent::TabConnected(RemoteScoped::new(
                scope.clone(),
                macsftp_core::TabConnected {
                    remote_root: macsftp_core::RemotePath::new(
                        self.shared_connection.remote_root.clone(),
                    ),
                },
            )))
            .await;

        self.shared_connection
            .active_channels
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!(session_id = ?self.session_id, "session cancelled by user");
                    break;
                }
                _ = self.shared_connection.connection_lost.cancelled() => {
                    // The physical layer records the classified cause before
                    // cancelling the token, so it is always readable here.
                    let recorded_cause = self
                        .shared_connection
                        .disconnect_cause
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .as_ref()
                        .copied();
                    let reason = recorded_cause
                        .map(crate::physical_connection::PhysicalDisconnectCause::disconnect_reason)
                        .unwrap_or(macsftp_core::DisconnectReason::ConnectionLost);
                    if let Err(error) = self
                        .event_tx
                        .send_async(AppEvent::TabDisconnected(RemoteScoped::new(
                            scope.clone(),
                            macsftp_core::TabDisconnected { reason },
                        )))
                        .await
                    {
                        warn!(error = %error, "physical disconnect event dropped");
                    }
                    break;
                }
                request = request_rx.recv_async() => {
                    let Ok(request) = request else {
                        break;
                    };
                    self.handle_request(&scope, request).await;
                }
            }
        }

        self.shared_connection
            .active_channels
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        *self
            .shared_connection
            .last_used
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = std::time::Instant::now();
    }

    async fn handle_request(&self, scope: &RemoteEventScope, request: RemoteSessionRequest) {
        match request {
            RemoteSessionRequest::ReadDir { path } => {
                self.read_remote_dir(&self.sftp, scope, path).await
            }
            RemoteSessionRequest::ResolveEntry { path } => {
                let resolution = resolve_remote_entry(&self.sftp, path).await;
                if self
                    .event_tx
                    .send_async(AppEvent::RemoteEntryResolved(RemoteScoped::new(
                        scope.clone(),
                        resolution,
                    )))
                    .await
                    .is_err()
                {
                    warn!("remote entry resolution event dropped");
                }
            }
            RemoteSessionRequest::AcquireTransferConnection { responder } => {
                if responder.send(self.shared_connection.clone()).is_err() {
                    warn!("transfer connection lease was not received");
                }
            }
            RemoteSessionRequest::PlanRemoteDownload {
                command,
                plan_id,
                created_at,
                next_transfer_id,
                cancel,
                responder,
            } => match open_transfer_sftp(&self.shared_connection.handle).await {
                Ok(planning_sftp) => {
                    let event_tx = self.event_tx.clone();
                    std::mem::drop(tokio::spawn(async move {
                        let jobs = plan_remote_download(
                            planning_sftp,
                            command,
                            plan_id,
                            created_at,
                            next_transfer_id,
                            event_tx,
                            cancel,
                        )
                        .await;
                        if responder.send(jobs).is_err() {
                            warn!("remote download planning result was not received");
                        }
                    }));
                }
                Err(error) => {
                    if self
                        .event_tx
                        .send_async(AppEvent::TransferPlanFailed { plan_id, error })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if responder.send(None).is_err() {
                        warn!("remote download planning failure was not received");
                    }
                }
            },
            RemoteSessionRequest::RemoveRemoteTempFile { transfer_id, path } => {
                if !path.as_str().contains(".macsftp-part-")
                    || path.as_str().contains('\0')
                    || path.as_str().chars().any(is_bidi_or_control)
                {
                    warn!(
                        path = path.as_str(),
                        "refusing to remove remote temp file that does not match .macsftp-part- pattern or contains forbidden characters"
                    );
                    return;
                }
                match self.sftp.remove_file(path.as_str()).await {
                    Ok(()) => {
                        let _ = self
                            .event_tx
                            .send_async(AppEvent::ResidualTempCleared {
                                transfer_id,
                                path: path.as_str().to_string(),
                            })
                            .await;
                    }
                    Err(SftpError::Status(status))
                        if status.status_code == StatusCode::NoSuchFile =>
                    {
                        let _ = self
                            .event_tx
                            .send_async(AppEvent::ResidualTempCleared {
                                transfer_id,
                                path: path.as_str().to_string(),
                            })
                            .await;
                    }
                    Err(error) => {
                        emit_temporary_file_warning(
                            &self.event_tx,
                            transfer_id,
                            format!(
                                "Remote temporary file remains at {}: {error}",
                                path.as_str()
                            ),
                        )
                        .await;
                    }
                }
            }
            RemoteSessionRequest::Fs {
                scope: fs_scope,
                op,
                refresh_path,
            } => {
                match execute_remote_fs_op(&self.sftp, &op).await {
                    Ok(()) => {
                        // Re-list using the actor's live remote scope so
                        // RemoteDirLoaded passes the stale-event guard.
                        self.read_remote_dir(&self.sftp, scope, refresh_path).await;
                    }
                    Err(failure) => {
                        let _ = self
                            .event_tx
                            .send_async(AppEvent::FsOperationFailed {
                                scope: fs_scope,
                                failure,
                            })
                            .await;
                    }
                }
            }
            RemoteSessionRequest::CheckRemoteEditSnapshot {
                edit_session_id,
                check_id,
                path,
            } => {
                // Authoritative live read: a stale UI directory listing must
                // never authorize overwriting a concurrent remote change, so
                // the actor stats the exact path immediately before the save
                // upload. The actor owns the live scope, so the result is
                // remote-session-scoped for the coordinator's stale guard.
                match self.sftp.symlink_metadata(path.as_str()).await {
                    Ok(metadata) => {
                        let snapshot = remote_snapshot_from_metadata(&metadata);
                        let _ = self
                            .event_tx
                            .send_async(AppEvent::RemoteEditSnapshotChecked(RemoteScoped::new(
                                scope.clone(),
                                RemoteEditSnapshotChecked {
                                    edit_session_id,
                                    check_id,
                                    path,
                                    snapshot,
                                },
                            )))
                            .await;
                    }
                    Err(error) => {
                        // A stat failure (NoSuchFile included) is
                        // indeterminate: the remote file may have been deleted
                        // or changed, so overwriting it is unsafe. Report a
                        // retryable failure so the coordinator keeps the local
                        // edit instead of authorizing an upload.
                        warn!(error = %error, "remote edit snapshot check failed");
                        let _ = self
                            .event_tx
                            .send_async(AppEvent::RemoteEditSnapshotCheckFailed(RemoteScoped::new(
                                scope.clone(),
                                RemoteEditSnapshotCheckFailed {
                                    edit_session_id,
                                    check_id,
                                    path,
                                    error: UserFacingError::new(
                                        ErrorCode::Unknown,
                                        "Server Check Failed",
                                        "The remote server returned an error while reading the file metadata.",
                                    )
                                    .with_retryable(true),
                                },
                            )))
                            .await;
                    }
                }
            }
        }
    }

    async fn read_remote_dir(
        &self,
        sftp: &SftpSession,
        scope: &RemoteEventScope,
        path: RemotePath,
    ) {
        if self
            .event_tx
            .send_async(AppEvent::RemoteDirLoading(RemoteScoped::new(
                scope.clone(),
                RemoteDirLoading { path: path.clone() },
            )))
            .await
            .is_err()
        {
            return;
        }

        let read_dir = match sftp.read_dir(path.as_str()).await {
            Ok(read_dir) => read_dir,
            Err(error) => {
                let user_error = read_directory_error(&error);
                let _ = self
                    .event_tx
                    .send_async(AppEvent::RemoteOperationFailed(RemoteScoped::new(
                        scope.clone(),
                        RemoteOperationFailure {
                            operation: "Read remote directory".to_string(),
                            path: Some(path),
                            error: user_error,
                        },
                    )))
                    .await;
                return;
            }
        };

        let entries = read_dir.map(remote_entry_from_dir_entry).collect();
        let _ = self
            .event_tx
            .send_async(AppEvent::RemoteDirLoaded(RemoteScoped::new(
                scope.clone(),
                RemoteDirSnapshot { path, entries },
            )))
            .await;
    }
}

pub(crate) fn sftp_status_guidance(error: &SftpError) -> &'static str {
    match error {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => "The remote file or folder does not exist.",
            StatusCode::PermissionDenied => "The server denied permission for this path.",
            StatusCode::Failure => "The server reported an error processing this request.",
            StatusCode::BadMessage => "The server received or sent an invalid SFTP message.",
            StatusCode::NoConnection | StatusCode::ConnectionLost => {
                "The SFTP connection was closed or lost."
            }
            StatusCode::OpUnsupported => "The server does not support this SFTP operation.",
            _ => "Check the remote server status and permissions.",
        },
        _ => "The SFTP connection encountered a protocol or communication error.",
    }
}

fn read_directory_error(error: &SftpError) -> UserFacingError {
    let (code, title, message) = match error {
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile => (
            ErrorCode::NotFound,
            "The server folder wasn’t found.",
            "This folder no longer exists. Go up one level or refresh.",
        ),
        SftpError::Status(status) if status.status_code == StatusCode::PermissionDenied => (
            ErrorCode::PermissionDenied,
            "Permission denied",
            "You can’t view this folder. Choose another folder or try again.",
        ),
        _ => (
            ErrorCode::Unknown,
            "The server folder couldn’t be loaded.",
            "This folder couldn’t be loaded. Check permissions and try again.",
        ),
    };
    let mut user_error = UserFacingError::new(code, title, message).with_retryable(true);
    user_error.detail = Some(sftp_status_guidance(error).to_string());
    user_error
}

async fn execute_remote_fs_op(sftp: &SftpSession, op: &FsOp) -> Result<(), UserFacingError> {
    match op {
        FsOp::Rename { from, to } => {
            let from = require_remote_path(from, "rename source")?;
            let to = require_remote_path(to, "rename destination")?;
            if from.as_str().trim_matches('/').is_empty()
                || to.as_str().trim_matches('/').is_empty()
            {
                return Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Invalid Path",
                    "The root directory cannot be renamed.",
                ));
            }
            if to.as_str().contains('\0') || to.as_str().chars().any(is_bidi_or_control) {
                return Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Invalid Name",
                    "The destination name is invalid or contains forbidden characters.",
                ));
            }
            sftp.rename(from.as_str(), to.as_str())
                .await
                .map_err(|error| remote_fs_error("The item couldn’t be renamed.", &error))
        }
        FsOp::CreateDirectory { parent, name } => {
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\0')
                || name.chars().any(is_bidi_or_control)
            {
                return Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Invalid Folder Name",
                    "The folder name is invalid or contains forbidden characters.",
                ));
            }
            let parent = require_remote_path(parent, "create directory parent")?;
            let path = parent.join(name);
            sftp.create_dir(path.as_str())
                .await
                .map_err(|error| remote_fs_error("The folder couldn’t be created.", &error))
        }
        FsOp::Delete { entries } => {
            for entry in entries {
                let path = require_remote_path(&entry.path, "delete path")?;
                let trimmed = path.as_str().trim_matches('/');
                if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
                    return Err(UserFacingError::new(
                        ErrorCode::Unknown,
                        "Invalid Path",
                        "The root directory cannot be deleted.",
                    ));
                }
                if entry.is_dir {
                    remove_remote_dir_recursive(sftp, path).await?;
                } else {
                    sftp.remove_file(path.as_str()).await.map_err(|error| {
                        remote_fs_error("The item couldn’t be deleted.", &error)
                    })?;
                }
            }
            Ok(())
        }
    }
}

fn require_remote_path<'a>(
    path: &'a FsPath,
    label: &str,
) -> Result<&'a RemotePath, UserFacingError> {
    path.as_remote().ok_or_else(|| {
        UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid remote path",
            format!("The {label} was not a remote path."),
        )
    })
}

/// Depth-first recursive delete. Fail-fast on the first SFTP error.
async fn remove_remote_dir_recursive(
    sftp: &SftpSession,
    path: &RemotePath,
) -> Result<(), UserFacingError> {
    let trimmed = path.as_str().trim_matches('/');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid Path",
            "The root directory cannot be deleted.",
        ));
    }
    let read_dir = sftp
        .read_dir(path.as_str())
        .await
        .map_err(|error| remote_fs_error("Folder Couldn’t Be Read", &error))?;
    for entry in read_dir {
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let child = path.join(&name);
        if entry.file_type() == SftpFileType::Dir {
            Box::pin(remove_remote_dir_recursive(sftp, &child)).await?;
        } else {
            sftp.remove_file(child.as_str())
                .await
                .map_err(|error| remote_fs_error("The item couldn’t be deleted.", &error))?;
        }
    }
    sftp.remove_dir(path.as_str())
        .await
        .map_err(|error| remote_fs_error("The folder couldn’t be deleted.", &error))
}

fn remote_fs_error(title: &str, error: &SftpError) -> UserFacingError {
    let code = match error {
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile => {
            ErrorCode::NotFound
        }
        SftpError::Status(status) if status.status_code == StatusCode::PermissionDenied => {
            ErrorCode::PermissionDenied
        }
        SftpError::Status(status) if status.status_code == StatusCode::Failure => {
            // Often "file exists" for create_dir — keep generic with detail.
            ErrorCode::Unknown
        }
        _ => ErrorCode::Unknown,
    };
    let mut user_error = UserFacingError::new(
        code,
        title,
        "Check the server path and permissions, then try again.",
    )
    .with_retryable(true);
    user_error.detail = Some(sftp_status_guidance(error).to_string());
    user_error
}

fn remote_entry_from_dir_entry(entry: DirEntry) -> RemoteEntry {
    let metadata = entry.metadata();
    let kind = match entry.file_type() {
        SftpFileType::Dir => FileKind::Directory,
        SftpFileType::File => FileKind::File,
        SftpFileType::Symlink => FileKind::Symlink,
        SftpFileType::Other => FileKind::Other,
    };

    RemoteEntry {
        name: entry.file_name(),
        path: RemotePath::new(entry.path()),
        kind,
        size: metadata.size,
        permissions: metadata.permissions,
        modified_at: metadata
            .mtime
            .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
        // Resolving symlink targets is deliberately a separate operation;
        // listing must not add one network round trip per entry.
        link_target: None,
    }
}

/// Resolve one listing entry at open time.
///
/// A listing keeps symlinks typed as symlinks (no per-entry round trip), so the
/// type is only discovered here. The kind comes from a server-side `stat` of the
/// link path (which follows it), and a file link's write-through path comes from
/// a server-side `realpath`: only the server knows how a relative target with
/// `..` interacts with a symlinked parent directory. The final `lstat` supplies
/// kind/size/mtime, so a target that changes between two requests cannot yield a
/// mismatched kind/target pair.
pub(crate) async fn resolve_remote_entry(
    sftp: &SftpSession,
    path: RemotePath,
) -> RemoteEntryResolution {
    let unresolved = |error: UserFacingError| RemoteEntryResolution {
        path: path.clone(),
        kind: FileKind::Symlink,
        target: None,
        size: None,
        modified_at: None,
        error: Some(error),
    };

    let metadata = match sftp.symlink_metadata(path.as_str()).await {
        Ok(metadata) => metadata,
        Err(error) => {
            let failure = remote_planning_error("Server Item Unavailable", &error);
            return RemoteEntryResolution {
                path: path.clone(),
                kind: FileKind::Unknown,
                target: None,
                size: None,
                modified_at: None,
                error: Some(failure),
            };
        }
    };

    let link_kind = metadata.file_type();
    if link_kind != SftpFileType::Symlink {
        return RemoteEntryResolution {
            path: path.clone(),
            kind: match link_kind {
                SftpFileType::Dir => FileKind::Directory,
                SftpFileType::File => FileKind::File,
                SftpFileType::Symlink => FileKind::Symlink,
                SftpFileType::Other => FileKind::Other,
            },
            target: None,
            size: metadata.size,
            modified_at: metadata
                .mtime
                .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
            error: None,
        };
    }

    // Follow the link server-side: this decides whether opening it means
    // "enter this folder" or "edit the file it points at".
    let followed = match sftp.metadata(path.as_str()).await {
        Ok(metadata) => metadata,
        Err(error) => {
            return unresolved(remote_planning_error("Server Link Unavailable", &error));
        }
    };
    if followed.file_type() == SftpFileType::Dir {
        return RemoteEntryResolution {
            path,
            kind: FileKind::Directory,
            target: None,
            size: followed.size,
            modified_at: followed
                .mtime
                .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
            error: None,
        };
    }

    // The write-through path must be what the server resolves, never a lexical
    // join of `readlink`: with `/srv/app/current -> releases/v2`, a link target
    // `../shared/app.conf` resolves under `releases/v2` on the server, not under
    // `/srv/app`. Guessing here would download one file and later overwrite
    // another.
    //
    // Three server behaviors are possible:
    //   * absolute REALPATH -> use it as-is;
    //   * relative REALPATH (non-conforming; SFTP v3 requires absolute) -> we can
    //     neither trust it as an absolute path nor lexically join it against the
    //     link's logical parent, so refuse;
    //   * no REALPATH -> readlink + lexical join is safe only when the target
    //     has no `..`, i.e. nothing that depends on the server's real parent.
    let mut current = match sftp.canonicalize(path.as_str()).await {
        Ok(canonical) => match write_through_start(&path, Some(&canonical), None) {
            Some(current) => current,
            None => return unresolved(unresolvable_link_error()),
        },
        // A server that cannot run `realpath` may only be edited when the
        // fallback cannot silently pick the wrong file. A target with `..`
        // depends on the server's real parent path, so refuse it instead of
        // guessing.
        Err(_) => {
            let target_text = match sftp.read_link(path.as_str()).await {
                Ok(target) => target,
                Err(error) => {
                    return unresolved(remote_planning_error("Server Link Unavailable", &error));
                }
            };
            match write_through_start(&path, None, Some(&target_text)) {
                Some(current) => current,
                None => return unresolved(unresolvable_link_error()),
            }
        }
    };

    // Even a successful `realpath` is treated as a starting point: some servers
    // only absolutize the path without dereferencing. Walk until a non-link
    // remains; refuse any `..` hop we would have to resolve ourselves.
    let mut hops = 0;
    loop {
        if hops >= MAX_LINK_HOPS {
            return unresolved(UserFacingError::new(
                ErrorCode::UnsupportedSymlink,
                "Link Loop Detected",
                "The link points back to itself and cannot be followed.",
            ));
        }
        hops += 1;
        let metadata = match sftp.symlink_metadata(current.as_str()).await {
            Ok(metadata) => metadata,
            Err(error) => {
                return unresolved(remote_planning_error("Server Link Unavailable", &error));
            }
        };
        if metadata.file_type() != SftpFileType::Symlink {
            let kind = match metadata.file_type() {
                SftpFileType::Dir => FileKind::Directory,
                SftpFileType::File => FileKind::File,
                SftpFileType::Other => FileKind::Other,
                SftpFileType::Symlink => unreachable!("checked above"),
            };
            return RemoteEntryResolution {
                path,
                target: (kind == FileKind::File).then_some(current),
                kind,
                size: metadata.size,
                modified_at: metadata
                    .mtime
                    .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
                error: None,
            };
        }
        let target_text = match sftp.read_link(current.as_str()).await {
            Ok(target) => target,
            Err(error) => {
                return unresolved(remote_planning_error("Server Link Unavailable", &error));
            }
        };
        if target_text.is_empty()
            || target_text.contains('\0')
            || target_text.chars().any(is_bidi_or_control)
            || target_escapes_through_parent(&target_text)
        {
            return unresolved(unresolvable_link_error());
        }
        current = current.resolve_link_target(&target_text);
    }
}

/// Decide the starting point for the write-through walk.
///
/// `canonical` is the server's REALPATH result; `target` is a `readlink` result
/// used only when the server has no REALPATH. Returns `None` when the client
/// cannot derive a trustworthy path, in which case the caller refuses the
/// write-through instead of risking a different file.
///
/// A relative REALPATH is rejected even without `..`: SFTP v3 requires an
/// absolute result, and joining it against the link's *logical* parent would
/// point inside the link itself whenever that parent is a symlink.
fn write_through_start(
    path: &RemotePath,
    canonical: Option<&str>,
    target: Option<&str>,
) -> Option<RemotePath> {
    match (canonical, target) {
        (Some(canonical), _) => {
            if !canonical.starts_with('/')
                || canonical.contains('\0')
                || canonical.chars().any(is_bidi_or_control)
            {
                None
            } else {
                Some(RemotePath::new(canonical))
            }
        }
        (None, Some(target)) => {
            if target.is_empty()
                || target.contains('\0')
                || target.chars().any(is_bidi_or_control)
                || target_escapes_through_parent(target)
            {
                None
            } else {
                Some(path.resolve_link_target(target))
            }
        }
        (None, None) => None,
    }
}

/// Whether a link target contains a `..` component. Its meaning depends on the
/// server's real parent directory, so the client cannot lexically collapse it
/// without risking a different file — including for an absolute target whose
/// prefix may itself pass through a symlink.
fn target_escapes_through_parent(target: &str) -> bool {
    target.split('/').any(|component| component == "..")
}

fn unresolvable_link_error() -> UserFacingError {
    UserFacingError::new(
        ErrorCode::UnsupportedSymlink,
        "Link Cannot Be Resolved",
        "The link target cannot be resolved safely on this server. Open the file from its real location instead.",
    )
    .with_retryable(false)
}

/// Convert a live SFTP `Metadata` into a core `RemoteSnapshot` for the
/// authoritative edit-check comparison. `symlink_metadata` (not `metadata`)
/// is used by the caller so a symlink is reported as a link rather than
/// silently dereferenced. `mtime` is whole-second, matching what a later
/// directory listing would report.
fn remote_snapshot_from_metadata(metadata: &russh_sftp::client::fs::Metadata) -> RemoteSnapshot {
    RemoteSnapshot {
        size: metadata.size,
        modified_at: metadata
            .mtime
            .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
    }
}

#[cfg(test)]
mod tests {
    use macsftp_core::{ErrorCode, RemotePath};
    use russh_sftp::client::error::Error as SftpError;
    use russh_sftp::protocol::{Status, StatusCode};

    use super::{
        read_directory_error, remote_fs_error, target_escapes_through_parent, write_through_start,
    };

    #[test]
    fn write_through_start_accepts_absolute_realpath() {
        let path = RemotePath::new("/srv/app/current/latest.conf");
        assert_eq!(
            write_through_start(&path, Some("/srv/releases/v2/app.conf"), None),
            Some(RemotePath::new("/srv/releases/v2/app.conf"))
        );
    }

    #[test]
    fn write_through_start_rejects_relative_realpath() {
        // SFTP v3 requires REALPATH to be absolute. Joining a relative answer
        // against the logical parent would reintroduce the symlinked-parent bug.
        let path = RemotePath::new("/srv/app/current/latest.conf");
        assert_eq!(
            write_through_start(&path, Some("releases/v2/app.conf"), None),
            None
        );
        assert_eq!(
            write_through_start(&path, Some("../shared/app.conf"), None),
            None
        );
    }

    #[test]
    fn write_through_start_falls_back_only_without_parent_traversal() {
        let path = RemotePath::new("/srv/app/latest.conf");
        assert_eq!(
            write_through_start(&path, None, Some("app.conf")),
            Some(RemotePath::new("/srv/app/app.conf"))
        );
        assert_eq!(
            write_through_start(&path, None, Some("../shared/app.conf")),
            None,
            "a relative target with .. depends on the server's real parent"
        );
        assert_eq!(
            write_through_start(&path, None, Some("/opt/shared//app.conf")),
            Some(RemotePath::new("/opt/shared/app.conf")),
            "an absolute target without .. is safe to normalize"
        );
        assert_eq!(
            write_through_start(&path, None, Some("/opt/../etc/app.conf")),
            None,
            "an absolute .. still depends on how the server resolves its prefix"
        );
    }

    #[test]
    fn write_through_start_rejects_bidi_and_null() {
        let path = RemotePath::new("/srv/app/latest.conf");
        assert_eq!(
            write_through_start(&path, Some("/srv/app/\0evil.conf"), None),
            None
        );
        assert_eq!(
            write_through_start(&path, Some("/srv/app/\u{202E}evil.conf"), None),
            None
        );
        assert_eq!(write_through_start(&path, None, Some("app\0.conf")), None);
        assert_eq!(
            write_through_start(&path, None, Some("app\u{202E}.conf")),
            None
        );
        assert_eq!(write_through_start(&path, None, Some("")), None);
    }

    #[test]
    fn target_traversal_detection_covers_absolute_and_relative_targets() {
        assert!(target_escapes_through_parent("../shared/app.conf"));
        assert!(target_escapes_through_parent("/opt/../etc/app.conf"));
        assert!(!target_escapes_through_parent("shared/app.conf"));
        assert!(!target_escapes_through_parent("/opt/shared/app.conf"));
        // A component that merely starts with dots is not traversal.
        assert!(!target_escapes_through_parent("..notes/app.conf"));
    }

    fn status_error(status_code: StatusCode) -> SftpError {
        SftpError::Status(Status {
            id: 1,
            status_code,
            error_message: "fixture failure".to_string(),
            language_tag: "en".to_string(),
        })
    }

    #[test]
    fn read_directory_maps_missing_directory_to_not_found() {
        let error = read_directory_error(&status_error(StatusCode::NoSuchFile));

        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.title, "The server folder wasn’t found.");
        assert!(error.retryable);
    }

    #[test]
    fn read_directory_maps_permission_denied_to_permission_error() {
        let error = read_directory_error(&status_error(StatusCode::PermissionDenied));

        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(error.title, "Permission denied");
        assert!(error.retryable);
    }

    #[test]
    fn read_directory_error_redacts_untrusted_server_error_messages() {
        let malicious_error = SftpError::Status(Status {
            id: 1,
            status_code: StatusCode::PermissionDenied,
            error_message: "MALICIOUS SERVER PAYLOAD: run sudo rm -rf /".to_string(),
            language_tag: "en".to_string(),
        });
        let error = read_directory_error(&malicious_error);
        let detail = error.detail.unwrap_or_default();
        assert!(!detail.contains("MALICIOUS SERVER PAYLOAD"));
        assert!(!detail.contains("sudo rm -rf"));
        assert!(!detail.contains("Status {"));
        assert_eq!(detail, "The server denied permission for this path.");
    }

    #[test]
    fn remote_fs_error_redacts_untrusted_server_error_messages() {
        let malicious_error = SftpError::Status(Status {
            id: 1,
            status_code: StatusCode::Failure,
            error_message: "MALICIOUS SERVER PAYLOAD: visit http://phishing.example".to_string(),
            language_tag: "en".to_string(),
        });
        let error = remote_fs_error("Item Couldn’t Be Deleted", &malicious_error);
        let detail = error.detail.unwrap_or_default();
        assert!(!detail.contains("MALICIOUS SERVER PAYLOAD"));
        assert!(!detail.contains("phishing.example"));
        assert!(!detail.contains("Status {"));
        assert_eq!(
            detail,
            "The server reported an error processing this request."
        );
    }
}
