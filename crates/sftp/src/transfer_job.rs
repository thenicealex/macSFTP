use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use macsftp_core::{
    AppEvent, ConflictDecision, ConflictPolicy, ConflictRequestId, ErrorCode, LocalPath,
    RemotePath, RenameStrategy, ResidualTempRecord, StartTransferCommand, Timestamp,
    TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferFailure, TransferId,
    TransferJob, TransferPlanId, TransferPlanProgress, TransferProgress, TransferSnapshot,
    TransferState, TransferWarning, UserFacingError, is_bidi_or_control,
};
use russh::client;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, FileType as SftpFileType, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use tracing::warn;

use crate::transfer_planner::PLAN_PROGRESS_BATCH_SIZE;

use crate::session_actor::sftp_status_guidance;

#[derive(Clone)]
pub(crate) struct TransferConflictContext {
    pub(crate) waiters: Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
    pub(crate) next_id: Arc<AtomicU64>,
    pub(crate) plan_policies: Arc<Mutex<HashMap<TransferPlanId, ConflictPolicy>>>,
}

pub(crate) struct ConflictWaiter {
    pub(crate) plan_id: TransferPlanId,
    pub(crate) sender: oneshot::Sender<ConflictDecision>,
}

fn resolve_waiters_for_plan(
    waiters: &Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
    plan_id: TransferPlanId,
    decision: ConflictDecision,
) {
    let Ok(mut waiters) = waiters.lock() else {
        return;
    };
    let request_ids: Vec<_> = waiters
        .iter()
        .filter_map(|(request_id, waiter)| (waiter.plan_id == plan_id).then_some(*request_id))
        .collect();
    for request_id in request_ids {
        if let Some(waiter) = waiters.remove(&request_id) {
            let _ = waiter.sender.send(decision.clone());
        }
    }
}

pub(crate) async fn open_transfer_sftp(
    handle: &client::Handle<crate::physical_connection::ClientHandler>,
) -> Result<SftpSession, UserFacingError> {
    let channel = handle.channel_open_session().await.map_err(|error| {
        warn!(
            target: "macsftp_sftp::transfer",
            failure = crate::physical_connection::TransportFailureKind::from_russh_error(&error).as_str(),
            "failed to open SSH session channel for transfer; technical detail redacted"
        );
        transfer_error(
            ErrorCode::ChannelClosed,
            "Transfer Couldn’t Start",
            "The transfer connection couldn’t open. Reconnect and try again.",
            "The SSH session channel could not be opened.",
        )
    })?;
    channel.request_subsystem(true, "sftp").await.map_err(|error| {
        warn!(
            target: "macsftp_sftp::transfer",
            failure = crate::physical_connection::TransportFailureKind::from_russh_error(&error).as_str(),
            "server rejected SFTP subsystem request for transfer; technical detail redacted"
        );
        transfer_error(
            ErrorCode::ChannelClosed,
            "Transfer Couldn’t Start",
            "The server rejected the SFTP subsystem for this transfer.",
            "The server rejected the SFTP subsystem.",
        )
    })?;
    let sftp_config = russh_sftp::client::Config {
        max_packet_len: 262144,
        max_concurrent_writes: 128,
        request_timeout_secs: 30,
    };
    SftpSession::new_with_config(channel.into_stream(), sftp_config)
        .await
        .map_err(|_| {
            warn!(
                target: "macsftp_sftp::transfer",
                "failed to initialize SFTP session for transfer"
            );
            transfer_error(
                ErrorCode::ChannelClosed,
                "Transfer Couldn’t Start",
                "The file transfer connection couldn’t start.",
                "The file transfer session could not be initialized.",
            )
        })
}

pub(crate) enum TransferSession {
    Standard(SftpSession),
    Raw {
        session: Arc<RawSftpSession>,
        chunk_size: u32,
        direction: TransferDirection,
    },
}

impl From<SftpSession> for TransferSession {
    fn from(sftp: SftpSession) -> Self {
        Self::Standard(sftp)
    }
}

/// A transfer session plus what jobs running on it have already proven about
/// the remote side.
///
/// Opening an SFTP channel costs several round trips (channel open, subsystem,
/// protocol init, limits), which dominates a transfer made of many small files
/// if every file pays it. Jobs within one plan therefore hand a session back
/// and forth through [`TransferSessionSlot`] instead of opening a channel per
/// file, and the facts a job learns — this directory exists, the server
/// accepts the atomic hardlink commit here — spare the next file from
/// rediscovering them.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlanTransferFacts {
    pub(crate) verified_directories: Arc<Mutex<HashSet<String>>>,
    /// Destination directories where the atomic hardlink commit has already
    /// been proven. Tracked per directory, not per session: a plan can write
    /// into a subdirectory that is a different mount, where hardlinks may be
    /// unsupported.
    pub(crate) hardlink_commit_directories: Arc<Mutex<HashSet<String>>>,
    pub(crate) destination_root: Arc<Mutex<Option<LocalPath>>>,
}

impl PlanTransferFacts {
    pub(crate) fn destination_root(&self) -> Option<LocalPath> {
        self.destination_root
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn set_destination_root(&self, root: LocalPath) {
        let mut guard = self
            .destination_root
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(root);
        }
    }
}

pub(crate) struct TransferSessionSlot {
    session: TransferSession,
    facts: PlanTransferFacts,
}

impl TransferSessionSlot {
    #[cfg(test)]
    pub(crate) fn new(session: TransferSession) -> Self {
        Self::with_facts(session, PlanTransferFacts::default())
    }

    pub(crate) fn with_facts(session: TransferSession, facts: PlanTransferFacts) -> Self {
        Self { session, facts }
    }

    pub(crate) fn destination_root(&self) -> Option<LocalPath> {
        self.facts.destination_root()
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (client_io, _server_io) = tokio::io::duplex(1);
        let sftp_config = russh_sftp::client::Config {
            max_packet_len: 262144,
            max_concurrent_writes: 16,
            request_timeout_secs: 1,
        };
        let raw = Arc::new(russh_sftp::client::RawSftpSession::new_with_config(
            client_io,
            sftp_config,
        ));
        Self {
            session: TransferSession::Raw {
                session: raw,
                chunk_size: 256 * 1024,
                direction: TransferDirection::Download,
            },
            facts: PlanTransferFacts::default(),
        }
    }

    pub(crate) fn session(&self) -> &TransferSession {
        &self.session
    }

    pub(crate) fn direction(&self) -> TransferDirection {
        match &self.session {
            TransferSession::Standard(_) => TransferDirection::Upload,
            TransferSession::Raw { direction, .. } => *direction,
        }
    }

    pub(crate) fn directory_verified(&self, path: &str) -> bool {
        self.facts
            .verified_directories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(path)
    }

    pub(crate) fn mark_directory_verified(&self, path: String) {
        self.facts
            .verified_directories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path);
    }

    pub(crate) fn hardlink_commit_confirmed(&self, directory: &str) -> bool {
        self.facts
            .hardlink_commit_directories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(directory)
    }

    pub(crate) fn confirm_hardlink_commit(&self, directory: String) {
        self.facts
            .hardlink_commit_directories
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(directory);
    }
}

impl TransferSession {
    pub(crate) async fn symlink_metadata(&self, path: &str) -> Result<FileAttributes, SftpError> {
        match self {
            Self::Standard(sftp) => sftp.symlink_metadata(path).await,
            Self::Raw { session, .. } => session.lstat(path).await.map(|attrs| attrs.attrs),
        }
    }

    pub(crate) async fn metadata(&self, path: &str) -> Result<FileAttributes, SftpError> {
        match self {
            Self::Standard(sftp) => sftp.metadata(path).await,
            Self::Raw { session, .. } => session.stat(path).await.map(|attrs| attrs.attrs),
        }
    }

    pub(crate) async fn read_link(&self, path: &str) -> Result<String, SftpError> {
        match self {
            Self::Standard(sftp) => sftp.read_link(path).await,
            Self::Raw { session, .. } => {
                let name = session.readlink(path).await?;
                match name.files.first() {
                    Some(file) => Ok(file.filename.clone()),
                    None => Err(SftpError::UnexpectedBehavior("no file".to_string())),
                }
            }
        }
    }
}

pub(crate) async fn open_transfer_session(
    handle: &client::Handle<crate::physical_connection::ClientHandler>,
    direction: macsftp_core::TransferDirection,
) -> Result<TransferSession, UserFacingError> {
    let (raw, chunk_size) =
        crate::pipelined_reader::open_transfer_raw_sftp(handle, direction).await?;
    Ok(TransferSession::Raw {
        session: raw,
        chunk_size,
        direction,
    })
}

/// Stream a remote download plan from a dedicated SFTP channel. Planning
/// needs remote metadata, but it must not monopolize the browsing channel:
/// each discovered entry becomes a child job before recursion continues.
pub(crate) async fn plan_remote_download(
    sftp: SftpSession,
    command: StartTransferCommand,
    plan_id: TransferPlanId,
    created_at: Timestamp,
    next_transfer_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
) -> Option<Vec<TransferJob>> {
    let sftp = Arc::new(sftp);
    let mut planner = RemoteDownloadPlanner {
        command,
        plan_id,
        created_at,
        next_transfer_id,
        event_tx,
        cancel,
        planned_count: 0,
        total_bytes: Some(0),
        child_jobs: Vec::new(),
        pending_progress: Vec::with_capacity(PLAN_PROGRESS_BATCH_SIZE),
    };

    match planner.plan(&sftp).await {
        Ok(()) if planner.finish().await => Some(planner.child_jobs),
        Ok(()) => None,
        Err(error) => {
            planner.finish_error(error).await;
            None
        }
    }
}

const MAX_CONCURRENT_DIR_SCANS: usize = 8;

struct RemoteDownloadPlanner {
    command: StartTransferCommand,
    plan_id: TransferPlanId,
    created_at: Timestamp,
    next_transfer_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
    planned_count: usize,
    total_bytes: Option<u64>,
    child_jobs: Vec<TransferJob>,
    pending_progress: Vec<TransferJob>,
}

impl RemoteDownloadPlanner {
    async fn plan(&mut self, sftp: &Arc<SftpSession>) -> Result<(), UserFacingError> {
        self.ensure_not_cancelled()?;
        if self.command.direction != TransferDirection::Download {
            return Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Invalid download plan",
                "The requested transfer is not a download.",
            ));
        }
        if self.command.sources.len() != 1 {
            return Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Choose one download source",
                "Select one remote file or directory and try again.",
            ));
        }
        let Some(TransferEndpoint::Remote(source)) = self.command.sources.first().cloned() else {
            return Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Invalid download source",
                "Downloads require a remote file or directory.",
            ));
        };
        let TransferEndpoint::Local(destination) = self.command.destination.clone() else {
            return Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Invalid download destination",
                "Downloads require a local destination.",
            ));
        };
        let metadata = self.metadata(sftp, &source).await?;
        if metadata.file_type() == SftpFileType::Dir {
            self.emit_child(source.clone(), destination.clone(), Some(0))
                .await?;
            self.plan_directory(sftp, &source, &destination).await
        } else {
            self.emit_child(source, destination, metadata.size).await
        }
    }

    async fn plan_directory(
        &mut self,
        sftp: &Arc<SftpSession>,
        root: &RemotePath,
        destination_root: &LocalPath,
    ) -> Result<(), UserFacingError> {
        let mut pending_directories = vec![root.clone()];
        let mut in_flight = tokio::task::JoinSet::new();

        loop {
            self.ensure_not_cancelled()?;

            while in_flight.len() < MAX_CONCURRENT_DIR_SCANS {
                let Some(current) = pending_directories.pop() else {
                    break;
                };
                let sftp = Arc::clone(sftp);
                in_flight.spawn(async move {
                    let result = sftp.read_dir(current.as_str()).await;
                    (current, result)
                });
            }

            if in_flight.is_empty() {
                break;
            }

            tokio::select! {
                _ = self.cancel.cancelled() => {
                    in_flight.abort_all();
                    return Err(cancelled_transfer_error());
                }
                next = in_flight.join_next() => {
                    let Some(join_res) = next else {
                        break;
                    };
                    let (_current, read_res) = match join_res {
                        Ok(res) => res,
                        Err(join_err) => {
                            if join_err.is_cancelled() {
                                return Err(cancelled_transfer_error());
                            }
                            return Err(UserFacingError::new(
                                ErrorCode::Unknown,
                                "Download Preparation Failed",
                                "The directory listing task failed.",
                            ));
                        }
                    };

                    let entries = match read_res {
                        Ok(entries) => entries,
                        Err(error) => {
                            in_flight.abort_all();
                            return Err(remote_planning_error("Server Folder Unavailable", &error));
                        }
                    };

                    for entry in entries {
                        self.ensure_not_cancelled()?;
                        let path = RemotePath::new(entry.path());
                        let relative = path
                            .as_str()
                            .strip_prefix(root.as_str())
                            .map(|relative| relative.trim_start_matches('/'))
                            .filter(|relative| !relative.is_empty())
                            .ok_or_else(|| {
                                in_flight.abort_all();
                                UserFacingError::new(
                                    ErrorCode::Unknown,
                                    "Download Preparation Failed",
                                    "The remote directory changed while it was being scanned. Try again.",
                                )
                            })?;
                        let destination = match local_download_destination(destination_root, relative) {
                            Ok(dest) => dest,
                            Err(error) => {
                                in_flight.abort_all();
                                return Err(error);
                            }
                        };
                        let is_dir = entry.file_type() == SftpFileType::Dir;
                        let size = if is_dir {
                            Some(0)
                        } else {
                            entry.metadata().size
                        };
                        if is_dir {
                            if let Err(error) = self.emit_child(path.clone(), destination, size).await {
                                in_flight.abort_all();
                                return Err(error);
                            }
                            pending_directories.push(path);
                        } else if let Err(error) = self.emit_child(path, destination, size).await {
                            in_flight.abort_all();
                            return Err(error);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn metadata(
        &self,
        sftp: &SftpSession,
        source: &RemotePath,
    ) -> Result<russh_sftp::client::fs::Metadata, UserFacingError> {
        tokio::select! {
            _ = self.cancel.cancelled() => Err(cancelled_transfer_error()),
            result = sftp.symlink_metadata(source.as_str()) => result.map_err(|error| {
                remote_planning_error("Server Item Unavailable", &error)
            }),
        }
    }

    async fn emit_child(
        &mut self,
        source: RemotePath,
        destination: LocalPath,
        size: Option<u64>,
    ) -> Result<(), UserFacingError> {
        self.ensure_not_cancelled()?;
        self.planned_count += 1;
        self.total_bytes = match (self.total_bytes, size) {
            (Some(total), Some(size)) => total.checked_add(size),
            _ => None,
        };
        let child_job = TransferJob {
            id: TransferId(self.next_transfer_id.fetch_add(1, Ordering::Relaxed)),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(source),
            destination: TransferEndpoint::Local(destination),
            state: TransferState::Queued,
            metadata_policy: self.command.metadata_policy.clone(),
            conflict_policy: self.command.conflict_policy.clone(),
            warnings: Vec::new(),
            created_at: self.created_at,
        };
        self.child_jobs.push(child_job.clone());
        self.pending_progress.push(child_job);
        if self.planned_count == 1 || self.pending_progress.len() >= PLAN_PROGRESS_BATCH_SIZE {
            self.flush_progress().await?;
        }
        Ok(())
    }

    async fn flush_progress(&mut self) -> Result<(), UserFacingError> {
        if self.pending_progress.is_empty() {
            return Ok(());
        }
        let child_jobs = std::mem::take(&mut self.pending_progress);
        tokio::select! {
            _ = self.cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = self.event_tx.send_async(AppEvent::TransferPlanProgress(TransferPlanProgress {
                plan_id: self.plan_id,
                child_jobs,
                planned_count: self.planned_count,
                total_bytes: self.total_bytes,
            })) => result.map_err(|_| UserFacingError::new(
                ErrorCode::ChannelClosed,
                "Preparation Stopped",
                "Transfer preparation updates stopped arriving.",
            ))?,
        }
        Ok(())
    }

    async fn finish(&mut self) -> bool {
        if self.cancel.is_cancelled() {
            self.cancelled().await;
            return false;
        }
        if let Err(error) = self.flush_progress().await {
            self.finish_error(error).await;
            return false;
        }
        if self.cancel.is_cancelled() {
            self.cancelled().await;
            return false;
        }
        self.event_tx
            .send_async(AppEvent::TransferPlanCompleted {
                plan_id: self.plan_id,
            })
            .await
            .is_ok()
    }

    async fn finish_error(&self, error: UserFacingError) {
        if error.code == ErrorCode::Cancelled {
            self.cancelled().await;
        } else if let Err(send_error) = self
            .event_tx
            .send_async(AppEvent::TransferPlanFailed {
                plan_id: self.plan_id,
                error,
            })
            .await
        {
            warn!(error = %send_error, "remote download planning event dropped");
        }
    }

    async fn cancelled(&self) {
        if let Err(send_error) = self
            .event_tx
            .send_async(AppEvent::TransferPlanCancelled {
                plan_id: self.plan_id,
            })
            .await
        {
            warn!(error = %send_error, "remote download planning cancellation event dropped");
        }
    }

    fn ensure_not_cancelled(&self) -> Result<(), UserFacingError> {
        if self.cancel.is_cancelled() {
            Err(cancelled_transfer_error())
        } else {
            Ok(())
        }
    }
}

pub(crate) fn remote_planning_error(title: &str, error: &SftpError) -> UserFacingError {
    let code = match error {
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile => {
            ErrorCode::NotFound
        }
        SftpError::Status(status) if status.status_code == StatusCode::PermissionDenied => {
            ErrorCode::PermissionDenied
        }
        _ => ErrorCode::Unknown,
    };
    transfer_error(
        code,
        title,
        "Check the remote source and permissions, then try again.",
        sftp_status_guidance(error),
    )
}

fn local_download_destination(
    destination_root: &LocalPath,
    relative: &str,
) -> Result<LocalPath, UserFacingError> {
    if relative.chars().any(is_bidi_or_control)
        || Path::new(relative)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Download Preparation Failed",
            "The remote directory contains an unsafe path. Refresh and try again.",
        ));
    }
    Ok(LocalPath::new(
        PathBuf::from(destination_root.as_str())
            .join(relative)
            .display()
            .to_string(),
    ))
}

pub(crate) enum TransferJobOutcome {
    Completed,
    Skipped { reusable_session: bool },
    Failed { retryable: bool },
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_transfer_job(
    transfer_session: Arc<TransferSessionSlot>,
    mut job: TransferJob,
    plan_id: TransferPlanId,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
    conflict_context: TransferConflictContext,
    connection_key: String,
    may_clear_destination: bool,
) -> TransferJobOutcome {
    if may_clear_destination
        && job.direction == macsftp_core::TransferDirection::Download
        && let TransferEndpoint::Local(destination) = &job.destination
    {
        transfer_session
            .facts
            .set_destination_root(destination.clone());
    }
    // Conflict prompt details for downloads perform a best-effort remote stat;
    // its errors are not propagated, so only upload sessions are known clean.
    let mut skipped_during_conflict = false;
    let result = match resolve_transfer_conflict(
        transfer_session.session(),
        &mut job,
        plan_id,
        &event_tx,
        &cancel,
        conflict_context,
    )
    .await
    {
        Ok(()) => match (job.direction, transfer_session.session()) {
            (
                macsftp_core::TransferDirection::Upload,
                TransferSession::Raw {
                    session,
                    chunk_size,
                    ..
                },
            ) => {
                upload_single_file_pipelined(
                    &transfer_session,
                    session,
                    *chunk_size,
                    &job,
                    &event_tx,
                    &cancel,
                    &connection_key,
                )
                .await
            }
            (macsftp_core::TransferDirection::Upload, TransferSession::Standard(sftp)) => {
                upload_transfer(
                    &transfer_session,
                    sftp,
                    &job,
                    &event_tx,
                    &cancel,
                    &connection_key,
                )
                .await
            }
            (
                macsftp_core::TransferDirection::Download,
                TransferSession::Raw {
                    session,
                    chunk_size,
                    ..
                },
            ) => {
                download_single_file_pipelined(
                    &transfer_session,
                    session,
                    *chunk_size,
                    &job,
                    &event_tx,
                    &cancel,
                    may_clear_destination,
                )
                .await
            }
            (macsftp_core::TransferDirection::Download, TransferSession::Standard(sftp)) => {
                download_single_file(
                    &transfer_session,
                    sftp,
                    &job,
                    &event_tx,
                    &cancel,
                    may_clear_destination,
                )
                .await
            }
        },
        Err(error) => {
            skipped_during_conflict =
                error.code == ErrorCode::Cancelled && job.direction == TransferDirection::Upload;
            Err(error)
        }
    };

    let (terminal_event, outcome) = match result {
        Ok(()) => (
            AppEvent::TransferCompleted {
                transfer_id: job.id,
            },
            TransferJobOutcome::Completed,
        ),
        Err(error) if error.code == ErrorCode::Cancelled => (
            AppEvent::TransferSkipped {
                transfer_id: job.id,
            },
            TransferJobOutcome::Skipped {
                reusable_session: skipped_during_conflict,
            },
        ),
        Err(error) => {
            let retryable = error.retryable;
            (
                AppEvent::TransferFailed(TransferFailure {
                    transfer_id: job.id,
                    error,
                }),
                TransferJobOutcome::Failed { retryable },
            )
        }
    };
    if let Err(error) = event_tx.send_async(terminal_event).await {
        warn!(error = %error, "transfer terminal event dropped");
    }
    outcome
}

async fn resolve_transfer_conflict(
    session: &TransferSession,
    job: &mut TransferJob,
    plan_id: TransferPlanId,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    conflict_context: TransferConflictContext,
) -> Result<(), UserFacingError> {
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        return Ok(());
    }
    if !destination_exists(session, job).await? {
        return Ok(());
    }
    if directories_can_merge(session, job).await? {
        return Ok(());
    }
    match job.conflict_policy.clone() {
        ConflictPolicy::OverwriteAll => Ok(()),
        ConflictPolicy::SkipAll => Err(cancelled_transfer_error()),
        ConflictPolicy::RenameAll(strategy) => {
            let new_name = match strategy {
                RenameStrategy::AppendCopySuffix => copy_name_for_destination(&job.destination),
                RenameStrategy::ExplicitName(name) => name,
            };
            rename_destination(job, &new_name)?;
            if destination_exists(session, job).await? {
                return Err(UserFacingError::new(
                    ErrorCode::FileExists,
                    "Destination already exists",
                    "Choose another name and try again.",
                ));
            }
            Ok(())
        }
        ConflictPolicy::Ask => {
            let request_id =
                ConflictRequestId(conflict_context.next_id.fetch_add(1, Ordering::Relaxed));
            let (decision_tx, decision_rx) = oneshot::channel();
            if let Ok(mut waiters) = conflict_context.waiters.lock() {
                waiters.insert(
                    request_id,
                    ConflictWaiter {
                        plan_id,
                        sender: decision_tx,
                    },
                );
            } else {
                return Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Choice Unavailable",
                    "The existing-file choice couldn’t be shown. Try again.",
                ));
            }
            let (source_size, source_modified_at) = conflict_source_details(session, job).await;
            event_tx
                .send_async(AppEvent::TransferConflict(TransferConflictPrompt {
                    request_id,
                    plan_id,
                    transfer_id: job.id,
                    source: job.source.clone(),
                    destination: job.destination.clone(),
                    source_size,
                    source_modified_at,
                }))
                .await
                .map_err(|_| {
                    UserFacingError::new(
                        ErrorCode::ChannelClosed,
                        "Transfer stopped",
                        "The application stopped receiving transfer updates.",
                    )
                })?;
            let decision = tokio::select! {
                _ = cancel.cancelled() => ConflictDecision::CancelJob,
                result = decision_rx => result.unwrap_or(ConflictDecision::CancelJob),
            };
            if let Ok(mut waiters) = conflict_context.waiters.lock() {
                waiters.remove(&request_id);
            }
            match decision {
                ConflictDecision::Overwrite { apply_to_all } => {
                    job.conflict_policy = ConflictPolicy::OverwriteAll;
                    if apply_to_all && let Ok(mut policies) = conflict_context.plan_policies.lock()
                    {
                        policies.insert(plan_id, ConflictPolicy::OverwriteAll);
                    }
                    if apply_to_all {
                        resolve_waiters_for_plan(
                            &conflict_context.waiters,
                            plan_id,
                            ConflictDecision::Overwrite {
                                apply_to_all: false,
                            },
                        );
                    }
                    Ok(())
                }
                ConflictDecision::Skip { apply_to_all } => {
                    if apply_to_all && let Ok(mut policies) = conflict_context.plan_policies.lock()
                    {
                        policies.insert(plan_id, ConflictPolicy::SkipAll);
                    }
                    if apply_to_all {
                        resolve_waiters_for_plan(
                            &conflict_context.waiters,
                            plan_id,
                            ConflictDecision::Skip {
                                apply_to_all: false,
                            },
                        );
                    }
                    Err(cancelled_transfer_error())
                }
                ConflictDecision::Rename {
                    new_name,
                    apply_to_all,
                } => {
                    rename_destination(job, &new_name)?;
                    if destination_exists(session, job).await? {
                        return Err(UserFacingError::new(
                            ErrorCode::FileExists,
                            "Destination already exists",
                            "Choose another name and try again.",
                        ));
                    }
                    if apply_to_all && let Ok(mut policies) = conflict_context.plan_policies.lock()
                    {
                        policies.insert(
                            plan_id,
                            ConflictPolicy::RenameAll(RenameStrategy::ExplicitName(new_name)),
                        );
                    }
                    Ok(())
                }
                ConflictDecision::CancelJob => Err(cancelled_transfer_error()),
            }
        }
    }
}

async fn directories_can_merge(
    session: &TransferSession,
    job: &TransferJob,
) -> Result<bool, UserFacingError> {
    match (&job.source, &job.destination) {
        (TransferEndpoint::Local(source), TransferEndpoint::Remote(destination)) => {
            let source_metadata = tokio::fs::symlink_metadata(source.as_str())
                .await
                .map_err(|error| local_transfer_error("Upload Item Unavailable", &error))?;
            if !source_metadata.is_dir() {
                return Ok(false);
            }
            match session.symlink_metadata(destination.as_str()).await {
                Ok(metadata) => Ok(metadata.file_type() == SftpFileType::Dir),
                Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                    Ok(false)
                }
                Err(error) => Err(transfer_error(
                    ErrorCode::Unknown,
                    "Server Folder Unavailable",
                    "The remote destination could not be checked. Try again.",
                    sftp_status_guidance(&error),
                )),
            }
        }
        (TransferEndpoint::Remote(source), TransferEndpoint::Local(destination)) => {
            let source_metadata =
                session
                    .symlink_metadata(source.as_str())
                    .await
                    .map_err(|error| {
                        transfer_error(
                            ErrorCode::Unknown,
                            "Server Item Unavailable",
                            "The remote source could not be checked. Try again.",
                            sftp_status_guidance(&error),
                        )
                    })?;
            if source_metadata.file_type() != SftpFileType::Dir {
                return Ok(false);
            }
            match tokio::fs::symlink_metadata(destination.as_str()).await {
                Ok(metadata) => Ok(metadata.is_dir()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(local_transfer_error("Download Folder Unavailable", &error)),
            }
        }
        _ => Ok(false),
    }
}

async fn conflict_source_details(
    session: &TransferSession,
    job: &TransferJob,
) -> (Option<u64>, Option<Timestamp>) {
    match &job.source {
        TransferEndpoint::Local(source) => match tokio::fs::symlink_metadata(source.as_str()).await
        {
            Ok(metadata) => (
                Some(metadata.len()),
                metadata.modified().ok().map(Timestamp),
            ),
            Err(error) => {
                warn!(error = %error, "could not read local conflict source metadata");
                (None, None)
            }
        },
        TransferEndpoint::Remote(source) => match session.symlink_metadata(source.as_str()).await {
            Ok(metadata) => (
                metadata.size,
                metadata
                    .mtime
                    .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
            ),
            Err(error) => {
                warn!(error = %error, "could not read remote conflict source metadata");
                (None, None)
            }
        },
    }
}

async fn destination_exists(
    session: &TransferSession,
    job: &TransferJob,
) -> Result<bool, UserFacingError> {
    match &job.destination {
        TransferEndpoint::Remote(destination) => match session.metadata(destination.as_str()).await
        {
            Ok(_) => Ok(true),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(false)
            }
            Err(error) => Err(transfer_error(
                ErrorCode::Unknown,
                "Server Destination Unavailable",
                "The remote destination could not be checked. Try again.",
                sftp_status_guidance(&error),
            )),
        },
        TransferEndpoint::Local(destination) => {
            match tokio::fs::symlink_metadata(destination.as_str()).await {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(local_transfer_error(
                    "Download Destination Unavailable",
                    &error,
                )),
            }
        }
    }
}

fn rename_destination(job: &mut TransferJob, new_name: &str) -> Result<(), UserFacingError> {
    if new_name.is_empty() || new_name == "." || new_name == ".." || new_name.contains('/') {
        return Err(UserFacingError::new(
            ErrorCode::NotFound,
            "Invalid destination name",
            "Choose a non-empty file name without path separators or parent-directory names.",
        ));
    }
    match &mut job.destination {
        TransferEndpoint::Remote(path) => {
            let parent = path
                .as_str()
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("");
            *path = RemotePath::new(if parent.is_empty() {
                new_name.to_string()
            } else {
                format!("{parent}/{new_name}")
            });
        }
        TransferEndpoint::Local(path) => {
            let parent = std::path::Path::new(path.as_str())
                .parent()
                .unwrap_or_else(|| std::path::Path::new(""));
            *path = macsftp_core::LocalPath::new(parent.join(new_name).display().to_string());
        }
    }
    Ok(())
}

fn copy_name_for_destination(destination: &TransferEndpoint) -> String {
    let path = match destination {
        TransferEndpoint::Local(path) => path.as_str(),
        TransferEndpoint::Remote(path) => path.as_str(),
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} (copy).{extension}"),
        _ => format!("{name} (copy)"),
    }
}

async fn upload_transfer(
    transfer_session: &TransferSessionSlot,
    sftp: &SftpSession,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    connection_key: &str,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Local(source), TransferEndpoint::Remote(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid upload job",
            "The upload source or destination is invalid.",
        ));
    };
    let metadata = tokio::fs::symlink_metadata(source.as_str())
        .await
        .map_err(|error| local_transfer_error("Upload Item Unavailable", &error))?;
    if metadata.file_type().is_symlink() {
        return upload_symlink(transfer_session, sftp, job, event_tx, cancel).await;
    }
    if metadata.is_dir() {
        emit_transfer_running(event_tx, job, Some(0)).await?;
        create_remote_directory(transfer_session, sftp, destination, cancel).await?;
        preserve_upload_metadata(sftp, job, &metadata, event_tx).await;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Upload source is not a file",
            "Choose a regular file or directory to upload.",
        ));
    }
    let bytes_total = Some(metadata.len());
    emit_transfer_running(event_tx, job, bytes_total).await?;

    ensure_remote_parent_directory(transfer_session, sftp, destination, cancel).await?;
    let temporary_destination = remote_temporary_path(destination, job.id);
    let mut destination_file = match sftp
        .open_with_flags(
            temporary_destination.as_str(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
        )
        .await
    {
        Ok(file) => Ok(file),
        Err(open_err) => {
            if cleanup_remote_temp(sftp, &temporary_destination, event_tx, job.id, None).await {
                sftp.open_with_flags(
                    temporary_destination.as_str(),
                    OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                )
                .await
                .map_err(remote_destination_create_error)
            } else {
                Err(remote_destination_create_error(open_err))
            }
        }
    }?;
    emit_residual_temp_created(
        event_tx,
        ResidualTempRecord {
            connection_key: connection_key.to_string(),
            direction: job.direction,
            transfer_id: job.id,
            path: temporary_destination.as_str().to_string(),
            created_at: Timestamp::from_system_time(std::time::SystemTime::now()),
        },
    )
    .await;
    let copy_result = async {
        let mut source_file = tokio::fs::File::open(source.as_str())
            .await
            .map_err(|error| local_transfer_error("Upload Item Unavailable", &error))?;
        copy_file_chunks(
            &mut source_file,
            &mut destination_file,
            job.id,
            bytes_total,
            event_tx,
            cancel,
        )
        .await
    }
    .await;
    if let Err(error) = copy_result {
        cleanup_remote_temp(sftp, &temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        // Proving the server accepts the atomic hardlink commit costs three
        // round trips; one success is enough for every later file this session
        // commits into the same directory.
        let commit_directory = destination
            .as_str()
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        if !transfer_session.hardlink_commit_confirmed(commit_directory) {
            verify_remote_hardlink(sftp, &temporary_destination, event_tx, job.id).await?;
            transfer_session.confirm_hardlink_commit(commit_directory.to_string());
        }
        remove_remote_destination(sftp, destination).await?;
    }
    if let Err(error) = commit_remote_temp(sftp, &temporary_destination, destination).await {
        cleanup_remote_temp(sftp, &temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    cleanup_remote_temp(sftp, &temporary_destination, event_tx, job.id, Some(job.id)).await;
    preserve_upload_metadata(sftp, job, &metadata, event_tx).await;
    Ok(())
}

fn remote_temporary_path(destination: &RemotePath, transfer_id: TransferId) -> RemotePath {
    let (parent, name) = destination
        .as_str()
        .rsplit_once('/')
        .unwrap_or(("", destination.as_str()));
    let temporary_name = format!(".{name}.macsftp-part-{}", transfer_id.0);
    RemotePath::new(if parent.is_empty() {
        temporary_name
    } else if parent == "/" {
        format!("/{temporary_name}")
    } else {
        format!("{parent}/{temporary_name}")
    })
}

async fn commit_remote_temp(
    sftp: &SftpSession,
    temporary: &RemotePath,
    destination: &RemotePath,
) -> Result<(), UserFacingError> {
    match sftp.hardlink(temporary.as_str(), destination.as_str()).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Atomic upload is not supported",
            "This server does not support the required atomic upload commit. Try another server or reconnect.",
        )
        .with_retryable(true)),
        Err(error) => Err(remote_destination_create_error(error)),
    }
}

async fn verify_remote_hardlink(
    sftp: &SftpSession,
    temporary: &RemotePath,
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
) -> Result<(), UserFacingError> {
    let probe = RemotePath::new(format!("{}.commit-probe", temporary.as_str()));
    if !cleanup_remote_temp(sftp, &probe, event_tx, transfer_id, None).await {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Upload Preparation Failed",
            "A previous temporary upload file could not be removed. Retry the transfer.",
        )
        .with_retryable(true));
    }
    match sftp.hardlink(temporary.as_str(), probe.as_str()).await {
        Ok(true) => {
            if cleanup_remote_temp(sftp, &probe, event_tx, transfer_id, None).await {
                Ok(())
            } else {
                Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Upload Preparation Failed",
                    "A temporary upload file could not be cleaned up. Retry the transfer.",
                )
                .with_retryable(true))
            }
        }
        Ok(false) => Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Atomic upload is not supported",
            "This server does not support the required atomic upload commit. Try another server or reconnect.",
        )
        .with_retryable(true)),
        Err(error) => Err(transfer_error(
            ErrorCode::Unknown,
            "Atomic upload is not supported",
            "The server could not verify the required atomic upload commit. Try again.",
            sftp_status_guidance(&error),
        )),
    }
}

async fn cleanup_remote_temp(
    sftp: &SftpSession,
    temporary: &RemotePath,
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    residual_transfer_id: Option<TransferId>,
) -> bool {
    if !temporary.as_str().contains(".macsftp-part-") {
        warn!(
            path = temporary.as_str(),
            "refusing to remove remote temp file that does not match .macsftp-part- pattern"
        );
        return false;
    }
    let cleaned = match sftp.remove_file(temporary.as_str()).await {
        Ok(()) => true,
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => true,
        Err(error) => {
            emit_temporary_file_warning(
                event_tx,
                transfer_id,
                format!(
                    "Remote temporary file remains at {}: {error}",
                    temporary.as_str()
                ),
            )
            .await;
            false
        }
    };
    if cleaned && let Some(id) = residual_transfer_id {
        emit_residual_temp_cleared(event_tx, id, temporary.as_str()).await;
    }
    cleaned
}

fn local_temporary_path(destination: &LocalPath, transfer_id: TransferId) -> LocalPath {
    let path = std::path::Path::new(destination.as_str());
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "download".into());
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new(""));
    LocalPath::new(
        parent
            .join(format!(".{name}.macsftp-part-{}", transfer_id.0))
            .display()
            .to_string(),
    )
}

async fn cleanup_local_temp(
    temporary: &LocalPath,
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    residual_transfer_id: Option<TransferId>,
) -> bool {
    if !temporary.as_str().contains(".macsftp-part-") {
        warn!(
            path = temporary.as_str(),
            "refusing to remove local temp file that does not match .macsftp-part- pattern"
        );
        return false;
    }
    let cleaned = match tokio::fs::remove_file(temporary.as_str()).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            emit_temporary_file_warning(
                event_tx,
                transfer_id,
                format!(
                    "Local temporary file remains at {}: {error}",
                    temporary.as_str()
                ),
            )
            .await;
            false
        }
    };
    if cleaned && let Some(id) = residual_transfer_id {
        emit_residual_temp_cleared(event_tx, id, temporary.as_str()).await;
    }
    cleaned
}

async fn commit_local_temp(
    temporary: &LocalPath,
    destination: &LocalPath,
) -> Result<(), UserFacingError> {
    tokio::fs::hard_link(temporary.as_str(), destination.as_str())
        .await
        .map_err(local_destination_create_error)
}

async fn verify_local_hardlink(
    temporary: &LocalPath,
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
) -> Result<(), UserFacingError> {
    let probe = LocalPath::new(format!("{}.commit-probe", temporary.as_str()));
    if !cleanup_local_temp(&probe, event_tx, transfer_id, None).await {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Download Preparation Failed",
            "A previous temporary download file could not be removed. Retry the transfer.",
        )
        .with_retryable(true));
    }
    match tokio::fs::hard_link(temporary.as_str(), probe.as_str()).await {
        Ok(()) => {
            if cleanup_local_temp(&probe, event_tx, transfer_id, None).await {
                Ok(())
            } else {
                Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Download Preparation Failed",
                    "A temporary download file could not be cleaned up. Retry the transfer.",
                )
                .with_retryable(true))
            }
        }
        Err(error) => Err(local_transfer_error(
            "Atomic download is not supported",
            &error,
        )),
    }
}

async fn ensure_remote_parent_directory(
    transfer_session: &TransferSessionSlot,
    sftp: &SftpSession,
    destination: &macsftp_core::RemotePath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let Some(parent) = destination
        .as_str()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
    else {
        return Ok(());
    };
    if parent.is_empty() {
        return Ok(());
    }
    create_remote_directory(
        transfer_session,
        sftp,
        &macsftp_core::RemotePath::new(parent),
        cancel,
    )
    .await
}

async fn create_remote_directory(
    transfer_session: &TransferSessionSlot,
    sftp: &SftpSession,
    directory: &macsftp_core::RemotePath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    if transfer_session.directory_verified(directory.as_str()) {
        return Ok(());
    }
    // Every file in a plan asks for a parent the previous files already
    // created. One stat on the full path settles that, where the walk below
    // would stat each component. On a miss the walk still runs unchanged, so
    // the extra failure-path round trip only lands on the first file.
    if !cancel.is_cancelled()
        && let Ok(metadata) = sftp.metadata(directory.as_str()).await
        && metadata.file_type() == SftpFileType::Dir
    {
        transfer_session.mark_directory_verified(directory.as_str().to_string());
        let mut ancestor = if directory.as_str().starts_with('/') {
            "/".to_string()
        } else {
            String::new()
        };
        for component in directory.as_str().split('/').filter(|c| !c.is_empty()) {
            if !ancestor.is_empty() && !ancestor.ends_with('/') {
                ancestor.push('/');
            }
            ancestor.push_str(component);
            transfer_session.mark_directory_verified(ancestor.clone());
        }
        return Ok(());
    }
    let mut current = if directory.as_str().starts_with('/') {
        "/".to_string()
    } else {
        String::new()
    };
    for component in directory
        .as_str()
        .split('/')
        .filter(|component| !component.is_empty())
    {
        if !current.is_empty() && !current.ends_with('/') {
            current.push('/');
        }
        current.push_str(component);
        if cancel.is_cancelled() {
            return Err(cancelled_transfer_error());
        }
        match sftp.metadata(current.clone()).await {
            Ok(metadata) => {
                if metadata.file_type() == SftpFileType::Dir {
                    transfer_session.mark_directory_verified(current.clone());
                }
            }
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                match sftp.create_dir(current.clone()).await {
                    Ok(()) => {
                        transfer_session.mark_directory_verified(current.clone());
                    }
                    Err(SftpError::Status(status)) if status.status_code == StatusCode::Failure => {
                        sftp.metadata(current.clone()).await.map(|_| ()).map_err(|error| {
                            transfer_error(
                                ErrorCode::Unknown,
                                "Server Folder Creation Failed",
                                "The remote directory could not be created. Check permissions and try again.",
                                sftp_status_guidance(&error),
                            )
                        })?;
                    }
                    Err(error) => {
                        return Err(transfer_error(
                            ErrorCode::Unknown,
                            "Server Folder Creation Failed",
                            "The remote directory could not be created. Check permissions and try again.",
                            sftp_status_guidance(&error),
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(transfer_error(
                    ErrorCode::Unknown,
                    "Server Folder Unavailable",
                    "The remote destination could not be checked. Try again.",
                    sftp_status_guidance(&error),
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_remote_parent_directory_raw(
    transfer_session: &TransferSessionSlot,
    raw: &RawSftpSession,
    destination: &macsftp_core::RemotePath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let Some(parent) = destination
        .as_str()
        .rsplit_once('/')
        .map(|(parent, _)| parent)
    else {
        return Ok(());
    };
    if parent.is_empty() {
        return Ok(());
    }
    create_remote_directory_raw(
        transfer_session,
        raw,
        &macsftp_core::RemotePath::new(parent),
        cancel,
    )
    .await
}

async fn create_remote_directory_raw(
    transfer_session: &TransferSessionSlot,
    raw: &RawSftpSession,
    directory: &macsftp_core::RemotePath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    if transfer_session.directory_verified(directory.as_str()) {
        return Ok(());
    }
    // Every file in a plan asks for a parent the previous files already
    // created. One stat on the full path settles that, where the walk below
    // would stat each component. On a miss the walk still runs unchanged, so
    // the extra failure-path round trip only lands on the first file.
    if !cancel.is_cancelled()
        && let Ok(attrs) = raw.stat(directory.as_str().to_string()).await
        && attrs.attrs.file_type() == SftpFileType::Dir
    {
        transfer_session.mark_directory_verified(directory.as_str().to_string());
        let mut ancestor = if directory.as_str().starts_with('/') {
            "/".to_string()
        } else {
            String::new()
        };
        for component in directory.as_str().split('/').filter(|c| !c.is_empty()) {
            if !ancestor.is_empty() && !ancestor.ends_with('/') {
                ancestor.push('/');
            }
            ancestor.push_str(component);
            transfer_session.mark_directory_verified(ancestor.clone());
        }
        return Ok(());
    }
    let mut current = if directory.as_str().starts_with('/') {
        "/".to_string()
    } else {
        String::new()
    };
    let components = directory
        .as_str()
        .split('/')
        .filter(|component| !component.is_empty());
    for component in components {
        if cancel.is_cancelled() {
            return Err(cancelled_transfer_error());
        }
        if !current.is_empty() && !current.ends_with('/') {
            current.push('/');
        }
        current.push_str(component);
        if transfer_session.directory_verified(&current) {
            continue;
        }
        if cancel.is_cancelled() {
            return Err(cancelled_transfer_error());
        }
        match raw.stat(current.clone()).await {
            Ok(attrs) => {
                if attrs.attrs.file_type() == SftpFileType::Dir {
                    transfer_session.mark_directory_verified(current.clone());
                }
            }
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                match raw.mkdir(current.clone(), FileAttributes::empty()).await {
                    Ok(_) => {
                        transfer_session.mark_directory_verified(current.clone());
                    }
                    Err(SftpError::Status(status)) if status.status_code == StatusCode::Failure => {
                        raw.stat(current.clone()).await.map(|_| ()).map_err(|error| {
                            transfer_error(
                                ErrorCode::Unknown,
                                "Server Folder Creation Failed",
                                "The remote directory could not be created. Check permissions and try again.",
                                sftp_status_guidance(&error),
                            )
                        })?;
                        transfer_session.mark_directory_verified(current.clone());
                    }
                    Err(error) => {
                        return Err(transfer_error(
                            ErrorCode::Unknown,
                            "Server Folder Creation Failed",
                            "The remote directory could not be created. Check permissions and try again.",
                            sftp_status_guidance(&error),
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(transfer_error(
                    ErrorCode::Unknown,
                    "Server Folder Unavailable",
                    "The remote destination could not be checked. Try again.",
                    sftp_status_guidance(&error),
                ));
            }
        }
    }
    Ok(())
}

async fn cleanup_remote_temp_raw(
    raw: &RawSftpSession,
    temporary: &RemotePath,
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    residual_transfer_id: Option<TransferId>,
) -> bool {
    let cleaned = match raw.remove(temporary.as_str()).await {
        Ok(_) => true,
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => true,
        Err(error) => {
            emit_temporary_file_warning(
                event_tx,
                transfer_id,
                format!(
                    "Remote temporary file remains at {}: {error}",
                    temporary.as_str()
                ),
            )
            .await;
            false
        }
    };
    if cleaned && let Some(id) = residual_transfer_id {
        emit_residual_temp_cleared(event_tx, id, temporary.as_str()).await;
    }
    cleaned
}

async fn commit_remote_temp_raw(
    raw: &RawSftpSession,
    temporary: &RemotePath,
    destination: &RemotePath,
) -> Result<(), UserFacingError> {
    match raw.hardlink(temporary.as_str(), destination.as_str()).await {
        Ok(_) => Ok(()),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::OpUnsupported => {
            Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Atomic upload is not supported",
                "This server does not support the required atomic upload commit. Try another server or reconnect.",
            )
            .with_retryable(true))
        }
        Err(error) => Err(remote_destination_create_error(error)),
    }
}

async fn verify_remote_hardlink_raw(
    raw: &RawSftpSession,
    temporary: &RemotePath,
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
) -> Result<(), UserFacingError> {
    let probe = RemotePath::new(format!("{}.commit-probe", temporary.as_str()));
    if !cleanup_remote_temp_raw(raw, &probe, event_tx, transfer_id, None).await {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Upload Preparation Failed",
            "A previous temporary upload file could not be removed. Retry the transfer.",
        )
        .with_retryable(true));
    }
    match raw.hardlink(temporary.as_str(), probe.as_str()).await {
        Ok(_) => {
            if cleanup_remote_temp_raw(raw, &probe, event_tx, transfer_id, None).await {
                Ok(())
            } else {
                Err(UserFacingError::new(
                    ErrorCode::Unknown,
                    "Upload Preparation Failed",
                    "A temporary upload file could not be cleaned up. Retry the transfer.",
                )
                .with_retryable(true))
            }
        }
        Err(SftpError::Status(status)) if status.status_code == StatusCode::OpUnsupported => {
            Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Atomic upload is not supported",
                "This server does not support the required atomic upload commit. Try another server or reconnect.",
            )
            .with_retryable(true))
        }
        Err(error) => Err(transfer_error(
            ErrorCode::Unknown,
            "Atomic upload is not supported",
            "The server could not verify the required atomic upload commit. Try again.",
            sftp_status_guidance(&error),
        )),
    }
}

async fn remove_remote_destination_raw(
    raw: &RawSftpSession,
    destination: &RemotePath,
) -> Result<(), UserFacingError> {
    match raw.remove(destination.as_str()).await {
        Ok(_) => Ok(()),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(()),
        Err(error) => Err(transfer_error(
            ErrorCode::Unknown,
            "Server File Replacement Failed",
            "The existing remote file could not be removed. Check permissions and try again.",
            sftp_status_guidance(&error),
        )),
    }
}

async fn preserve_upload_metadata_raw(
    raw: &RawSftpSession,
    job: &TransferJob,
    source_metadata: &std::fs::Metadata,
    event_tx: &flume::Sender<AppEvent>,
) {
    let TransferEndpoint::Remote(destination) = &job.destination else {
        return;
    };
    let mut attributes = FileAttributes::empty();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if job.metadata_policy.preserve_permissions {
            attributes.permissions = Some(source_metadata.mode());
        }
    }
    if job.metadata_policy.preserve_mtime {
        attributes.mtime = source_metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| u32::try_from(duration.as_secs()).ok());
    }
    if attributes.permissions.is_none() && attributes.mtime.is_none() {
        return;
    }
    if let Err(error) = raw.setstat(destination.as_str(), attributes).await {
        emit_metadata_warning(event_tx, job.id, error.to_string()).await;
    }
}

async fn upload_symlink_raw(
    transfer_session: &TransferSessionSlot,
    raw: &RawSftpSession,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Local(source), TransferEndpoint::Remote(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Invalid symlink upload",
            "The symlink source or remote destination is invalid.",
        ));
    };
    emit_transfer_running(event_tx, job, Some(0)).await?;
    if cancel.is_cancelled() {
        return Err(cancelled_transfer_error());
    }
    let target = tokio::fs::read_link(source.as_str())
        .await
        .map_err(|error| local_transfer_error("Link Destination Unavailable", &error))?;
    ensure_remote_parent_directory_raw(transfer_session, raw, destination, cancel).await?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_remote_destination_raw(raw, destination).await?;
    }
    raw.symlink(target.display().to_string(), destination.as_str())
        .await
        .map(|_| ())
        .map_err(|error| {
            transfer_error(
                ErrorCode::UnsupportedSymlink,
                "Server Link Creation Failed",
                "The remote symlink could not be created. Check server support and try again.",
                sftp_status_guidance(&error),
            )
        })
}

async fn upload_single_file_pipelined(
    transfer_session: &TransferSessionSlot,
    raw: &Arc<RawSftpSession>,
    chunk_size: u32,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    connection_key: &str,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Local(source), TransferEndpoint::Remote(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid upload job",
            "The upload source or destination is invalid.",
        ));
    };
    let metadata = tokio::fs::symlink_metadata(source.as_str())
        .await
        .map_err(|error| local_transfer_error("Upload Item Unavailable", &error))?;
    if metadata.file_type().is_symlink() {
        return upload_symlink_raw(transfer_session, raw, job, event_tx, cancel).await;
    }
    if metadata.is_dir() {
        emit_transfer_running(event_tx, job, Some(0)).await?;
        create_remote_directory_raw(transfer_session, raw, destination, cancel).await?;
        preserve_upload_metadata_raw(raw, job, &metadata, event_tx).await;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Upload source is not a file",
            "Choose a regular file or directory to upload.",
        ));
    }
    let bytes_total = Some(metadata.len());
    emit_transfer_running(event_tx, job, bytes_total).await?;

    ensure_remote_parent_directory_raw(transfer_session, raw, destination, cancel).await?;
    let temporary_destination = remote_temporary_path(destination, job.id);
    let handle_packet = match raw
        .open(
            temporary_destination.as_str(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            FileAttributes::empty(),
        )
        .await
    {
        Ok(packet) => Ok(packet),
        Err(open_err) => {
            // A previous attempt of this job may have left a temporary file.
            // Clean it up and retry opening.
            if cleanup_remote_temp_raw(raw, &temporary_destination, event_tx, job.id, None).await {
                raw.open(
                    temporary_destination.as_str(),
                    OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                    FileAttributes::empty(),
                )
                .await
                .map_err(remote_destination_create_error)
            } else {
                Err(remote_destination_create_error(open_err))
            }
        }
    }?;
    let handle = handle_packet.handle;
    emit_residual_temp_created(
        event_tx,
        ResidualTempRecord {
            connection_key: connection_key.to_string(),
            direction: job.direction,
            transfer_id: job.id,
            path: temporary_destination.as_str().to_string(),
            created_at: Timestamp::from_system_time(std::time::SystemTime::now()),
        },
    )
    .await;
    let copy_result = async {
        let mut source_file = tokio::fs::File::open(source.as_str())
            .await
            .map_err(|error| local_transfer_error("Upload Item Unavailable", &error))?;

        let writer = crate::pipelined_writer::PipelinedUploadWriter::new(
            raw.clone(),
            handle.clone(),
            bytes_total,
        )
        .with_chunk_size(chunk_size)
        .with_window_capacity(crate::pipelined_writer::optimal_window_capacity(chunk_size));

        let upload_result = writer
            .upload_from(&mut source_file, job.id, event_tx, cancel)
            .await;

        if let Err(close_error) = raw.close(handle).await {
            warn!(error = %close_error, "failed to close remote file handle after upload");
            if upload_result.is_ok() {
                return Err(transfer_error(
                    ErrorCode::Unknown,
                    "Transfer Write Failed",
                    "The remote file could not be closed after writing.",
                    sftp_status_guidance(&close_error),
                ));
            }
        }
        upload_result.map(|_| ())
    }
    .await;
    if let Err(error) = copy_result {
        cleanup_remote_temp_raw(raw, &temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        let commit_directory = destination
            .as_str()
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        if !transfer_session.hardlink_commit_confirmed(commit_directory) {
            verify_remote_hardlink_raw(raw, &temporary_destination, event_tx, job.id).await?;
            transfer_session.confirm_hardlink_commit(commit_directory.to_string());
        }
        remove_remote_destination_raw(raw, destination).await?;
    }
    if let Err(error) = commit_remote_temp_raw(raw, &temporary_destination, destination).await {
        cleanup_remote_temp_raw(raw, &temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    cleanup_remote_temp_raw(raw, &temporary_destination, event_tx, job.id, Some(job.id)).await;
    preserve_upload_metadata_raw(raw, job, &metadata, event_tx).await;
    Ok(())
}

async fn download_symlink_raw(
    transfer_session: &TransferSessionSlot,
    raw: &Arc<RawSftpSession>,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Remote(source), TransferEndpoint::Local(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Invalid symlink download",
            "The remote symlink source or local destination is invalid.",
        ));
    };
    emit_transfer_running(event_tx, job, Some(0)).await?;
    if cancel.is_cancelled() {
        return Err(cancelled_transfer_error());
    }
    let session = TransferSession::Raw {
        session: Arc::clone(raw),
        chunk_size: 0,
        direction: TransferDirection::Download,
    };
    let target = session.read_link(source.as_str()).await.map_err(|error| {
        transfer_error(
            ErrorCode::UnsupportedSymlink,
            "Server Link Unavailable",
            "The remote symlink target could not be read. Check server support and try again.",
            sftp_status_guidance(&error),
        )
    })?;
    validate_download_symlink_target(
        transfer_session.destination_root().as_ref(),
        destination,
        &target,
    )?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_downloaded_file_destination(destination.as_str()).await?;
    }
    ensure_local_parent_directory(transfer_session, destination, cancel).await?;
    tokio::fs::symlink(target, destination.as_str())
        .await
        .map_err(|error| local_transfer_error("Local Link Creation Failed", &error))
}

async fn download_single_file_pipelined(
    transfer_session: &TransferSessionSlot,
    raw: &Arc<RawSftpSession>,
    chunk_size: u32,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    may_clear_destination: bool,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Remote(source), TransferEndpoint::Local(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid download job",
            "The download source or destination is invalid.",
        ));
    };
    let source_metadata = raw
        .lstat(source.as_str())
        .await
        .map(|attrs| attrs.attrs)
        .map_err(|error| {
            transfer_error(
                ErrorCode::Unknown,
                "Server File Unavailable",
                "The remote file could not be read. Check permissions and try again.",
                sftp_status_guidance(&error),
            )
        })?;
    if source_metadata.file_type() == SftpFileType::Symlink {
        return download_symlink_raw(transfer_session, raw, job, event_tx, cancel).await;
    }
    if source_metadata.file_type() == SftpFileType::Dir {
        return download_directory(
            transfer_session,
            job,
            &source_metadata,
            event_tx,
            cancel,
            may_clear_destination,
        )
        .await;
    }
    let bytes_total = source_metadata.size;
    emit_transfer_running(event_tx, job, bytes_total).await?;

    ensure_local_parent_directory(transfer_session, destination, cancel).await?;
    let temporary_destination = local_temporary_path(destination, job.id);
    let mut destination_file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary_destination.as_str())
        .await
    {
        Ok(file) => Ok(file),
        Err(open_err) if open_err.kind() == std::io::ErrorKind::AlreadyExists => {
            // A previous attempt of this job may have left a temporary file.
            // Clean it up and retry opening.
            if cleanup_local_temp(&temporary_destination, event_tx, job.id, None).await {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(temporary_destination.as_str())
                    .await
                    .map_err(local_destination_create_error)
            } else {
                Err(local_destination_create_error(open_err))
            }
        }
        Err(open_err) => Err(local_destination_create_error(open_err)),
    }?;
    emit_residual_temp_created(
        event_tx,
        ResidualTempRecord {
            connection_key: "local".to_string(),
            direction: job.direction,
            transfer_id: job.id,
            path: temporary_destination.as_str().to_string(),
            created_at: Timestamp::from_system_time(std::time::SystemTime::now()),
        },
    )
    .await;
    if let Some(total) = bytes_total
        && total > 0
        && let Err(error) = destination_file.set_len(total).await
    {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(local_transfer_error("Download Preparation Failed", &error));
    }
    let copy_result = async {
        let handle_packet = raw
            .open(source.as_str(), OpenFlags::READ, FileAttributes::empty())
            .await
            .map_err(|error| {
                transfer_error(
                    ErrorCode::Unknown,
                    "Server File Unavailable",
                    "The remote file could not be opened. Check permissions and try again.",
                    sftp_status_guidance(&error),
                )
            })?;
        let handle = handle_packet.handle;
        let reader = crate::pipelined_reader::PipelinedDownloadReader::new(
            raw.clone(),
            handle.clone(),
            bytes_total,
        )
        .with_chunk_size(chunk_size)
        .with_window_capacity(crate::pipelined_reader::optimal_window_capacity(chunk_size));
        let result = reader
            .download_to(&mut destination_file, job.id, event_tx, cancel)
            .await;
        if let Err(close_error) = raw.close(handle).await {
            warn!(error = %close_error, "failed to close remote file handle");
        }
        result
    }
    .await;
    if let Err(error) = copy_result {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    prepare_download_destination(
        transfer_session,
        job,
        destination,
        &temporary_destination,
        event_tx,
    )
    .await?;
    if let Err(error) = commit_local_temp(&temporary_destination, destination).await {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
    preserve_download_metadata(job, &source_metadata, event_tx).await;
    Ok(())
}

async fn download_single_file(
    transfer_session: &TransferSessionSlot,
    sftp: &SftpSession,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    may_clear_destination: bool,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Remote(source), TransferEndpoint::Local(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid download job",
            "The download source or destination is invalid.",
        ));
    };
    let source_metadata = sftp
        .symlink_metadata(source.as_str())
        .await
        .map_err(|error| {
            transfer_error(
                ErrorCode::Unknown,
                "Server File Unavailable",
                "The remote file could not be read. Check permissions and try again.",
                sftp_status_guidance(&error),
            )
        })?;
    if source_metadata.file_type() == SftpFileType::Symlink {
        return download_symlink(transfer_session, sftp, job, event_tx, cancel).await;
    }
    if source_metadata.file_type() == SftpFileType::Dir {
        return download_directory(
            transfer_session,
            job,
            &source_metadata,
            event_tx,
            cancel,
            may_clear_destination,
        )
        .await;
    }
    let bytes_total = source_metadata.size;
    emit_transfer_running(event_tx, job, bytes_total).await?;

    ensure_local_parent_directory(transfer_session, destination, cancel).await?;
    let temporary_destination = local_temporary_path(destination, job.id);
    let mut destination_file = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary_destination.as_str())
        .await
    {
        Ok(file) => Ok(file),
        Err(open_err) if open_err.kind() == std::io::ErrorKind::AlreadyExists => {
            if cleanup_local_temp(&temporary_destination, event_tx, job.id, None).await {
                tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(temporary_destination.as_str())
                    .await
                    .map_err(local_destination_create_error)
            } else {
                Err(local_destination_create_error(open_err))
            }
        }
        Err(open_err) => Err(local_destination_create_error(open_err)),
    }?;
    emit_residual_temp_created(
        event_tx,
        ResidualTempRecord {
            connection_key: "local".to_string(),
            direction: job.direction,
            transfer_id: job.id,
            path: temporary_destination.as_str().to_string(),
            created_at: Timestamp::from_system_time(std::time::SystemTime::now()),
        },
    )
    .await;
    if let Some(total) = bytes_total
        && total > 0
        && let Err(error) = destination_file.set_len(total).await
    {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(local_transfer_error("Download Preparation Failed", &error));
    }
    let copy_result = async {
        let mut source_file = sftp.open(source.as_str()).await.map_err(|error| {
            transfer_error(
                ErrorCode::Unknown,
                "Server File Unavailable",
                "The remote file could not be opened. Check permissions and try again.",
                sftp_status_guidance(&error),
            )
        })?;
        copy_file_chunks(
            &mut source_file,
            &mut destination_file,
            job.id,
            bytes_total,
            event_tx,
            cancel,
        )
        .await
    }
    .await;
    if let Err(error) = copy_result {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    prepare_download_destination(
        transfer_session,
        job,
        destination,
        &temporary_destination,
        event_tx,
    )
    .await?;
    if let Err(error) = commit_local_temp(&temporary_destination, destination).await {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
    preserve_download_metadata(job, &source_metadata, event_tx).await;
    Ok(())
}

/// Validate and clear the destination right before a downloaded temporary
/// file is committed.
///
/// Both checks can fail after the temporary file has been written, so the
/// temporary file is removed first: a leftover `.macsftp-part` would otherwise
/// only be reclaimed by a later transfer to the same destination. Removing it
/// also updates the residual-temp record, so the failed job leaves nothing
/// behind.
async fn prepare_download_destination(
    transfer_session: &TransferSessionSlot,
    job: &TransferJob,
    destination: &LocalPath,
    temporary_destination: &LocalPath,
    event_tx: &flume::Sender<AppEvent>,
) -> Result<(), UserFacingError> {
    if !matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        return Ok(());
    }
    let commit_directory = std::path::Path::new(destination.as_str())
        .parent()
        .and_then(|parent| parent.to_str())
        .unwrap_or("");
    if !transfer_session.hardlink_commit_confirmed(commit_directory) {
        if let Err(error) = verify_local_hardlink(temporary_destination, event_tx, job.id).await {
            cleanup_local_temp(temporary_destination, event_tx, job.id, Some(job.id)).await;
            return Err(error);
        }
        transfer_session.confirm_hardlink_commit(commit_directory.to_string());
    }
    if let Err(error) = remove_downloaded_file_destination(destination.as_str()).await {
        cleanup_local_temp(temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    Ok(())
}

async fn download_directory(
    transfer_session: &TransferSessionSlot,
    job: &TransferJob,
    source_metadata: &FileAttributes,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    may_clear_destination: bool,
) -> Result<(), UserFacingError> {
    let TransferEndpoint::Local(destination) = &job.destination else {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid download directory",
            "The download destination is invalid.",
        ));
    };
    emit_transfer_running(event_tx, job, Some(0)).await?;
    if cancel.is_cancelled() {
        return Err(cancelled_transfer_error());
    }
    // Only a root directory job that runs before any child has started clears
    // the destination. A nested directory job shares the freshly created tree
    // with concurrently running children, and a retried root must not delete
    // output that already completed.
    if may_clear_destination && matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_local_destination(destination.as_str()).await?;
    }
    if !transfer_session.directory_verified(destination.as_str()) {
        tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = tokio::fs::create_dir_all(destination.as_str()) => result.map_err(|error| {
                local_transfer_error("Download Folder Creation Failed", &error)
            })?,
        }
        let dest_path = std::path::Path::new(destination.as_str());
        let mut curr: &std::path::Path = dest_path;
        while !curr.as_os_str().is_empty() {
            let curr_str = curr.to_string_lossy();
            if transfer_session.directory_verified(&curr_str) {
                break;
            }
            transfer_session.mark_directory_verified(curr_str.to_string());
            match curr.parent() {
                Some(p) => curr = p,
                None => break,
            }
        }
    }
    preserve_download_metadata(job, source_metadata, event_tx).await;
    Ok(())
}

async fn upload_symlink(
    transfer_session: &TransferSessionSlot,
    sftp: &SftpSession,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Local(source), TransferEndpoint::Remote(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Invalid symlink upload",
            "The symlink source or remote destination is invalid.",
        ));
    };
    emit_transfer_running(event_tx, job, Some(0)).await?;
    if cancel.is_cancelled() {
        return Err(cancelled_transfer_error());
    }
    let target = tokio::fs::read_link(source.as_str())
        .await
        .map_err(|error| local_transfer_error("Link Destination Unavailable", &error))?;
    ensure_remote_parent_directory(transfer_session, sftp, destination, cancel).await?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_remote_destination(sftp, destination).await?;
    }
    // OpenSSH's sftp-server treats the *first* SSH_FXP_SYMLINK argument as
    // the symlink target and the *second* as the link location (the
    // long-standing OpenSSH argument swap), so pass (target, link) here
    // even though russh's `symlink(path, target)` parameter names suggest
    // the reverse. Passing (destination, target) would create the link at
    // the target string resolved against the server's home directory.
    sftp.symlink(target.display().to_string(), destination.as_str())
        .await
        .map_err(|error| {
            transfer_error(
                ErrorCode::UnsupportedSymlink,
                "Server Link Creation Failed",
                "The remote symlink could not be created. Check server support and try again.",
                sftp_status_guidance(&error),
            )
        })
}

async fn download_symlink(
    transfer_session: &TransferSessionSlot,
    sftp: &SftpSession,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let (TransferEndpoint::Remote(source), TransferEndpoint::Local(destination)) =
        (&job.source, &job.destination)
    else {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Invalid symlink download",
            "The remote symlink source or local destination is invalid.",
        ));
    };
    emit_transfer_running(event_tx, job, Some(0)).await?;
    if cancel.is_cancelled() {
        return Err(cancelled_transfer_error());
    }
    let target = sftp.read_link(source.as_str()).await.map_err(|error| {
        transfer_error(
            ErrorCode::UnsupportedSymlink,
            "Server Link Unavailable",
            "The remote symlink target could not be read. Check server support and try again.",
            sftp_status_guidance(&error),
        )
    })?;
    validate_download_symlink_target(
        transfer_session.destination_root().as_ref(),
        destination,
        &target,
    )?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_downloaded_file_destination(destination.as_str()).await?;
    }
    ensure_local_parent_directory(transfer_session, destination, cancel).await?;
    tokio::fs::symlink(target, destination.as_str())
        .await
        .map_err(|error| local_transfer_error("Local Link Creation Failed", &error))
}

async fn preserve_upload_metadata(
    sftp: &SftpSession,
    job: &TransferJob,
    source_metadata: &std::fs::Metadata,
    event_tx: &flume::Sender<AppEvent>,
) {
    let TransferEndpoint::Remote(destination) = &job.destination else {
        return;
    };
    let mut attributes = FileAttributes::empty();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if job.metadata_policy.preserve_permissions {
            // The full mode is kept on the way out, unlike the download path,
            // which strips the special bits. The asymmetry is deliberate: an
            // upload carries the user's own file to their own server, so its
            // mode is intent worth preserving; a download carries bytes from a
            // server into the local disk, where an attacker-chosen setuid bit
            // has no business being created on the user's behalf.
            attributes.permissions = Some(source_metadata.mode());
        }
    }
    if job.metadata_policy.preserve_mtime {
        attributes.mtime = source_metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| u32::try_from(duration.as_secs()).ok());
    }
    if attributes.permissions.is_none() && attributes.mtime.is_none() {
        return;
    }
    if let Err(error) = sftp.set_metadata(destination.as_str(), attributes).await {
        emit_metadata_warning(event_tx, job.id, error.to_string()).await;
    }
}

async fn remove_remote_destination(
    sftp: &SftpSession,
    destination: &RemotePath,
) -> Result<(), UserFacingError> {
    match sftp.remove_file(destination.as_str()).await {
        Ok(()) => Ok(()),
        Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(()),
        Err(error) => Err(transfer_error(
            ErrorCode::Unknown,
            "Server File Replacement Failed",
            "The existing remote file could not be removed. Check permissions and try again.",
            sftp_status_guidance(&error),
        )),
    }
}

fn remote_destination_create_error(error: SftpError) -> UserFacingError {
    let code = match &error {
        SftpError::Status(status) if status.status_code == StatusCode::PermissionDenied => {
            ErrorCode::PermissionDenied
        }
        SftpError::Status(status) if status.status_code == StatusCode::Failure => {
            ErrorCode::FileExists
        }
        _ => ErrorCode::Unknown,
    };
    transfer_error(
        code,
        "Server File Creation Failed",
        "The remote destination already exists or could not be created. Refresh and try again.",
        sftp_status_guidance(&error),
    )
}

/// Replace an existing file or symlink at `destination`.
///
/// A directory at that path is always refused, never recursively deleted. The
/// plan-root clear permission (`may_clear_destination`) exists so a *folder*
/// download can own its destination tree; it must not authorize a *file* or
/// *link* job to delete an unrelated local directory that merely shares the
/// name. The conflict modal only reports "A file with this name already
/// exists", so proceeding would delete data the user never agreed to lose.
async fn remove_downloaded_file_destination(destination: &str) -> Result<(), UserFacingError> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(destination).await
        && metadata.is_dir()
    {
        return Err(UserFacingError::new(
            ErrorCode::FileExists,
            "Destination Changed Type",
            "The destination path is a folder, but the server item is a file or link. Resolve the conflict manually.",
        ));
    }
    remove_local_destination(destination).await
}

async fn remove_local_destination(destination: &str) -> Result<(), UserFacingError> {
    match tokio::fs::symlink_metadata(destination).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(destination)
            .await
            .map_err(|error| local_transfer_error("Downloaded File Replacement Failed", &error)),
        Ok(_) => tokio::fs::remove_file(destination)
            .await
            .map_err(|error| local_transfer_error("Downloaded File Replacement Failed", &error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(local_transfer_error(
            "Download Destination Unavailable",
            &error,
        )),
    }
}

async fn ensure_local_parent_directory(
    transfer_session: &TransferSessionSlot,
    destination: &LocalPath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let Some(parent) = std::path::Path::new(destination.as_str()).parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let parent_str = parent.to_string_lossy();
    if transfer_session.directory_verified(&parent_str) {
        return Ok(());
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(cancelled_transfer_error()),
        result = tokio::fs::create_dir_all(parent) => {
            result.map_err(|error| {
                local_transfer_error("Download Folder Creation Failed", &error)
            })?;
            let mut curr = parent;
            while !curr.as_os_str().is_empty() {
                let curr_str = curr.to_string_lossy();
                if transfer_session.directory_verified(&curr_str) {
                    break;
                }
                transfer_session.mark_directory_verified(curr_str.to_string());
                match curr.parent() {
                    Some(p) => curr = p,
                    None => break,
                }
            }
            Ok(())
        }
    }
}

fn local_destination_create_error(error: std::io::Error) -> UserFacingError {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return UserFacingError::new(
            ErrorCode::FileExists,
            "Download destination already exists",
            "The local destination appeared while the transfer was starting. Try again.",
        )
        .with_retryable(true);
    }
    local_transfer_error("Downloaded File Creation Failed", &error)
}

async fn preserve_download_metadata(
    job: &TransferJob,
    source_metadata: &FileAttributes,
    event_tx: &flume::Sender<AppEvent>,
) {
    let TransferEndpoint::Local(destination) = &job.destination else {
        return;
    };
    let result = async {
        if job.metadata_policy.preserve_permissions
            && let Some(permissions) = source_metadata.permissions
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let is_dir = source_metadata.file_type() == SftpFileType::Dir
                    || tokio::fs::metadata(destination.as_str())
                        .await
                        .map(|m| m.is_dir())
                        .unwrap_or(false);
                // Strip special bits (SUID 0o4000, SGID 0o2000, sticky 0o1000)
                // to prevent privilege escalation or security risks.
                let safe_permissions = permissions & 0o777;
                let mode = if is_dir {
                    // OpenSSH/POSIX sftp semantics: directories must retain
                    // owner read/write/execute (0o700) so files inside them
                    // can be written during download and manipulated locally.
                    safe_permissions | 0o700
                } else {
                    safe_permissions
                };

                tokio::fs::set_permissions(
                    destination.as_str(),
                    std::fs::Permissions::from_mode(mode),
                )
                .await?;
            }
        }
        if job.metadata_policy.preserve_mtime
            && let Some(mtime) = source_metadata.mtime
        {
            let destination = destination.as_str().to_string();
            tokio::task::spawn_blocking(move || {
                let file = std::fs::File::open(destination)?;
                file.set_times(std::fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime.into()),
                ))
            })
            .await
            .map_err(std::io::Error::other)??;
        }
        Ok::<(), std::io::Error>(())
    }
    .await;
    if let Err(error) = result {
        emit_metadata_warning(event_tx, job.id, error.to_string()).await;
    }
}

async fn emit_metadata_warning(
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: macsftp_core::TransferId,
    detail: String,
) {
    emit_transfer_warning(
        event_tx,
        transfer_id,
        ErrorCode::MetadataPreservationFailed,
        "File data transferred, but metadata could not be preserved.",
        detail,
    )
    .await;
}

pub(crate) async fn emit_temporary_file_warning(
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    detail: String,
) {
    emit_transfer_warning(
        event_tx,
        transfer_id,
        ErrorCode::Unknown,
        "Temporary transfer file could not be removed.",
        detail,
    )
    .await;
}

/// Record that a temporary `.macsftp-part-*` file now exists for an
/// in-flight transfer, so a crash/hard-kill can be reconciled on the next
/// launch (current architecture §8).
async fn emit_residual_temp_created(
    event_tx: &flume::Sender<AppEvent>,
    record: ResidualTempRecord,
) {
    if let Err(error) = event_tx
        .send_async(AppEvent::ResidualTempCreated(record))
        .await
    {
        warn!(error = %error, "residual temp created event dropped");
    }
}

/// Report that a temporary `.macsftp-part-*` file was successfully
/// removed, so its residual record can be dropped.
async fn emit_residual_temp_cleared(
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    path: &str,
) {
    if let Err(error) = event_tx
        .send_async(AppEvent::ResidualTempCleared {
            transfer_id,
            path: path.to_string(),
        })
        .await
    {
        warn!(error = %error, "residual temp cleared event dropped");
    }
}

async fn emit_transfer_warning(
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: TransferId,
    code: ErrorCode,
    message: &str,
    detail: String,
) {
    let mut warning = TransferWarning::new(transfer_id, code, message);
    warning.detail = Some(detail);
    if let Err(error) = event_tx
        .send_async(AppEvent::TransferWarning(warning))
        .await
    {
        warn!(error = %error, "transfer warning event dropped");
    }
}

async fn copy_file_chunks<R, W>(
    source: &mut R,
    destination: &mut W,
    transfer_id: macsftp_core::TransferId,
    bytes_total: Option<u64>,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    const BUFFER_SIZE: usize = 256 * 1024;
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut bytes_done = 0;
    let mut throttle = crate::ProgressThrottle::new(10);

    loop {
        let bytes_read = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = source.read(&mut buffer) => result.map_err(|error| {
                local_transfer_error("Transfer Read Failed", &error)
            })?,
        };
        if bytes_read == 0 {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = destination.write_all(&buffer[..bytes_read]) => result.map_err(|error| {
                local_transfer_error("Transfer Write Failed", &error)
            })?,
        }
        bytes_done += bytes_read as u64;
        if throttle.should_send(std::time::Instant::now()) {
            emit_transfer_progress(event_tx, transfer_id, bytes_done, bytes_total).await?;
        }
    }
    tokio::select! {
        _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
        result = destination.shutdown() => result.map_err(|error| {
            local_transfer_error("Transfer Couldn’t Finish", &error)
        })?,
    }
    throttle.reset();
    emit_transfer_progress(event_tx, transfer_id, bytes_done, bytes_total).await
}

async fn emit_transfer_running(
    event_tx: &flume::Sender<AppEvent>,
    job: &TransferJob,
    bytes_total: Option<u64>,
) -> Result<(), UserFacingError> {
    let mut running_job = job.clone();
    running_job.state = TransferState::Running {
        bytes_done: 0,
        bytes_total,
        started_at: Timestamp(std::time::SystemTime::now()),
    };
    event_tx
        .send_async(AppEvent::TransferRunning(TransferSnapshot {
            job: running_job,
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

async fn emit_transfer_progress(
    event_tx: &flume::Sender<AppEvent>,
    transfer_id: macsftp_core::TransferId,
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

fn relative_link_dir<'a>(destination_root: &'a Path, destination: &'a Path) -> Option<&'a Path> {
    if destination == destination_root {
        return Some(Path::new(""));
    }
    let relative = destination.strip_prefix(destination_root).ok()?;
    if relative.as_os_str().is_empty() {
        Some(Path::new(""))
    } else {
        relative.parent()
    }
}

fn symlink_target_escapes_root(relative_dir: &Path, target: &str) -> bool {
    let mut stack: Vec<&str> = Vec::new();
    for component in relative_dir.components() {
        match component {
            Component::Normal(name) => {
                if let Some(segment) = name.to_str() {
                    stack.push(segment);
                } else {
                    return true;
                }
            }
            Component::CurDir => {}
            _ => return true,
        }
    }

    for component in target.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            if stack.pop().is_none() {
                return true;
            }
        } else {
            stack.push(component);
        }
    }

    false
}

fn validate_download_symlink_target(
    destination_root: Option<&LocalPath>,
    destination: &LocalPath,
    target: &str,
) -> Result<(), UserFacingError> {
    let is_windows_drive_prefix = target.len() >= 2
        && target.as_bytes()[1] == b':'
        && target.as_bytes()[0].is_ascii_alphabetic();

    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.starts_with('~')
        || target.contains('\\')
        || is_windows_drive_prefix
        || target.chars().any(is_bidi_or_control)
    {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Unsafe Link Target",
            "The remote symlink target cannot be recreated safely inside the download folder.",
        ));
    }

    let dest_path = Path::new(destination.as_str());
    let root_path = destination_root
        .map(|root| Path::new(root.as_str()))
        .unwrap_or(dest_path);

    let Some(relative_dir) = relative_link_dir(root_path, dest_path) else {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Unsafe Link Target",
            "The remote symlink target cannot be recreated safely inside the download folder.",
        ));
    };

    if symlink_target_escapes_root(relative_dir, target) {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Unsafe Link Target",
            "The remote symlink target cannot be recreated safely inside the download folder.",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use macsftp_core::{
        ConflictPolicy, ErrorCode, LocalPath, MetadataPolicy, RemotePath, Timestamp,
        TransferDirection, TransferEndpoint, TransferId, TransferJob, TransferPlanId,
        TransferState,
    };
    use russh_sftp::client::error::Error as SftpError;
    use russh_sftp::protocol::{Status, StatusCode};
    use tokio_util::sync::CancellationToken;

    use super::{
        TransferSessionSlot, local_download_destination, local_temporary_path,
        prepare_download_destination, remote_planning_error, remote_temporary_path,
        rename_destination, validate_download_symlink_target,
    };

    #[test]
    fn download_symlink_rejects_unsafe_targets() {
        let dest = LocalPath::new("/tmp/downloads/link");
        assert!(validate_download_symlink_target(None, &dest, "").is_err());
        assert!(validate_download_symlink_target(None, &dest, "/etc/passwd").is_err());
        assert!(validate_download_symlink_target(None, &dest, "/Users/alex").is_err());
        assert!(validate_download_symlink_target(None, &dest, "../outside").is_err());
        assert!(validate_download_symlink_target(None, &dest, "sub/../../secret").is_err());
        assert!(validate_download_symlink_target(None, &dest, r"C:\Windows\System32").is_err());
        assert!(validate_download_symlink_target(None, &dest, "C:Windows").is_err());
        assert!(validate_download_symlink_target(None, &dest, "~/secret").is_err());
        assert!(validate_download_symlink_target(None, &dest, "evil\u{202E}txt.exe").is_err());
        assert!(validate_download_symlink_target(None, &dest, "evil\x00file").is_err());
        assert!(validate_download_symlink_target(None, &dest, "evil\x1b[31mfile").is_err());
        assert!(validate_download_symlink_target(None, &dest, r"sub\..\secret").is_err());

        let err = validate_download_symlink_target(None, &dest, "/etc/passwd")
            .expect_err("absolute target must be rejected");
        assert_eq!(err.code, ErrorCode::UnsupportedSymlink);
        assert_eq!(err.title, "Unsafe Link Target");

        assert!(validate_download_symlink_target(None, &dest, "sibling.txt").is_ok());
        assert!(validate_download_symlink_target(None, &dest, "sub/file.txt").is_ok());
        assert!(validate_download_symlink_target(None, &dest, "version.1.0").is_ok());
    }

    #[test]
    fn download_symlink_allows_in_tree_relative_targets() {
        let root = LocalPath::new("/tmp/downloads/tree");
        let nested_link = LocalPath::new("/tmp/downloads/tree/sub/link");
        let deep_link = LocalPath::new("/tmp/downloads/tree/a/b/link");
        let root_link = LocalPath::new("/tmp/downloads/tree/link");

        // Target pointing to sibling from subdirectory (Issue #63)
        assert!(
            validate_download_symlink_target(Some(&root), &nested_link, "../sibling.txt").is_ok()
        );
        assert!(
            validate_download_symlink_target(Some(&root), &nested_link, "../other_sub/file.txt")
                .is_ok()
        );

        // Multi-level parent traversal staying inside tree
        assert!(
            validate_download_symlink_target(Some(&root), &deep_link, "../../sibling.txt").is_ok()
        );
        assert!(
            validate_download_symlink_target(Some(&root), &deep_link, "../sibling.txt").is_ok()
        );

        // Self-cancelling `..` segments
        assert!(
            validate_download_symlink_target(Some(&root), &root_link, "sub/../sibling.txt").is_ok()
        );
        assert!(
            validate_download_symlink_target(Some(&root), &nested_link, "sub2/../file.txt").is_ok()
        );

        // Target pointing to the download root itself from a subdirectory
        assert!(validate_download_symlink_target(Some(&root), &nested_link, "..").is_ok());

        // Single-file download with self-cancelling in-tree `..`
        let single_file_link = LocalPath::new("/tmp/downloads/single_link");
        assert!(
            validate_download_symlink_target(
                Some(&single_file_link),
                &single_file_link,
                "sub/../sibling.txt"
            )
            .is_ok()
        );

        // Reject targets that escape download tree
        assert!(
            validate_download_symlink_target(Some(&root), &nested_link, "../../outside").is_err()
        );
        assert!(
            validate_download_symlink_target(Some(&root), &deep_link, "../../../outside").is_err()
        );
        assert!(validate_download_symlink_target(Some(&root), &root_link, "../outside").is_err());
        assert!(validate_download_symlink_target(Some(&root), &root_link, "..").is_err());
        assert!(
            validate_download_symlink_target(
                Some(&single_file_link),
                &single_file_link,
                "../outside"
            )
            .is_err()
        );
        assert!(
            validate_download_symlink_target(Some(&single_file_link), &single_file_link, "..")
                .is_err()
        );
    }

    fn unique_test_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "macsftp-session-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("test directory must be created");
        path
    }

    /// A file or link download must never recursively delete a local directory,
    /// not even as the plan root. The destination-clear permission exists for
    /// folder-over-folder plans; a root *file* job that finds a directory at
    /// its destination used to `remove_dir_all` an unrelated local tree while
    /// the conflict modal only said "A file with this name already exists".
    #[tokio::test]
    async fn file_download_refuses_to_delete_a_directory_even_as_plan_root() {
        let dir = unique_test_dir("file-over-directory");
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).expect("nested directory must be created");
        std::fs::write(nested.join("keep.txt"), b"keep").expect("child output must be written");
        let destination = dir.to_string_lossy().to_string();

        let error = super::remove_downloaded_file_destination(&destination)
            .await
            .expect_err("a file download must not delete a directory tree");
        assert_eq!(error.title, "Destination Changed Type");
        assert!(
            nested.join("keep.txt").exists(),
            "unrelated local files must survive the refused replacement"
        );
        assert!(dir.exists(), "the directory itself must survive");
    }

    #[tokio::test]
    async fn file_download_replaces_an_existing_file() {
        let dir = unique_test_dir("file-over-file");
        let destination = dir.join("item.txt");
        std::fs::write(&destination, b"old").expect("existing file must be written");
        let destination = destination.to_string_lossy().to_string();

        super::remove_downloaded_file_destination(&destination)
            .await
            .expect("an existing file is replaced");
        assert!(!std::path::Path::new(&destination).exists());
    }

    #[test]
    fn remote_planning_error_redacts_untrusted_server_error_messages() {
        let malicious_error = SftpError::Status(Status {
            id: 1,
            status_code: StatusCode::NoSuchFile,
            error_message: "MALICIOUS SERVER PAYLOAD".to_string(),
            language_tag: "en".to_string(),
        });
        let error = remote_planning_error("Download Planning Failed", &malicious_error);
        let detail = error.detail.unwrap_or_default();
        assert!(!detail.contains("MALICIOUS SERVER PAYLOAD"));
        assert!(!detail.contains("Status {"));
        assert_eq!(detail, "The remote file or folder does not exist.");
    }

    #[test]
    fn rename_destination_rejects_parent_directory_names() {
        let destination = TransferEndpoint::Remote(RemotePath::new("/srv/existing.txt"));
        let mut job = TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source.txt")),
            destination: destination.clone(),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::Ask,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        };

        let error = rename_destination(&mut job, "..").expect_err("parent name must be rejected");

        assert_eq!(error.title, "Invalid destination name");
        assert_eq!(
            job.destination, destination,
            "destination must remain unchanged"
        );
    }

    #[test]
    fn directory_download_rejects_parent_path_components() {
        let error = local_download_destination(&LocalPath::new("/tmp/downloads"), "../escape")
            .expect_err("parent component must be rejected");

        assert_eq!(error.title, "Download Preparation Failed");
    }

    #[test]
    fn directory_download_rejects_bidi_and_control_characters() {
        let error =
            local_download_destination(&LocalPath::new("/tmp/downloads"), "evil\u{202E}txt.exe")
                .expect_err("bidi override must be rejected");

        assert_eq!(error.title, "Download Preparation Failed");

        let error_ctrl =
            local_download_destination(&LocalPath::new("/tmp/downloads"), "evil\x00file")
                .expect_err("control characters must be rejected");

        assert_eq!(error_ctrl.title, "Download Preparation Failed");
    }

    #[test]
    fn temporary_paths_are_hidden_and_unique_per_transfer() {
        assert_eq!(
            remote_temporary_path(&RemotePath::new("/srv/report.txt"), TransferId(4)).as_str(),
            "/srv/.report.txt.macsftp-part-4"
        );
        assert_eq!(
            local_temporary_path(&LocalPath::new("/tmp/report.txt"), TransferId(5)).as_str(),
            "/tmp/.report.txt.macsftp-part-5"
        );
    }

    /// A destination that turned out to be a directory is refused after the
    /// temporary file exists. The failed job must not leave that file behind
    /// for a later transfer to find.
    #[tokio::test]
    async fn refused_destination_removes_the_downloaded_temporary_file() {
        let dir = unique_test_dir("refused-destination-temp");
        let destination = dir.join("target");
        std::fs::create_dir_all(&destination).expect("destination directory created");
        let temporary = dir.join(".target.macsftp-part-9");
        std::fs::write(&temporary, b"downloaded bytes").expect("temporary file written");

        let job = TransferJob {
            id: TransferId(9),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/remote/target")),
            destination: TransferEndpoint::Local(LocalPath::new(
                destination.to_string_lossy().to_string(),
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(0),
        };
        let (event_tx, _event_rx) = flume::bounded(8);
        let transfer_session = TransferSessionSlot::for_test();

        let error = prepare_download_destination(
            &transfer_session,
            &job,
            &LocalPath::new(destination.to_string_lossy().to_string()),
            &LocalPath::new(temporary.to_string_lossy().to_string()),
            &event_tx,
        )
        .await
        .expect_err("a directory destination must be refused");

        assert!(error.message.contains("directory") || error.title.contains("Changed Type"));
        assert!(
            !temporary.exists(),
            "the temporary file must not outlive the refused job"
        );
        assert!(
            destination.is_dir(),
            "the existing directory must be untouched"
        );
    }

    #[tokio::test]
    async fn download_destination_confirms_hardlink_once_per_directory() {
        let dir = unique_test_dir("hardlink-confirm-dir");
        let dest1 = dir.join("file1.txt");
        let dest2 = dir.join("file2.txt");
        std::fs::write(&dest1, b"old 1").expect("file1 created");
        std::fs::write(&dest2, b"old 2").expect("file2 created");

        let temp1 = dir.join(".file1.txt.macsftp-part-10");
        let temp2 = dir.join(".file2.txt.macsftp-part-11");
        std::fs::write(&temp1, b"new 1").expect("temp1 written");
        std::fs::write(&temp2, b"new 2").expect("temp2 written");

        let job1 = TransferJob {
            id: TransferId(10),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/remote/file1.txt")),
            destination: TransferEndpoint::Local(LocalPath::new(
                dest1.to_string_lossy().to_string(),
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(0),
        };
        let job2 = TransferJob {
            id: TransferId(11),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/remote/file2.txt")),
            destination: TransferEndpoint::Local(LocalPath::new(
                dest2.to_string_lossy().to_string(),
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(0),
        };
        let (event_tx, _event_rx) = flume::bounded(8);
        let transfer_session = TransferSessionSlot::for_test();

        let parent_dir = dir.to_string_lossy().to_string();
        assert!(!transfer_session.hardlink_commit_confirmed(&parent_dir));

        prepare_download_destination(
            &transfer_session,
            &job1,
            &LocalPath::new(dest1.to_string_lossy().to_string()),
            &LocalPath::new(temp1.to_string_lossy().to_string()),
            &event_tx,
        )
        .await
        .expect("prepare for job1 should succeed");

        assert!(transfer_session.hardlink_commit_confirmed(&parent_dir));

        prepare_download_destination(
            &transfer_session,
            &job2,
            &LocalPath::new(dest2.to_string_lossy().to_string()),
            &LocalPath::new(temp2.to_string_lossy().to_string()),
            &event_tx,
        )
        .await
        .expect("prepare for job2 should succeed");

        assert!(transfer_session.hardlink_commit_confirmed(&parent_dir));
    }

    #[tokio::test]
    async fn directory_download_metadata_preserves_owner_permissions_for_read_only_remote_dir() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_test_dir("read-only-dir-download");
        let dest = dir.join("readonly_folder");
        std::fs::create_dir_all(&dest).expect("folder created");

        let job = TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/remote/readonly_folder")),
            destination: TransferEndpoint::Local(LocalPath::new(
                dest.to_string_lossy().to_string(),
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy {
                preserve_permissions: true,
                preserve_mtime: false,
                ..MetadataPolicy::default()
            },
            conflict_policy: ConflictPolicy::Ask,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        };

        let mut source_metadata = russh_sftp::protocol::FileAttributes::empty();
        source_metadata.permissions = Some(0o555);

        let (event_tx, _event_rx) = flume::bounded(10);
        super::preserve_download_metadata(&job, &source_metadata, &event_tx).await;

        let meta = std::fs::metadata(&dest).expect("metadata");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o700,
            0o700,
            "downloaded directory must retain owner rwx permissions (got {mode:o})"
        );

        let child = dest.join("child.txt");
        std::fs::write(&child, b"hello")
            .expect("must be able to write child in downloaded directory");
    }

    #[tokio::test]
    async fn file_download_metadata_strips_setuid_setgid_sticky_bits() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp_dir =
            std::env::temp_dir().join(format!("macsftp-download-mode-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("test_file.bin");
        std::fs::write(&file_path, b"data").expect("write file");

        let job = TransferJob {
            id: TransferId(2),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/remote/test_file.bin")),
            destination: TransferEndpoint::Local(LocalPath::new(
                file_path.to_string_lossy().to_string(),
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy {
                preserve_permissions: true,
                preserve_mtime: false,
                ..MetadataPolicy::default()
            },
            conflict_policy: ConflictPolicy::Ask,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        };

        let mut source_metadata = russh_sftp::protocol::FileAttributes::empty();
        // Server sends SUID + SGID + sticky bits (0o7755)
        source_metadata.permissions = Some(0o7755);

        let (event_tx, _event_rx) = flume::bounded(10);
        super::preserve_download_metadata(&job, &source_metadata, &event_tx).await;

        let meta = std::fs::metadata(&file_path).expect("metadata");
        let full_mode = meta.permissions().mode();
        assert_eq!(
            full_mode & 0o7000,
            0,
            "downloaded file must have special bits (SUID/SGID/sticky) stripped (got mode {full_mode:o})"
        );
        assert_eq!(full_mode & 0o777, 0o755);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn directory_download_metadata_strips_setuid_setgid_sticky_bits() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp_dir = std::env::temp_dir().join(format!(
            "macsftp-download-dir-mode-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let dir_path = temp_dir.join("test_dir");
        std::fs::create_dir(&dir_path).expect("create test dir");

        let job = TransferJob {
            id: TransferId(3),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/remote/test_dir")),
            destination: TransferEndpoint::Local(LocalPath::new(
                dir_path.to_string_lossy().to_string(),
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy {
                preserve_permissions: true,
                preserve_mtime: false,
                ..MetadataPolicy::default()
            },
            conflict_policy: ConflictPolicy::Ask,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        };

        let mut source_metadata = russh_sftp::protocol::FileAttributes::empty();
        source_metadata.permissions = Some(0o7555);

        let (event_tx, _event_rx) = flume::bounded(10);
        super::preserve_download_metadata(&job, &source_metadata, &event_tx).await;

        let meta = std::fs::metadata(&dir_path).expect("metadata");
        let full_mode = meta.permissions().mode();
        assert_eq!(
            full_mode & 0o7000,
            0,
            "downloaded dir must have special bits (SUID/SGID/sticky) stripped (got mode {full_mode:o})"
        );
        assert_eq!(
            full_mode & 0o777,
            0o755,
            "directory retains safe permissions with owner rwx (got mode {full_mode:o})"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[tokio::test]
    async fn conflict_resolution_overwrite_all_bypasses_remote_stat_roundtrips() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let (client_io, _server_io) = tokio::io::duplex(1024);
        let sftp_config = russh_sftp::client::Config {
            max_packet_len: 262144,
            max_concurrent_writes: 16,
            request_timeout_secs: 1,
        };
        // Raw session without a server running — any network round-trip would hang or fail
        let raw = Arc::new(russh_sftp::client::RawSftpSession::new_with_config(
            client_io,
            sftp_config,
        ));
        let session = super::TransferSession::Raw {
            session: raw,
            chunk_size: 256 * 1024,
            direction: TransferDirection::Upload,
        };
        let mut job = TransferJob {
            id: TransferId(101),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source.bin")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/dest.bin")),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        };
        let (event_tx, _event_rx) = flume::bounded(10);
        let cancel = CancellationToken::new();
        let conflict_context = super::TransferConflictContext {
            next_id: Arc::new(AtomicU64::new(1)),
            waiters: Arc::new(std::sync::Mutex::new(HashMap::new())),
            plan_policies: Arc::new(std::sync::Mutex::new(HashMap::new())),
        };

        // Must succeed immediately without blocking on network I/O
        let result = super::resolve_transfer_conflict(
            &session,
            &mut job,
            TransferPlanId(1),
            &event_tx,
            &cancel,
            conflict_context,
        )
        .await;

        assert!(
            result.is_ok(),
            "OverwriteAll must bypass destination checks and succeed without round-trips"
        );
    }
}
