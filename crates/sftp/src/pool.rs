use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use macsftp_core::{
    AppEvent, ConnectionKey, ConnectionPoolIdentity, ConnectionSettings, ErrorCode,
    RemoteEventScope, TrustRequestId, UserFacingError,
};
use russh::client;
use russh_sftp::client::SftpSession;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::known_hosts::KnownHostsStore;
use crate::physical_connection::{
    ClientHandler, ConnectFailure, establish_physical_connection, host_key_mismatch_event,
    log_connect_failure, sftp_connection_error,
};
use crate::session_actor::HostTrustConfig;
use crate::trust::TrustRegistry;

pub struct SharedConnection {
    pub connection_key: ConnectionKey,
    pub handle: client::Handle<ClientHandler>,
    pub remote_root: String,
    pub host_port_string: String,
    pub active_channels: AtomicUsize,
    pub last_used: Mutex<Instant>,
    pub connection_lost: CancellationToken,
}

impl std::fmt::Debug for SharedConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedConnection")
            .field("host_port", &self.host_port_string)
            .field(
                "active_channels",
                &self
                    .active_channels
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

pub enum PoolEntry {
    Connecting(broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>>),
    Connected(Arc<SharedConnection>),
}

pub struct ConnectionManager {
    pool: Mutex<HashMap<ConnectionKey, PoolEntry>>,
    idle_timeout: Duration,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
            idle_timeout: Duration::from_secs(30),
        }
    }

    /// Trust, known_hosts, and event plumbing are co-required for handshake.
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_connect(
        self: &Arc<Self>,
        settings: &ConnectionSettings,
        pool_identity: &ConnectionPoolIdentity,
        scope: &RemoteEventScope,
        trust_request_id: TrustRequestId,
        known_hosts: Arc<Mutex<KnownHostsStore>>,
        trust_config: Arc<HostTrustConfig>,
        trust_registry: Arc<TrustRegistry>,
        event_tx: flume::Sender<AppEvent>,
    ) -> broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>> {
        let key = ConnectionKey::new(settings, pool_identity.clone());
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if matches!(
            pool.get(&key),
            Some(PoolEntry::Connected(shared)) if shared.connection_lost.is_cancelled()
        ) {
            pool.remove(&key);
        }

        if let Some(entry) = pool.get(&key) {
            match entry {
                PoolEntry::Connecting(rx) => {
                    info!(
                        target: "macsftp_sftp::connection",
                        host = settings.host.as_str(),
                        port = settings.port,
                        tab_id = scope.tab_id.0,
                        session_id = scope.session_id.0,
                        session_epoch = scope.session_epoch,
                        "waiting for an in-progress pooled SSH connection"
                    );
                    return rx.resubscribe();
                }
                PoolEntry::Connected(shared) => {
                    info!(
                        target: "macsftp_sftp::connection",
                        host = settings.host.as_str(),
                        port = settings.port,
                        tab_id = scope.tab_id.0,
                        session_id = scope.session_id.0,
                        session_epoch = scope.session_epoch,
                        "reusing an authenticated pooled SSH connection"
                    );
                    let (tx, rx) = broadcast::channel(1);
                    let _ = tx.send(Ok(shared.clone()));
                    return rx;
                }
            }
        }

        let (tx, rx) = broadcast::channel(1);
        pool.insert(key.clone(), PoolEntry::Connecting(rx.resubscribe()));

        let settings = settings.clone();
        let scope = scope.clone();

        let manager = self.clone();
        let key_clone = key.clone();
        tokio::spawn(async move {
            let connection_lost = CancellationToken::new();
            let result = async {
                let handle = establish_physical_connection(
                    &settings,
                    &scope,
                    trust_request_id,
                    known_hosts,
                    trust_config,
                    trust_registry,
                    event_tx,
                    connection_lost.clone(),
                )
                .await?;

                info!(
                    target: "macsftp_sftp::connection",
                    host = settings.host.as_str(),
                    port = settings.port,
                    tab_id = scope.tab_id.0,
                    session_id = scope.session_id.0,
                    session_epoch = scope.session_epoch,
                    "SFTP subsystem initialization started"
                );
                let channel = handle.channel_open_session().await.map_err(|error| {
                    ConnectFailure::Connection(sftp_connection_error(
                        "Could not open an SSH channel.",
                        "The server did not open an SSH channel for SFTP.",
                        &error,
                    ))
                })?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(sftp_connection_error(
                            "The server rejected the SFTP subsystem.",
                            "The server did not accept the SFTP subsystem request.",
                            &error,
                        ))
                    })?;
                let sftp = SftpSession::new(channel.into_stream())
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(sftp_connection_error(
                            "Could not start the SFTP session.",
                            "The SFTP subsystem did not become ready.",
                            &error,
                        ))
                    })?;
                let root = sftp.canonicalize(".").await.map_err(|error| {
                    ConnectFailure::Connection(sftp_connection_error(
                        "Could not resolve the remote home directory.",
                        "The remote home directory was unavailable after SFTP started.",
                        &error,
                    ))
                })?;

                Ok(Arc::new(SharedConnection {
                    connection_key: key_clone.clone(),
                    handle,
                    remote_root: root,
                    host_port_string: format!("{}:{}", settings.host, settings.port),
                    active_channels: AtomicUsize::new(0),
                    last_used: Mutex::new(Instant::now()),
                    connection_lost,
                }))
            }
            .await;

            match &result {
                Ok(shared) => manager.mark_connected(&key_clone, shared.clone()),
                Err(_) => manager.remove(&key_clone),
            }
            let _ = tx.send(result);
        });

        rx
    }

    /// Establish a physical connection via the pool and open a dedicated
    /// SFTP session on it, returning both. This is the convenience entry
    /// point for callers (integration tests, and the runtime) that need an
    /// already-connected `RemoteSessionActor`: the returned `SftpSession` is
    /// a fresh channel multiplexed over the shared SSH connection.
    #[allow(clippy::too_many_arguments)]
    pub fn connect_session(
        self: &Arc<Self>,
        settings: &ConnectionSettings,
        pool_identity: &ConnectionPoolIdentity,
        scope: &RemoteEventScope,
        trust_request_id: TrustRequestId,
        known_hosts: Arc<Mutex<KnownHostsStore>>,
        trust_config: Arc<HostTrustConfig>,
        trust_registry: Arc<TrustRegistry>,
        event_tx: flume::Sender<AppEvent>,
    ) -> flume::Receiver<Result<(Arc<SharedConnection>, SftpSession), ConnectFailure>> {
        info!(
            target: "macsftp_sftp::connection",
            host = settings.host.as_str(),
            port = settings.port,
            username = settings.username.as_str(),
            tab_id = scope.tab_id.0,
            session_id = scope.session_id.0,
            session_epoch = scope.session_epoch,
            "SFTP connection started"
        );
        let cm = self.clone();
        let settings = settings.clone();
        let pool_identity = pool_identity.clone();
        let scope = scope.clone();
        let known_hosts = known_hosts.clone();
        let trust_config = trust_config.clone();
        let trust_registry = trust_registry.clone();
        let event_tx = event_tx.clone();
        // Separate clone kept for the failure-event path below; `event_tx`
        // itself is moved into `get_or_connect`.
        let event_tx_fail = event_tx.clone();

        let mut connect_rx = cm.get_or_connect(
            &settings,
            &pool_identity,
            &scope,
            trust_request_id,
            known_hosts,
            trust_config,
            trust_registry,
            event_tx,
        );

        let (tx, rx) = flume::bounded(1);
        tokio::spawn(async move {
            let result = async {
                let connect = connect_rx.recv().await.unwrap_or_else(|_| {
                    Err(ConnectFailure::Connection(UserFacingError::new(
                        ErrorCode::Unknown,
                        "Connection channel closed",
                        "The connection attempt ended before a session was established.",
                    )))
                });
                let shared = connect?;
                let channel = shared
                    .handle
                    .channel_open_session()
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(sftp_connection_error(
                            "Could not open SFTP channel on shared connection",
                            "The server did not open an SSH channel for SFTP.",
                            &error,
                        ))
                    })?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(sftp_connection_error(
                            "The server rejected the SFTP subsystem.",
                            "The server did not accept the SFTP subsystem request.",
                            &error,
                        ))
                    })?;
                let sftp = SftpSession::new(channel.into_stream())
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(sftp_connection_error(
                            "Could not start the SFTP session.",
                            "The SFTP subsystem did not become ready.",
                            &error,
                        ))
                    })?;
                info!(
                    target: "macsftp_sftp::connection",
                    host = settings.host.as_str(),
                    port = settings.port,
                    tab_id = scope.tab_id.0,
                    session_id = scope.session_id.0,
                    session_epoch = scope.session_epoch,
                    "SFTP connection succeeded"
                );
                Ok((shared, sftp))
            }
            .await;
            // Mirror the runtime's connection-failure handling (runtime.rs):
            // translate a failed connection into the same lifecycle event the
            // runtime would emit, so callers (integration tests, and any
            // future consumer) observe a consistent event stream.
            if let Err(failure) = &result {
                log_connect_failure(&settings.host, settings.port, &scope, failure);
                match failure {
                    ConnectFailure::HostKeyMismatch(details) => {
                        // Emit one scoped event for THIS logical attempt. The
                        // pooled handshake is shared, but the mismatch event
                        // is built per waiter so each gets its own scope. The
                        // failure still propagates as Err — emitting the event
                        // does not convert failure into success.
                        if let Err(send_error) = event_tx_fail
                            .send_async(host_key_mismatch_event(scope.clone(), details.clone()))
                            .await
                        {
                            warn!(error = %send_error, "host key mismatch event dropped");
                        }
                    }
                    ConnectFailure::TrustRejected => {
                        let _ = event_tx_fail
                            .send_async(AppEvent::TabDisconnected(macsftp_core::RemoteScoped::new(
                                scope.clone(),
                                macsftp_core::TabDisconnected {
                                    reason: macsftp_core::DisconnectReason::UserRequested,
                                },
                            )))
                            .await;
                    }
                    ConnectFailure::TrustTimeout => {
                        let _ = event_tx_fail
                            .send_async(AppEvent::TabDisconnected(macsftp_core::RemoteScoped::new(
                                scope.clone(),
                                macsftp_core::TabDisconnected {
                                    reason: macsftp_core::DisconnectReason::Error(
                                        UserFacingError::new(
                                            ErrorCode::Unknown,
                                            "Trust prompt timed out",
                                            "You did not respond to the host key prompt in time.",
                                        ),
                                    ),
                                },
                            )))
                            .await;
                    }
                    ConnectFailure::AuthFailed(_) => {}
                    ConnectFailure::Connection(error) => {
                        let _ = event_tx_fail
                            .send_async(AppEvent::TabDisconnected(macsftp_core::RemoteScoped::new(
                                scope.clone(),
                                macsftp_core::TabDisconnected {
                                    reason: macsftp_core::DisconnectReason::Error(error.clone()),
                                },
                            )))
                            .await;
                    }
                }
            }
            let _ = tx.send(result);
        });

        rx
    }

    pub fn mark_connected(self: &Arc<Self>, key: &ConnectionKey, shared: Arc<SharedConnection>) {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pool.insert(key.clone(), PoolEntry::Connected(shared.clone()));
        drop(pool);

        let manager = self.clone();
        let key = key.clone();
        let idle_timeout = self.idle_timeout;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shared.connection_lost.cancelled() => {
                        manager.remove_if_same(&key, &shared);
                        return;
                    }
                    _ = tokio::time::sleep(idle_timeout) => {
                        let idle = shared
                            .last_used
                            .lock()
                            .map(|last_used| last_used.elapsed() >= idle_timeout)
                            .unwrap_or(false);
                        if idle
                            && shared.active_channels.load(std::sync::atomic::Ordering::Relaxed) == 0
                            && Arc::strong_count(&shared) == 2
                        {
                            manager.remove_if_same(&key, &shared);
                            return;
                        }
                    }
                }
            }
        });
    }

    pub fn remove(&self, key: &ConnectionKey) {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pool.remove(key);
    }

    pub(crate) fn connected(&self, key: &ConnectionKey) -> Option<Arc<SharedConnection>> {
        let pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match pool.get(key) {
            Some(PoolEntry::Connected(shared)) if !shared.connection_lost.is_cancelled() => {
                Some(shared.clone())
            }
            _ => None,
        }
    }

    fn remove_if_same(&self, key: &ConnectionKey, shared: &Arc<SharedConnection>) {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(pool.get(key), Some(PoolEntry::Connected(current)) if Arc::ptr_eq(current, shared))
        {
            pool.remove(key);
        }
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}
