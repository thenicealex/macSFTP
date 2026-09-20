use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use macsftp_core::{
    AppEvent, ConnectionKey, ConnectionPoolIdentity, ConnectionSettings, RemoteEventScope,
    TrustRequestId,
};
use russh::client;
use russh_sftp::client::SftpSession;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::keyboard_interactive::KeyboardInteractiveRegistry;
use crate::known_hosts::KnownHostsStore;
use crate::physical_connection::{
    ClientHandler, ConnectFailure, PhysicalDisconnectCause, establish_physical_connection,
    sftp_connection_error,
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
    /// Classified cause of the physical drop, recorded by the russh
    /// handler before `connection_lost` fires (see `ClientHandler`).
    pub disconnect_cause: Arc<std::sync::Mutex<Option<PhysicalDisconnectCause>>>,
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
    keyboard_interactive_registry: Arc<KeyboardInteractiveRegistry>,
    next_keyboard_interactive_id: Arc<AtomicU64>,
    next_trust_id: Arc<AtomicU64>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self::with_keyboard_interactive(
            Arc::new(KeyboardInteractiveRegistry::new()),
            Arc::new(AtomicU64::new(1)),
            Arc::new(AtomicU64::new(1)),
        )
    }

    pub fn with_keyboard_interactive(
        keyboard_interactive_registry: Arc<KeyboardInteractiveRegistry>,
        next_keyboard_interactive_id: Arc<AtomicU64>,
        next_trust_id: Arc<AtomicU64>,
    ) -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
            idle_timeout: Duration::from_secs(30),
            keyboard_interactive_registry,
            next_keyboard_interactive_id,
            next_trust_id,
        }
    }

    /// Trust, known_hosts, and event plumbing are co-required for handshake.
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_connect(
        self: &Arc<Self>,
        settings: &ConnectionSettings,
        pool_identity: &ConnectionPoolIdentity,
        scope: &RemoteEventScope,
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
        let trust_request_id = TrustRequestId(
            self.next_trust_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        let jump_trust_request_id = TrustRequestId(
            self.next_trust_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );

        let manager = self.clone();
        let key_clone = key.clone();
        tokio::spawn(async move {
            let connection_lost = CancellationToken::new();
            let disconnect_cause = Arc::new(std::sync::Mutex::new(None));
            let result = async {
                let handle = establish_physical_connection(
                    &settings,
                    &scope,
                    trust_request_id,
                    jump_trust_request_id,
                    known_hosts,
                    trust_config,
                    trust_registry,
                    manager.keyboard_interactive_registry.clone(),
                    manager.next_keyboard_interactive_id.clone(),
                    event_tx,
                    connection_lost.clone(),
                    disconnect_cause.clone(),
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
                    disconnect_cause,
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
