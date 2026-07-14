use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use macsftp_core::{
    AppEvent, ConnectionKey, ConnectionPoolIdentity, ConnectionSettings, ErrorCode,
    RemoteEventScope, TrustRequestId, UserFacingError,
};
use russh::client;
use russh_sftp::client::SftpSession;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::known_hosts::KnownHostsStore;
use crate::physical_connection::{ClientHandler, ConnectFailure, establish_physical_connection};
use crate::session_actor::HostTrustConfig;
use crate::trust::TrustRegistry;

pub struct SharedConnection {
    pub handle: client::Handle<ClientHandler>,
    pub remote_root: String,
    pub host_port_string: String,
    pub active_channels: AtomicUsize,
    pub last_used: Mutex<Instant>,
}

pub enum PoolEntry {
    Connecting(broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>>),
    Connected(Arc<SharedConnection>),
}

pub struct ConnectionManager {
    pool: Mutex<HashMap<ConnectionKey, PoolEntry>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            pool: Mutex::new(HashMap::new()),
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
        connection_lost: CancellationToken,
    ) -> broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>> {
        let key = ConnectionKey::new(settings, pool_identity.clone());
        let mut pool = self
            .pool
            .lock()
            .expect("connection pool mutex must not be poisoned");

        if let Some(entry) = pool.get(&key) {
            match entry {
                PoolEntry::Connecting(rx) => return rx.resubscribe(),
                PoolEntry::Connected(shared) => {
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
            let result = async {
                let handle = establish_physical_connection(
                    &settings,
                    &scope,
                    trust_request_id,
                    known_hosts,
                    trust_config,
                    trust_registry,
                    event_tx,
                    connection_lost,
                )
                .await?;

                let channel = handle.channel_open_session().await.map_err(|error| {
                    ConnectFailure::Connection(UserFacingError::new(
                        ErrorCode::ChannelClosed,
                        "Could not open an SSH channel.",
                        error.to_string(),
                    ))
                })?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "The server rejected the SFTP subsystem.",
                            error.to_string(),
                        ))
                    })?;
                let sftp = SftpSession::new(channel.into_stream())
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Could not start the SFTP session.",
                            error.to_string(),
                        ))
                    })?;
                let root = sftp.canonicalize(".").await.map_err(|error| {
                    ConnectFailure::Connection(UserFacingError::new(
                        ErrorCode::ChannelClosed,
                        "Could not resolve the remote home directory.",
                        error.to_string(),
                    ))
                })?;

                Ok(Arc::new(SharedConnection {
                    handle,
                    remote_root: root,
                    host_port_string: format!("{}:{}", settings.host, settings.port),
                    active_channels: AtomicUsize::new(0),
                    last_used: Mutex::new(Instant::now()),
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
        connection_lost: CancellationToken,
    ) -> flume::Receiver<Result<(Arc<SharedConnection>, SftpSession), ConnectFailure>> {
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
            connection_lost,
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
                        ConnectFailure::Connection(UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Could not open SFTP channel on shared connection",
                            error.to_string(),
                        ))
                    })?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "The server rejected the SFTP subsystem.",
                            error.to_string(),
                        ))
                    })?;
                let sftp = SftpSession::new(channel.into_stream())
                    .await
                    .map_err(|error| {
                        ConnectFailure::Connection(UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Could not start the SFTP session.",
                            error.to_string(),
                        ))
                    })?;
                Ok((shared, sftp))
            }
            .await;
            // Mirror the runtime's connection-failure handling (runtime.rs):
            // translate a failed connection into the same lifecycle event the
            // runtime would emit, so callers (integration tests, and any
            // future consumer) observe a consistent event stream.
            if let Err(failure) = &result {
                match failure {
                    ConnectFailure::HostKeyMismatch => {}
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

    pub fn mark_connected(&self, key: &ConnectionKey, shared: Arc<SharedConnection>) {
        let mut pool = self
            .pool
            .lock()
            .expect("connection pool mutex must not be poisoned");
        pool.insert(key.clone(), PoolEntry::Connected(shared));
    }

    pub fn remove(&self, key: &ConnectionKey) {
        let mut pool = self
            .pool
            .lock()
            .expect("connection pool mutex must not be poisoned");
        pool.remove(key);
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}
