use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use std::sync::atomic::AtomicUsize;

use macsftp_core::{ConnectionKey, ConnectionSettings, RemoteEventScope, TrustRequestId, AppEvent, UserFacingError, ErrorCode};
use russh::client;
use russh_sftp::client::SftpSession;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::physical_connection::{ClientHandler, ConnectFailure, establish_physical_connection};
use crate::known_hosts::KnownHostsStore;
use crate::trust::TrustRegistry;
use crate::session_actor::HostTrustConfig;

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

    pub fn get_or_connect(
        self: &Arc<Self>,
        settings: &ConnectionSettings,
        scope: &RemoteEventScope,
        trust_request_id: TrustRequestId,
        known_hosts: Arc<Mutex<KnownHostsStore>>,
        trust_config: Arc<HostTrustConfig>,
        trust_registry: Arc<TrustRegistry>,
        event_tx: flume::Sender<AppEvent>,
        connection_lost: CancellationToken,
    ) -> broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>> {
        let key = settings.connection_key();
        let mut pool = self.pool.lock().unwrap();

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
                    ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "Could not open an SSH channel.", &error.to_string()))
                })?;
                channel.request_subsystem(true, "sftp").await.map_err(|error| {
                    ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "The server rejected the SFTP subsystem.", &error.to_string()))
                })?;
                let sftp = SftpSession::new(channel.into_stream()).await.map_err(|error| {
                    ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "Could not start the SFTP session.", &error.to_string()))
                })?;
                let root = sftp.canonicalize(".").await.map_err(|error| {
                    ConnectFailure::Connection(UserFacingError::new(ErrorCode::ChannelClosed, "Could not resolve the remote home directory.", &error.to_string()))
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

    pub fn mark_connected(&self, key: &ConnectionKey, shared: Arc<SharedConnection>) {
        let mut pool = self.pool.lock().unwrap();
        pool.insert(key.clone(), PoolEntry::Connected(shared));
    }

    pub fn remove(&self, key: &ConnectionKey) {
        let mut pool = self.pool.lock().unwrap();
        pool.remove(key);
    }
}
