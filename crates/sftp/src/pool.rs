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
use tracing::{info, warn};

use crate::keyboard_interactive::KeyboardInteractiveRegistry;
use crate::known_hosts::KnownHostsStore;
use crate::physical_connection::{
    ClientHandler, ConnectFailure, PhysicalDisconnectCause, establish_physical_connection,
    handshake_timeout_error, sftp_connection_error,
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
    Connecting {
        receiver: broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>>,
        /// When this attempt started. A `Connecting` entry that outlives
        /// every bounded handshake phase means its task died without
        /// reporting (panic, abort, runtime teardown). Left in place it would
        /// make every later attempt wait on a receiver that never fires.
        started_at: Instant,
        /// Identifies this attempt within the manager. A superseded attempt
        /// that finishes late must not overwrite or remove the entry a newer
        /// attempt installed for the same connection key.
        attempt: u64,
    },
    Connected(Arc<SharedConnection>),
}

/// How long a `Connecting` entry may live before a new attempt replaces it.
///
/// Every peer-facing setup phase is separately bounded by `handshake_timeout`
/// (TCP connect, key exchange, one authentication request, agent socket and
/// key listing, channel open, subsystem request, SFTP init, remote home
/// resolution), and a human trust prompt may add `trust_prompt_timeout` on top
/// of the handshake that carries it.
///
/// The multiplier is a deliberately loose estimate, not a hard budget: a jump
/// host adds a second full sequence, an agent can offer many identities, and
/// two trust prompts can each use the full prompt timeout. It only ever reaps
/// entries whose task died without reporting; evicting a live attempt is
/// survivable (its waiters still receive the connection, and the newer attempt
/// owns the pool entry), but it opens a second connection and can prompt
/// twice, so the bound errs on the long side.
pub(crate) fn stale_connecting_after(trust_config: &HostTrustConfig) -> Duration {
    trust_config.handshake_timeout * 6 + trust_config.trust_prompt_timeout
}

pub struct ConnectionManager {
    pool: Mutex<HashMap<ConnectionKey, PoolEntry>>,
    idle_timeout: Duration,
    keyboard_interactive_registry: Arc<KeyboardInteractiveRegistry>,
    next_keyboard_interactive_id: Arc<AtomicU64>,
    next_trust_id: Arc<AtomicU64>,
    next_attempt: AtomicU64,
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
            next_attempt: AtomicU64::new(1),
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

        let stale_after = stale_connecting_after(&trust_config);
        if let Some(PoolEntry::Connecting {
            receiver,
            started_at,
            ..
        }) = pool.get(&key)
        {
            if started_at.elapsed() >= stale_after {
                warn!(
                    target: "macsftp_sftp::connection",
                    host = settings.host.as_str(),
                    port = settings.port,
                    elapsed_secs = started_at.elapsed().as_secs(),
                    "discarding a stalled pooled connection attempt"
                );
                pool.remove(&key);
            } else {
                info!(
                    target: "macsftp_sftp::connection",
                    host = settings.host.as_str(),
                    port = settings.port,
                    tab_id = scope.tab_id.0,
                    session_id = scope.session_id.0,
                    session_epoch = scope.session_epoch,
                    "waiting for an in-progress pooled SSH connection"
                );
                return receiver.resubscribe();
            }
        }

        // A still-fresh `Connecting` entry was returned above; anything left
        // that is not `Connected` is stale and gets replaced by this attempt.
        if let Some(PoolEntry::Connected(shared)) = pool.get(&key) {
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
        pool.remove(&key);

        let (tx, rx) = broadcast::channel(1);
        let attempt = self
            .next_attempt
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        pool.insert(
            key.clone(),
            PoolEntry::Connecting {
                receiver: rx.resubscribe(),
                started_at: Instant::now(),
                attempt,
            },
        );

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
        let setup_timeout = trust_config.handshake_timeout;
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
                // Every remaining setup step talks to the peer, so each gets
                // the same phase bound as the handshake. Without it a server
                // that accepts the SFTP subsystem and then stops answering
                // leaves the pooled entry `Connecting` until the stale reaper
                // fires.
                let channel = open_session_with_timeout(&handle, setup_timeout).await?;
                let channel = request_subsystem_with_timeout(channel, setup_timeout).await?;
                let (_sftp, root) = initialize_sftp_with_timeout(channel, setup_timeout).await?;

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
                Ok(shared) => {
                    if !manager.mark_connected(&key_clone, attempt, shared.clone()) {
                        warn!(
                            target: "macsftp_sftp::connection",
                            host = settings.host.as_str(),
                            port = settings.port,
                            attempt,
                            "discarding the result of a superseded connection attempt"
                        );
                    }
                }
                Err(_) => manager.abandon_attempt(&key_clone, attempt),
            }
            let _ = tx.send(result);
        });

        rx
    }

    /// Install the successful result of `attempt`.
    ///
    /// Returns `false` when a newer attempt already replaced this one's
    /// `Connecting` entry: a handshake that waited on a slow human can outlive
    /// the stale-attempt bound, and overwriting the newer entry would strand
    /// its waiters and leave two live connections for one key.
    pub fn mark_connected(
        self: &Arc<Self>,
        key: &ConnectionKey,
        attempt: u64,
        shared: Arc<SharedConnection>,
    ) -> bool {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !is_current_attempt(&pool, key, attempt) {
            return false;
        }
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

        true
    }

    /// Drop this attempt's `Connecting` entry after a failure, unless a newer
    /// attempt already installed its own entry for the same key.
    fn abandon_attempt(&self, key: &ConnectionKey, attempt: u64) {
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if is_current_attempt(&pool, key, attempt) {
            pool.remove(key);
        }
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

/// Open an SSH session channel under the connection's phase bound.
async fn open_session_with_timeout(
    handle: &client::Handle<ClientHandler>,
    timeout: Duration,
) -> Result<russh::Channel<client::Msg>, ConnectFailure> {
    match tokio::time::timeout(timeout, handle.channel_open_session()).await {
        Ok(Ok(channel)) => Ok(channel),
        Ok(Err(error)) => Err(ConnectFailure::Connection(sftp_connection_error(
            "Could not open an SSH channel.",
            "The server did not open an SSH channel for SFTP.",
            &error,
        ))),
        Err(_) => Err(ConnectFailure::Connection(handshake_timeout_error(
            "opening an SSH channel",
        ))),
    }
}

/// Request the `sftp` subsystem under the connection's phase bound.
async fn request_subsystem_with_timeout(
    channel: russh::Channel<client::Msg>,
    timeout: Duration,
) -> Result<russh::Channel<client::Msg>, ConnectFailure> {
    match tokio::time::timeout(timeout, channel.request_subsystem(true, "sftp")).await {
        Ok(Ok(())) => Ok(channel),
        Ok(Err(error)) => Err(ConnectFailure::Connection(sftp_connection_error(
            "The server rejected the SFTP subsystem.",
            "The server did not accept the SFTP subsystem request.",
            &error,
        ))),
        Err(_) => Err(ConnectFailure::Connection(handshake_timeout_error(
            "the SFTP subsystem request",
        ))),
    }
}

/// Start SFTP and resolve the remote home under the phase bound.
async fn initialize_sftp_with_timeout(
    channel: russh::Channel<client::Msg>,
    timeout: Duration,
) -> Result<(SftpSession, String), ConnectFailure> {
    let sftp = match tokio::time::timeout(timeout, SftpSession::new(channel.into_stream())).await {
        Ok(Ok(sftp)) => sftp,
        Ok(Err(error)) => {
            return Err(ConnectFailure::Connection(sftp_connection_error(
                "The file connection couldn’t start.",
                "The SFTP subsystem did not become ready.",
                &error,
            )));
        }
        Err(_) => {
            return Err(ConnectFailure::Connection(handshake_timeout_error(
                "starting the SFTP subsystem",
            )));
        }
    };
    match tokio::time::timeout(timeout, sftp.canonicalize(".")).await {
        Ok(Ok(root)) => Ok((sftp, root)),
        Ok(Err(error)) => Err(ConnectFailure::Connection(sftp_connection_error(
            "Could not resolve the remote home directory.",
            "The remote home directory was unavailable after SFTP started.",
            &error,
        ))),
        Err(_) => Err(ConnectFailure::Connection(handshake_timeout_error(
            "resolving the remote home directory",
        ))),
    }
}

/// Whether the pooled entry for `key` is still the `Connecting` entry created
/// by `attempt`.
fn is_current_attempt(
    pool: &HashMap<ConnectionKey, PoolEntry>,
    key: &ConnectionKey,
    attempt: u64,
) -> bool {
    matches!(
        pool.get(key),
        Some(PoolEntry::Connecting { attempt: current, .. }) if *current == attempt
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use macsftp_core::{
        AuthCredential, ConnectionPoolIdentity, ConnectionSettings, RemoteEventScope,
        ResolvedConnectionRoute, SessionId, TabId,
    };
    use tokio::net::TcpListener;

    use super::{ConnectionManager, PoolEntry, is_current_attempt, stale_connecting_after};
    use crate::known_hosts::KnownHostsStore;
    use crate::session_actor::HostTrustConfig;
    use crate::trust::TrustRegistry;

    fn test_settings(port: u16) -> ConnectionSettings {
        ConnectionSettings {
            host: "127.0.0.1".to_string(),
            port,
            username: "tester".to_string(),
            auth: AuthCredential::Password {
                password: "unused".to_string(),
            },
            route: ResolvedConnectionRoute::Direct,
        }
    }

    fn test_trust_config(dir: &std::path::Path, handshake: Duration) -> HostTrustConfig {
        let mut config = HostTrustConfig::new(dir.join("known_hosts"), None);
        config.handshake_timeout = handshake;
        config.trust_prompt_timeout = Duration::from_millis(50);
        config
    }

    fn unique_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "macsftp-pool-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("pool test directory must be created");
        dir
    }

    /// A peer that accepts TCP and then never speaks must fail the attempt
    /// within the handshake bound. Before the bound existed this awaited
    /// forever, and the pooled `Connecting` entry pinned every later attempt
    /// on the same connection key.
    #[tokio::test]
    async fn silent_peer_fails_within_the_handshake_bound_and_releases_the_pool() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        // Accept and then stay silent: the classic black-hole peer.
        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            std::mem::forget(stream);
            futures_pending().await;
        });

        let dir = unique_dir("silent-peer");
        let trust_config = std::sync::Arc::new(test_trust_config(&dir, Duration::from_millis(120)));
        let manager = std::sync::Arc::new(ConnectionManager::new());
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        let (event_tx, _event_rx) = flume::bounded(8);

        let mut receiver = manager.get_or_connect(
            &test_settings(port),
            &ConnectionPoolIdentity::Ephemeral(SessionId(1)),
            &scope,
            std::sync::Arc::new(std::sync::Mutex::new(KnownHostsStore::empty())),
            trust_config,
            std::sync::Arc::new(TrustRegistry::new()),
            event_tx,
        );

        let result = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("the attempt must not hang past the handshake bound");

        match result {
            Ok(Err(_failure)) => {}
            other => panic!("expected a timed-out connect failure, got {other:?}"),
        }

        // The failed task must remove its own `Connecting` entry, so a later
        // attempt starts fresh instead of waiting on a dead receiver.
        let leaked = manager
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&macsftp_core::ConnectionKey::new(
                &test_settings(port),
                ConnectionPoolIdentity::Ephemeral(SessionId(1)),
            ));
        assert!(!leaked, "a failed attempt must not leave a pooled entry");

        accepted.abort();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Safety net for a task that never reports at all (panic, abort,
    /// runtime teardown): a `Connecting` entry older than every bounded
    /// phase is discarded instead of being reused forever.
    #[tokio::test]
    async fn an_abandoned_connecting_entry_is_discarded() {
        let dir = unique_dir("abandoned");
        let trust_config = test_trust_config(&dir, Duration::from_millis(100));
        let manager = std::sync::Arc::new(ConnectionManager::new());
        let settings = test_settings(9);
        let identity = ConnectionPoolIdentity::Ephemeral(SessionId(7));
        let key = macsftp_core::ConnectionKey::new(&settings, identity.clone());

        {
            let (tx, rx) = tokio::sync::broadcast::channel(1);
            std::mem::forget(tx);
            let mut pool = manager
                .pool
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pool.insert(
                key.clone(),
                PoolEntry::Connecting {
                    receiver: rx,
                    started_at: std::time::Instant::now()
                        - stale_connecting_after(&trust_config)
                        - Duration::from_secs(1),
                    attempt: 0,
                },
            );
        }

        assert!(
            stale_connecting_after(&trust_config) > trust_config.trust_prompt_timeout,
            "the bound must cover a live trust prompt plus every phase"
        );

        let scope = RemoteEventScope::new(TabId(1), SessionId(7), 1);
        let (event_tx, _event_rx) = flume::bounded(8);
        let _receiver = manager.get_or_connect(
            &settings,
            &identity,
            &scope,
            std::sync::Arc::new(std::sync::Mutex::new(KnownHostsStore::empty())),
            std::sync::Arc::new(trust_config),
            std::sync::Arc::new(TrustRegistry::new()),
            event_tx,
        );

        let entry = manager
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key);
        match entry {
            Some(PoolEntry::Connecting { started_at, .. }) => assert!(
                started_at.elapsed() < Duration::from_secs(5),
                "the stale entry must be replaced by the new attempt"
            ),
            Some(PoolEntry::Connected(_)) => {}
            None => panic!("the new attempt must be registered"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A handshake that waited on a slow human can exceed the stale-attempt
    /// bound and be evicted while its task is still running. When that task
    /// finishes, it must not claim the entry a newer attempt installed for the
    /// same key.
    #[test]
    fn a_superseded_attempt_does_not_own_the_pooled_entry() {
        let key = macsftp_core::ConnectionKey::new(
            &test_settings(22),
            ConnectionPoolIdentity::Ephemeral(SessionId(1)),
        );
        let (tx, rx) = tokio::sync::broadcast::channel(1);
        std::mem::forget(tx);
        let mut pool = std::collections::HashMap::new();
        pool.insert(
            key.clone(),
            PoolEntry::Connecting {
                receiver: rx,
                started_at: std::time::Instant::now(),
                attempt: 7,
            },
        );

        assert!(is_current_attempt(&pool, &key, 7));
        assert!(
            !is_current_attempt(&pool, &key, 6),
            "the replaced attempt must no longer own the entry"
        );
    }

    async fn futures_pending() {
        std::future::pending::<()>().await;
    }
}
