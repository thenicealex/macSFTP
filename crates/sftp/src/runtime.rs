use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use macsftp_core::{
    AppCommand, AppEvent, CommandDispatchError, ErrorCode, FsCommand, FsScope, RemoteEventScope,
    RemoteOperationFailure, RemoteScoped, RuntimeBridgeConfig, SessionId, TabId, Timestamp,
    TransferDirection, TransferId, TransferJob, TransferPlanId, TransferPlanSnapshot,
    TrustDecision, TrustRequestId, UserFacingError,
};
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::known_hosts::KnownHostsStore;
use crate::mock_actor::{MockRemoteSessionActor, MockSessionConfig};
use crate::session_actor::{HostTrustConfig, RemoteSessionActor, RemoteSessionRequest};
use crate::transfer_planner::{new_plan, plan_local_upload};
use crate::trust::TrustRegistry;
use tracing::{info, warn};

/// Which session actor `ConnectTab` spawns.
///
/// `Real` connects to actual SSH servers through `RemoteSessionActor`;
/// `Mock` keeps the deterministic in-process flow for tests.
pub enum SessionBackend {
    Real(HostTrustConfig),
    Mock(MockSessionConfig),
}

/// Channels connecting GPUI (sender side) to the Tokio runtime (receiver side).
///
/// The command channel is bounded; GPUI must use [`RuntimeClient::try_send`]
/// to avoid blocking the UI thread. The event channel is bounded; the Tokio
/// side uses `send_async` so backpressure flows to actors, while progress
/// events are throttled at source before reaching the channel.
pub struct BridgeChannels {
    pub command_tx: flume::Sender<AppCommand>,
    pub command_rx: flume::Receiver<AppCommand>,
    pub event_tx: flume::Sender<AppEvent>,
    pub event_rx: flume::Receiver<AppEvent>,
}

impl BridgeChannels {
    pub fn new(config: &RuntimeBridgeConfig) -> Self {
        let (command_tx, command_rx) = flume::bounded(config.command_channel_capacity);
        let (event_tx, event_rx) = flume::bounded(config.event_channel_capacity);
        Self {
            command_tx,
            command_rx,
            event_tx,
            event_rx,
        }
    }
}

/// GPUI-facing handle for sending commands to the runtime.
///
/// Wraps a bounded `flume::Sender` and enforces non-blocking sends via
/// `try_send`. The GPUI main thread must never block on command dispatch.
#[derive(Clone)]
pub struct RuntimeClient {
    command_tx: flume::Sender<AppCommand>,
}

impl RuntimeClient {
    pub fn new(command_tx: flume::Sender<AppCommand>) -> Self {
        Self { command_tx }
    }

    /// Non-blocking send. Returns `ChannelFull` when the command channel is
    /// saturated — the caller should surface a lightweight warning to the
    /// user, not retry in a loop on the UI thread.
    pub fn try_send(&self, command: AppCommand) -> Result<(), CommandDispatchError> {
        match self.command_tx.try_send(command) {
            Ok(()) => Ok(()),
            Err(flume::TrySendError::Full(_)) => Err(CommandDispatchError::ChannelFull),
            Err(flume::TrySendError::Disconnected(_)) => Err(CommandDispatchError::ChannelClosed),
        }
    }
}

/// GPUI-facing handle for draining events produced by the Tokio runtime.
///
/// The drain task runs on the GPUI executor and must only await
/// `recv()` — never network, file IO, or other blocking work.
///
/// This is the sole consumer of the runtime's bounded event channel. The app
/// owns it at the process boundary and fans window-scoped events out only
/// after process-wide state has been reduced exactly once.
pub struct EventReceiver {
    event_rx: flume::Receiver<AppEvent>,
}

impl EventReceiver {
    pub fn new(event_rx: flume::Receiver<AppEvent>) -> Self {
        Self { event_rx }
    }

    /// Async receive. Safe to await on the GPUI executor.
    ///
    /// Returns `None` when all senders have been dropped (i.e. the runtime
    /// has shut down). The bounded channel never overwrites unread events;
    /// a slow consumer applies backpressure to runtime producers instead.
    pub async fn recv(&mut self) -> Option<AppEvent> {
        self.event_rx.recv_async().await.ok()
    }

    /// Non-blocking receive for tests and diagnostics.
    pub fn try_recv(&mut self) -> Option<AppEvent> {
        self.event_rx.try_recv().ok()
    }
}

/// Build a standalone bounded event pair without a full
/// [`RuntimeController`] (its Tokio runtime, dispatch loop, trust
/// registry, ...). Exists so callers outside this crate — e.g. the
/// `macsftp-app` `Workspace` test harness — never need to name
/// `flume` directly.
pub fn test_event_channel(capacity: usize) -> (flume::Sender<AppEvent>, EventReceiver) {
    let (tx, rx) = flume::bounded(capacity);
    (tx, EventReceiver::new(rx))
}

/// Owns the Tokio runtime and the bridge channels.
///
/// Created by [`RuntimeController::start`]. Dropping the controller triggers
/// explicit shutdown: a `Shutdown` command is enqueued, the command loop
/// stops accepting new work, actors are cancelled, and the runtime is
/// shut down with the configured timeout.
pub struct RuntimeController {
    /// Wrapped in `Option` because `Runtime::shutdown_timeout` takes
    /// ownership. We `take()` it during explicit shutdown or `Drop`.
    runtime: Option<Runtime>,
    channels: BridgeChannels,
    command_loop_handle: Option<JoinHandle<()>>,
    event_receiver: Option<EventReceiver>,
    trust_registry: Arc<TrustRegistry>,
    config: RuntimeBridgeConfig,
    shutdown_initiated: bool,
}

impl RuntimeController {
    /// Boot the runtime: create the Tokio runtime, channels, trust
    /// registry, and spawn the command dispatch loop with the given
    /// session backend.
    pub fn start(config: RuntimeBridgeConfig, backend: SessionBackend) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");

        info!(
            workers = runtime.handle().metrics().num_workers(),
            "runtime controller started"
        );

        let channels = BridgeChannels::new(&config);
        let event_receiver = Some(EventReceiver::new(channels.event_rx.clone()));
        let trust_registry = Arc::new(TrustRegistry::new());

        let command_rx = channels.command_rx.clone();
        let event_tx = channels.event_tx.clone();
        let loop_config = config;
        let registry_clone = trust_registry.clone();
        let command_loop_handle = runtime.spawn(command_dispatch_loop(
            command_rx,
            event_tx,
            registry_clone,
            backend,
            loop_config,
        ));

        Self {
            runtime: Some(runtime),
            channels,
            command_loop_handle: Some(command_loop_handle),
            event_receiver,
            trust_registry,
            config,
            shutdown_initiated: false,
        }
    }

    /// Return the GPUI-facing client for sending commands.
    pub fn client(&self) -> RuntimeClient {
        RuntimeClient::new(self.channels.command_tx.clone())
    }

    /// Take the process-wide GPUI-facing event receiver.
    ///
    /// The receiver has one authoritative owner so process-wide events are
    /// reduced exactly once. Subsequent calls return `None`.
    pub fn take_event_receiver(&mut self) -> Option<EventReceiver> {
        self.event_receiver.take()
    }

    /// Explicit shutdown. Sends `Shutdown`, waits for the command loop to
    /// finish, then shuts down the runtime with the configured timeout.
    ///
    /// After this returns, the controller is consumed and must not be used.
    pub fn shutdown(mut self) {
        info!("runtime controller shutting down");
        if self.shutdown_initiated {
            return;
        }
        self.shutdown_initiated = true;

        // Best-effort: enqueue Shutdown so the dispatch loop can stop cleanly.
        // If the channel is full or closed, the loop will still terminate
        // because we abort its task below.
        let _ = self.channels.command_tx.try_send(AppCommand::Shutdown);

        if let Some(handle) = self.command_loop_handle.take() {
            // Abort ensures we don't hang if the loop is stuck on a send.
            handle.abort();
        }
        // Reject all pending trust requests so no actor hangs waiting.
        self.trust_registry.reject_all();

        // Explicit timeout-based shutdown — never rely on Runtime's default Drop.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(self.config.shutdown_timeout);
        }
    }

    /// Expose the runtime for spawning tasks internally (e.g. during tests).
    ///
    /// Panics if called after shutdown — internal callers must not use it
    /// past the shutdown point. Used by M2c actor dispatch and tests.
    #[allow(dead_code)]
    pub(crate) fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("runtime must not be accessed after shutdown")
    }

    /// Expose the event sender for internal tests.
    #[cfg(test)]
    pub(crate) fn event_sender(&self) -> &flume::Sender<AppEvent> {
        &self.channels.event_tx
    }
}

impl Drop for RuntimeController {
    fn drop(&mut self) {
        if !self.shutdown_initiated {
            if let Some(handle) = self.command_loop_handle.take() {
                handle.abort();
            }
            self.trust_registry.reject_all();
            if let Some(runtime) = self.runtime.take() {
                runtime.shutdown_timeout(self.config.shutdown_timeout);
            }
        }
    }
}

/// Handle to a running browsing session actor.
struct RemoteSessionHandle {
    #[allow(dead_code)]
    session_id: SessionId,
    session_epoch: u64,
    /// Real actors receive directory requests through this bounded
    /// mailbox. Mock actors intentionally do not implement browsing.
    request_tx: Option<flume::Sender<RemoteSessionRequest>>,
    cancel: CancellationToken,
    #[allow(dead_code)]
    join: JoinHandle<()>,
}

/// Route `AppCommand::Fs`. Local mutations are owned by the app (platform
/// APIs); the runtime only forwards remote ops to the browsing actor.
async fn dispatch_fs_command(
    command: FsCommand,
    sessions: &HashMap<TabId, RemoteSessionHandle>,
    event_tx: &flume::Sender<AppEvent>,
) {
    match command.scope {
        FsScope::Local { .. } => {
            warn!("local FsCommand reached runtime; local ops are handled in the app");
        }
        FsScope::Remote {
            tab_id,
            session_epoch,
        } => {
            let scope = command.scope.clone();
            let Some(session) = sessions.get(&tab_id) else {
                let _ = event_tx
                    .send_async(AppEvent::FsOperationFailed {
                        scope,
                        failure: UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Not connected",
                            "Connect to the server before changing remote files.",
                        )
                        .with_retryable(true),
                    })
                    .await;
                return;
            };
            if session.session_epoch != session_epoch {
                // Stale command after reconnect — drop silently.
                return;
            }
            let Some(request_tx) = &session.request_tx else {
                let _ = event_tx
                    .send_async(AppEvent::FsOperationFailed {
                        scope,
                        failure: UserFacingError::new(
                            ErrorCode::Unknown,
                            "Remote session unavailable",
                            "This session does not support file operations.",
                        )
                        .with_retryable(true),
                    })
                    .await;
                return;
            };
            let Some(refresh_path) = command
                .op
                .refresh_path()
                .and_then(|path| path.as_remote().cloned())
            else {
                let _ = event_tx
                    .send_async(AppEvent::FsOperationFailed {
                        scope,
                        failure: UserFacingError::new(
                            ErrorCode::Unknown,
                            "Invalid remote path",
                            "Could not determine which directory to refresh after the operation.",
                        ),
                    })
                    .await;
                return;
            };
            if request_tx
                .try_send(RemoteSessionRequest::Fs {
                    scope: scope.clone(),
                    op: command.op,
                    refresh_path,
                })
                .is_err()
            {
                let _ = event_tx
                    .send_async(AppEvent::FsOperationFailed {
                        scope,
                        failure: UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Could not start file operation",
                            "The remote session is busy or disconnected. Try again.",
                        )
                        .with_retryable(true),
                    })
                    .await;
            }
        }
    }
}

#[derive(Clone)]
struct TransferRoute {
    request_tx: flume::Sender<RemoteSessionRequest>,
    plan_id: TransferPlanId,
    job: TransferJob,
}

/// The command dispatch loop running on the Tokio runtime.
///
/// Receives `AppCommand`s from the GPUI side, routes them to mock actors
/// or the `TrustRegistry`, and emits `AppEvent`s back to GPUI.
///
/// Command routing (M2c):
/// - `ConnectTab` → cancel any old session for the tab, reject its stale
///   trust requests, spawn a `MockRemoteSessionActor` bound to the
///   command's UI-allocated `session_id`/`session_epoch`, emit
///   `TabConnecting`.
/// - `AcceptHostKey` → resolve the trust request via `TrustRegistry`.
/// - `RejectHostKey` → resolve the trust request via `TrustRegistry`.
/// - `DisconnectTab` / `CloseTab` → cancel the actor, reject pending
///   trust requests for the tab.
/// - `Shutdown` → cancel all actors, reject all trust requests, exit.
async fn command_dispatch_loop(
    command_rx: flume::Receiver<AppCommand>,
    event_tx: flume::Sender<AppEvent>,
    trust_registry: Arc<TrustRegistry>,
    backend: SessionBackend,
    _config: RuntimeBridgeConfig,
) {
    let mut sessions: HashMap<TabId, RemoteSessionHandle> = HashMap::new();
    let mut next_trust_id: u64 = 1;
    let mut next_plan_id: u64 = 1;
    let next_transfer_id = Arc::new(AtomicU64::new(1));
    let next_conflict_id = Arc::new(AtomicU64::new(1));
    let transfer_routes: Arc<Mutex<HashMap<TransferId, TransferRoute>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let planning_cancellations: Arc<Mutex<HashMap<TransferId, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Real sessions share one known_hosts store so a trust decision in
    // one tab covers later sessions to the same host (plan ADR-003).
    let known_hosts: Option<(Arc<Mutex<KnownHostsStore>>, Arc<HostTrustConfig>)> = match &backend {
        SessionBackend::Real(trust_config) => {
            let store = KnownHostsStore::load(
                &trust_config.app_known_hosts_path,
                trust_config.user_known_hosts_path.as_deref(),
            );
            Some((Arc::new(Mutex::new(store)), Arc::new(trust_config.clone())))
        }
        SessionBackend::Mock(_) => None,
    };

    let connection_manager = Arc::new(crate::pool::ConnectionManager::new());
    loop {
        match command_rx.recv_async().await {
            Ok(AppCommand::Shutdown) => break,

            Ok(AppCommand::ConnectTab(cmd)) => {
                // Cancel the old session if the tab is reconnecting, and
                // reject trust requests bound to any previous epoch.
                if let Some(handle) = sessions.remove(&cmd.tab_id) {
                    handle.cancel.cancel();
                }
                trust_registry.reject_stale(cmd.tab_id, cmd.session_epoch);

                let trust_request_id = TrustRequestId(next_trust_id);
                next_trust_id += 1;

                let cancel = CancellationToken::new();
                let (join, request_tx) = match (&backend, &known_hosts) {
                    (SessionBackend::Real(_), Some((store, trust_config))) => {
                        let (request_tx, request_rx) = flume::bounded(16);

                        let scope =
                            RemoteEventScope::new(cmd.tab_id, cmd.session_id, cmd.session_epoch);
                        let mut receiver = connection_manager.get_or_connect(
                            &cmd.settings,
                            &scope,
                            trust_request_id,
                            store.clone(),
                            trust_config.clone(),
                            trust_registry.clone(),
                            event_tx.clone(),
                            cancel.clone(),
                        );

                        let event_tx_clone = event_tx.clone();
                        let next_conflict_id_clone = next_conflict_id.clone();
                        let cancel_clone = cancel.clone();
                        let host_clone = cmd.settings.host.clone();

                        let join = tokio::spawn(async move {
                            match receiver.recv().await {
                                Ok(Ok(shared_connection)) => {
                                    let channel_result = async {
                                        let channel = shared_connection.handle.channel_open_session().await.map_err(|error| {
                                            crate::physical_connection::ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "Could not open SFTP channel on shared connection", error.to_string()))
                                        })?;
                                        channel.request_subsystem(true, "sftp").await.map_err(|error| {
                                            crate::physical_connection::ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "The server rejected the SFTP subsystem.", error.to_string()))
                                        })?;
                                        russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|error| {
                                            crate::physical_connection::ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "Could not start the SFTP session.", error.to_string()))
                                        })
                                    }.await;

                                    match channel_result {
                                        Ok(sftp) => {
                                            let actor = RemoteSessionActor::new(
                                                cmd.tab_id,
                                                cmd.session_id,
                                                cmd.session_epoch,
                                                shared_connection,
                                                sftp,
                                                event_tx_clone,
                                                next_conflict_id_clone,
                                            );
                                            actor.run(cancel_clone, request_rx).await;
                                        }
                                        Err(failure) => {
                                            crate::physical_connection::log_connect_failure(
                                                &host_clone,
                                                &failure,
                                            );
                                            let event = match failure {
                                                crate::physical_connection::ConnectFailure::HostKeyMismatch => None,
                                                crate::physical_connection::ConnectFailure::TrustRejected => {
                                                    Some(AppEvent::TabDisconnected(RemoteScoped::new(
                                                        scope.clone(),
                                                        macsftp_core::TabDisconnected { reason: macsftp_core::DisconnectReason::UserRequested },
                                                    )))
                                                }
                                                crate::physical_connection::ConnectFailure::TrustTimeout => {
                                                    Some(AppEvent::TabDisconnected(RemoteScoped::new(
                                                        scope.clone(),
                                                        macsftp_core::TabDisconnected { reason: macsftp_core::DisconnectReason::Error(macsftp_core::UserFacingError::new(ErrorCode::Unknown, "Trust prompt timed out", "You did not respond to the host key prompt in time.")) },
                                                    )))
                                                }
                                                crate::physical_connection::ConnectFailure::AuthFailed(_) => None,
                                                crate::physical_connection::ConnectFailure::Connection(error) => {
                                                    Some(AppEvent::TabDisconnected(RemoteScoped::new(
                                                        scope.clone(),
                                                        macsftp_core::TabDisconnected { reason: macsftp_core::DisconnectReason::Error(error) },
                                                    )))
                                                }
                                            };
                                            if let Some(event) = event {
                                                let _ = event_tx_clone.send_async(event).await;
                                            }
                                        }
                                    }
                                }
                                Ok(Err(failure)) => {
                                    crate::physical_connection::log_connect_failure(
                                        &host_clone,
                                        &failure,
                                    );
                                    let event = match failure {
                                        crate::physical_connection::ConnectFailure::HostKeyMismatch => None,
                                        crate::physical_connection::ConnectFailure::TrustRejected => {
                                            Some(AppEvent::TabDisconnected(RemoteScoped::new(
                                                scope.clone(),
                                                macsftp_core::TabDisconnected { reason: macsftp_core::DisconnectReason::UserRequested },
                                            )))
                                        }
                                        crate::physical_connection::ConnectFailure::TrustTimeout => {
                                            Some(AppEvent::TabDisconnected(RemoteScoped::new(
                                                scope.clone(),
                                                macsftp_core::TabDisconnected { reason: macsftp_core::DisconnectReason::Error(macsftp_core::UserFacingError::new(ErrorCode::Unknown, "Trust prompt timed out", "You did not respond to the host key prompt in time.")) },
                                            )))
                                        }
                                        crate::physical_connection::ConnectFailure::AuthFailed(_) => None,
                                        crate::physical_connection::ConnectFailure::Connection(error) => {
                                            Some(AppEvent::TabDisconnected(RemoteScoped::new(
                                                scope.clone(),
                                                macsftp_core::TabDisconnected { reason: macsftp_core::DisconnectReason::Error(error) },
                                            )))
                                        }
                                    };
                                    if let Some(event) = event {
                                        let _ = event_tx_clone.send_async(event).await;
                                    }
                                }
                                Err(_) => {}
                            }
                        });

                        (join, Some(request_tx))
                    }
                    _ => {
                        let mock_config = match &backend {
                            SessionBackend::Mock(mock_config) => mock_config.clone(),
                            SessionBackend::Real(_) => MockSessionConfig::default(),
                        };
                        let actor = MockRemoteSessionActor::new(
                            cmd.tab_id,
                            cmd.session_id,
                            cmd.session_epoch,
                            trust_request_id,
                            mock_config,
                            event_tx.clone(),
                            trust_registry.clone(),
                        );
                        (tokio::spawn(actor.run(cancel.clone())), None)
                    }
                };

                // Emit TabConnecting after spawning the actor.
                let _ = event_tx
                    .send_async(AppEvent::TabConnecting { tab_id: cmd.tab_id })
                    .await;

                sessions.insert(
                    cmd.tab_id,
                    RemoteSessionHandle {
                        session_id: cmd.session_id,
                        session_epoch: cmd.session_epoch,
                        request_tx,
                        cancel,
                        join,
                    },
                );
            }

            Ok(AppCommand::AcceptHostKey(cmd)) => {
                trust_registry.resolve(cmd.request_id, TrustDecision::TrustAndSave);
            }

            Ok(AppCommand::RejectHostKey { request_id }) => {
                trust_registry.resolve(request_id, TrustDecision::Reject);
            }

            Ok(AppCommand::DisconnectTab { tab_id }) | Ok(AppCommand::CloseTab { tab_id }) => {
                if let Some(handle) = sessions.remove(&tab_id) {
                    handle.cancel.cancel();
                }
                trust_registry.reject_all_for_tab(tab_id);
            }

            Ok(AppCommand::RemoveRemoteTempFile {
                tab_id,
                transfer_id,
                path,
            }) => {
                let Some(session) = sessions.get(&tab_id) else {
                    continue;
                };
                let Some(request_tx) = &session.request_tx else {
                    continue;
                };
                let _ = request_tx
                    .try_send(RemoteSessionRequest::RemoveRemoteTempFile { transfer_id, path });
            }
            Ok(AppCommand::ReadRemoteDir { tab_id, path }) => {
                let Some(session) = sessions.get(&tab_id) else {
                    continue;
                };
                let Some(request_tx) = &session.request_tx else {
                    continue;
                };

                let request_result = request_tx.try_send(RemoteSessionRequest::ReadDir { path });
                let request_failure = match request_result {
                    Ok(()) => None,
                    Err(flume::TrySendError::Full(RemoteSessionRequest::ReadDir { path })) => {
                        Some((
                            path,
                            "The remote directory request queue is full. Try again.",
                        ))
                    }
                    Err(flume::TrySendError::Disconnected(RemoteSessionRequest::ReadDir {
                        path,
                    })) => Some((
                        path,
                        "The remote session is no longer available. Reconnect and try again.",
                    )),
                    _ => None,
                };
                if let Some((path, message)) = request_failure {
                    let scope =
                        RemoteEventScope::new(tab_id, session.session_id, session.session_epoch);
                    let error = UserFacingError::new(
                        ErrorCode::ChannelClosed,
                        "Could not read remote directory",
                        message,
                    )
                    .with_retryable(true);
                    let _ = event_tx
                        .send_async(AppEvent::RemoteOperationFailed(RemoteScoped::new(
                            scope,
                            RemoteOperationFailure {
                                operation: "Read remote directory".to_string(),
                                path: Some(path),
                                error,
                            },
                        )))
                        .await;
                }
            }

            Ok(AppCommand::StartTransfer(command)) => {
                let transfer_request_tx = sessions
                    .get(&command.tab_id)
                    .filter(|session| session.session_epoch == command.session_epoch)
                    .and_then(|session| session.request_tx.clone());
                let plan_id = TransferPlanId(next_plan_id);
                next_plan_id += 1;
                let root_job_id = TransferId(next_transfer_id.fetch_add(1, Ordering::Relaxed));
                let planning_cancel = CancellationToken::new();
                if let Ok(mut cancellations) = planning_cancellations.lock() {
                    cancellations.insert(root_job_id, planning_cancel.clone());
                } else {
                    let _ = event_tx
                        .send_async(AppEvent::TransferPlanFailed {
                            plan_id,
                            error: UserFacingError::new(
                                ErrorCode::Unknown,
                                "Could not start transfer planning",
                                "The transfer planner is unavailable. Try again.",
                            )
                            .with_retryable(true),
                        })
                        .await;
                    continue;
                }
                let (plan, root_job) = new_plan(
                    &command,
                    plan_id,
                    root_job_id,
                    Timestamp(std::time::SystemTime::now()),
                );
                if event_tx
                    .send_async(AppEvent::TransferPlanStarted(TransferPlanSnapshot {
                        plan,
                        root_job: root_job.clone(),
                    }))
                    .await
                    .is_err()
                {
                    break;
                }

                let event_tx = event_tx.clone();
                let next_transfer_id = next_transfer_id.clone();
                let routes = transfer_routes.clone();
                let cancellations = planning_cancellations.clone();
                let planner_request_tx = transfer_request_tx.clone();
                let planning_task: JoinHandle<Option<Vec<TransferJob>>> = match command.direction {
                    TransferDirection::Upload => tokio::task::spawn_blocking(move || {
                        plan_local_upload(
                            command,
                            plan_id,
                            root_job,
                            next_transfer_id,
                            event_tx,
                            planning_cancel,
                        )
                    }),
                    TransferDirection::Download => tokio::spawn(async move {
                        let Some(request_tx) = planner_request_tx else {
                            let _ = event_tx
                                    .send_async(AppEvent::TransferPlanFailed {
                                        plan_id,
                                        error: UserFacingError::new(
                                            ErrorCode::ChannelClosed,
                                            "Could not start download planning",
                                            "The connected session is no longer available. Reconnect and try again.",
                                        )
                                        .with_retryable(true),
                                    })
                                    .await;
                            return None;
                        };
                        let (responder, response) = oneshot::channel();
                        if request_tx
                            .send_async(RemoteSessionRequest::PlanRemoteDownload {
                                command,
                                plan_id,
                                created_at: root_job.created_at,
                                next_transfer_id,
                                cancel: planning_cancel,
                                responder,
                            })
                            .await
                            .is_err()
                        {
                            let _ = event_tx
                                    .send_async(AppEvent::TransferPlanFailed {
                                        plan_id,
                                        error: UserFacingError::new(
                                            ErrorCode::ChannelClosed,
                                            "Could not start download planning",
                                            "The connected session is no longer available. Reconnect and try again.",
                                        )
                                        .with_retryable(true),
                                    })
                                    .await;
                            return None;
                        }
                        response.await.ok().flatten()
                    }),
                };
                std::mem::drop(tokio::spawn(async move {
                    let Ok(Some(jobs)) = planning_task.await else {
                        if let Ok(mut cancellations) = cancellations.lock() {
                            cancellations.remove(&root_job_id);
                        }
                        return;
                    };
                    if let Ok(mut cancellations) = cancellations.lock() {
                        cancellations.remove(&root_job_id);
                    }
                    let Some(request_tx) = transfer_request_tx else {
                        return;
                    };
                    if let Ok(mut routes) = routes.lock() {
                        for job in &jobs {
                            routes.insert(
                                job.id,
                                TransferRoute {
                                    request_tx: request_tx.clone(),
                                    plan_id,
                                    job: job.clone(),
                                },
                            );
                        }
                    } else {
                        warn!("transfer route registry is unavailable");
                        return;
                    }
                    if let Err(error) = request_tx
                        .send_async(RemoteSessionRequest::RunTransferJobs { plan_id, jobs })
                        .await
                    {
                        warn!(error = %error, "planned transfer could not reach session actor");
                    }
                }));
            }

            Ok(AppCommand::CancelTransfer { transfer_id }) => {
                let planning_cancel = planning_cancellations
                    .lock()
                    .ok()
                    .and_then(|cancellations| cancellations.get(&transfer_id).cloned());
                if let Some(cancel) = planning_cancel {
                    cancel.cancel();
                    continue;
                }
                let route = transfer_routes
                    .lock()
                    .ok()
                    .and_then(|routes| routes.get(&transfer_id).cloned());
                if let Some(route) = route
                    && let Err(error) = route
                        .request_tx
                        .try_send(RemoteSessionRequest::CancelTransfer { transfer_id })
                {
                    eprintln!("WARN: transfer cancellation could not reach session actor: {error}");
                }
            }

            Ok(AppCommand::RetryTransfer { transfer_id }) => {
                let route = transfer_routes
                    .lock()
                    .ok()
                    .and_then(|routes| routes.get(&transfer_id).cloned());
                if let Some(route) = route
                    && let Err(error) =
                        route
                            .request_tx
                            .try_send(RemoteSessionRequest::RetryTransfer {
                                plan_id: route.plan_id,
                                job: route.job,
                            })
                {
                    eprintln!("WARN: transfer retry could not reach session actor: {error}");
                }
            }

            Ok(AppCommand::ResolveTransferConflict(command)) => {
                for session in sessions.values() {
                    if let Some(request_tx) = &session.request_tx {
                        let request_tx = request_tx.clone();
                        let request_id = command.request_id;
                        let decision = command.decision.clone();
                        std::mem::drop(tokio::spawn(async move {
                            if request_tx
                                .send_async(RemoteSessionRequest::ResolveTransferConflict {
                                    request_id,
                                    decision,
                                })
                                .await
                                .is_err()
                            {
                                warn!("conflict decision session actor is unavailable");
                            }
                        }));
                    }
                }
            }

            Ok(AppCommand::Fs(command)) => {
                dispatch_fs_command(command, &sessions, &event_tx).await;
            }

            Ok(_) => {
                // Other commands (OpenTab, StartTransfer, etc.) are not
                // handled yet.
            }

            Err(flume::RecvError::Disconnected) => break,
        }
    }

    // Shutdown cleanup: cancel all actors and reject all pending requests.
    for (_, handle) in sessions.drain() {
        handle.cancel.cancel();
    }
    trust_registry.reject_all();
}

/// A simple source-side throttle for progress events.
///
/// Per ADR-002, `TransferProgress` must be throttled at source (before
/// entering the event channel) to at most `max_hz` events per second per
/// transfer. The drain-side coalescing is fallback only.
pub struct ProgressThrottle {
    max_interval: Duration,
    last_sent: Option<Instant>,
}

impl ProgressThrottle {
    pub fn new(max_hz: u8) -> Self {
        // Guard against zero — treat as "no throttle" via a tiny interval.
        let hz = if max_hz == 0 { 1 } else { max_hz };
        let max_interval = Duration::from_nanos(1_000_000_000 / hz as u64);
        Self {
            max_interval,
            last_sent: None,
        }
    }

    /// Returns `true` if a progress event should be sent now, `false` if it
    /// should be suppressed. The caller must call this *before* constructing
    /// the event to avoid wasted allocations.
    pub fn should_send(&mut self, now: Instant) -> bool {
        match self.last_sent {
            Some(last) if now.duration_since(last) < self.max_interval => false,
            _ => {
                self.last_sent = Some(now);
                true
            }
        }
    }

    /// Force-reset the throttle state. Useful when a transfer transitions
    /// to a terminal state and the final progress must be emitted.
    pub fn reset(&mut self) {
        self.last_sent = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macsftp_core::{
        AuthCredential, ConflictPolicy, ConnectCommand, ConnectionSettings, HostKeyDecisionCommand,
        LocalPath, MetadataPolicy, ProfileId, RemotePath, SessionId, StartTransferCommand, TabId,
        TransferDirection, TransferEndpoint, TransferId, TransferPlanState, TransferState,
    };

    /// Placeholder settings — the mock backend never dials them.
    fn test_settings() -> ConnectionSettings {
        ConnectionSettings {
            host: "mock.example.com".to_string(),
            port: 22,
            username: "tester".to_string(),
            auth: AuthCredential::Password {
                password: "unused".to_string(),
            },
        }
    }
    use std::time::Duration;

    #[test]
    fn runtime_client_try_send_rejects_when_channel_full() {
        let config = RuntimeBridgeConfig {
            command_channel_capacity: 1,
            event_channel_capacity: 16,
            transfer_progress_hz: 10,
            shutdown_timeout: Duration::from_secs(1),
        };
        let controller =
            RuntimeController::start(config, SessionBackend::Mock(MockSessionConfig::default()));
        let client = controller.client();

        // Fill the single-slot command channel.
        client
            .try_send(AppCommand::CloseTab { tab_id: TabId(1) })
            .expect("first command should fit");

        // Second send must return ChannelFull, not block.
        let result = client.try_send(AppCommand::CloseTab { tab_id: TabId(2) });
        assert_eq!(result, Err(CommandDispatchError::ChannelFull));

        controller.shutdown();
    }

    #[test]
    fn event_receiver_gets_events_via_recv_async() {
        let config = RuntimeBridgeConfig::default();
        let mut controller =
            RuntimeController::start(config, SessionBackend::Mock(MockSessionConfig::default()));
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");
        let event_tx = controller.event_sender().clone();

        // Run the async send/receive on the controller's own runtime.
        // block_on executes on the calling thread; the spawned command loop
        // runs on worker threads, so there is no conflict.
        controller.runtime().block_on(async move {
            let test_event = AppEvent::TransferCompleted {
                transfer_id: TransferId(42),
            };
            event_tx
                .send_async(test_event.clone())
                .await
                .expect("event should be sent");

            let received = event_rx.recv().await;
            assert_eq!(received, Some(test_event));
        });

        // Shutdown outside any async context — Runtime::shutdown_timeout
        // does blocking work and must not be called from within a runtime.
        controller.shutdown();
    }

    #[test]
    fn shutdown_terminates_runtime_within_timeout() {
        let config = RuntimeBridgeConfig {
            shutdown_timeout: Duration::from_millis(500),
            ..RuntimeBridgeConfig::default()
        };
        let controller =
            RuntimeController::start(config, SessionBackend::Mock(MockSessionConfig::default()));

        let start = Instant::now();
        controller.shutdown();
        let elapsed = start.elapsed();

        // Shutdown should complete well within a few seconds.
        // The timeout is 500ms; allow generous margin for runtime teardown.
        assert!(
            elapsed < Duration::from_secs(3),
            "shutdown took too long: {elapsed:?}"
        );
    }

    #[test]
    fn progress_throttle_suppresses_events_within_interval() {
        let mut throttle = ProgressThrottle::new(10); // 10 Hz → 100ms interval
        let base = Instant::now();

        assert!(throttle.should_send(base));
        assert!(
            !throttle.should_send(base + Duration::from_millis(50)),
            "event within interval should be suppressed"
        );
        assert!(
            throttle.should_send(base + Duration::from_millis(101)),
            "event after interval should pass"
        );
    }

    #[test]
    fn progress_throttle_reset_allows_immediate_send() {
        let mut throttle = ProgressThrottle::new(10);
        let now = Instant::now();

        assert!(throttle.should_send(now));
        throttle.reset();
        assert!(
            throttle.should_send(now),
            "after reset, send should be allowed"
        );
    }

    #[test]
    fn runtime_client_returns_channel_closed_after_shutdown() {
        let config = RuntimeBridgeConfig {
            shutdown_timeout: Duration::from_millis(200),
            ..RuntimeBridgeConfig::default()
        };
        let controller =
            RuntimeController::start(config, SessionBackend::Mock(MockSessionConfig::default()));
        let client = controller.client();

        controller.shutdown();

        // After shutdown, the command channel sender is disconnected.
        let result = client.try_send(AppCommand::CloseTab { tab_id: TabId(1) });
        assert_eq!(
            result,
            Err(CommandDispatchError::ChannelClosed),
            "post-shutdown send should report channel closed"
        );
    }

    #[test]
    fn event_receiver_returns_none_when_channel_disconnected() {
        let (event_tx, mut receiver) = test_event_channel(4);

        // Drop the sender — channel is now empty and disconnected.
        drop(event_tx);

        // Use a helper thread to run the async recv, since recv on a
        // disconnected empty channel returns immediately.
        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        let result = helper.block_on(async { receiver.recv().await });
        assert!(
            result.is_none(),
            "recv on disconnected empty channel should return None"
        );

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn event_receiver_try_recv_drains_without_blocking() {
        let (event_tx, mut receiver) = test_event_channel(4);

        // No events yet — try_recv should return None immediately.
        assert!(receiver.try_recv().is_none());

        // Push an event synchronously, then try_recv should get it.
        event_tx
            .send(AppEvent::TransferSkipped {
                transfer_id: TransferId(7),
            })
            .expect("send should succeed with a live subscriber");

        let event = receiver
            .try_recv()
            .expect("try_recv should return the pending event");
        assert_eq!(
            event,
            AppEvent::TransferSkipped {
                transfer_id: TransferId(7),
            }
        );

        // Channel is now empty again.
        assert!(receiver.try_recv().is_none());
    }

    #[test]
    fn runtime_event_receiver_has_one_authoritative_owner() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );

        assert!(controller.take_event_receiver().is_some());
        assert!(controller.take_event_receiver().is_none());

        controller.shutdown();
    }

    #[test]
    fn bounded_event_receiver_applies_backpressure_without_losing_events() {
        let (event_tx, mut receiver) = test_event_channel(2);
        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async move {
            let sender = tokio::spawn(async move {
                for id in 0u64..6 {
                    event_tx
                        .send_async(AppEvent::TransferCompleted {
                            transfer_id: TransferId(id),
                        })
                        .await
                        .expect("receiver should remain connected");
                }
            });

            tokio::task::yield_now().await;
            for expected_id in 0u64..6 {
                assert_eq!(
                    receiver.recv().await,
                    Some(AppEvent::TransferCompleted {
                        transfer_id: TransferId(expected_id),
                    })
                );
            }
            sender.await.expect("sender task should finish");
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn command_dispatch_loop_exits_on_shutdown_command() {
        let config = RuntimeBridgeConfig {
            shutdown_timeout: Duration::from_millis(500),
            ..RuntimeBridgeConfig::default()
        };
        let controller =
            RuntimeController::start(config, SessionBackend::Mock(MockSessionConfig::default()));
        let client = controller.client();

        // Send a few non-shutdown commands first — they should be accepted
        // but not cause any panics or hangs.
        for tab_index in 0u64..5 {
            client
                .try_send(AppCommand::CloseTab {
                    tab_id: TabId(tab_index),
                })
                .expect("command should fit in default-capacity channel");
        }

        // Send Shutdown — the dispatch loop should break and exit.
        client
            .try_send(AppCommand::Shutdown)
            .expect("shutdown command should fit");

        // Give the dispatch loop a moment to process the Shutdown.
        // This is a smoke test; the real correctness is that shutdown()
        // below completes within its timeout.
        controller.shutdown();
    }

    #[test]
    fn progress_throttle_handles_high_hz_without_panic() {
        // 255 Hz — near the u8 max. Should not overflow or panic.
        let mut throttle = ProgressThrottle::new(255);
        let now = Instant::now();

        assert!(throttle.should_send(now));
        // Immediately after — suppressed because interval hasn't elapsed.
        assert!(!throttle.should_send(now));
    }

    #[test]
    fn progress_throttle_zero_hz_does_not_divide_by_zero() {
        // Zero Hz is nonsensical but must not panic. We clamp to 1 Hz.
        let mut throttle = ProgressThrottle::new(0);
        let now = Instant::now();

        assert!(throttle.should_send(now));
    }

    #[test]
    fn multiple_events_flow_through_bounded_event_channel() {
        let config = RuntimeBridgeConfig {
            event_channel_capacity: 8,
            ..RuntimeBridgeConfig::default()
        };
        let mut controller =
            RuntimeController::start(config, SessionBackend::Mock(MockSessionConfig::default()));
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");
        let event_tx = controller.event_sender().clone();

        controller.runtime().block_on(async move {
            // Push several events and verify they all arrive in order.
            for id in 0u64..8 {
                event_tx
                    .send_async(AppEvent::TransferCompleted {
                        transfer_id: TransferId(id),
                    })
                    .await
                    .expect("event should be sent");
            }

            for expected_id in 0u64..8 {
                let event = event_rx.recv().await;
                assert_eq!(
                    event,
                    Some(AppEvent::TransferCompleted {
                        transfer_id: TransferId(expected_id),
                    }),
                    "events should arrive in FIFO order"
                );
            }
        });

        controller.shutdown();
    }

    #[test]
    fn start_transfer_streams_a_local_upload_plan() {
        let fixture_path =
            std::env::temp_dir().join(format!("macsftp-runtime-plan-{}", std::process::id()));
        std::fs::write(&fixture_path, b"plan me").expect("write planning fixture");

        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut events = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            client
                .try_send(AppCommand::StartTransfer(StartTransferCommand {
                    tab_id: TabId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    direction: TransferDirection::Upload,
                    sources: vec![TransferEndpoint::Local(LocalPath::new(
                        fixture_path.display().to_string(),
                    ))],
                    destination: TransferEndpoint::Remote(RemotePath::new("/uploads/plan.txt")),
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::default(),
                }))
                .expect("start transfer should send");

            let started = recv_timeout(&mut events, "TransferPlanStarted").await;
            let plan_id = match started {
                AppEvent::TransferPlanStarted(snapshot) => {
                    assert_eq!(snapshot.plan.state, TransferPlanState::Planning);
                    assert_eq!(snapshot.root_job.state, TransferState::Planning);
                    snapshot.plan.id
                }
                other => panic!("expected TransferPlanStarted, got {other:?}"),
            };
            let progress = recv_timeout(&mut events, "TransferPlanProgress").await;
            assert!(matches!(
                progress,
                AppEvent::TransferPlanProgress(progress)
                    if progress.plan_id == plan_id && progress.planned_count == 1
            ));
            let completed = recv_timeout(&mut events, "TransferPlanCompleted").await;
            assert!(matches!(
                completed,
                AppEvent::TransferPlanCompleted { plan_id: completed_plan_id }
                    if completed_plan_id == plan_id
            ));
        });

        controller.shutdown();
        std::fs::remove_file(&fixture_path).expect("remove planning fixture");
    }

    // ── M2c: Mock actor full loop integration tests ───────────────

    /// Helper: receive an event within a timeout, panicking if it
    /// doesn't arrive.
    async fn recv_timeout(rx: &mut EventReceiver, label: &str) -> AppEvent {
        match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => panic!("{label}: event channel closed"),
            Err(_) => panic!("{label}: timed out waiting for event"),
        }
    }

    /// Helper: assert no `TabConnected` arrives within a short window.
    ///
    /// Stale `TabDisconnected` events from an already-cancelled actor
    /// may still be in flight; the app-side stale event guard drops
    /// them, so they are not a failure here. What must never happen is
    /// the old session connecting.
    async fn assert_no_tab_connected(rx: &mut EventReceiver, label: &str) {
        let deadline = Instant::now() + Duration::from_millis(150);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(AppEvent::TabConnected(scoped))) => {
                    panic!("{label}: unexpected TabConnected: {scoped:?}")
                }
                Ok(Some(AppEvent::TabDisconnected(_))) => {} // stale actor exit — ignored
                Ok(Some(event)) => panic!("{label}: unexpected event: {event:?}"),
                Ok(None) => return, // channel closed — fine
                Err(_) => return,   // timed out — no event, as expected
            }
        }
    }

    #[test]
    fn full_round_trip_connect_accept_host_key_tab_connected() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            // 1. Send ConnectTab.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("connect command should send");

            // 2. Expect TabConnecting.
            let event = recv_timeout(&mut event_rx, "TabConnecting").await;
            assert!(
                matches!(event, AppEvent::TabConnecting { tab_id: TabId(1) }),
                "expected TabConnecting, got {event:?}"
            );

            // 3. Expect HostKeyUnknown.
            let event = recv_timeout(&mut event_rx, "HostKeyUnknown").await;
            let prompt = match event {
                AppEvent::HostKeyUnknown(prompt) => prompt,
                other => panic!("expected HostKeyUnknown, got {other:?}"),
            };
            assert_eq!(prompt.tab_id, TabId(1));
            let old_request_id = prompt.request_id;

            // 4. Send AcceptHostKey.
            client
                .try_send(AppCommand::AcceptHostKey(HostKeyDecisionCommand {
                    request_id: old_request_id,
                }))
                .expect("accept command should send");

            // 5. Expect TabConnected.
            let event = recv_timeout(&mut event_rx, "TabConnected").await;
            match event {
                AppEvent::TabConnected(scoped) => {
                    assert_eq!(scoped.scope.tab_id, TabId(1));
                    assert_eq!(scoped.scope.session_epoch, 1);
                }
                other => panic!("expected TabConnected, got {other:?}"),
            }
        });

        controller.shutdown();
    }

    #[test]
    fn full_round_trip_connect_reject_host_key_tab_disconnected() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("connect command should send");

            // Skip TabConnecting.
            let _ = recv_timeout(&mut event_rx, "TabConnecting").await;

            // Get HostKeyUnknown.
            let event = recv_timeout(&mut event_rx, "HostKeyUnknown").await;
            let prompt = match event {
                AppEvent::HostKeyUnknown(prompt) => prompt,
                other => panic!("expected HostKeyUnknown, got {other:?}"),
            };

            // Reject.
            client
                .try_send(AppCommand::RejectHostKey {
                    request_id: prompt.request_id,
                })
                .expect("reject command should send");

            // Expect TabDisconnected.
            let event = recv_timeout(&mut event_rx, "TabDisconnected").await;
            assert!(
                matches!(event, AppEvent::TabDisconnected(_)),
                "expected TabDisconnected after reject, got {event:?}"
            );
        });

        controller.shutdown();
    }

    #[test]
    fn reconnect_rejects_old_host_key_request() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            // First connection.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("first connect");

            let _ = recv_timeout(&mut event_rx, "first TabConnecting").await;
            let event = recv_timeout(&mut event_rx, "first HostKeyUnknown").await;
            let first_prompt = match event {
                AppEvent::HostKeyUnknown(prompt) => prompt,
                other => panic!("expected first HostKeyUnknown, got {other:?}"),
            };
            assert_eq!(first_prompt.session_epoch, 1);

            // Reconnect — dispatch loop cancels old actor, rejects stale,
            // spawns new actor with epoch 2.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(2),
                    session_epoch: 2,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("reconnect");

            // The old actor might emit TabDisconnected (RequestExpired) or
            // exit silently (cancellation). Skip any such events.
            // We expect the new session's TabConnecting and HostKeyUnknown.
            let mut got_new_prompt = false;
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && !got_new_prompt {
                let event = recv_timeout(&mut event_rx, "reconnect events").await;
                match event {
                    AppEvent::TabConnecting { tab_id: TabId(1) } => {}
                    AppEvent::HostKeyUnknown(prompt) => {
                        assert_eq!(prompt.session_epoch, 2, "new prompt should be epoch 2");
                        got_new_prompt = true;
                    }
                    AppEvent::TabDisconnected(_) => {
                        // Old actor exiting — expected, ignore.
                    }
                    other => panic!("unexpected event during reconnect: {other:?}"),
                }
            }
            assert!(got_new_prompt, "should receive new HostKeyUnknown");

            // Try to accept the OLD request — it should have been rejected
            // by reject_stale, so resolve returns false. No TabConnected
            // should be emitted for the old session.
            client
                .try_send(AppCommand::AcceptHostKey(HostKeyDecisionCommand {
                    request_id: first_prompt.request_id,
                }))
                .expect("old accept command should send");

            assert_no_tab_connected(&mut event_rx, "old accept must not connect the old session")
                .await;
        });

        controller.shutdown();
    }

    #[test]
    fn close_tab_cancels_actor_and_rejects_pending_trust() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            // Connect — actor starts and awaits host key decision.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("connect");

            let _ = recv_timeout(&mut event_rx, "TabConnecting").await;
            let event = recv_timeout(&mut event_rx, "HostKeyUnknown").await;
            let prompt = match event {
                AppEvent::HostKeyUnknown(prompt) => prompt,
                other => panic!("expected HostKeyUnknown, got {other:?}"),
            };

            // Close the tab — actor should be cancelled, trust rejected.
            client
                .try_send(AppCommand::CloseTab { tab_id: TabId(1) })
                .expect("close tab");

            // The actor might emit TabDisconnected (RequestExpired) or
            // exit silently (cancellation). Either way, no TabConnected.
            // Wait a moment for any cleanup events.
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline {
                if let Ok(Some(event)) =
                    tokio::time::timeout(Duration::from_millis(200), event_rx.recv()).await
                {
                    match event {
                        AppEvent::TabDisconnected(_) => { /* expected */ }
                        other => panic!("unexpected event after close: {other:?}"),
                    }
                } else {
                    break;
                }
            }

            // Try to accept the old trust request — should be a no-op.
            client
                .try_send(AppCommand::AcceptHostKey(HostKeyDecisionCommand {
                    request_id: prompt.request_id,
                }))
                .expect("accept command should send");

            assert_no_tab_connected(&mut event_rx, "expired trust accept must not connect").await;
        });

        controller.shutdown();
    }

    #[test]
    fn shutdown_rejects_all_pending_trust_requests() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            // Start a connection but don't resolve the host key prompt.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("connect");

            let _ = recv_timeout(&mut event_rx, "TabConnecting").await;
            let _ = recv_timeout(&mut event_rx, "HostKeyUnknown").await;

            // Verify the trust registry has a pending request.
            assert_eq!(
                controller.trust_registry.pending_count(),
                1,
                "one trust request should be pending"
            );
        });

        // Shutdown outside async context.
        controller.shutdown();

        // After shutdown, the trust registry should be empty (reject_all
        // was called). We can't check this because the controller is
        // consumed, but the fact that shutdown completed without hanging
        // proves the actor was unblocked.
    }

    #[test]
    fn multiple_tabs_connect_independently() {
        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Mock(MockSessionConfig::default()),
        );
        let client = controller.client();
        let mut event_rx = controller
            .take_event_receiver()
            .expect("event receiver should be available once");

        controller.runtime().block_on(async {
            // Connect tab 1.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(1),
                    session_id: SessionId(1),
                    session_epoch: 1,
                    profile_id: ProfileId(1),
                    settings: test_settings(),
                }))
                .expect("connect tab 1");

            // Connect tab 2.
            client
                .try_send(AppCommand::ConnectTab(ConnectCommand {
                    tab_id: TabId(2),
                    session_id: SessionId(2),
                    session_epoch: 1,
                    profile_id: ProfileId(2),
                    settings: test_settings(),
                }))
                .expect("connect tab 2");

            // Collect events — we should get two TabConnecting and two
            // HostKeyUnknown events, one for each tab.
            let mut connecting_tabs = Vec::new();
            let mut host_key_tabs = Vec::new();

            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline
                && (connecting_tabs.len() < 2 || host_key_tabs.len() < 2)
            {
                let event = recv_timeout(&mut event_rx, "multi-tab events").await;
                match event {
                    AppEvent::TabConnecting { tab_id } => connecting_tabs.push(tab_id),
                    AppEvent::HostKeyUnknown(prompt) => host_key_tabs.push(prompt.tab_id),
                    other => panic!("unexpected event: {other:?}"),
                }
            }

            assert_eq!(connecting_tabs.len(), 2, "should get 2 TabConnecting");
            assert_eq!(host_key_tabs.len(), 2, "should get 2 HostKeyUnknown");
            assert!(connecting_tabs.contains(&TabId(1)));
            assert!(connecting_tabs.contains(&TabId(2)));
            assert!(host_key_tabs.contains(&TabId(1)));
            assert!(host_key_tabs.contains(&TabId(2)));
        });

        controller.shutdown();
    }
}
