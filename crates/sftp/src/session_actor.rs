use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use macsftp_core::{
    AppEvent, ConflictDecision, ConflictPolicy, ConflictRequestId, EditCheckId, EditSessionId,
    ErrorCode, FileKind, FsOp, FsPath, FsScope, LocalPath, RemoteDirLoading, RemoteDirSnapshot,
    RemoteEditSnapshotCheckFailed, RemoteEditSnapshotChecked, RemoteEntry, RemoteEntryResolution,
    RemoteEventScope, RemoteOperationFailure, RemotePath, RemoteScoped, RemoteSnapshot,
    RenameStrategy, ResidualTempRecord, SessionId, StartTransferCommand, TabId, Timestamp,
    TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferFailure, TransferId,
    TransferJob, TransferPlanId, TransferPlanProgress, TransferProgress, TransferSnapshot,
    TransferState, TransferWarning, UserFacingError, is_bidi_or_control,
};
use russh::client;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::fs::DirEntry;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, FileType as SftpFileType, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use tracing::warn;

use crate::transfer_planner::PLAN_PROGRESS_BATCH_SIZE;

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
    next_conflict_id: Arc<AtomicU64>,
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
    RunTransferJobs {
        plan_id: TransferPlanId,
        jobs: Vec<TransferJob>,
    },
    PlanRemoteDownload {
        command: StartTransferCommand,
        plan_id: TransferPlanId,
        created_at: Timestamp,
        next_transfer_id: Arc<AtomicU64>,
        cancel: CancellationToken,
        responder: oneshot::Sender<Option<Vec<TransferJob>>>,
    },
    CancelTransfer {
        transfer_id: macsftp_core::TransferId,
    },
    RetryTransfer {
        plan_id: TransferPlanId,
        job: TransferJob,
    },
    ResolveTransferConflict {
        request_id: ConflictRequestId,
        decision: ConflictDecision,
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

const MAX_CONCURRENT_TRANSFERS: usize = 4;

/// Upper bound when chasing a remote symlink chain at open time. Servers cap
/// `ELOOP` themselves; this only stops a misbehaving one from looping forever.
const MAX_LINK_HOPS: usize = 16;

struct TransferQueue {
    pending: VecDeque<QueuedTransfer>,
    running: HashMap<macsftp_core::TransferId, (TransferPlanId, CancellationToken)>,
    plan_policies: Arc<Mutex<HashMap<TransferPlanId, ConflictPolicy>>>,
    /// First job of each plan: the folder job for directory transfers, which
    /// removes the destination before any child may start.
    plan_root_jobs: HashMap<TransferPlanId, TransferId>,
    /// Plans whose non-root job started at least once; a retried root must not
    /// clear the destination again.
    children_started: std::collections::HashSet<TransferPlanId>,
}

struct QueuedTransfer {
    plan_id: TransferPlanId,
    job: TransferJob,
}

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

impl TransferQueue {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            running: HashMap::new(),
            plan_policies: Arc::new(Mutex::new(HashMap::new())),
            plan_root_jobs: HashMap::new(),
            children_started: std::collections::HashSet::new(),
        }
    }

    fn enqueue(&mut self, plan_id: TransferPlanId, jobs: Vec<TransferJob>, record_root: bool) {
        if let Some(job) = jobs.first()
            && let Ok(mut policies) = self.plan_policies.lock()
        {
            policies
                .entry(plan_id)
                .or_insert_with(|| job.conflict_policy.clone());
        }
        // A retried single job is never the plan root, even when the plan's
        // root mapping was already cleaned up.
        if record_root && let Some(root_job) = jobs.first() {
            self.plan_root_jobs.entry(plan_id).or_insert(root_job.id);
        }
        self.pending
            .extend(jobs.into_iter().map(|job| QueuedTransfer { plan_id, job }));
    }

    fn next(&mut self) -> Option<QueuedTransfer> {
        let position = self.pending.iter().position(|queued| {
            if queued.job.direction != TransferDirection::Download {
                return true;
            }
            let Some(root_id) = self.plan_root_jobs.get(&queued.plan_id) else {
                return true;
            };
            if queued.job.id == *root_id {
                return !self
                    .running
                    .values()
                    .any(|(plan_id, _)| *plan_id == queued.plan_id);
            }
            let root_pending = self
                .pending
                .iter()
                .any(|other| other.plan_id == queued.plan_id && other.job.id == *root_id);
            !root_pending && !self.running.contains_key(root_id)
        });
        position.and_then(|position| self.pending.remove(position))
    }

    fn has_capacity(&self) -> bool {
        self.running.len() < MAX_CONCURRENT_TRANSFERS
    }

    fn finish(&mut self, transfer_id: macsftp_core::TransferId) {
        let Some((plan_id, _)) = self.running.remove(&transfer_id) else {
            return;
        };
        let plan_busy = self.pending.iter().any(|queued| queued.plan_id == plan_id)
            || self
                .running
                .values()
                .any(|(running_plan, _)| *running_plan == plan_id);
        if !plan_busy {
            self.plan_root_jobs.remove(&plan_id);
            self.children_started.remove(&plan_id);
        }
    }

    fn policy_for(&self, plan_id: TransferPlanId, fallback: &ConflictPolicy) -> ConflictPolicy {
        self.plan_policies
            .lock()
            .ok()
            .and_then(|policies| policies.get(&plan_id).cloned())
            .unwrap_or_else(|| fallback.clone())
    }

    fn cancel(&mut self, transfer_id: macsftp_core::TransferId) -> Option<bool> {
        if let Some(position) = self
            .pending
            .iter()
            .position(|transfer| transfer.job.id == transfer_id)
        {
            let plan_id = self.pending.remove(position).map(|queued| queued.plan_id);
            if let Some(plan_id) = plan_id {
                let plan_busy = self.pending.iter().any(|queued| queued.plan_id == plan_id)
                    || self
                        .running
                        .values()
                        .any(|(running_plan, _)| *running_plan == plan_id);
                if !plan_busy {
                    self.plan_root_jobs.remove(&plan_id);
                    self.children_started.remove(&plan_id);
                }
            }
            return Some(true);
        }
        if let Some((_, cancel)) = self.running.get(&transfer_id) {
            cancel.cancel();
            return Some(false);
        }
        None
    }
}

impl RemoteSessionActor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab_id: TabId,
        session_id: SessionId,
        session_epoch: u64,
        shared_connection: std::sync::Arc<crate::pool::SharedConnection>,
        sftp: russh_sftp::client::SftpSession,
        event_tx: flume::Sender<AppEvent>,
        next_conflict_id: Arc<AtomicU64>,
    ) -> Self {
        Self {
            tab_id,
            session_id,
            session_epoch,
            shared_connection,
            sftp,
            event_tx,
            next_conflict_id,
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

        let (transfer_complete_tx, transfer_complete_rx) = flume::bounded(16);
        let mut transfer_queue = TransferQueue::new();
        let conflict_waiters = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

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
                    self
                        .handle_request(
                            &scope,
                            cancel.clone(),
                            request,
                            &mut transfer_queue,
                            &transfer_complete_tx,
                            conflict_waiters.clone(),
                        )
                        .await;
                }
                completed = transfer_complete_rx.recv_async() => {
                    let Ok(transfer_id) = completed else {
                        break;
                    };
                    transfer_queue.finish(transfer_id);
                    self
                        .start_queued_transfer_jobs(
                            cancel.clone(),
                            &mut transfer_queue,
                            &transfer_complete_tx,
                            conflict_waiters.clone(),
                        )
                        .await;
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

    #[allow(clippy::too_many_arguments)]
    async fn handle_request(
        &self,
        scope: &RemoteEventScope,
        cancel: CancellationToken,
        request: RemoteSessionRequest,
        transfer_queue: &mut TransferQueue,
        transfer_complete_tx: &flume::Sender<macsftp_core::TransferId>,
        conflict_waiters: Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
    ) {
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
            RemoteSessionRequest::RunTransferJobs { plan_id, jobs } => {
                transfer_queue.enqueue(plan_id, jobs, true);
                self.start_queued_transfer_jobs(
                    cancel,
                    transfer_queue,
                    transfer_complete_tx,
                    conflict_waiters,
                )
                .await;
            }
            RemoteSessionRequest::CancelTransfer { transfer_id } => {
                if transfer_queue.cancel(transfer_id) == Some(true) {
                    let _ = self
                        .event_tx
                        .send_async(AppEvent::TransferSkipped { transfer_id })
                        .await;
                }
            }
            RemoteSessionRequest::RetryTransfer { plan_id, job } => {
                transfer_queue.enqueue(plan_id, vec![job], false);
                self.start_queued_transfer_jobs(
                    cancel,
                    transfer_queue,
                    transfer_complete_tx,
                    conflict_waiters,
                )
                .await;
            }
            RemoteSessionRequest::ResolveTransferConflict {
                request_id,
                decision,
            } => {
                if let Ok(mut waiters) = conflict_waiters.lock()
                    && let Some(waiter) = waiters.remove(&request_id)
                {
                    let _ = waiter.sender.send(decision);
                }
            }
            RemoteSessionRequest::RemoveRemoteTempFile { transfer_id, path } => {
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

    async fn start_queued_transfer_jobs(
        &self,
        cancel: CancellationToken,
        transfer_queue: &mut TransferQueue,
        transfer_complete_tx: &flume::Sender<macsftp_core::TransferId>,
        conflict_waiters: Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
    ) {
        while transfer_queue.has_capacity() {
            // `host:port` identity used to reconcile residual remote temp
            // files left by a previous run when this connection is
            // re-established. Recomputed per iteration so each spawned job
            // owns its copy (the `async move` closure would otherwise move
            // the single binding out of the loop).
            let connection_key = self.shared_connection.host_port_string.clone();
            let Some(queued_transfer) = transfer_queue.next() else {
                return;
            };
            let mut job = queued_transfer.job;
            job.conflict_policy =
                transfer_queue.policy_for(queued_transfer.plan_id, &job.conflict_policy);
            let transfer_session =
                match open_transfer_session(&self.shared_connection.handle, job.direction).await {
                    Ok(session) => Arc::new(TransferSessionSlot::new(session)),
                    Err(error) => {
                        if self
                            .event_tx
                            .send_async(AppEvent::TransferFailed(TransferFailure {
                                transfer_id: job.id,
                                error,
                            }))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        continue;
                    }
                };
            let event_tx = self.event_tx.clone();
            let transfer_cancel = cancel.child_token();
            let transfer_id = job.id;
            let is_plan_root = transfer_queue
                .plan_root_jobs
                .get(&queued_transfer.plan_id)
                .is_some_and(|root_id| *root_id == transfer_id);
            let may_clear_destination = is_plan_root
                && !transfer_queue
                    .children_started
                    .contains(&queued_transfer.plan_id);
            if !is_plan_root {
                transfer_queue
                    .children_started
                    .insert(queued_transfer.plan_id);
            }
            let next_conflict_id = self.next_conflict_id.clone();
            let plan_policies = transfer_queue.plan_policies.clone();
            transfer_queue.running.insert(
                transfer_id,
                (queued_transfer.plan_id, transfer_cancel.clone()),
            );
            let transfer_complete_tx = transfer_complete_tx.clone();
            let conflict_waiters = conflict_waiters.clone();
            let conflict_context = TransferConflictContext {
                waiters: conflict_waiters,
                next_id: next_conflict_id,
                plan_policies,
            };
            std::mem::drop(tokio::spawn(async move {
                run_transfer_job(
                    transfer_session,
                    job,
                    queued_transfer.plan_id,
                    event_tx,
                    transfer_cancel,
                    conflict_context,
                    connection_key.clone(),
                    may_clear_destination,
                )
                .await;
                if let Err(error) = transfer_complete_tx.send_async(transfer_id).await {
                    warn!(error = %error, "transfer complete notification dropped");
                }
            }));
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
}

pub(crate) struct TransferSessionSlot {
    session: TransferSession,
    facts: PlanTransferFacts,
}

impl TransferSessionSlot {
    pub(crate) fn new(session: TransferSession) -> Self {
        Self::with_facts(session, PlanTransferFacts::default())
    }

    pub(crate) fn with_facts(session: TransferSession, facts: PlanTransferFacts) -> Self {
        Self { session, facts }
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

const MAX_CONCURRENT_DIR_SCANS: usize = 4;

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

fn remote_planning_error(title: &str, error: &SftpError) -> UserFacingError {
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
    Skipped,
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
                download_single_file(sftp, &job, &event_tx, &cancel, may_clear_destination).await
            }
        },
        Err(error) => Err(error),
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
            TransferJobOutcome::Skipped,
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
    validate_download_symlink_target(&target)?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_downloaded_file_destination(destination.as_str()).await?;
    }
    ensure_local_parent_directory(destination, cancel).await?;
    tokio::fs::symlink(target, destination.as_str())
        .await
        .map_err(|error| local_transfer_error("Local Link Creation Failed", &error))
}

async fn download_single_file_pipelined(
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
        return download_symlink_raw(raw, job, event_tx, cancel).await;
    }
    if source_metadata.file_type() == SftpFileType::Dir {
        return download_directory(
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

    ensure_local_parent_directory(destination, cancel).await?;
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
    prepare_download_destination(job, destination, &temporary_destination, event_tx).await?;
    if let Err(error) = commit_local_temp(&temporary_destination, destination).await {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
    preserve_download_metadata(job, &source_metadata, event_tx).await;
    Ok(())
}

async fn download_single_file(
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
        return download_symlink(sftp, job, event_tx, cancel).await;
    }
    if source_metadata.file_type() == SftpFileType::Dir {
        return download_directory(
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

    ensure_local_parent_directory(destination, cancel).await?;
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
    prepare_download_destination(job, destination, &temporary_destination, event_tx).await?;
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
    job: &TransferJob,
    destination: &LocalPath,
    temporary_destination: &LocalPath,
    event_tx: &flume::Sender<AppEvent>,
) -> Result<(), UserFacingError> {
    if !matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        return Ok(());
    }
    if let Err(error) = verify_local_hardlink(temporary_destination, event_tx, job.id).await {
        cleanup_local_temp(temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    if let Err(error) = remove_downloaded_file_destination(destination.as_str()).await {
        cleanup_local_temp(temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    Ok(())
}

async fn download_directory(
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
    tokio::select! {
        _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
        result = tokio::fs::create_dir_all(destination.as_str()) => result.map_err(|error| {
            local_transfer_error("Download Folder Creation Failed", &error)
        })?,
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
    validate_download_symlink_target(&target)?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_downloaded_file_destination(destination.as_str()).await?;
    }
    ensure_local_parent_directory(destination, cancel).await?;
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
    destination: &LocalPath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
    let Some(parent) = std::path::Path::new(destination.as_str()).parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(cancelled_transfer_error()),
        result = tokio::fs::create_dir_all(parent) => result.map_err(|error| {
            local_transfer_error("Download Folder Creation Failed", &error)
        }),
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

async fn emit_temporary_file_warning(
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
            sftp.rename(from.as_str(), to.as_str())
                .await
                .map_err(|error| remote_fs_error("The item couldn’t be renamed.", &error))
        }
        FsOp::CreateDirectory { parent, name } => {
            let parent = require_remote_path(parent, "create directory parent")?;
            let path = parent.join(name);
            sftp.create_dir(path.as_str())
                .await
                .map_err(|error| remote_fs_error("The folder couldn’t be created.", &error))
        }
        FsOp::Delete { entries } => {
            for entry in entries {
                let path = require_remote_path(&entry.path, "delete path")?;
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
        if target_escapes_through_parent(&target_text) {
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
        (Some(canonical), _) => canonical
            .starts_with('/')
            .then(|| RemotePath::new(canonical)),
        (None, Some(target)) => {
            (!target_escapes_through_parent(target)).then(|| path.resolve_link_target(target))
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

fn validate_download_symlink_target(target: &str) -> Result<(), UserFacingError> {
    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\\')
        || target_escapes_through_parent(target)
    {
        return Err(UserFacingError::new(
            ErrorCode::UnsupportedSymlink,
            "Unsafe Link Target",
            "The remote symlink target cannot be recreated safely inside the download folder.",
        ));
    }
    Ok(())
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
    use macsftp_core::{
        ConflictPolicy, ErrorCode, LocalPath, MetadataPolicy, RemotePath, Timestamp,
        TransferDirection, TransferEndpoint, TransferId, TransferJob, TransferPlanId,
        TransferState,
    };
    use russh_sftp::client::error::Error as SftpError;
    use russh_sftp::protocol::{Status, StatusCode};
    use tokio_util::sync::CancellationToken;

    use super::{
        TransferQueue, local_download_destination, local_temporary_path,
        prepare_download_destination, read_directory_error, remote_fs_error, remote_planning_error,
        remote_temporary_path, rename_destination, target_escapes_through_parent,
        validate_download_symlink_target, write_through_start,
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
    fn target_traversal_detection_covers_absolute_and_relative_targets() {
        assert!(target_escapes_through_parent("../shared/app.conf"));
        assert!(target_escapes_through_parent("/opt/../etc/app.conf"));
        assert!(!target_escapes_through_parent("shared/app.conf"));
        assert!(!target_escapes_through_parent("/opt/shared/app.conf"));
        // A component that merely starts with dots is not traversal.
        assert!(!target_escapes_through_parent("..notes/app.conf"));
    }

    #[test]
    fn download_symlink_rejects_unsafe_targets() {
        assert!(validate_download_symlink_target("").is_err());
        assert!(validate_download_symlink_target("/etc/passwd").is_err());
        assert!(validate_download_symlink_target("/Users/alex").is_err());
        assert!(validate_download_symlink_target("../outside").is_err());
        assert!(validate_download_symlink_target("sub/../../secret").is_err());
        assert!(validate_download_symlink_target(r"C:\Windows\System32").is_err());
        assert!(validate_download_symlink_target(r"sub\..\secret").is_err());

        let err = validate_download_symlink_target("/etc/passwd")
            .expect_err("absolute target must be rejected");
        assert_eq!(err.code, ErrorCode::UnsupportedSymlink);
        assert_eq!(err.title, "Unsafe Link Target");

        assert!(validate_download_symlink_target("sibling.txt").is_ok());
        assert!(validate_download_symlink_target("sub/file.txt").is_ok());
        assert!(validate_download_symlink_target("version.1.0").is_ok());
    }

    fn queue_job(id: u64, direction: TransferDirection) -> TransferJob {
        TransferJob {
            id: TransferId(id),
            direction,
            source: TransferEndpoint::Remote(RemotePath::new(format!("/srv/{id}"))),
            destination: TransferEndpoint::Local(LocalPath::new(format!("/tmp/{id}"))),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(0),
        }
    }

    #[test]
    fn transfer_queue_blocks_download_children_behind_the_root() {
        let mut queue = TransferQueue::new();
        let plan_id = TransferPlanId(1);
        queue.enqueue(
            plan_id,
            vec![
                queue_job(1, TransferDirection::Download),
                queue_job(2, TransferDirection::Download),
            ],
            true,
        );

        let root = queue.next().expect("the plan root must start first");
        assert_eq!(root.job.id, TransferId(1));
        queue
            .running
            .insert(root.job.id, (plan_id, CancellationToken::new()));
        assert!(
            queue.next().is_none(),
            "children must wait while the root runs"
        );

        queue.finish(root.job.id);
        let child = queue
            .next()
            .expect("children start after the root finishes");
        assert_eq!(child.job.id, TransferId(2));
    }

    #[test]
    fn transfer_queue_does_not_promote_a_retried_child_to_root() {
        let mut queue = TransferQueue::new();
        let plan_id = TransferPlanId(2);
        queue.enqueue(
            plan_id,
            vec![
                queue_job(1, TransferDirection::Download),
                queue_job(2, TransferDirection::Download),
            ],
            true,
        );

        let root = queue.next().expect("root must be queued");
        queue
            .running
            .insert(root.job.id, (plan_id, CancellationToken::new()));
        queue.finish(root.job.id);
        assert_eq!(
            queue.plan_root_jobs.get(&plan_id),
            Some(&TransferId(1)),
            "the root mapping survives while the plan still has work"
        );

        let child = queue.next().expect("child must be queued");
        queue
            .running
            .insert(child.job.id, (plan_id, CancellationToken::new()));
        queue.finish(child.job.id);
        assert!(
            !queue.plan_root_jobs.contains_key(&plan_id),
            "an idle plan releases its root mapping"
        );

        queue.enqueue(
            plan_id,
            vec![queue_job(2, TransferDirection::Download)],
            false,
        );
        assert!(
            !queue.plan_root_jobs.contains_key(&plan_id),
            "a retried child must not become the plan root"
        );
    }

    #[test]
    fn transfer_queue_keeps_upload_jobs_concurrent() {
        let mut queue = TransferQueue::new();
        let plan_id = TransferPlanId(3);
        queue.enqueue(
            plan_id,
            vec![
                queue_job(1, TransferDirection::Upload),
                queue_job(2, TransferDirection::Upload),
            ],
            true,
        );

        assert_eq!(
            queue.next().map(|queued| queued.job.id),
            Some(TransferId(1))
        );
        assert_eq!(
            queue.next().map(|queued| queued.job.id),
            Some(TransferId(2))
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

        let error = prepare_download_destination(
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
}
