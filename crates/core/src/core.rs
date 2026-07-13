use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub fn crate_name() -> &'static str {
    "macsftp-core"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TabId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TransferId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TransferPlanId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TrustRequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ConflictRequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CredentialRequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ProfileId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ProfileGroupId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub SystemTime);

impl Timestamp {
    pub fn unix_epoch() -> Self {
        Self(UNIX_EPOCH)
    }

    pub fn from_secs_since_epoch(seconds: u64) -> Self {
        Self(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    /// Build a `Timestamp` from a `SystemTime` (e.g. a file mtime). Times
    /// before the Unix epoch collapse to the epoch rather than underflow.
    pub fn from_system_time(time: SystemTime) -> Self {
        Self(time)
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::unix_epoch()
    }
}

/// `SystemTime` has no built-in serde support, so we round-trip through a
/// signed Unix timestamp (seconds). Epoch-relative arithmetic keeps this
/// independent of platform time representations.
impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let seconds = self
            .0
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        serializer.serialize_i64(seconds)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let seconds = i64::deserialize(deserializer)?;
        let duration = Duration::from_secs(seconds.max(0) as u64);
        Ok(Timestamp(UNIX_EPOCH + duration))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct LocalPath(String);

impl LocalPath {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parent directory, or `None` at the filesystem root.
    pub fn parent(&self) -> Option<Self> {
        parent_path(&self.0).map(Self)
    }

    /// Join a single path component (not a multi-segment path).
    pub fn join(&self, name: &str) -> Self {
        Self(join_path_component(&self.0, name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct RemotePath(String);

impl RemotePath {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parent directory, or `None` at the filesystem root.
    pub fn parent(&self) -> Option<Self> {
        parent_path(&self.0).map(Self)
    }

    /// Join a single path component (not a multi-segment path).
    pub fn join(&self, name: &str) -> Self {
        Self(join_path_component(&self.0, name))
    }
}

/// Shared parent logic for slash-separated absolute paths.
/// `/a/b` → `/a`, `/a` → `/`, `/` → None.
fn parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(index) => Some(trimmed[..index].to_string()),
        None => None,
    }
}

fn join_path_component(parent: &str, name: &str) -> String {
    let name = name.trim_matches('/');
    if parent.is_empty() || parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

/// Operation target backend for unified file ops (phase 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsSide {
    Local,
    Remote,
}

/// Pins an Fs op/event to a tab; remote also carries `session_epoch` for
/// the stale-event guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsScope {
    Local { tab_id: TabId },
    Remote { tab_id: TabId, session_epoch: u64 },
}

impl FsScope {
    pub fn tab_id(&self) -> TabId {
        match self {
            Self::Local { tab_id } | Self::Remote { tab_id, .. } => *tab_id,
        }
    }

    pub fn session_epoch(&self) -> Option<u64> {
        match self {
            Self::Local { .. } => None,
            Self::Remote { session_epoch, .. } => Some(*session_epoch),
        }
    }

    pub fn side(&self) -> FsSide {
        match self {
            Self::Local { .. } => FsSide::Local,
            Self::Remote { .. } => FsSide::Remote,
        }
    }
}

/// Path typed by backend so side and path kind stay a single source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsPath {
    Local(LocalPath),
    Remote(RemotePath),
}

impl FsPath {
    pub fn as_local(&self) -> Option<&LocalPath> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote(_) => None,
        }
    }

    pub fn as_remote(&self) -> Option<&RemotePath> {
        match self {
            Self::Local(_) => None,
            Self::Remote(path) => Some(path),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Local(path) => path.as_str(),
            Self::Remote(path) => path.as_str(),
        }
    }

    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Local(path) => path.parent().map(Self::Local),
            Self::Remote(path) => path.parent().map(Self::Remote),
        }
    }

    pub fn join(&self, name: &str) -> Self {
        match self {
            Self::Local(path) => Self::Local(path.join(name)),
            Self::Remote(path) => Self::Remote(path.join(name)),
        }
    }

    pub fn side(&self) -> FsSide {
        match self {
            Self::Local(_) => FsSide::Local,
            Self::Remote(_) => FsSide::Remote,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntryRef {
    pub path: FsPath,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOp {
    Delete { entries: Vec<FsEntryRef> },
    Rename { from: FsPath, to: FsPath },
    CreateDirectory { parent: FsPath, name: String },
}

/// Unified create/rename/delete command (phase 1). Read paths stay on the
/// existing `ReadLocalDir` / `ReadRemoteDir` commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsCommand {
    pub scope: FsScope,
    pub op: FsOp,
}

impl FsOp {
    /// Directory listing that should refresh after a successful mutation.
    pub fn refresh_path(&self) -> Option<FsPath> {
        match self {
            Self::Delete { entries } => entries.first().and_then(|entry| entry.path.parent()),
            Self::Rename { from, .. } => from.parent(),
            Self::CreateDirectory { parent, .. } => Some(parent.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build a `SecretRef` that resolves to a Keychain entry for the given
    /// profile and secret kind. The actual secret is written to (and read
    /// from) the macOS Keychain; this is only the handle. The Keychain
    /// backend lives in `macsftp_storage::keychain` (plan §11).
    pub fn keychain_ref(profile_id: ProfileId, kind: &str) -> Self {
        Self::new(format!("keychain:macsftp:{}:{}", profile_id.0, kind))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacingError {
    pub code: ErrorCode,
    pub title: String,
    pub message: String,
    pub detail: Option<String>,
    pub retryable: bool,
}

impl UserFacingError {
    pub fn new(code: ErrorCode, title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            title: title.into(),
            message: message.into(),
            detail: None,
            retryable: false,
        }
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    AuthFailed,
    HostKeyMismatch,
    TrustRequestTimeout,
    UserCancelledTrustRequest,
    PermissionDenied,
    FileExists,
    NotFound,
    UnsupportedSymlink,
    MetadataPreservationFailed,
    ChannelClosed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub default_remote_path: Option<RemotePath>,
    pub group_id: Option<ProfileGroupId>,
    pub last_local_path: Option<LocalPath>,
}

impl ConnectionProfile {
    pub fn new(
        id: ProfileId,
        name: impl Into<String>,
        host: impl Into<String>,
        username: impl Into<String>,
        auth: AuthMethod,
    ) -> Self {
        Self {
            id,
            revision: 1,
            name: name.into(),
            host: host.into(),
            port: 22,
            username: username.into(),
            auth,
            default_remote_path: None,
            group_id: None,
            last_local_path: None,
        }
    }

    /// Build a persistable profile from a connection form. The in-memory
    /// secret is mapped to a `SecretRef` handle (see `SecretRef::keychain_ref`)
    /// and is never written into the profile — `profiles.json` stores only
    /// the handle, not the credential. The actual secret is persisted to
    /// the macOS Keychain by the storage layer (plan §11).
    pub fn from_connection_settings(
        id: ProfileId,
        name: impl Into<String>,
        settings: &ConnectionSettings,
    ) -> Self {
        let auth = match &settings.auth {
            AuthCredential::Password { .. } => AuthMethod::Password {
                secret_ref: SecretRef::keychain_ref(id, "password"),
            },
            AuthCredential::PrivateKey {
                key_path,
                passphrase,
                ..
            } => AuthMethod::PrivateKey {
                key_path: LocalPath::new(key_path.clone()),
                passphrase_ref: passphrase
                    .as_ref()
                    .map(|_| SecretRef::keychain_ref(id, "passphrase")),
                remember_passphrase: passphrase.is_some(),
            },
        };
        let mut profile =
            ConnectionProfile::new(id, name, &settings.host, &settings.username, auth);
        profile.port = settings.port;
        profile
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    Password {
        secret_ref: SecretRef,
    },
    PrivateKey {
        key_path: LocalPath,
        passphrase_ref: Option<SecretRef>,
        remember_passphrase: bool,
    },
}

impl AuthMethod {
    pub fn kind(&self) -> AuthMethodKind {
        match self {
            Self::Password { .. } => AuthMethodKind::Password,
            Self::PrivateKey { .. } => AuthMethodKind::PrivateKey,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthMethodKind {
    Password,
    PrivateKey,
}

/// Everything needed to open one SSH connection, including the secret.
/// Secrets live in memory only for the connection attempt and are
/// zeroized on drop; they must never be logged, persisted to plain
/// files, or embedded in errors.
///
/// The UI collects these in the connect form and sends them with
/// `ConnectCommand`. When the user saves a profile, the secret is also
/// persisted to the macOS Keychain (keyed by the profile's `SecretRef`)
/// so it can be restored later; `Debug` is manually implemented to redact
/// auth.
#[derive(Clone, PartialEq, Eq, Hash, Zeroize, ZeroizeOnDrop)]
pub struct ConnectionSettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthCredential,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ConnectionKey {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_hash: u64,
}

impl ConnectionSettings {
    pub fn connection_key(&self) -> ConnectionKey {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.auth.hash(&mut hasher);
        ConnectionKey {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth_hash: hasher.finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Zeroize, ZeroizeOnDrop)]
pub enum AuthCredential {
    Password {
        password: String,
    },
    PrivateKey {
        key_path: String,
        passphrase: Option<String>,
    },
}

impl std::fmt::Debug for ConnectionSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("auth", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthFingerprint {
    pub method: AuthMethodKind,
    pub secret_ref: Option<SecretRef>,
    pub private_key_path_hash: Option<String>,
    pub profile_revision: u64,
}

impl AuthFingerprint {
    pub fn password(secret_ref: SecretRef, profile_revision: u64) -> Self {
        Self {
            method: AuthMethodKind::Password,
            secret_ref: Some(secret_ref),
            private_key_path_hash: None,
            profile_revision,
        }
    }

    pub fn private_key(
        private_key_path_hash: impl Into<String>,
        passphrase_ref: Option<SecretRef>,
        profile_revision: u64,
    ) -> Self {
        Self {
            method: AuthMethodKind::PrivateKey,
            secret_ref: passphrase_ref,
            private_key_path_hash: Some(private_key_path_hash.into()),
            profile_revision,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferSessionMode {
    Dedicated,
    BorrowBrowsingSession { tab_id: TabId, session_epoch: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBridgeConfig {
    pub command_channel_capacity: usize,
    pub event_channel_capacity: usize,
    pub transfer_progress_hz: u8,
    pub shutdown_timeout: Duration,
}

impl Default for RuntimeBridgeConfig {
    fn default() -> Self {
        Self {
            command_channel_capacity: 256,
            event_channel_capacity: 1024,
            transfer_progress_hz: 10,
            shutdown_timeout: Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDispatchError {
    ChannelFull,
    ChannelClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEventScope {
    pub tab_id: TabId,
    pub session_id: SessionId,
    pub session_epoch: u64,
}

impl RemoteEventScope {
    pub fn new(tab_id: TabId, session_id: SessionId, session_epoch: u64) -> Self {
        Self {
            tab_id,
            session_id,
            session_epoch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppState {
    pub tabs: TabStore,
    pub modals: ModalStore,
    /// Monotonic source for `SessionId`s. Session identity is allocated
    /// here (UI side) at connect time and carried in `ConnectCommand`,
    /// so remote events can be correlated against the tab's epoch.
    next_session_id: u64,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next unique session id for a connection attempt.
    pub fn allocate_session_id(&mut self) -> SessionId {
        self.next_session_id += 1;
        SessionId(self.next_session_id)
    }

    /// Single entry point for the GPUI event drain task to check
    /// whether an event should be processed or dropped as stale.
    ///
    /// Remote-scoped events (directory listings, connection state
    /// changes, host key prompts) are checked against the live tab
    /// session. Global events (transfers, tab lifecycle, local
    /// directory listings) are always accepted.
    pub fn should_accept_event(&self, event: &AppEvent) -> bool {
        match event.remote_scope() {
            Some(scope) => self.tabs.accepts_remote_event(&scope),
            None => true,
        }
    }

    /// Remove all modals whose session binding no longer matches a
    /// live tab session. Should be called after tab close, reconnect,
    /// or any state change that invalidates pending modal requests.
    ///
    /// Returns the removed modal requests so the caller can log or
    /// clean up runtime-side resources (e.g. reject trust request
    /// oneshot channels).
    pub fn drain_expired_modals(&mut self) -> Vec<ModalRequest> {
        let tabs = &self.tabs;
        let mut expired = Vec::new();
        self.modals.active.retain(|request| {
            let is_expired = match request {
                ModalRequest::HostKey(prompt) => !tabs.accepts_remote_event(
                    &RemoteEventScope::new(prompt.tab_id, prompt.session_id, prompt.session_epoch),
                ),
                ModalRequest::TransferConflict(_) => false,
                ModalRequest::Error(_) => false,
            };
            if is_expired {
                expired.push(request.clone());
                false
            } else {
                true
            }
        });
        expired
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabStore {
    pub tabs: Vec<TabState>,
    pub active_tab_id: Option<TabId>,
}

impl TabStore {
    pub fn find_tab(&self, tab_id: TabId) -> Option<&TabState> {
        self.tabs.iter().find(|tab| tab.id == tab_id)
    }

    pub fn find_tab_mut(&mut self, tab_id: TabId) -> Option<&mut TabState> {
        self.tabs.iter_mut().find(|tab| tab.id == tab_id)
    }

    pub fn active_tab(&self) -> Option<&TabState> {
        self.active_tab_id
            .and_then(|active_tab_id| self.find_tab(active_tab_id))
    }

    /// Centralized stale event guard.
    ///
    /// Returns `true` if the event scope matches a live tab session:
    /// the tab must exist, its `session_epoch` must match, and the
    /// connection state must reference the same `session_id`.
    ///
    /// This is the single entry point the GPUI event drain task should
    /// call before mutating any tab-scoped state. Putting it here (in
    /// `core`, not in the view) prevents stale-check logic from
    /// scattering across view files.
    pub fn accepts_remote_event(&self, scope: &RemoteEventScope) -> bool {
        match self.find_tab(scope.tab_id) {
            Some(tab) => tab.accepts_remote_event(scope),
            None => false,
        }
    }

    /// Add a new tab to the store. If no tab is active yet, the new
    /// tab becomes active.
    pub fn open_tab(&mut self, tab: TabState) {
        if self.active_tab_id.is_none() {
            self.active_tab_id = Some(tab.id);
        }
        self.tabs.push(tab);
    }

    /// Remove a tab from the store. After this, any late-arriving
    /// remote event for this tab is rejected by `accepts_remote_event`
    /// because the tab no longer exists.
    ///
    /// If the closed tab was active, the next remaining tab (by order)
    /// becomes active. Returns the removed tab if it existed.
    pub fn close_tab(&mut self, tab_id: TabId) -> Option<TabState> {
        let position = self.tabs.iter().position(|tab| tab.id == tab_id)?;
        let removed = self.tabs.remove(position);

        if self.active_tab_id == Some(tab_id) {
            self.active_tab_id = self.tabs.first().map(|tab| tab.id);
        }

        Some(removed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferStore {
    pub plans: Vec<TransferPlan>,
    pub jobs: Vec<TransferJob>,
    pub pending_conflicts: Vec<ConflictRequest>,
}

impl TransferStore {
    pub fn find_plan(&self, plan_id: TransferPlanId) -> Option<&TransferPlan> {
        self.plans.iter().find(|plan| plan.id == plan_id)
    }

    pub fn find_job(&self, transfer_id: TransferId) -> Option<&TransferJob> {
        self.jobs.iter().find(|job| job.id == transfer_id)
    }

    pub fn find_conflict(&self, request_id: ConflictRequestId) -> Option<&ConflictRequest> {
        self.pending_conflicts
            .iter()
            .find(|conflict| conflict.id == request_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModalStore {
    pub active: Vec<ModalRequest>,
}

impl ModalStore {
    pub fn find_request(&self, request_id: ModalRequestId) -> Option<&ModalRequest> {
        self.active
            .iter()
            .find(|request| request.request_id() == Some(request_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabState {
    pub id: TabId,
    pub session_epoch: u64,
    pub title: String,
    pub profile_id: Option<ProfileId>,
    pub local: LocalPaneState,
    pub remote: RemotePaneState,
    pub connection: ConnectionState,
    pub sort: FileSort,
    pub selection: SelectionState,
    pub pending: Vec<PendingOperation>,
}

impl TabState {
    pub fn new(id: TabId, title: impl Into<String>) -> Self {
        Self {
            id,
            session_epoch: 0,
            title: title.into(),
            profile_id: None,
            local: LocalPaneState::default(),
            remote: RemotePaneState::default(),
            connection: ConnectionState::Empty,
            sort: FileSort::default(),
            selection: SelectionState::default(),
            pending: Vec::new(),
        }
    }

    pub fn accepts_remote_event(&self, scope: &RemoteEventScope) -> bool {
        self.id == scope.tab_id
            && self.session_epoch == scope.session_epoch
            && self
                .connection
                .matches_session(scope.session_id, scope.session_epoch)
    }

    /// Begin a new connection attempt.
    ///
    /// Increments `session_epoch` so all events from any previous
    /// session immediately become stale. This is the primary mechanism
    /// that makes the stale event guard work — old sessions' events
    /// carry the old epoch and are rejected.
    pub fn begin_connect(&mut self, session_id: SessionId) {
        self.session_epoch += 1;
        self.connection = ConnectionState::Connecting {
            session_id,
            session_epoch: self.session_epoch,
        };
    }

    /// Transition to `AwaitingHostKey`. Called when the SSH handshake
    /// encounters an unknown host key and needs user confirmation.
    /// The `session_epoch` must already be set by `begin_connect`.
    pub fn await_host_key(&mut self, session_id: SessionId, request_id: TrustRequestId) {
        self.connection = ConnectionState::AwaitingHostKey {
            session_id,
            session_epoch: self.session_epoch,
            request_id,
        };
    }

    /// Transition to `Connected`. Called after the SSH handshake and
    /// SFTP subsystem negotiation succeed.
    pub fn complete_connect(&mut self, session_id: SessionId, connected_at: Timestamp) {
        self.connection = ConnectionState::Connected {
            session_id,
            session_epoch: self.session_epoch,
            connected_at,
        };
    }

    /// Begin a reconnect attempt.
    ///
    /// Increments `session_epoch` (same as `begin_connect`) and
    /// records the previous session id for diagnostics. Events from
    /// the old session are rejected by the epoch mismatch.
    pub fn begin_reconnect(&mut self, session_id: SessionId) {
        let previous_session_id = match &self.connection {
            ConnectionState::Connected { session_id, .. } => *session_id,
            ConnectionState::Disconnected { .. } => SessionId::default(),
            _ => SessionId::default(),
        };
        self.session_epoch += 1;
        self.connection = ConnectionState::Reconnecting {
            session_id,
            previous_session_id,
            session_epoch: self.session_epoch,
        };
    }

    /// Transition to `Disconnected`. Called when the user requests
    /// disconnect or the connection is lost.
    pub fn disconnect(&mut self, reason: DisconnectReason) {
        self.connection = ConnectionState::Disconnected { reason };
    }

    /// Transition to `Failed`. Called when an unrecoverable error
    /// occurs (auth failure, host key mismatch, etc.).
    pub fn fail(&mut self, error: UserFacingError) {
        self.connection = ConnectionState::Failed { error };
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocalPaneState {
    pub path: Option<LocalPath>,
    pub entries: Vec<LocalEntry>,
    /// Latest recoverable local Fs error for the current path (phase 1).
    pub error: Option<UserFacingError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemotePaneState {
    pub path: Option<RemotePath>,
    pub entries: Vec<RemoteEntry>,
    /// A directory request is in flight. The existing entries remain
    /// visible while this is true so refreshes do not flash empty.
    pub is_refreshing: bool,
    /// The latest recoverable directory error for the current path.
    /// Connection-level failures continue to live in `ConnectionState`.
    pub error: Option<UserFacingError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Empty,
    Connecting {
        session_id: SessionId,
        session_epoch: u64,
    },
    AwaitingHostKey {
        session_id: SessionId,
        session_epoch: u64,
        request_id: TrustRequestId,
    },
    AwaitingCredentials {
        session_id: SessionId,
        session_epoch: u64,
        request_id: CredentialRequestId,
    },
    Connected {
        session_id: SessionId,
        session_epoch: u64,
        connected_at: Timestamp,
    },
    Reconnecting {
        session_id: SessionId,
        previous_session_id: SessionId,
        session_epoch: u64,
    },
    Disconnected {
        reason: DisconnectReason,
    },
    Failed {
        error: UserFacingError,
    },
}

impl ConnectionState {
    pub fn matches_session(&self, session_id: SessionId, session_epoch: u64) -> bool {
        matches!(
            self,
            Self::Connected {
                session_id: current_session_id,
                session_epoch: current_session_epoch,
                ..
            }
            | Self::Connecting {
                session_id: current_session_id,
                session_epoch: current_session_epoch,
            }
            | Self::AwaitingHostKey {
                session_id: current_session_id,
                session_epoch: current_session_epoch,
                ..
            }
            | Self::AwaitingCredentials {
                session_id: current_session_id,
                session_epoch: current_session_epoch,
                ..
            }
            | Self::Reconnecting {
                session_id: current_session_id,
                session_epoch: current_session_epoch,
                ..
            } if *current_session_id == session_id && *current_session_epoch == session_epoch
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    UserRequested,
    RuntimeShutdown,
    ConnectionLost,
    Error(UserFacingError),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectionState {
    pub selected_paths: Vec<EntryPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryPath {
    Local(LocalPath),
    Remote(RemotePath),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOperation {
    ReadRemoteDir(RemotePath),
    ReadLocalDir(LocalPath),
    Transfer(TransferPlanId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSort {
    pub field: FileSortField,
    pub direction: SortDirection,
    pub directories_first: bool,
}

impl Default for FileSort {
    fn default() -> Self {
        Self {
            field: FileSortField::Name,
            direction: SortDirection::Ascending,
            directories_first: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSortField {
    Name,
    Kind,
    Size,
    ModifiedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// Field accessors shared by local and remote entries so sorting lives
/// in `core` once instead of being duplicated per pane.
pub trait FileEntryFields {
    fn entry_name(&self) -> &str;
    fn entry_kind(&self) -> FileKind;
    fn entry_size(&self) -> Option<u64>;
    fn entry_modified_at(&self) -> Option<Timestamp>;
    /// Full path used as the stable tie-break so equal keys keep a
    /// deterministic order across re-sorts.
    fn entry_path_str(&self) -> &str;
}

impl FileEntryFields for LocalEntry {
    fn entry_name(&self) -> &str {
        &self.name
    }
    fn entry_kind(&self) -> FileKind {
        self.kind
    }
    fn entry_size(&self) -> Option<u64> {
        self.size
    }
    fn entry_modified_at(&self) -> Option<Timestamp> {
        self.modified_at
    }
    fn entry_path_str(&self) -> &str {
        self.path.as_str()
    }
}

impl FileEntryFields for RemoteEntry {
    fn entry_name(&self) -> &str {
        &self.name
    }
    fn entry_kind(&self) -> FileKind {
        self.kind
    }
    fn entry_size(&self) -> Option<u64> {
        self.size
    }
    fn entry_modified_at(&self) -> Option<Timestamp> {
        self.modified_at
    }
    fn entry_path_str(&self) -> &str {
        self.path.as_str()
    }
}

/// Sort entries per the documented default rules: directories first
/// (when enabled), then the selected field in the selected direction,
/// with the full path as a stable tie-break.
pub fn sort_entries<E: FileEntryFields>(entries: &mut [E], sort: &FileSort) {
    entries.sort_by(|a, b| compare_entries(a, b, sort));
}

fn compare_entries<E: FileEntryFields>(a: &E, b: &E, sort: &FileSort) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    if sort.directories_first {
        let a_is_dir = a.entry_kind() == FileKind::Directory;
        let b_is_dir = b.entry_kind() == FileKind::Directory;
        match (a_is_dir, b_is_dir) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
    }

    let field_ordering = match sort.field {
        FileSortField::Name => compare_names(a.entry_name(), b.entry_name()),
        FileSortField::Kind => kind_rank(a.entry_kind())
            .cmp(&kind_rank(b.entry_kind()))
            .then_with(|| compare_names(a.entry_name(), b.entry_name())),
        FileSortField::Size => a
            .entry_size()
            .cmp(&b.entry_size())
            .then_with(|| compare_names(a.entry_name(), b.entry_name())),
        FileSortField::ModifiedAt => a
            .entry_modified_at()
            .map(|timestamp| timestamp.0)
            .cmp(&b.entry_modified_at().map(|timestamp| timestamp.0))
            .then_with(|| compare_names(a.entry_name(), b.entry_name())),
    };

    let directed = match sort.direction {
        SortDirection::Ascending => field_ordering,
        SortDirection::Descending => field_ordering.reverse(),
    };

    directed.then_with(|| a.entry_path_str().cmp(b.entry_path_str()))
}

/// Case-insensitive name comparison with a case-sensitive fallback so
/// `README` and `readme` still order deterministically.
fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    a_lower.cmp(&b_lower).then_with(|| a.cmp(b))
}

fn kind_rank(kind: FileKind) -> u8 {
    match kind {
        FileKind::Directory => 0,
        FileKind::File => 1,
        FileKind::Symlink => 2,
        FileKind::Other => 3,
        FileKind::Unknown => 4,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub path: RemotePath,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified_at: Option<Timestamp>,
    pub link_target: Option<RemotePath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEntry {
    pub name: String,
    pub path: LocalPath,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified_at: Option<Timestamp>,
    pub link_target: Option<LocalPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    OpenTab(OpenTabCommand),
    CloseTab {
        tab_id: TabId,
    },
    ConnectTab(ConnectCommand),
    DisconnectTab {
        tab_id: TabId,
    },
    ReadRemoteDir {
        tab_id: TabId,
        path: RemotePath,
    },
    /// Best-effort delete of a residual remote temp file left by a
    /// previous run, routed to the connected session for `tab_id`.
    RemoveRemoteTempFile {
        tab_id: TabId,
        transfer_id: TransferId,
        path: RemotePath,
    },
    ReadLocalDir {
        tab_id: TabId,
        path: LocalPath,
    },
    /// Create / rename / delete for local or remote (phase 1).
    Fs(FsCommand),
    StartTransfer(StartTransferCommand),
    CancelTransfer {
        transfer_id: TransferId,
    },
    RetryTransfer {
        transfer_id: TransferId,
    },
    ResolveTransferConflict(ConflictDecisionCommand),
    AcceptHostKey(HostKeyDecisionCommand),
    RejectHostKey {
        request_id: TrustRequestId,
    },
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTabCommand {
    pub profile_id: Option<ProfileId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectCommand {
    pub tab_id: TabId,
    /// Session identity allocated by the UI/core side *before* the
    /// command is sent, so every event the new session emits echoes the
    /// tab's own `session_epoch` and passes the stale event guard.
    pub session_id: SessionId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    /// Connection target and credentials for this attempt (interim —
    /// Keychain-backed profile resolution replaces this later).
    pub settings: ConnectionSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartTransferCommand {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub direction: TransferDirection,
    pub sources: Vec<TransferEndpoint>,
    pub destination: TransferEndpoint,
    pub metadata_policy: MetadataPolicy,
    pub conflict_policy: ConflictPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictDecisionCommand {
    pub plan_id: TransferPlanId,
    pub request_id: ConflictRequestId,
    pub decision: ConflictDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyDecisionCommand {
    pub request_id: TrustRequestId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    TabOpened(TabSnapshot),
    TabClosed {
        tab_id: TabId,
    },
    TabConnecting {
        tab_id: TabId,
    },
    HostKeyUnknown(HostKeyPrompt),
    HostKeyMismatch(HostKeyMismatch),
    TabConnected(RemoteScoped<TabConnected>),
    TabDisconnected(RemoteScoped<TabDisconnected>),
    AuthFailed(RemoteScoped<AuthFailure>),
    RemoteDirLoading(RemoteScoped<RemoteDirLoading>),
    RemoteDirLoaded(RemoteScoped<RemoteDirSnapshot>),
    RemoteOperationFailed(RemoteScoped<RemoteOperationFailure>),
    LocalDirLoaded(LocalDirSnapshot),
    /// Create / rename / delete failure shared by local and remote.
    FsOperationFailed {
        scope: FsScope,
        failure: UserFacingError,
    },
    TransferPlanStarted(TransferPlanSnapshot),
    TransferPlanProgress(TransferPlanProgress),
    TransferPlanCompleted {
        plan_id: TransferPlanId,
    },
    TransferPlanCancelled {
        plan_id: TransferPlanId,
    },
    TransferPlanFailed {
        plan_id: TransferPlanId,
        error: UserFacingError,
    },
    TransferQueued(TransferSnapshot),
    TransferPlanning {
        transfer_id: TransferId,
    },
    TransferConflict(TransferConflictPrompt),
    TransferRunning(TransferSnapshot),
    TransferProgress(TransferProgress),
    TransferWarning(TransferWarning),
    TransferCompleted {
        transfer_id: TransferId,
    },
    TransferSkipped {
        transfer_id: TransferId,
    },
    TransferFailed(TransferFailure),
    /// A temporary `.macsftp-part-*` file was created for an in-flight
    /// transfer. The app persists it so a crash or hard-kill can be
    /// reconciled on the next launch (plan M5/M6 residual).
    ResidualTempCreated(ResidualTempRecord),
    /// The temporary file for `transfer_id` at `path` was cleaned, so its
    /// residual record can be dropped.
    ResidualTempCleared {
        transfer_id: TransferId,
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteScoped<T> {
    pub scope: RemoteEventScope,
    pub payload: T,
}

impl<T> RemoteScoped<T> {
    pub fn new(scope: RemoteEventScope, payload: T) -> Self {
        Self { scope, payload }
    }
}

impl AppEvent {
    /// Extract the remote event scope from tab-scoped events.
    ///
    /// Returns `Some` for events that originate from a remote session
    /// and must pass the stale event guard before mutating state.
    /// Returns `None` for global events (tab lifecycle, transfers,
    /// local directory listings) that don't depend on a specific
    /// session being live.
    ///
    /// `HostKeyUnknown` is included because the prompt carries the
    /// same `(tab_id, session_id, session_epoch)` triple — a stale
    /// host key prompt must not activate a modal for a new session.
    pub fn remote_scope(&self) -> Option<RemoteEventScope> {
        match self {
            Self::TabConnected(scoped) => Some(scoped.scope.clone()),
            Self::TabDisconnected(scoped) => Some(scoped.scope.clone()),
            Self::AuthFailed(scoped) => Some(scoped.scope.clone()),
            Self::RemoteDirLoading(scoped) => Some(scoped.scope.clone()),
            Self::RemoteDirLoaded(scoped) => Some(scoped.scope.clone()),
            Self::RemoteOperationFailed(scoped) => Some(scoped.scope.clone()),
            Self::HostKeyUnknown(prompt) => Some(RemoteEventScope::new(
                prompt.tab_id,
                prompt.session_id,
                prompt.session_epoch,
            )),
            // FsOperationFailed is filtered in the app by tab_id + epoch
            // (FsScope has no session_id, so it cannot use the full remote
            // guard without a false SessionId).
            _ => None,
        }
    }

    /// Returns `true` if this event is tab-scoped and must pass the
    /// stale event guard. Transfer events return `false` — they flow
    /// to the global `TransferStore` regardless of tab state.
    pub fn is_remote_scoped(&self) -> bool {
        self.remote_scope().is_some()
    }

    /// Returns `true` if this event is a transfer event that does
    /// not depend on tab survival. The event drain task should route
    /// these directly to `TransferStore` without checking the stale
    /// event guard.
    pub fn is_transfer_event(&self) -> bool {
        matches!(
            self,
            Self::TransferQueued(_)
                | Self::TransferPlanStarted(_)
                | Self::TransferPlanProgress(_)
                | Self::TransferPlanCompleted { .. }
                | Self::TransferPlanCancelled { .. }
                | Self::TransferPlanFailed { .. }
                | Self::TransferPlanning { .. }
                | Self::TransferConflict(_)
                | Self::TransferRunning(_)
                | Self::TransferProgress(_)
                | Self::TransferWarning(_)
                | Self::TransferCompleted { .. }
                | Self::TransferSkipped { .. }
                | Self::TransferFailed(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSnapshot {
    pub tab: TabState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPrompt {
    pub request_id: TrustRequestId,
    pub tab_id: TabId,
    pub session_id: SessionId,
    pub session_epoch: u64,
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRequest {
    pub id: TrustRequestId,
    pub tab_id: TabId,
    pub session_id: SessionId,
    pub session_epoch: u64,
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint_sha256: String,
    pub expires_at: Timestamp,
}

impl TrustRequest {
    pub fn new(
        id: TrustRequestId,
        scope: RemoteEventScope,
        host: impl Into<String>,
        port: u16,
        algorithm: impl Into<String>,
        fingerprint_sha256: impl Into<String>,
        expires_at: Timestamp,
    ) -> Self {
        Self {
            id,
            tab_id: scope.tab_id,
            session_id: scope.session_id,
            session_epoch: scope.session_epoch,
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            fingerprint_sha256: fingerprint_sha256.into(),
            expires_at,
        }
    }

    pub fn prompt(&self) -> HostKeyPrompt {
        HostKeyPrompt {
            request_id: self.id,
            tab_id: self.tab_id,
            session_id: self.session_id,
            session_epoch: self.session_epoch,
            host: self.host.clone(),
            port: self.port,
            algorithm: self.algorithm.clone(),
            fingerprint_sha256: self.fingerprint_sha256.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    TrustAndSave,
    Reject,
    TimedOut,
    RequestExpired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyMismatch {
    pub tab_id: TabId,
    pub host: String,
    pub port: u16,
    pub expected_fingerprint_sha256: Option<String>,
    pub actual_fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabConnected {
    pub remote_root: RemotePath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabDisconnected {
    pub reason: DisconnectReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailure {
    pub reason: UserFacingError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDirLoading {
    pub path: RemotePath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDirSnapshot {
    pub path: RemotePath,
    pub entries: Vec<RemoteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOperationFailure {
    pub operation: String,
    /// The affected path when this operation targets one. Keeping it on
    /// the event lets the pane ignore a late failure for an older
    /// navigation request in the same session.
    pub path: Option<RemotePath>,
    pub error: UserFacingError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDirSnapshot {
    pub tab_id: TabId,
    pub path: LocalPath,
    pub entries: Vec<LocalEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferSnapshot {
    pub job: TransferJob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlanSnapshot {
    pub plan: TransferPlan,
    /// A plan root is displayed while the planner discovers child jobs.
    pub root_job: TransferJob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlanProgress {
    pub plan_id: TransferPlanId,
    pub child_job: TransferJob,
    pub planned_count: usize,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub transfer_id: TransferId,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferConflictPrompt {
    pub request_id: ConflictRequestId,
    pub plan_id: TransferPlanId,
    pub transfer_id: TransferId,
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    /// Best-effort source details for the conflict decision UI. Missing
    /// metadata must not prevent a user from resolving the conflict.
    pub source_size: Option<u64>,
    pub source_modified_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictRequest {
    pub id: ConflictRequestId,
    pub plan_id: TransferPlanId,
    pub transfer_id: TransferId,
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    pub source_size: Option<u64>,
    pub source_modified_at: Option<Timestamp>,
    pub detected_at: Timestamp,
}

impl ConflictRequest {
    pub fn new(
        id: ConflictRequestId,
        plan_id: TransferPlanId,
        transfer_id: TransferId,
        source: TransferEndpoint,
        destination: TransferEndpoint,
        detected_at: Timestamp,
    ) -> Self {
        Self {
            id,
            plan_id,
            transfer_id,
            source,
            destination,
            source_size: None,
            source_modified_at: None,
            detected_at,
        }
    }

    pub fn with_source_details(
        mut self,
        source_size: Option<u64>,
        source_modified_at: Option<Timestamp>,
    ) -> Self {
        self.source_size = source_size;
        self.source_modified_at = source_modified_at;
        self
    }

    pub fn prompt(&self) -> TransferConflictPrompt {
        TransferConflictPrompt {
            request_id: self.id,
            plan_id: self.plan_id,
            transfer_id: self.transfer_id,
            source: self.source.clone(),
            destination: self.destination.clone(),
            source_size: self.source_size,
            source_modified_at: self.source_modified_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferFailure {
    pub transfer_id: TransferId,
    pub error: UserFacingError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferJob {
    pub id: TransferId,
    pub direction: TransferDirection,
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    pub state: TransferState,
    pub metadata_policy: MetadataPolicy,
    pub conflict_policy: ConflictPolicy,
    pub warnings: Vec<TransferWarning>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub id: TransferPlanId,
    pub root_job_id: TransferId,
    pub source_root: TransferEndpoint,
    pub destination_root: TransferEndpoint,
    pub state: TransferPlanState,
    pub planned_count: usize,
    pub total_bytes: Option<u64>,
    pub child_jobs: Vec<TransferId>,
    pub conflict_policy: ConflictPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferPlanState {
    Queued,
    Planning,
    Running,
    Completed,
    Cancelled,
    Failed { error: UserFacingError },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Planning,
    WaitingForConflictDecision {
        plan_id: TransferPlanId,
        request_id: ConflictRequestId,
    },
    Running {
        bytes_done: u64,
        bytes_total: Option<u64>,
        started_at: Timestamp,
    },
    Cancelling,
    Completed,
    Skipped,
    Failed {
        error: UserFacingError,
        retryable: bool,
    },
}

/// A temporary `.macsftp-part-*` file left behind by an in-flight
/// transfer. Recorded eagerly the moment the temp file is created so a
/// crash or hard-kill mid-transfer can be reconciled on the next launch
/// (plan M5/M6 residual). `transfer_id` + `path` together uniquely name
/// the temp file and double as the removal key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidualTempRecord {
    /// `host:port` for remote temps (matches the connected profile), or
    /// `"local"` for download temps that are cleaned without a connection.
    pub connection_key: String,
    pub direction: TransferDirection,
    pub transfer_id: TransferId,
    pub path: String,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferWarning {
    pub transfer_id: TransferId,
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<String>,
}

impl TransferWarning {
    pub fn new(transfer_id: TransferId, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            transfer_id,
            code,
            message: message.into(),
            detail: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TransferHistoryId(pub u64);

impl std::fmt::Display for TransferHistoryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Outcome tag for a session-scoped transfer history record.
///
/// Product policy: history is **not** a cross-launch catalog. Live
/// Active/Completed/Failed jobs live in `TransferStore` for the process
/// lifetime. These records exist for mid-session helpers/tests (e.g. retry
/// command rebuild) and are wiped on quit/launch residual purge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferHistoryStatus {
    Unfinished,
    Completed,
    Failed { message: String, retryable: bool },
    Cancelled,
}

/// Session-scoped transfer snapshot used to rebuild a
/// `StartTransferCommand` for the *current* connected tab on retry
/// (does not restore a closed tab's session actor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferHistoryRecord {
    pub id: TransferHistoryId,
    pub profile_id: Option<ProfileId>,
    pub direction: TransferDirection,
    pub sources: Vec<TransferEndpoint>,
    pub destination: TransferEndpoint,
    pub metadata_policy: MetadataPolicy,
    pub conflict_policy: ConflictPolicy,
    pub status: TransferHistoryStatus,
    pub started_at: Timestamp,
    pub last_updated: Timestamp,
}

impl TransferHistoryRecord {
    /// Rebuild a transfer command for retry against the live session
    /// identified by `tab_id` / `session_epoch` / `profile_id`.
    pub fn to_start_command(
        &self,
        tab_id: TabId,
        session_epoch: u64,
        profile_id: ProfileId,
    ) -> StartTransferCommand {
        StartTransferCommand {
            tab_id,
            session_epoch,
            profile_id,
            direction: self.direction,
            sources: self.sources.clone(),
            destination: self.destination.clone(),
            metadata_policy: self.metadata_policy.clone(),
            conflict_policy: self.conflict_policy.clone(),
        }
    }

    /// Whether this record can be retried (re-enqueued as a fresh
    /// transfer on a connected tab).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.status,
            TransferHistoryStatus::Unfinished
                | TransferHistoryStatus::Failed {
                    retryable: true,
                    ..
                }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalRequest {
    HostKey(HostKeyPrompt),
    TransferConflict(TransferConflictPrompt),
    Error(UserFacingError),
}

impl ModalRequest {
    pub fn request_id(&self) -> Option<ModalRequestId> {
        match self {
            Self::HostKey(prompt) => Some(ModalRequestId::Trust(prompt.request_id)),
            Self::TransferConflict(prompt) => Some(ModalRequestId::Conflict(prompt.request_id)),
            Self::Error(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModalRequestId {
    Trust(TrustRequestId),
    Conflict(ConflictRequestId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferEndpoint {
    Local(LocalPath),
    Remote(RemotePath),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataPolicy {
    pub preserve_permissions: bool,
    pub preserve_mtime: bool,
    pub preserve_symlinks: bool,
}

impl Default for MetadataPolicy {
    fn default() -> Self {
        Self {
            preserve_permissions: true,
            preserve_mtime: true,
            preserve_symlinks: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ConflictPolicy {
    #[default]
    Ask,
    OverwriteAll,
    SkipAll,
    RenameAll(RenameStrategy),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenameStrategy {
    AppendCopySuffix,
    ExplicitName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictDecision {
    Overwrite {
        apply_to_all: bool,
    },
    Skip {
        apply_to_all: bool,
    },
    Rename {
        new_name: String,
        apply_to_all: bool,
    },
    CancelJob,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::AuthFailed => "auth_failed",
            Self::HostKeyMismatch => "host_key_mismatch",
            Self::TrustRequestTimeout => "trust_request_timeout",
            Self::UserCancelledTrustRequest => "user_cancelled_trust_request",
            Self::PermissionDenied => "permission_denied",
            Self::FileExists => "file_exists",
            Self::NotFound => "not_found",
            Self::UnsupportedSymlink => "unsupported_symlink",
            Self::MetadataPreservationFailed => "metadata_preservation_failed",
            Self::ChannelClosed => "channel_closed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        };
        formatter.write_str(code)
    }
}

#[cfg(test)]
mod tests {
    use super::AppEvent;
    use super::{
        AppState, AuthFingerprint, AuthMethod, AuthMethodKind, ConflictPolicy, ConflictRequest,
        ConflictRequestId, ConnectionProfile, ConnectionState, DisconnectReason, HostKeyPrompt,
        LocalPath, MetadataPolicy, ModalRequest, ModalRequestId, ProfileId, RemoteEventScope,
        RemotePath, RemoteScoped, RuntimeBridgeConfig, SecretRef, SessionId, TabId, TabState,
        Timestamp, TransferDirection, TransferEndpoint, TransferHistoryId, TransferHistoryRecord,
        TransferHistoryStatus, TransferId, TransferPlanId, TrustRequest, TrustRequestId,
    };

    #[test]
    fn exposes_crate_name() {
        assert_eq!(super::crate_name(), "macsftp-core");
    }

    #[test]
    fn path_join_and_parent_round_trip() {
        assert_eq!(
            LocalPath::new("/home/alex").join("docs").as_str(),
            "/home/alex/docs"
        );
        assert_eq!(RemotePath::new("/").join("srv").as_str(), "/srv");
        assert_eq!(
            LocalPath::new("/home/alex/docs").parent(),
            Some(LocalPath::new("/home/alex"))
        );
    }

    #[test]
    fn fs_scope_and_path_side_mapping() {
        let local_scope = super::FsScope::Local { tab_id: TabId(1) };
        assert_eq!(local_scope.tab_id(), TabId(1));
        assert_eq!(local_scope.session_epoch(), None);
        assert_eq!(local_scope.side(), super::FsSide::Local);

        let remote_scope = super::FsScope::Remote {
            tab_id: TabId(2),
            session_epoch: 7,
        };
        assert_eq!(remote_scope.session_epoch(), Some(7));
        assert_eq!(remote_scope.side(), super::FsSide::Remote);

        let local_path = super::FsPath::Local(LocalPath::new("/tmp/a"));
        let remote_path = super::FsPath::Remote(RemotePath::new("/srv/a"));
        assert_eq!(local_path.side(), super::FsSide::Local);
        assert_eq!(remote_path.side(), super::FsSide::Remote);
        assert_eq!(local_path.as_local().map(LocalPath::as_str), Some("/tmp/a"));
        assert_eq!(
            remote_path.as_remote().map(RemotePath::as_str),
            Some("/srv/a")
        );
    }

    #[test]
    fn fs_op_refresh_path_uses_parent_or_target_dir() {
        let delete = super::FsOp::Delete {
            entries: vec![super::FsEntryRef {
                path: super::FsPath::Local(LocalPath::new("/tmp/dir/file.txt")),
                is_dir: false,
            }],
        };
        assert_eq!(
            delete.refresh_path(),
            Some(super::FsPath::Local(LocalPath::new("/tmp/dir")))
        );

        let create = super::FsOp::CreateDirectory {
            parent: super::FsPath::Remote(RemotePath::new("/srv")),
            name: "new".into(),
        };
        assert_eq!(
            create.refresh_path(),
            Some(super::FsPath::Remote(RemotePath::new("/srv")))
        );
    }

    #[test]
    fn connection_profile_revision_starts_at_one() {
        let profile = ConnectionProfile::new(
            super::ProfileId(7),
            "Production",
            "example.com",
            "alex",
            AuthMethod::Password {
                secret_ref: SecretRef::new("profile-7-password"),
            },
        );

        assert_eq!(profile.revision, 1);
        assert_eq!(profile.port, 22);
    }

    #[test]
    fn auth_fingerprint_uses_profile_revision_without_secret_values() {
        let fingerprint = AuthFingerprint::password(SecretRef::new("secret-ref"), 3);

        assert_eq!(fingerprint.method, AuthMethodKind::Password);
        assert_eq!(fingerprint.profile_revision, 3);
        assert_eq!(fingerprint.secret_ref, Some(SecretRef::new("secret-ref")));
    }

    #[test]
    fn trust_request_prompt_preserves_session_binding() {
        let request = TrustRequest::new(
            TrustRequestId(4),
            RemoteEventScope::new(TabId(1), SessionId(9), 2),
            "example.com",
            22,
            "ssh-ed25519",
            "SHA256:abc",
            Timestamp::from_secs_since_epoch(300),
        );
        let prompt = request.prompt();

        assert_eq!(prompt.request_id, TrustRequestId(4));
        assert_eq!(prompt.tab_id, TabId(1));
        assert_eq!(prompt.session_id, SessionId(9));
        assert_eq!(prompt.session_epoch, 2);
        assert_eq!(prompt.fingerprint_sha256, "SHA256:abc");
    }

    #[test]
    fn tab_accepts_only_current_remote_event_scope() {
        let mut tab = TabState::new(TabId(1), "example.com");
        tab.session_epoch = 2;
        tab.connection = ConnectionState::Connected {
            session_id: SessionId(9),
            session_epoch: 2,
            connected_at: Timestamp::from_secs_since_epoch(1),
        };

        assert!(tab.accepts_remote_event(&RemoteEventScope::new(TabId(1), SessionId(9), 2)));
        assert!(!tab.accepts_remote_event(&RemoteEventScope::new(TabId(1), SessionId(8), 2)));
        assert!(!tab.accepts_remote_event(&RemoteEventScope::new(TabId(1), SessionId(9), 1)));
        assert!(!tab.accepts_remote_event(&RemoteEventScope::new(TabId(2), SessionId(9), 2)));
    }

    #[test]
    fn tab_accepts_current_in_flight_remote_event_scope() {
        let mut tab = TabState::new(TabId(1), "example.com");
        tab.session_epoch = 2;
        tab.connection = ConnectionState::Connecting {
            session_id: SessionId(9),
            session_epoch: 2,
        };

        assert!(tab.accepts_remote_event(&RemoteEventScope::new(TabId(1), SessionId(9), 2)));
        assert!(!tab.accepts_remote_event(&RemoteEventScope::new(TabId(1), SessionId(8), 2)));
    }

    #[test]
    fn metadata_policy_preserves_requested_metadata_by_default() {
        let policy = MetadataPolicy::default();

        assert!(policy.preserve_permissions);
        assert!(policy.preserve_mtime);
        assert!(policy.preserve_symlinks);
    }

    #[test]
    fn private_key_auth_reports_kind() {
        let method = AuthMethod::PrivateKey {
            key_path: LocalPath::new("/Users/alex/.ssh/id_ed25519"),
            passphrase_ref: Some(SecretRef::new("key-passphrase")),
            remember_passphrase: true,
        };

        assert_eq!(method.kind(), AuthMethodKind::PrivateKey);
    }

    #[test]
    fn runtime_bridge_defaults_match_adr_capacity_and_shutdown() {
        let config = RuntimeBridgeConfig::default();

        assert_eq!(config.command_channel_capacity, 256);
        assert_eq!(config.event_channel_capacity, 1024);
        assert_eq!(config.transfer_progress_hz, 10);
        assert_eq!(config.shutdown_timeout, std::time::Duration::from_secs(3));
    }

    #[test]
    fn app_state_keeps_long_lived_state_in_stores() {
        let mut state = AppState::new();
        state.tabs.tabs.push(TabState::new(TabId(1), "example.com"));
        state.tabs.active_tab_id = Some(TabId(1));

        let active_tab = match state.tabs.active_tab() {
            Some(tab) => tab,
            None => panic!("active tab should be found in the tab store"),
        };

        assert_eq!(active_tab.id, TabId(1));
        assert!(state.modals.active.is_empty());
    }

    #[test]
    fn conflict_request_prompt_binds_apply_to_all_to_transfer_plan() {
        let conflict = ConflictRequest::new(
            ConflictRequestId(2),
            TransferPlanId(8),
            TransferId(13),
            TransferEndpoint::Local(LocalPath::new("/Users/alex/report.txt")),
            TransferEndpoint::Remote(RemotePath::new("/srv/report.txt")),
            Timestamp::from_secs_since_epoch(99),
        )
        .with_source_details(Some(42), Some(Timestamp::from_secs_since_epoch(100)));
        let prompt = conflict.prompt();

        assert_eq!(prompt.request_id, ConflictRequestId(2));
        assert_eq!(prompt.plan_id, TransferPlanId(8));
        assert_eq!(prompt.transfer_id, TransferId(13));
        assert_eq!(prompt.source_size, Some(42));
        assert_eq!(
            prompt.source_modified_at,
            Some(Timestamp::from_secs_since_epoch(100))
        );
    }

    #[test]
    fn modal_store_finds_bound_request_ids() {
        let request = TrustRequest::new(
            TrustRequestId(4),
            RemoteEventScope::new(TabId(1), SessionId(9), 2),
            "example.com",
            22,
            "ssh-ed25519",
            "SHA256:abc",
            Timestamp::from_secs_since_epoch(300),
        );
        let mut state = AppState::new();
        state
            .modals
            .active
            .push(ModalRequest::HostKey(request.prompt()));

        assert_eq!(
            state
                .modals
                .find_request(ModalRequestId::Trust(TrustRequestId(4)))
                .map(ModalRequest::request_id),
            Some(Some(ModalRequestId::Trust(TrustRequestId(4))))
        );
    }

    // ── M2b: Stale event guard tests ──────────────────────────────

    /// Helper: create a connected tab with a known session.
    fn connected_tab(tab_id: u64, session_id: u64, epoch: u64) -> TabState {
        let mut tab = TabState::new(TabId(tab_id), "example.com");
        tab.session_epoch = epoch;
        tab.connection = ConnectionState::Connected {
            session_id: SessionId(session_id),
            session_epoch: epoch,
            connected_at: Timestamp::from_secs_since_epoch(100),
        };
        tab
    }

    #[test]
    fn tab_store_rejects_event_for_closed_tab() {
        let mut state = AppState::new();
        let tab = connected_tab(1, 10, 1);
        state.tabs.open_tab(tab);

        // While the tab exists, the event is accepted.
        let live_event = AppEvent::RemoteDirLoaded(RemoteScoped::new(
            RemoteEventScope::new(TabId(1), SessionId(10), 1),
            super::RemoteDirSnapshot {
                path: RemotePath::new("/home/alex"),
                entries: Vec::new(),
            },
        ));
        assert!(
            state.should_accept_event(&live_event),
            "event for a live tab should be accepted"
        );

        // Close the tab — the same event must now be rejected.
        state.tabs.close_tab(TabId(1));
        assert!(
            !state.should_accept_event(&live_event),
            "event for a closed tab must be rejected"
        );
    }

    #[test]
    fn tab_store_rejects_old_session_event_after_reconnect() {
        let mut state = AppState::new();
        let mut tab = TabState::new(TabId(1), "example.com");
        // First connection: epoch 1, session 10.
        tab.begin_connect(SessionId(10));
        tab.complete_connect(SessionId(10), Timestamp::from_secs_since_epoch(100));
        state.tabs.open_tab(tab);

        // Reconnect: epoch becomes 2, new session 11.
        state
            .tabs
            .find_tab_mut(TabId(1))
            .expect("tab must exist")
            .begin_reconnect(SessionId(11));
        state
            .tabs
            .find_tab_mut(TabId(1))
            .expect("tab must exist")
            .complete_connect(SessionId(11), Timestamp::from_secs_since_epoch(200));

        // Old session's TabDisconnected must not overwrite the new Connected state.
        let stale_disconnect = AppEvent::TabDisconnected(RemoteScoped::new(
            RemoteEventScope::new(TabId(1), SessionId(10), 1),
            super::TabDisconnected {
                reason: DisconnectReason::ConnectionLost,
            },
        ));
        assert!(
            !state.should_accept_event(&stale_disconnect),
            "old session disconnect must not overwrite new session"
        );

        // New session's event is accepted.
        let live_event = AppEvent::RemoteDirLoaded(RemoteScoped::new(
            RemoteEventScope::new(TabId(1), SessionId(11), 2),
            super::RemoteDirSnapshot {
                path: RemotePath::new("/home/alex"),
                entries: Vec::new(),
            },
        ));
        assert!(
            state.should_accept_event(&live_event),
            "new session event should be accepted"
        );
    }

    #[test]
    fn host_key_modal_expired_after_reconnect() {
        let mut state = AppState::new();
        let mut tab = TabState::new(TabId(1), "example.com");
        tab.begin_connect(SessionId(10)); // epoch → 1
        tab.await_host_key(SessionId(10), TrustRequestId(5));
        state.tabs.open_tab(tab);

        // Push a host key modal bound to epoch 1.
        state
            .modals
            .active
            .push(ModalRequest::HostKey(HostKeyPrompt {
                request_id: TrustRequestId(5),
                tab_id: TabId(1),
                session_id: SessionId(10),
                session_epoch: 1,
                host: "example.com".to_string(),
                port: 22,
                algorithm: "ssh-ed25519".to_string(),
                fingerprint_sha256: "SHA256:abc".to_string(),
            }));

        // Reconnect: epoch → 2. The old modal is now stale.
        state
            .tabs
            .find_tab_mut(TabId(1))
            .expect("tab must exist")
            .begin_reconnect(SessionId(11));

        let expired = state.drain_expired_modals();
        assert_eq!(expired.len(), 1, "stale host key modal should be drained");
        assert!(state.modals.active.is_empty(), "no modals should remain");
    }

    #[test]
    fn transfer_events_survive_tab_close() {
        let mut state = AppState::new();
        state.tabs.open_tab(connected_tab(1, 10, 1));

        // Close the tab.
        state.tabs.close_tab(TabId(1));

        // Transfer events should still be accepted — they flow to the
        // global TransferStore, not to the tab.
        let transfer_progress = AppEvent::TransferProgress(super::TransferProgress {
            transfer_id: TransferId(42),
            bytes_done: 1024,
            bytes_total: Some(4096),
        });
        assert!(
            state.should_accept_event(&transfer_progress),
            "transfer events must not depend on tab survival"
        );

        let transfer_completed = AppEvent::TransferCompleted {
            transfer_id: TransferId(42),
        };
        assert!(
            state.should_accept_event(&transfer_completed),
            "transfer completed must be accepted after tab close"
        );
    }

    #[test]
    fn remote_scope_extracts_from_remote_scoped_events() {
        let scope = RemoteEventScope::new(TabId(3), SessionId(7), 2);

        let dir_loaded = AppEvent::RemoteDirLoaded(RemoteScoped::new(
            scope.clone(),
            super::RemoteDirSnapshot {
                path: RemotePath::new("/srv"),
                entries: Vec::new(),
            },
        ));
        assert_eq!(dir_loaded.remote_scope(), Some(scope));
        assert!(dir_loaded.is_remote_scoped());
        assert!(!dir_loaded.is_transfer_event());

        let tab_connected = AppEvent::TabConnected(RemoteScoped::new(
            RemoteEventScope::new(TabId(1), SessionId(5), 1),
            super::TabConnected {
                remote_root: RemotePath::new("/home/alex"),
            },
        ));
        assert!(tab_connected.is_remote_scoped());
        assert!(!tab_connected.is_transfer_event());
    }

    #[test]
    fn remote_scope_extracts_from_host_key_unknown() {
        let prompt = HostKeyPrompt {
            request_id: TrustRequestId(9),
            tab_id: TabId(2),
            session_id: SessionId(4),
            session_epoch: 3,
            host: "example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:xyz".to_string(),
        };
        let event = AppEvent::HostKeyUnknown(prompt);

        let scope = event
            .remote_scope()
            .expect("HostKeyUnknown should have a scope");
        assert_eq!(scope.tab_id, TabId(2));
        assert_eq!(scope.session_id, SessionId(4));
        assert_eq!(scope.session_epoch, 3);
        assert!(event.is_remote_scoped());
    }

    #[test]
    fn transfer_events_have_no_remote_scope() {
        let events = [
            AppEvent::TransferQueued(super::TransferSnapshot {
                job: super::TransferJob {
                    id: TransferId(1),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/a")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/a")),
                    state: super::TransferState::Queued,
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: super::ConflictPolicy::default(),
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(1),
                },
            }),
            AppEvent::TransferPlanning {
                transfer_id: TransferId(1),
            },
            AppEvent::TransferProgress(super::TransferProgress {
                transfer_id: TransferId(1),
                bytes_done: 0,
                bytes_total: Some(100),
            }),
            AppEvent::TransferCompleted {
                transfer_id: TransferId(1),
            },
        ];

        for event in &events {
            assert!(
                event.remote_scope().is_none(),
                "transfer events must not carry remote scope"
            );
            assert!(
                event.is_transfer_event(),
                "transfer events should be classified as such"
            );
            assert!(
                !event.is_remote_scoped(),
                "transfer events are not remote-scoped"
            );
        }
    }

    #[test]
    fn tab_store_open_close_lifecycle() {
        let mut store = super::TabStore::default();

        // Open first tab — becomes active.
        let tab1 = TabState::new(TabId(1), "host-a");
        store.open_tab(tab1);
        assert_eq!(store.active_tab_id, Some(TabId(1)));
        assert_eq!(store.tabs.len(), 1);

        // Open second tab — first remains active.
        let tab2 = TabState::new(TabId(2), "host-b");
        store.open_tab(tab2);
        assert_eq!(store.active_tab_id, Some(TabId(1)));
        assert_eq!(store.tabs.len(), 2);

        // Close active tab — second tab becomes active.
        let removed = store.close_tab(TabId(1));
        assert!(removed.is_some(), "close_tab should return the removed tab");
        assert_eq!(store.active_tab_id, Some(TabId(2)));
        assert_eq!(store.tabs.len(), 1);

        // Close last tab — no active tab.
        store.close_tab(TabId(2));
        assert_eq!(store.active_tab_id, None);
        assert!(store.tabs.is_empty());

        // Close non-existent tab — returns None.
        assert!(store.close_tab(TabId(99)).is_none());
    }

    #[test]
    fn begin_connect_increments_session_epoch() {
        let mut tab = TabState::new(TabId(1), "example.com");
        assert_eq!(tab.session_epoch, 0);

        tab.begin_connect(SessionId(10));
        assert_eq!(tab.session_epoch, 1);
        assert!(matches!(
            tab.connection,
            ConnectionState::Connecting {
                session_id: SessionId(10),
                session_epoch: 1,
            }
        ));

        // Reconnect increments again.
        tab.begin_reconnect(SessionId(11));
        assert_eq!(tab.session_epoch, 2);
        assert!(matches!(
            tab.connection,
            ConnectionState::Reconnecting {
                session_id: SessionId(11),
                session_epoch: 2,
                ..
            }
        ));
    }

    #[test]
    fn stale_host_key_unknown_event_rejected_after_reconnect() {
        let mut state = AppState::new();
        let mut tab = TabState::new(TabId(1), "example.com");
        tab.begin_connect(SessionId(10)); // epoch 1
        state.tabs.open_tab(tab);

        // Reconnect: epoch 2.
        state
            .tabs
            .find_tab_mut(TabId(1))
            .expect("tab must exist")
            .begin_reconnect(SessionId(11));

        // Old host key prompt (epoch 1) should be rejected.
        let stale_prompt = AppEvent::HostKeyUnknown(HostKeyPrompt {
            request_id: TrustRequestId(1),
            tab_id: TabId(1),
            session_id: SessionId(10),
            session_epoch: 1,
            host: "example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:old".to_string(),
        });
        assert!(
            !state.should_accept_event(&stale_prompt),
            "stale host key prompt must be rejected after reconnect"
        );

        // New host key prompt (epoch 2) should be accepted.
        let live_prompt = AppEvent::HostKeyUnknown(HostKeyPrompt {
            request_id: TrustRequestId(2),
            tab_id: TabId(1),
            session_id: SessionId(11),
            session_epoch: 2,
            host: "example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:new".to_string(),
        });
        assert!(
            state.should_accept_event(&live_prompt),
            "current host key prompt should be accepted"
        );
    }

    #[test]
    fn global_events_always_accepted() {
        let state = AppState::new(); // no tabs at all

        // TabOpened, TabClosed, LocalDirLoaded — all global, no tab needed.
        let tab_opened = AppEvent::TabOpened(super::TabSnapshot {
            tab: TabState::new(TabId(1), "example.com"),
        });
        assert!(state.should_accept_event(&tab_opened));

        let local_dir = AppEvent::LocalDirLoaded(super::LocalDirSnapshot {
            tab_id: TabId(1),
            path: LocalPath::new("/Users/alex"),
            entries: Vec::new(),
        });
        assert!(state.should_accept_event(&local_dir));
    }

    // ── Default sort rules (plan §12) ──────────────────────────────

    fn remote_entry(name: &str, kind: super::FileKind, size: Option<u64>) -> super::RemoteEntry {
        super::RemoteEntry {
            name: name.to_string(),
            path: RemotePath::new(format!("/srv/{name}")),
            kind,
            size,
            permissions: None,
            modified_at: None,
            link_target: None,
        }
    }

    #[test]
    fn default_sort_puts_directories_first_then_name_ascending() {
        use super::FileKind;

        let mut entries = vec![
            remote_entry("zeta.txt", FileKind::File, Some(10)),
            remote_entry("Alpha", FileKind::Directory, None),
            remote_entry("beta.txt", FileKind::File, Some(5)),
            remote_entry("Gamma", FileKind::Directory, None),
        ];
        super::sort_entries(&mut entries, &super::FileSort::default());

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Gamma", "beta.txt", "zeta.txt"]);
    }

    #[test]
    fn name_sort_is_case_insensitive_with_stable_fallback() {
        use super::FileKind;

        let mut entries = vec![
            remote_entry("readme", FileKind::File, None),
            remote_entry("README", FileKind::File, None),
            remote_entry("banana", FileKind::File, None),
        ];
        super::sort_entries(&mut entries, &super::FileSort::default());

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // Case-insensitive: banana < readme/README; the equal-fold pair
        // orders deterministically by the case-sensitive fallback.
        assert_eq!(names, ["banana", "README", "readme"]);
    }

    #[test]
    fn size_sort_descending_keeps_directories_first() {
        use super::{FileKind, FileSort, FileSortField, SortDirection};

        let mut entries = vec![
            remote_entry("small.txt", FileKind::File, Some(1)),
            remote_entry("big.txt", FileKind::File, Some(1000)),
            remote_entry("dir", FileKind::Directory, None),
        ];
        super::sort_entries(
            &mut entries,
            &FileSort {
                field: FileSortField::Size,
                direction: SortDirection::Descending,
                directories_first: true,
            },
        );

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["dir", "big.txt", "small.txt"]);
    }

    #[test]
    fn sort_ties_break_by_full_path_for_stability() {
        use super::FileKind;

        let mut entries = vec![
            super::RemoteEntry {
                name: "same.txt".to_string(),
                path: RemotePath::new("/srv/b/same.txt"),
                kind: FileKind::File,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            },
            super::RemoteEntry {
                name: "same.txt".to_string(),
                path: RemotePath::new("/srv/a/same.txt"),
                kind: FileKind::File,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            },
        ];
        super::sort_entries(&mut entries, &super::FileSort::default());

        assert_eq!(entries[0].path.as_str(), "/srv/a/same.txt");
        assert_eq!(entries[1].path.as_str(), "/srv/b/same.txt");
    }

    #[test]
    fn session_ids_allocate_monotonically() {
        let mut state = AppState::new();
        assert_eq!(state.allocate_session_id(), SessionId(1));
        assert_eq!(state.allocate_session_id(), SessionId(2));
        assert_eq!(state.allocate_session_id(), SessionId(3));
    }

    #[test]
    fn path_parent_walks_to_root() {
        assert_eq!(
            RemotePath::new("/home/alex/docs").parent(),
            Some(RemotePath::new("/home/alex"))
        );
        assert_eq!(
            RemotePath::new("/home").parent(),
            Some(RemotePath::new("/"))
        );
        assert_eq!(RemotePath::new("/").parent(), None);
        assert_eq!(
            LocalPath::new("/Users/alex/").parent(),
            Some(LocalPath::new("/Users"))
        );
    }

    #[test]
    fn local_entries_sort_with_same_rules() {
        use super::FileKind;

        let mut entries = vec![
            super::LocalEntry {
                name: "notes.txt".to_string(),
                path: LocalPath::new("/Users/alex/notes.txt"),
                kind: FileKind::File,
                size: Some(3),
                permissions: None,
                modified_at: None,
                link_target: None,
            },
            super::LocalEntry {
                name: "Documents".to_string(),
                path: LocalPath::new("/Users/alex/Documents"),
                kind: FileKind::Directory,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            },
        ];
        super::sort_entries(&mut entries, &super::FileSort::default());

        assert_eq!(entries[0].name, "Documents");
        assert_eq!(entries[1].name, "notes.txt");
    }

    #[test]
    fn from_connection_settings_maps_secret_to_keychain_ref() {
        use super::{AuthCredential, ConnectionSettings};

        // Password: the secret is never copied into the profile; only a
        // keychain stub ref is stored.
        let password_settings = ConnectionSettings {
            host: "example.com".into(),
            port: 2222,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "super-secret".into(),
            },
        };
        let profile = ConnectionProfile::from_connection_settings(
            ProfileId(1),
            "Production",
            &password_settings,
        );
        assert_eq!(profile.host, "example.com");
        assert_eq!(profile.port, 2222);
        assert_eq!(
            profile.auth,
            AuthMethod::Password {
                secret_ref: SecretRef::keychain_ref(ProfileId(1), "password")
            }
        );

        // Private key with passphrase: key path is persisted (not secret),
        // passphrase becomes a keychain ref only when provided.
        let key_settings = ConnectionSettings {
            host: "example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::PrivateKey {
                key_path: "/Users/alex/.ssh/id_ed25519".into(),
                passphrase: Some("key-phrase".into()),
            },
        };
        let key_profile =
            ConnectionProfile::from_connection_settings(ProfileId(2), "Jump", &key_settings);
        assert_eq!(
            key_profile.auth,
            AuthMethod::PrivateKey {
                key_path: LocalPath::new("/Users/alex/.ssh/id_ed25519"),
                passphrase_ref: Some(SecretRef::keychain_ref(ProfileId(2), "passphrase")),
                remember_passphrase: true,
            }
        );

        // Private key without passphrase: no keychain ref, nothing to remember.
        let no_pass = ConnectionSettings {
            host: "example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::PrivateKey {
                key_path: "/Users/alex/.ssh/id_rsa".into(),
                passphrase: None,
            },
        };
        let no_pass_profile =
            ConnectionProfile::from_connection_settings(ProfileId(3), "Bare", &no_pass);
        assert_eq!(
            no_pass_profile.auth,
            AuthMethod::PrivateKey {
                key_path: LocalPath::new("/Users/alex/.ssh/id_rsa"),
                passphrase_ref: None,
                remember_passphrase: false,
            }
        );
    }

    #[test]
    fn transfer_history_record_round_trips_through_json() {
        let record = TransferHistoryRecord {
            id: TransferHistoryId(42),
            profile_id: Some(ProfileId(3)),
            direction: TransferDirection::Upload,
            sources: vec![TransferEndpoint::Local(LocalPath::new("/tmp/a.txt"))],
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::Ask,
            status: TransferHistoryStatus::Failed {
                message: "boom".into(),
                retryable: true,
            },
            started_at: Timestamp::from_secs_since_epoch(1_700_000_000),
            last_updated: Timestamp::from_secs_since_epoch(1_700_000_123),
        };
        let json = serde_json::to_string(&record).expect("serialize");
        let restored: TransferHistoryRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, restored);
        assert!(restored.is_retryable());
    }

    #[test]
    fn timestamp_serializes_as_unix_seconds() {
        let ts = Timestamp::from_secs_since_epoch(1_234_567_890);
        let json = serde_json::to_string(&ts).expect("serialize");
        assert_eq!(json, "1234567890");
        let restored: Timestamp = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, ts);
    }
}
