use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use macsftp_core::{
    AppEvent, ConflictDecision, ConflictPolicy, ConflictRequestId, ErrorCode, FileKind, FsOp,
    FsPath, FsScope, LocalPath, RemoteDirLoading, RemoteDirSnapshot, RemoteEntry, RemoteEventScope,
    RemoteOperationFailure, RemotePath, RemoteScoped, RenameStrategy, ResidualTempRecord,
    SessionId, StartTransferCommand, TabId, Timestamp, TransferConflictPrompt, TransferDirection,
    TransferEndpoint, TransferFailure, TransferId, TransferJob, TransferPlanId,
    TransferPlanProgress, TransferProgress, TransferSnapshot, TransferState, TransferWarning,
    UserFacingError,
};
use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::fs::DirEntry;
use russh_sftp::protocol::{FileAttributes, FileType as SftpFileType, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use tracing::warn;

/// Host trust file locations and prompt policy (plan §10).
#[derive(Debug, Clone)]
pub struct HostTrustConfig {
    /// App-owned known_hosts — the only file macSFTP writes.
    pub app_known_hosts_path: PathBuf,
    /// The user's `~/.ssh/known_hosts`, read-only.
    pub user_known_hosts_path: Option<PathBuf>,
    /// How long an unanswered trust prompt blocks the handshake before
    /// it is rejected (ADR-004: default 5 minutes).
    pub trust_prompt_timeout: Duration,
}

impl HostTrustConfig {
    pub fn new(app_known_hosts_path: PathBuf, user_known_hosts_path: Option<PathBuf>) -> Self {
        Self {
            app_known_hosts_path,
            user_known_hosts_path,
            trust_prompt_timeout: Duration::from_secs(300),
        }
    }
}

/// Why `check_server_key` refused the key — recorded by the handler so
/// the actor can emit the right terminal event once `connect` fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(any())]
enum HostKeyRejection {
    Mismatch,
    UserRejected,
    PromptTimeout,
}

/// The real per-tab browsing session actor (plan §9, milestone M3).
///
/// Establishes TCP + SSH handshake with host key verification through
/// `KnownHostsStore` and the `TrustRegistry` prompt flow, authenticates
/// with a password or private key, opens the SFTP subsystem, and holds
/// the connection until cancelled or the server disconnects.
///
/// Event contract is identical to `MockRemoteSessionActor` so the app
/// side needs no changes when the dispatch loop switches over.
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
    /// the app to reconcile the M5/M6 residual on reconnect.
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
}

#[cfg(any())]
struct ConnectedSession {
    handle: client::Handle<crate::physical_connection::ClientHandler>,
    sftp: SftpSession,
    remote_root: RemotePath,
}

const MAX_CONCURRENT_TRANSFERS: usize = 4;

struct TransferQueue {
    pending: VecDeque<QueuedTransfer>,
    running: HashMap<macsftp_core::TransferId, CancellationToken>,
    plan_policies: Arc<Mutex<HashMap<TransferPlanId, ConflictPolicy>>>,
}

struct QueuedTransfer {
    plan_id: TransferPlanId,
    job: TransferJob,
}

#[derive(Clone)]
struct TransferConflictContext {
    waiters: Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
    next_id: Arc<AtomicU64>,
    plan_policies: Arc<Mutex<HashMap<TransferPlanId, ConflictPolicy>>>,
}

struct ConflictWaiter {
    plan_id: TransferPlanId,
    sender: oneshot::Sender<ConflictDecision>,
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
        }
    }

    fn enqueue(&mut self, plan_id: TransferPlanId, jobs: Vec<TransferJob>) {
        if let Some(job) = jobs.first()
            && let Ok(mut policies) = self.plan_policies.lock()
        {
            policies
                .entry(plan_id)
                .or_insert_with(|| job.conflict_policy.clone());
        }
        self.pending
            .extend(jobs.into_iter().map(|job| QueuedTransfer { plan_id, job }));
    }

    fn next(&mut self) -> Option<QueuedTransfer> {
        self.pending.pop_front()
    }

    fn has_capacity(&self) -> bool {
        self.running.len() < MAX_CONCURRENT_TRANSFERS
    }

    fn finish(&mut self, transfer_id: macsftp_core::TransferId) {
        self.running.remove(&transfer_id);
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
            self.pending.remove(position);
            return Some(true);
        }
        if let Some(cancel) = self.running.get(&transfer_id) {
            cancel.cancel();
            return Some(false);
        }
        None
    }
}

#[cfg(any())]
enum ConnectFailure {
    /// `HostKeyMismatch` was already emitted by the handler; the
    /// connection is blocked with no override (plan §10).
    HostKeyMismatch,
    TrustRejected,
    TrustTimeout,
    Auth(UserFacingError),
    Connection(UserFacingError),
}

/// Log the terminal outcome of a failed connect attempt. Secrets are
/// never logged — only the host and the user-facing error title.
#[cfg(any())]
fn log_connect_failure(host: &str, failure: &ConnectFailure) {
    match failure {
        ConnectFailure::HostKeyMismatch => {
            info!(
                host,
                "host key mismatch; connection blocked (potential MITM)"
            )
        }
        ConnectFailure::TrustRejected => info!(host, "host key trust rejected by user"),
        ConnectFailure::TrustTimeout => warn!(host, "host key trust prompt timed out"),
        ConnectFailure::Auth(error) => warn!(host, error = %error.title, "authentication failed"),
        ConnectFailure::Connection(error) => {
            warn!(host, error = %error.title, "connection failed")
        }
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

        tracing::info!(
            session_id = ?self.session_id,
            "sftp session established; serving requests"
        );
        let _ = self
            .event_tx
            .send_async(AppEvent::TabConnected(RemoteScoped::new(
                scope.clone(),
                macsftp_core::TabConnected {
                    remote_root: macsftp_core::RemotePath::new(self.shared_connection.remote_root.clone()),
                },
            )))
            .await;

        let (transfer_complete_tx, transfer_complete_rx) = flume::bounded(16);
        let mut transfer_queue = TransferQueue::new();
        let conflict_waiters = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        
        self.shared_connection.active_channels.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!(session_id = ?self.session_id, "session cancelled by user");
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
        
        self.shared_connection.active_channels.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        *self.shared_connection.last_used.lock().unwrap() = std::time::Instant::now();
    }

    #[cfg(any())]
    async fn establish(
        &self,
        scope: &RemoteEventScope,
        connection_lost: CancellationToken,
    ) -> Result<ConnectedSession, ConnectFailure> {
        let config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(15)),
            keepalive_max: 3,
            ..client::Config::default()
        });

        info!(
            host = String::new().as_str(),
            port = 22,
            username = String::new().as_str(),
            session_id = ?self.session_id,
            tab_id = ?self.tab_id,
            "establishing SSH/SFTP session"
        );

        let rejection: Arc<Mutex<Option<HostKeyRejection>>> = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            host: String::new().clone(),
            port: 22,
            scope: scope.clone(),
            trust_request_id: TrustRequestId(0),
            known_hosts: Arc::new(Mutex::new(KnownHostsStore::load(&std::path::PathBuf::from(""), None))).clone(),
            trust_config: Arc::new(HostTrustConfig::new(std::path::PathBuf::from(""), None)).clone(),
            trust_registry: Arc::new(crate::trust::TrustRegistry::new()).clone(),
            event_tx: self.event_tx.clone(),
            rejection: rejection.clone(),
            connection_lost,
        };

        let address = (String::new().as_str(), 22);
        let mut handle = match client::connect(config, address, handler).await {
            Ok(handle) => handle,
            Err(error) => {
                warn!(
                    host = String::new().as_str(),
                    port = 22,
                    error = %error,
                    "ssh tcp/handshake failed"
                );
                let recorded = rejection
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                return Err(match recorded {
                    Some(HostKeyRejection::Mismatch) => ConnectFailure::HostKeyMismatch,
                    Some(HostKeyRejection::UserRejected) => ConnectFailure::TrustRejected,
                    Some(HostKeyRejection::PromptTimeout) => ConnectFailure::TrustTimeout,
                    None => ConnectFailure::Connection(connection_error(
                        &String::new(),
                        22,
                        &error,
                    )),
                });
            }
        };

        self.authenticate(&mut handle).await?;

        // Open the SFTP subsystem and resolve the remote root. Kept in
        // M3 so "connected" means a usable SFTP session, not just auth.
        let sftp_result: Result<(SftpSession, String), UserFacingError> = async {
            let channel = handle.channel_open_session().await.map_err(|error| {
                subsystem_error("Could not open an SSH channel.", &error.to_string())
            })?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|error| {
                    subsystem_error(
                        "The server rejected the SFTP subsystem.",
                        &error.to_string(),
                    )
                })?;
            let sftp = SftpSession::new(channel.into_stream())
                .await
                .map_err(|error| {
                    subsystem_error("Could not start the SFTP session.", &error.to_string())
                })?;
            let root = sftp.canonicalize(".").await.map_err(|error| {
                subsystem_error(
                    "Could not resolve the remote home directory.",
                    &error.to_string(),
                )
            })?;
            Ok((sftp, root))
        }
        .await;

        let (sftp, remote_root) = sftp_result.map_err(ConnectFailure::Connection)?;

        Ok(ConnectedSession {
            handle,
            sftp,
            remote_root: RemotePath::new(remote_root),
        })
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
                transfer_queue.enqueue(plan_id, jobs);
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
                transfer_queue.enqueue(plan_id, vec![job]);
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
                        self.read_remote_dir(&self.sftp, scope, refresh_path)
                            .await;
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
            let transfer_sftp = match open_transfer_sftp(&self.shared_connection.handle).await {
                Ok(sftp) => sftp,
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
            let next_conflict_id = self.next_conflict_id.clone();
            let plan_policies = transfer_queue.plan_policies.clone();
            transfer_queue
                .running
                .insert(transfer_id, transfer_cancel.clone());
            let transfer_complete_tx = transfer_complete_tx.clone();
            let conflict_waiters = conflict_waiters.clone();
            let conflict_context = TransferConflictContext {
                waiters: conflict_waiters,
                next_id: next_conflict_id,
                plan_policies,
            };
            std::mem::drop(tokio::spawn(async move {
                run_transfer_job(
                    transfer_sftp,
                    job,
                    queued_transfer.plan_id,
                    event_tx,
                    transfer_cancel,
                    conflict_context,
                    connection_key.clone(),
                )
                .await;
                let _ = transfer_complete_tx.send_async(transfer_id).await;
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

    #[cfg(any())]
    async fn authenticate(
        &self,
        handle: &mut client::Handle<crate::physical_connection::ClientHandler>,
    ) -> Result<(), ConnectFailure> {
        // Log only the method kind — never the secret itself
        // (password / key passphrase live in `AuthCredential::Password { password: String::new() }`).
        let method = match &(AuthCredential::Password { password: String::new() }) {
            AuthCredential::Password { .. } => "password",
            AuthCredential::PrivateKey { .. } => "private_key",
        };
        info!(
            host = String::new().as_str(),
            username = String::new().as_str(),
            method,
            "authenticating with server"
        );
        let auth_result = match &(AuthCredential::Password { password: String::new() }) {
            AuthCredential::Password { password } => handle
                .authenticate_password(String::new().clone(), password.clone())
                .await
                .map_err(|error| {
                    ConnectFailure::Connection(connection_error(
                        &String::new(),
                        22,
                        &error,
                    ))
                })?,
            AuthCredential::PrivateKey {
                key_path,
                passphrase,
            } => {
                let key = load_secret_key(key_path, passphrase.as_deref())
                    .map_err(|error| ConnectFailure::Auth(private_key_error(&error)))?;
                let best_hash = handle
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(connection_error(
                            &String::new(),
                            22,
                            &error,
                        ))
                    })?
                    .flatten();
                handle
                    .authenticate_publickey(
                        String::new().clone(),
                        PrivateKeyWithHashAlg::new(Arc::new(key), best_hash),
                    )
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(connection_error(
                            &String::new(),
                            22,
                            &error,
                        ))
                    })?
            }
        };

        match auth_result {
            russh::client::AuthResult::Success => Ok(()),
            russh::client::AuthResult::Failure {
                remaining_methods, ..
            } => {
                // Plan §11: keyboard-interactive is not supported in the
                // MVP — say so instead of disguising it as a bad password.
                let wants_keyboard_interactive = remaining_methods
                    .contains(&russh::MethodKind::KeyboardInteractive)
                    && !remaining_methods.contains(&russh::MethodKind::Password);
                let error = if wants_keyboard_interactive
                    && matches!(AuthCredential::Password { password: String::new() }, AuthCredential::Password { .. })
                {
                    UserFacingError::new(
                        ErrorCode::AuthFailed,
                        "Authentication method not supported",
                        "This server requires keyboard-interactive authentication, \
                         which this version does not support yet.",
                    )
                } else {
                    UserFacingError::new(
                        ErrorCode::AuthFailed,
                        "Authentication failed",
                        "The server rejected the credentials. Check the username \
                         and credentials, then try again.",
                    )
                    .with_retryable(true)
                };
                Err(ConnectFailure::Auth(error))
            }
        }
    }
}

async fn open_transfer_sftp(
    handle: &client::Handle<crate::physical_connection::ClientHandler>,
) -> Result<SftpSession, UserFacingError> {
    let channel = handle.channel_open_session().await.map_err(|error| {
        transfer_error(
            ErrorCode::ChannelClosed,
            "Could not start transfer",
            "Could not open a transfer channel. Reconnect and try again.",
            &error.to_string(),
        )
    })?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| {
            transfer_error(
                ErrorCode::ChannelClosed,
                "Could not start transfer",
                "The server rejected the SFTP subsystem for this transfer.",
                &error.to_string(),
            )
        })?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|error| {
            transfer_error(
                ErrorCode::ChannelClosed,
                "Could not start transfer",
                "Could not start the transfer SFTP session.",
                &error.to_string(),
            )
        })
}

/// Stream a remote download plan from a dedicated SFTP channel. Planning
/// needs remote metadata, but it must not monopolize the browsing channel:
/// each discovered entry becomes a child job before recursion continues.
async fn plan_remote_download(
    sftp: SftpSession,
    command: StartTransferCommand,
    plan_id: TransferPlanId,
    created_at: Timestamp,
    next_transfer_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
) -> Option<Vec<TransferJob>> {
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
}

impl RemoteDownloadPlanner {
    async fn plan(&mut self, sftp: &SftpSession) -> Result<(), UserFacingError> {
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
        sftp: &SftpSession,
        root: &RemotePath,
        destination_root: &LocalPath,
    ) -> Result<(), UserFacingError> {
        let mut pending_directories = vec![root.clone()];
        while let Some(current) = pending_directories.pop() {
            self.ensure_not_cancelled()?;
            let entries = tokio::select! {
                _ = self.cancel.cancelled() => return Err(cancelled_transfer_error()),
                result = sftp.read_dir(current.as_str()) => result.map_err(|error| {
                    remote_planning_error("Could not read remote download directory", &error)
                })?,
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
                        UserFacingError::new(
                            ErrorCode::Unknown,
                            "Could not plan download",
                            "The remote directory changed while it was being scanned. Try again.",
                        )
                    })?;
                let destination = local_download_destination(destination_root, relative)?;
                let metadata = entry.metadata().clone();
                if entry.file_type() == SftpFileType::Dir {
                    self.emit_child(path.clone(), destination, Some(0)).await?;
                    pending_directories.push(path);
                } else {
                    self.emit_child(path, destination, metadata.size).await?;
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
                remote_planning_error("Could not inspect remote download source", &error)
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
        tokio::select! {
            _ = self.cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = self.event_tx.send_async(AppEvent::TransferPlanProgress(TransferPlanProgress {
                plan_id: self.plan_id,
                child_job: child_job.clone(),
                planned_count: self.planned_count,
                total_bytes: self.total_bytes,
            })) => result.map_err(|_| UserFacingError::new(
                ErrorCode::ChannelClosed,
                "Transfer planning stopped",
                "The application stopped receiving planning updates.",
            ))?,
        }
        self.child_jobs.push(child_job);
        Ok(())
    }

    async fn finish(&self) -> bool {
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
        &error.to_string(),
    )
}

fn local_download_destination(
    destination_root: &LocalPath,
    relative: &str,
) -> Result<LocalPath, UserFacingError> {
    if Path::new(relative)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Could not plan download",
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

async fn run_transfer_job(
    sftp: SftpSession,
    mut job: TransferJob,
    plan_id: TransferPlanId,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
    conflict_context: TransferConflictContext,
    connection_key: String,
) {
    let result = match resolve_transfer_conflict(
        &sftp,
        &mut job,
        plan_id,
        &event_tx,
        &cancel,
        conflict_context,
    )
    .await
    {
        Ok(()) => match job.direction {
            macsftp_core::TransferDirection::Upload => {
                upload_transfer(&sftp, &job, &event_tx, &cancel, &connection_key).await
            }
            macsftp_core::TransferDirection::Download => {
                download_single_file(&sftp, &job, &event_tx, &cancel, "local").await
            }
        },
        Err(error) => Err(error),
    };

    let terminal_event = match result {
        Ok(()) => AppEvent::TransferCompleted {
            transfer_id: job.id,
        },
        Err(error) if error.code == ErrorCode::Cancelled => AppEvent::TransferSkipped {
            transfer_id: job.id,
        },
        Err(error) => AppEvent::TransferFailed(TransferFailure {
            transfer_id: job.id,
            error,
        }),
    };
    if let Err(error) = event_tx.send_async(terminal_event).await {
        warn!(error = %error, "transfer terminal event dropped");
    }
}

async fn resolve_transfer_conflict(
    sftp: &SftpSession,
    job: &mut TransferJob,
    plan_id: TransferPlanId,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    conflict_context: TransferConflictContext,
) -> Result<(), UserFacingError> {
    if !destination_exists(sftp, job).await? {
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
            if destination_exists(sftp, job).await? {
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
                    "Could not request conflict decision",
                    "The transfer conflict resolver is unavailable. Try again.",
                ));
            }
            let (source_size, source_modified_at) = conflict_source_details(sftp, job).await;
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
                    if destination_exists(sftp, job).await? {
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

async fn conflict_source_details(
    sftp: &SftpSession,
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
        TransferEndpoint::Remote(source) => match sftp.symlink_metadata(source.as_str()).await {
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
    sftp: &SftpSession,
    job: &TransferJob,
) -> Result<bool, UserFacingError> {
    match &job.destination {
        TransferEndpoint::Remote(destination) => match sftp.metadata(destination.as_str()).await {
            Ok(_) => Ok(true),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(false)
            }
            Err(error) => Err(transfer_error(
                ErrorCode::Unknown,
                "Could not inspect remote destination",
                "The remote destination could not be checked. Try again.",
                &error.to_string(),
            )),
        },
        TransferEndpoint::Local(destination) => {
            match tokio::fs::symlink_metadata(destination.as_str()).await {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(local_transfer_error(
                    "Could not inspect download destination",
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
        .map_err(|error| local_transfer_error("Could not read upload source", &error))?;
    if metadata.file_type().is_symlink() {
        return upload_symlink(sftp, job, event_tx, cancel).await;
    }
    if metadata.is_dir() {
        emit_transfer_running(event_tx, job, Some(0)).await?;
        create_remote_directory(sftp, destination, cancel).await?;
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

    ensure_remote_parent_directory(sftp, destination, cancel).await?;
    let temporary_destination = remote_temporary_path(destination, job.id);
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
    if !cleanup_remote_temp(sftp, &temporary_destination, event_tx, job.id, Some(job.id)).await {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Could not prepare upload",
            "A previous temporary upload file could not be removed. Retry the transfer.",
        )
        .with_retryable(true));
    }
    let copy_result = async {
        let mut source_file = tokio::fs::File::open(source.as_str())
            .await
            .map_err(|error| local_transfer_error("Could not open upload source", &error))?;
        let mut destination_file = sftp
            .open_with_flags(
                temporary_destination.as_str(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            )
            .await
            .map_err(remote_destination_create_error)?;
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
        verify_remote_hardlink(sftp, &temporary_destination, event_tx, job.id).await?;
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
            "Could not prepare upload",
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
                    "Could not prepare upload",
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
            &error.to_string(),
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
            "Could not prepare download",
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
                    "Could not prepare download",
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
    create_remote_directory(sftp, &macsftp_core::RemotePath::new(parent), cancel).await
}

async fn create_remote_directory(
    sftp: &SftpSession,
    directory: &macsftp_core::RemotePath,
    cancel: &CancellationToken,
) -> Result<(), UserFacingError> {
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
            Ok(_) => {}
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                match sftp.create_dir(current.clone()).await {
                    Ok(()) => {}
                    Err(SftpError::Status(status)) if status.status_code == StatusCode::Failure => {
                        sftp.metadata(current.clone()).await.map(|_| ()).map_err(|error| {
                            transfer_error(
                                ErrorCode::Unknown,
                                "Could not create remote directory",
                                "The remote directory could not be created. Check permissions and try again.",
                                &error.to_string(),
                            )
                        })?;
                    }
                    Err(error) => {
                        return Err(transfer_error(
                            ErrorCode::Unknown,
                            "Could not create remote directory",
                            "The remote directory could not be created. Check permissions and try again.",
                            &error.to_string(),
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(transfer_error(
                    ErrorCode::Unknown,
                    "Could not inspect remote directory",
                    "The remote destination could not be checked. Try again.",
                    &error.to_string(),
                ));
            }
        }
    }
    Ok(())
}

async fn download_single_file(
    sftp: &SftpSession,
    job: &TransferJob,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
    connection_key: &str,
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
                "Could not read remote file",
                "The remote file could not be read. Check permissions and try again.",
                &error.to_string(),
            )
        })?;
    if source_metadata.file_type() == SftpFileType::Symlink {
        return download_symlink(sftp, job, event_tx, cancel).await;
    }
    if source_metadata.file_type() == SftpFileType::Dir {
        return download_directory(job, &source_metadata, event_tx, cancel).await;
    }
    let bytes_total = source_metadata.size;
    emit_transfer_running(event_tx, job, bytes_total).await?;

    ensure_local_parent_directory(destination, cancel).await?;
    let temporary_destination = local_temporary_path(destination, job.id);
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
    if !cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await {
        return Err(UserFacingError::new(
            ErrorCode::Unknown,
            "Could not prepare download",
            "A previous temporary download file could not be removed. Retry the transfer.",
        )
        .with_retryable(true));
    }
    let copy_result = async {
        let mut source_file = sftp.open(source.as_str()).await.map_err(|error| {
            transfer_error(
                ErrorCode::Unknown,
                "Could not open remote file",
                "The remote file could not be opened. Check permissions and try again.",
                &error.to_string(),
            )
        })?;
        let mut destination_file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary_destination.as_str())
            .await
            .map_err(local_destination_create_error)?;
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
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        verify_local_hardlink(&temporary_destination, event_tx, job.id).await?;
        remove_local_destination(destination.as_str()).await?;
    }
    if let Err(error) = commit_local_temp(&temporary_destination, destination).await {
        cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
        return Err(error);
    }
    cleanup_local_temp(&temporary_destination, event_tx, job.id, Some(job.id)).await;
    preserve_download_metadata(job, &source_metadata, event_tx).await;
    Ok(())
}

async fn download_directory(
    job: &TransferJob,
    source_metadata: &FileAttributes,
    event_tx: &flume::Sender<AppEvent>,
    cancel: &CancellationToken,
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
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_local_destination(destination.as_str()).await?;
    }
    tokio::select! {
        _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
        result = tokio::fs::create_dir_all(destination.as_str()) => result.map_err(|error| {
            local_transfer_error("Could not create download directory", &error)
        })?,
    }
    preserve_download_metadata(job, source_metadata, event_tx).await;
    Ok(())
}

async fn upload_symlink(
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
        .map_err(|error| local_transfer_error("Could not read symlink target", &error))?;
    ensure_remote_parent_directory(sftp, destination, cancel).await?;
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
                "Could not create remote symlink",
                "The remote symlink could not be created. Check server support and try again.",
                &error.to_string(),
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
            "Could not read remote symlink",
            "The remote symlink target could not be read. Check server support and try again.",
            &error.to_string(),
        )
    })?;
    if matches!(job.conflict_policy, ConflictPolicy::OverwriteAll) {
        remove_local_destination(destination.as_str()).await?;
    }
    ensure_local_parent_directory(destination, cancel).await?;
    tokio::fs::symlink(target, destination.as_str())
        .await
        .map_err(|error| local_transfer_error("Could not create local symlink", &error))
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
            "Could not replace remote file",
            "The existing remote file could not be removed. Check permissions and try again.",
            &error.to_string(),
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
        "Could not create remote file",
        "The remote destination already exists or could not be created. Refresh and try again.",
        &error.to_string(),
    )
}

async fn remove_local_destination(destination: &str) -> Result<(), UserFacingError> {
    match tokio::fs::symlink_metadata(destination).await {
        Ok(metadata) if metadata.is_dir() => {
            tokio::fs::remove_dir_all(destination)
                .await
                .map_err(|error| {
                    local_transfer_error("Could not replace download destination", &error)
                })
        }
        Ok(_) => tokio::fs::remove_file(destination).await.map_err(|error| {
            local_transfer_error("Could not replace download destination", &error)
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(local_transfer_error(
            "Could not inspect download destination",
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
            local_transfer_error("Could not create download directory", &error)
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
    local_transfer_error("Could not create download destination", &error)
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

                tokio::fs::set_permissions(
                    destination.as_str(),
                    std::fs::Permissions::from_mode(permissions),
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
/// launch (plan M5/M6 residual).
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
    const BUFFER_SIZE: usize = 64 * 1024;
    let mut buffer = vec![0; BUFFER_SIZE];
    let mut bytes_done = 0;
    let mut throttle = crate::ProgressThrottle::new(10);

    loop {
        let bytes_read = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = source.read(&mut buffer) => result.map_err(|error| {
                local_transfer_error("Could not read transfer data", &error)
            })?,
        };
        if bytes_read == 0 {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled_transfer_error()),
            result = destination.write_all(&buffer[..bytes_read]) => result.map_err(|error| {
                local_transfer_error("Could not write transfer data", &error)
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
            local_transfer_error("Could not finish transfer", &error)
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
        &error.to_string(),
    )
}

fn transfer_error(code: ErrorCode, title: &str, message: &str, detail: &str) -> UserFacingError {
    let mut error = UserFacingError::new(code, title, message).with_retryable(true);
    error.detail = Some(detail.to_string());
    error
}

fn read_directory_error(error: &SftpError) -> UserFacingError {
    let (code, title, message) = match error {
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile => (
            ErrorCode::NotFound,
            "Remote directory not found",
            "The directory no longer exists. Go to its parent directory or refresh.",
        ),
        SftpError::Status(status) if status.status_code == StatusCode::PermissionDenied => (
            ErrorCode::PermissionDenied,
            "Permission denied",
            "You do not have permission to view this directory. Choose another directory or retry.",
        ),
        _ => (
            ErrorCode::Unknown,
            "Could not read remote directory",
            "The directory could not be loaded. Check permissions and try again.",
        ),
    };
    let mut user_error = UserFacingError::new(code, title, message).with_retryable(true);
    user_error.detail = Some(error.to_string());
    user_error
}

async fn execute_remote_fs_op(sftp: &SftpSession, op: &FsOp) -> Result<(), UserFacingError> {
    match op {
        FsOp::Rename { from, to } => {
            let from = require_remote_path(from, "rename source")?;
            let to = require_remote_path(to, "rename destination")?;
            sftp.rename(from.as_str(), to.as_str())
                .await
                .map_err(|error| remote_fs_error("Could not rename", &error))
        }
        FsOp::CreateDirectory { parent, name } => {
            let parent = require_remote_path(parent, "create directory parent")?;
            let path = parent.join(name);
            sftp.create_dir(path.as_str())
                .await
                .map_err(|error| remote_fs_error("Could not create folder", &error))
        }
        FsOp::Delete { entries } => {
            for entry in entries {
                let path = require_remote_path(&entry.path, "delete path")?;
                if entry.is_dir {
                    remove_remote_dir_recursive(sftp, path).await?;
                } else {
                    sftp.remove_file(path.as_str())
                        .await
                        .map_err(|error| remote_fs_error("Could not delete", &error))?;
                }
            }
            Ok(())
        }
    }
}

fn require_remote_path<'a>(path: &'a FsPath, label: &str) -> Result<&'a RemotePath, UserFacingError> {
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
        .map_err(|error| remote_fs_error("Could not list directory for delete", &error))?;
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
                .map_err(|error| remote_fs_error("Could not delete", &error))?;
        }
    }
    sftp.remove_dir(path.as_str())
        .await
        .map_err(|error| remote_fs_error("Could not delete folder", &error))
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
        "Check the remote path and permissions, then try again.",
    )
    .with_retryable(true);
    user_error.detail = Some(error.to_string());
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

#[cfg(test)]
mod tests {
    use macsftp_core::{
        ConflictPolicy, ErrorCode, LocalPath, MetadataPolicy, RemotePath, Timestamp,
        TransferDirection, TransferEndpoint, TransferId, TransferJob, TransferState,
    };
    use russh_sftp::client::error::Error as SftpError;
    use russh_sftp::protocol::{Status, StatusCode};

    use super::{
        local_download_destination, local_temporary_path, read_directory_error,
        remote_temporary_path, rename_destination,
    };

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
        assert_eq!(error.title, "Remote directory not found");
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

        assert_eq!(error.title, "Could not plan download");
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
}
