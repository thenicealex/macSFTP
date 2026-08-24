use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub fn crate_name() -> &'static str {
    "macsftp-core"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TabId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct WindowSessionId(pub u64);

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

    /// Truncate to whole seconds since the Unix epoch, discarding any
    /// sub-second component. SFTP servers report mtime at whole-second
    /// precision, so a snapshot derived from a local file's `SystemTime`
    /// (which carries nanoseconds) must be truncated to that granularity
    /// before it can be compared against a server listing without spurious
    /// divergence. Times before the epoch collapse to the epoch.
    pub fn truncated_to_secs(self) -> Self {
        let seconds = self
            .0
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        Self::from_secs_since_epoch(seconds)
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
    LocalNetworkPermissionDenied,
    PermissionDenied,
    FileExists,
    NotFound,
    UnsupportedSymlink,
    MetadataPreservationFailed,
    ChannelClosed,
    /// Mid-session loss: no inbound traffic for ~60s (keepalive gave up).
    NetworkTimeout,
    /// Mid-session loss: any other local transport failure.
    NetworkError,
    /// Mid-session loss: the server sent an SSH DISCONNECT message.
    ServerDisconnected,
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
                has_passphrase: passphrase.is_some(),
                passphrase_ref: passphrase
                    .as_ref()
                    .map(|_| SecretRef::keychain_ref(id, "passphrase")),
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
    /// Private-key auth with an explicit three-state passphrase:
    ///
    /// 1. `has_passphrase: false` — the key is not encrypted;
    /// 2. `has_passphrase: true, passphrase_ref: None` — the key needs a
    ///    passphrase but it is not remembered (entered once per session);
    /// 3. `has_passphrase: true, passphrase_ref: Some` — the passphrase is
    ///    remembered in the Keychain under that ref.
    PrivateKey {
        key_path: LocalPath,
        /// Defaults to false so pre-tri-state files (which spelled this
        /// state as `remember_passphrase`) still deserialize.
        #[serde(default)]
        has_passphrase: bool,
        passphrase_ref: Option<SecretRef>,
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
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
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
    pub identity: ConnectionPoolIdentity,
}

impl ConnectionKey {
    pub fn new(settings: &ConnectionSettings, identity: ConnectionPoolIdentity) -> Self {
        Self {
            host: settings.host.clone(),
            port: settings.port,
            username: settings.username.clone(),
            identity,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
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

/// Non-secret identity used to decide whether an established SSH connection
/// can be reused. Saved profiles use only persisted credential references and
/// their revision; one-off connections are scoped to the UI-allocated session
/// and never share a physical connection across attempts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConnectionPoolIdentity {
    Saved(AuthFingerprint),
    Ephemeral(SessionId),
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

    /// Apply one runtime transfer event at the process-wide state boundary.
    ///
    /// The reducer is deliberately idempotent: replaying an event must not
    /// duplicate plans, jobs, warnings, or conflicts. Window views observe
    /// this store but never replay runtime transfer events themselves.
    pub fn apply_event(&mut self, event: &AppEvent, now: Timestamp) -> bool {
        match event {
            AppEvent::TransferPlanStarted(snapshot) => {
                self.insert_plan(snapshot.plan.clone()) | self.insert_job(snapshot.root_job.clone())
            }
            AppEvent::TransferPlanProgress(progress) => {
                let mut changed = false;
                if let Some(plan) = self
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == progress.plan_id)
                {
                    // A terminal plan can no longer be extended. A late progress
                    // (e.g. a child streamed before an async planning failure)
                    // must not resurrect or grow a finished plan.
                    if matches!(
                        plan.state,
                        TransferPlanState::Completed
                            | TransferPlanState::Cancelled
                            | TransferPlanState::Failed { .. }
                    ) {
                        return false;
                    }
                    if progress.planned_count > plan.planned_count {
                        plan.planned_count = progress.planned_count;
                        plan.total_bytes = progress.total_bytes;
                        changed = true;
                    }
                    for child_job in &progress.child_jobs {
                        if !plan.child_jobs.contains(&child_job.id) {
                            plan.child_jobs.push(child_job.id);
                            changed = true;
                        }
                    }
                }
                for child_job in &progress.child_jobs {
                    changed |= self.insert_job(child_job.clone());
                }
                changed
            }
            AppEvent::TransferPlanCompleted { plan_id } => {
                let Some(plan) = self.plans.iter_mut().find(|plan| plan.id == *plan_id) else {
                    return false;
                };
                if matches!(
                    plan.state,
                    TransferPlanState::Completed
                        | TransferPlanState::Cancelled
                        | TransferPlanState::Failed { .. }
                ) {
                    return false;
                }
                let root_job_id = plan.root_job_id;
                let empty = plan.child_jobs.is_empty();
                let plan_state = if empty {
                    TransferPlanState::Completed
                } else {
                    TransferPlanState::Queued
                };
                let root_state = if empty {
                    TransferState::Completed
                } else {
                    TransferState::Queued
                };
                let mut changed = false;
                if plan.state != plan_state {
                    plan.state = plan_state;
                    changed = true;
                }
                if let Some(root_job) = self.jobs.iter_mut().find(|job| job.id == root_job_id)
                    && root_job.state != root_state
                {
                    root_job.state = root_state;
                    changed = true;
                }
                changed
            }
            AppEvent::TransferPlanCancelled { plan_id } => self.set_plan_terminal_state(
                *plan_id,
                TransferPlanState::Cancelled,
                TransferState::Skipped,
            ),
            AppEvent::TransferPlanFailed { plan_id, error } => self.set_plan_terminal_state(
                *plan_id,
                TransferPlanState::Failed {
                    error: error.clone(),
                },
                TransferState::Failed {
                    retryable: error.retryable,
                    error: error.clone(),
                },
            ),
            AppEvent::TransferQueued(snapshot) | AppEvent::TransferRunning(snapshot) => {
                self.upsert_job(snapshot.job.clone())
            }
            AppEvent::TransferPlanning { transfer_id } => {
                self.set_job_state(*transfer_id, TransferState::Planning)
            }
            AppEvent::TransferConflict(prompt) => {
                let waiting = self.set_job_state(
                    prompt.transfer_id,
                    TransferState::WaitingForConflictDecision {
                        plan_id: prompt.plan_id,
                        request_id: prompt.request_id,
                    },
                );
                let conflict = ConflictRequest::new(
                    prompt.request_id,
                    prompt.plan_id,
                    prompt.transfer_id,
                    prompt.source.clone(),
                    prompt.destination.clone(),
                    now,
                )
                .with_source_details(prompt.source_size, prompt.source_modified_at);
                waiting | self.upsert_conflict(conflict)
            }
            AppEvent::TransferProgress(progress) => {
                let Some(job) = self
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == progress.transfer_id)
                else {
                    return false;
                };
                if matches!(
                    job.state,
                    TransferState::Cancelling
                        | TransferState::Completed
                        | TransferState::Skipped
                        | TransferState::Failed { .. }
                ) {
                    return false;
                }
                let started_at = match job.state {
                    TransferState::Running { started_at, .. } => started_at,
                    _ => now,
                };
                let state = TransferState::Running {
                    bytes_done: progress.bytes_done,
                    bytes_total: progress.bytes_total,
                    started_at,
                };
                if job.state == state {
                    false
                } else {
                    job.state = state;
                    true
                }
            }
            AppEvent::TransferWarning(warning) => {
                let Some(job) = self
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == warning.transfer_id)
                else {
                    return false;
                };
                if job.warnings.contains(warning) {
                    false
                } else {
                    job.warnings.push(warning.clone());
                    true
                }
            }
            AppEvent::TransferCompleted { transfer_id } => {
                self.set_transfer_terminal(*transfer_id, TransferState::Completed)
            }
            AppEvent::TransferSkipped { transfer_id } => {
                self.set_transfer_terminal(*transfer_id, TransferState::Skipped)
            }
            AppEvent::TransferFailed(failure) => self.set_transfer_terminal(
                failure.transfer_id,
                TransferState::Failed {
                    retryable: failure.error.retryable,
                    error: failure.error.clone(),
                },
            ),
            _ => false,
        }
    }

    pub fn mark_cancelling(&mut self, transfer_id: TransferId) -> bool {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == transfer_id) else {
            return false;
        };
        if matches!(
            job.state,
            TransferState::Queued | TransferState::Running { .. }
        ) {
            job.state = TransferState::Cancelling;
            true
        } else {
            false
        }
    }

    pub fn remove_conflict(&mut self, request_id: ConflictRequestId) -> bool {
        let previous_len = self.pending_conflicts.len();
        self.pending_conflicts
            .retain(|conflict| conflict.id != request_id);
        self.pending_conflicts.len() != previous_len
    }

    fn insert_plan(&mut self, plan: TransferPlan) -> bool {
        if self.plans.iter().any(|existing| existing.id == plan.id) {
            false
        } else {
            self.plans.push(plan);
            true
        }
    }

    fn insert_job(&mut self, job: TransferJob) -> bool {
        if self.jobs.iter().any(|existing| existing.id == job.id) {
            false
        } else {
            self.jobs.push(job);
            true
        }
    }

    fn upsert_job(&mut self, job: TransferJob) -> bool {
        if let Some(existing) = self.jobs.iter_mut().find(|existing| existing.id == job.id) {
            if *existing == job {
                false
            } else {
                *existing = job;
                true
            }
        } else {
            self.jobs.push(job);
            true
        }
    }

    fn upsert_conflict(&mut self, conflict: ConflictRequest) -> bool {
        if let Some(existing) = self
            .pending_conflicts
            .iter_mut()
            .find(|existing| existing.id == conflict.id)
        {
            if *existing == conflict {
                false
            } else {
                *existing = conflict;
                true
            }
        } else {
            self.pending_conflicts.push(conflict);
            true
        }
    }

    fn set_job_state(&mut self, transfer_id: TransferId, state: TransferState) -> bool {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == transfer_id) else {
            return false;
        };
        if job.state == state {
            false
        } else {
            job.state = state;
            true
        }
    }

    fn set_plan_terminal_state(
        &mut self,
        plan_id: TransferPlanId,
        plan_state: TransferPlanState,
        job_state: TransferState,
    ) -> bool {
        let Some(plan_index) = self.plans.iter().position(|plan| plan.id == plan_id) else {
            return false;
        };
        let root_job_id = self.plans[plan_index].root_job_id;
        // Clone the child ids first so the borrow checker lets us mutate jobs
        // while the plan stays borrowed by index below.
        let child_job_ids = self.plans[plan_index].child_jobs.clone();
        let mut changed = false;
        if self.plans[plan_index].state != plan_state {
            self.plans[plan_index].state = plan_state;
            changed = true;
        }
        changed |= self.set_job_state(root_job_id, job_state.clone());
        for child_job_id in child_job_ids {
            changed |= self.set_job_state(child_job_id, job_state.clone());
        }
        changed
    }

    fn set_transfer_terminal(&mut self, transfer_id: TransferId, state: TransferState) -> bool {
        let changed = self.set_job_state(transfer_id, state);
        let conflict_ids: Vec<_> = self
            .pending_conflicts
            .iter()
            .filter_map(|conflict| (conflict.transfer_id == transfer_id).then_some(conflict.id))
            .collect();
        let mut conflicts_changed = false;
        for request_id in conflict_ids {
            conflicts_changed |= self.remove_conflict(request_id);
        }
        changed | conflicts_changed | self.finalize_plan_for_job(transfer_id)
    }

    fn finalize_plan_for_job(&mut self, transfer_id: TransferId) -> bool {
        let Some(plan_index) = self
            .plans
            .iter()
            .position(|plan| plan.child_jobs.contains(&transfer_id))
        else {
            return false;
        };
        if matches!(
            self.plans[plan_index].state,
            TransferPlanState::Completed
                | TransferPlanState::Cancelled
                | TransferPlanState::Failed { .. }
        ) {
            return false;
        }
        let child_jobs = self.plans[plan_index].child_jobs.clone();
        let mut any_failed = false;
        for child_id in &child_jobs {
            let Some(job) = self.jobs.iter().find(|job| job.id == *child_id) else {
                return false;
            };
            match job.state {
                TransferState::Completed | TransferState::Skipped => {}
                TransferState::Failed { .. } => any_failed = true,
                _ => return false,
            }
        }

        let root_job_id = self.plans[plan_index].root_job_id;
        let (plan_state, root_state) = if any_failed {
            let error = UserFacingError::new(
                ErrorCode::Unknown,
                "Transfer finished with failures",
                "Some files could not be transferred.",
            );
            (
                TransferPlanState::Failed {
                    error: error.clone(),
                },
                TransferState::Failed {
                    retryable: true,
                    error,
                },
            )
        } else {
            (TransferPlanState::Completed, TransferState::Completed)
        };
        self.plans[plan_index].state = plan_state;
        if let Some(root_job) = self.jobs.iter_mut().find(|job| job.id == root_job_id) {
            root_job.state = root_state;
        }
        true
    }

    pub fn clear_terminal(&mut self) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|job| {
            !matches!(
                job.state,
                TransferState::Completed | TransferState::Skipped | TransferState::Failed { .. }
            )
        });
        let remaining_ids: std::collections::HashSet<TransferId> =
            self.jobs.iter().map(|job| job.id).collect();
        self.plans.retain(|plan| {
            remaining_ids.contains(&plan.root_job_id)
                || plan.child_jobs.iter().any(|id| remaining_ids.contains(id))
        });
        self.jobs.len() != before
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
    /// Cached credentials for reconnect-without-retyping. Populated for every
    /// successful connect: for saved profiles this mirrors what was last
    /// resolved from the Keychain, for manual entries it is the only copy and
    /// never persists anywhere. Zeroized on drop; Debug is fully redacted.
    /// MUST stay out of any serialized surface (session snapshots take only
    /// host/port/user).
    /// Boxed: cold field, keeps TabSnapshot-carrying events compact.
    pub connection_settings: Option<Box<ConnectionSettings>>,
    /// Non-secret identity of this tab's current physical connection, built
    /// from `connection_settings` + pool identity at connect time — the same
    /// key the connection pool uses for reuse decisions. Remote editing reads
    /// it so dedup follows the physical connection rather than the profile
    /// record, which may be re-pointed at another host mid-connection.
    /// Boxed: cold field, keeps TabSnapshot-carrying events compact.
    pub connection_key: Option<Box<ConnectionKey>>,
    pub nav: TabNavState,
    pub pending: Vec<PendingOperation>,
    /// Non-secret connection metadata restored from `session.json` or
    /// recents, consumed by reconnect prefill and snapshot building.
    /// Boxed: cold field, keeps TabSnapshot-carrying events compact.
    pub restored_target: Option<Box<RestoredTabTarget>>,
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
            connection_settings: None,
            connection_key: None,
            nav: TabNavState::default(),
            pending: Vec::new(),
            restored_target: None,
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
    /// Monotonic generation for background local directory reads. Bumped on
    /// every request; a completed read is applied only when its epoch still
    /// matches (stale-result guard, same discipline as the remote
    /// `RemoteEventScope` check). Lives here so it dies with the tab.
    pub read_epoch: u64,
}

impl LocalPaneState {
    /// Start a new background directory read and return its generation.
    pub fn begin_local_read(&mut self) -> u64 {
        self.read_epoch = self.read_epoch.saturating_add(1);
        self.read_epoch
    }

    /// Whether a finished read may still be applied: the tab must not have
    /// started a newer read, and must still be showing the requested path.
    pub fn accepts_local_result(&self, request_epoch: u64, requested_path: &LocalPath) -> bool {
        self.read_epoch == request_epoch && self.path.as_ref() == Some(requested_path)
    }
}

/// How a path change interacts with [`PaneNavHistory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOp {
    /// Record the previous path on the back stack and clear forward.
    Push,
    /// Change path without touching history (refresh-adjacent or rare replace).
    Replace,
    /// Pop back → go there; push current onto forward.
    Back,
    /// Pop forward → go there; push current onto back.
    Forward,
}

/// Back/forward stacks for one pane side.
///
/// Session navigation history is per-tab business state (like selection and
/// sort) but is intentionally not persisted across app restarts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneNavHistory {
    pub back: Vec<String>,
    pub forward: Vec<String>,
}

impl PaneNavHistory {
    pub const MAX: usize = 50;

    /// Record a user navigation from `from` to `to`.
    ///
    /// No-op when `from` is missing or equal to `to`. Otherwise pushes `from`
    /// onto back, clears forward, and trims back to [`Self::MAX`].
    pub fn push_navigating_from(&mut self, from: Option<&str>, to: &str) {
        let Some(from) = from else {
            return;
        };
        if from == to {
            return;
        }
        self.back.push(from.to_string());
        if self.back.len() > Self::MAX {
            let excess = self.back.len() - Self::MAX;
            self.back.drain(0..excess);
        }
        self.forward.clear();
    }

    /// Pop the previous path; push `current` onto forward.
    pub fn go_back(&mut self, current: &str) -> Option<String> {
        let target = self.back.pop()?;
        self.forward.push(current.to_string());
        Some(target)
    }

    /// Pop the next path; push `current` onto back.
    pub fn go_forward(&mut self, current: &str) -> Option<String> {
        let target = self.forward.pop()?;
        self.back.push(current.to_string());
        Some(target)
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

/// Local + remote history for one connection tab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabNavState {
    pub local: PaneNavHistory,
    pub remote: PaneNavHistory,
}

/// Non-secret connection metadata restored from `session.json` or the
/// recents list, used for reconnect prefill and session snapshot building.
/// Deliberately secret-free: credentials live only in
/// [`TabState::connection_settings`] (memory, zeroized) and the Keychain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoredTabTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_id: Option<ProfileId>,
    pub remote_path: Option<RemotePath>,
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
    /// Release every browsing session owned by a window in one bounded
    /// command. Transfers keep their independent runtime-owned leases.
    CloseTabs {
        tab_ids: Vec<TabId>,
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
    /// Check live remote metadata immediately before an edit upload so a stale
    /// UI directory listing cannot authorize overwriting a concurrent remote
    /// change. Routed to the connected browsing actor for `tab_id`; the actor
    /// echoes back `edit_session_id` + `check_id` with its live scope.
    CheckRemoteEditSnapshot(CheckRemoteEditSnapshotCommand),
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
    pub pool_identity: ConnectionPoolIdentity,
    /// Connection target and credentials for this attempt. Persisted copies
    /// are resolved through the Keychain before the command is created.
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
pub struct CheckRemoteEditSnapshotCommand {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
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
    /// Live remote metadata for an edit session's path, read by the browsing
    /// actor immediately before upload. Carries the actor's live
    /// `(tab_id, session_id, session_epoch)` scope so the coordinator's stale
    /// guard applies.
    RemoteEditSnapshotChecked(RemoteScoped<RemoteEditSnapshotChecked>),
    /// The actor could not read the remote metadata (missing file, permission,
    /// transport). Also scoped: a stale scope must not surface a conflict for a
    /// replacement session.
    RemoteEditSnapshotCheckFailed(RemoteScoped<RemoteEditSnapshotCheckFailed>),
    /// The runtime could not even route the check command to a live actor
    /// (missing/stale session, full/disconnected queue). No legitimate
    /// `SessionId` exists here, so this is epoch-correlated rather than
    /// remote-session-scoped: the coordinator matches it only by the exact
    /// `tab_id`/`session_epoch`/`edit_session_id`/`check_id`/`path` tuple.
    RemoteEditSnapshotDispatchFailed(RemoteEditSnapshotDispatchFailed),
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
    /// `HostKeyMismatch` is included for the same reason: a stale
    /// mismatch from a superseded session must not fail a replacement
    /// session, while a current mismatch must still hard-block.
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
            Self::HostKeyMismatch(mismatch) => Some(mismatch.scope.clone()),
            // Both authoritative edit-check outcomes carry the actor's live
            // scope, so a stale scope must not advance or conflict a
            // replacement session. `RemoteEditSnapshotDispatchFailed` is
            // intentionally excluded: no SessionId exists when routing never
            // reached a live actor, so it is epoch-correlated instead.
            Self::RemoteEditSnapshotChecked(scoped) => Some(scoped.scope.clone()),
            Self::RemoteEditSnapshotCheckFailed(scoped) => Some(scoped.scope.clone()),
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
    pub scope: RemoteEventScope,
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
    /// Newly discovered jobs since the previous planning update. The first
    /// child is emitted immediately; large trees are then delivered in
    /// bounded batches to preserve streaming feedback without one event per
    /// filesystem entry.
    pub child_jobs: Vec<TransferJob>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditSessionId(pub u64);

/// Stable identity for a single authoritative remote-metadata check issued
/// before an edit upload. UI/core is the sole allocator (`EditSessionStore::
/// next_check_id`); the runtime and the browsing actor only echo it back so a
/// delayed result from an earlier retry of the same local save can be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditCheckId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSnapshot {
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
}

impl RemoteSnapshot {
    /// A copy with `modified_at` truncated to whole seconds. Used when a
    /// snapshot is derived from a LOCAL file's `SystemTime` (sub-second
    /// precision) so it matches the whole-second mtime an SFTP server — and
    /// therefore a future directory listing — will report.
    pub fn truncated_to_secs(self) -> Self {
        Self {
            size: self.size,
            modified_at: self.modified_at.map(Timestamp::truncated_to_secs),
        }
    }

    /// Whether two snapshots agree once both mtimes are truncated to whole
    /// seconds. Size must match exactly; mtime only at second granularity.
    /// This is the "sub-second noise only" test: `true` means the sole
    /// possible difference is nanoseconds within the same second, which is an
    /// artifact of our own local-file rebase, not a real remote change.
    pub fn agrees_at_second_granularity(self, other: Self) -> bool {
        self.truncated_to_secs() == other.truncated_to_secs()
    }
}

/// Live remote metadata the browsing actor read for an in-flight edit check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotChecked {
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub snapshot: RemoteSnapshot,
}

/// The browsing actor could not read the remote metadata for an in-flight edit
/// check. Deletion is treated as indeterminate/unsafe for overwrite, not
/// "unchanged": the local edit is preserved and retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotCheckFailed {
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub error: UserFacingError,
}

/// The runtime could not route an edit check command to a live actor. Carries
/// no `SessionId`: it reports that routing never reached a session, so it is
/// epoch-correlated rather than remote-session-scoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotDispatchFailed {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub error: UserFacingError,
}

/// The lifecycle phase of a remote-edit session. Every variant is a *live*
/// (non-terminal) phase: a session is only stored while it is actively being
/// downloaded, edited, uploaded back, or resolving a remote conflict. A
/// download that fails has no terminal `Failed` state — the session is removed
/// outright (see `advance_downloading` in the app), so [`find_active`] can
/// treat every stored session as re-edit-blocking.
///
/// [`find_active`]: EditSessionStore::find_active
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPhase {
    Downloading,
    Editing,
    /// A live, authoritative remote-metadata check is in flight before upload.
    /// Treated as a live phase (blocks duplicate dispatch and re-edit) so the
    /// watcher cannot redispatch a check while one is pending. Both
    /// `pending_check_id` and `checking_local_mtime` are `Some` only here.
    CheckingRemote,
    UploadingBack,
    RemoteConflict,
}

/// Number of *consecutive* watcher ticks on which an edit temp file's metadata
/// must be unreadable before the session is torn down. The watcher polls once a
/// second, so this tolerates a few seconds of transient unavailability (atomic
/// saves, `EINTR`, mount hiccups) while still reaping a session whose file the
/// user genuinely deleted. One tick is not enough: an editor's rename-over save
/// leaves a sub-second window in which the path does not resolve.
pub const EDIT_MISSING_TICKS_LIMIT: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSession {
    pub id: EditSessionId,
    pub remote_path: RemotePath,
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    /// Non-secret identity of the physical connection this edit was opened
    /// through, captured on the tab at connect time. Dedup keys on
    /// `(connection_key, remote_path)` rather than `profile_id`: two manual
    /// connections to different servers (or a saved profile re-pointed at a
    /// new host) must never share one temp file for the same remote path.
    pub connection_key: ConnectionKey,
    pub local_temp_path: LocalPath,
    pub phase: EditPhase,
    pub remote_snapshot: RemoteSnapshot,
    pub local_mtime: Option<Timestamp>,
    pub active_transfer: Option<TransferId>,
    /// The `EditCheckId` of an in-flight authoritative remote-metadata check,
    /// set when the phase enters `CheckingRemote` and cleared on every
    /// transition out of it. Binds a returned result to the exact check that
    /// initiated it, so a delayed result from an earlier retry of the same
    /// local save is rejected.
    pub pending_check_id: Option<EditCheckId>,
    /// The local-save mtime captured when the check was dispatched. The result
    /// is only honored if the temp file still carries this exact mtime at
    /// result time; if the user saved again mid-flight the session reverts and
    /// rechecks the newer save rather than authorizing a stale result.
    pub checking_local_mtime: Option<Timestamp>,
    /// Consecutive watcher ticks on which the temp file's metadata could not be
    /// read. A single miss is treated as transient (an editor's atomic save
    /// briefly unlinks the file, `EINTR`, a mount hiccup) and does not tear the
    /// session down; only [`EDIT_MISSING_TICKS_LIMIT`] consecutive misses are
    /// taken as "the file is really gone". Reset to 0 on any successful stat.
    pub missing_ticks: u32,
}

impl EditSession {
    /// 仅 Editing 阶段、且本地 mtime 严格变新时返回 true。
    pub fn local_changed(&self, current_mtime: Option<Timestamp>) -> bool {
        if self.phase != EditPhase::Editing {
            return false;
        }
        match (self.local_mtime, current_mtime) {
            (Some(last), Some(now)) => now > last,
            _ => false,
        }
    }

    /// 远程 (size, mtime) 与下载时快照不一致即视为已改动。
    pub fn remote_diverged(&self, current: RemoteSnapshot) -> bool {
        current != self.remote_snapshot
    }
}

#[derive(Debug, Default)]
pub struct EditSessionStore {
    sessions: Vec<EditSession>,
    next_id: u64,
    next_check_id: u64,
}

impl EditSessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            next_id: 1,
            next_check_id: 1,
        }
    }

    pub fn next_id(&mut self) -> EditSessionId {
        let id = EditSessionId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Allocate the next unique edit-check id. UI/core is the sole check-ID
    /// allocator; the runtime and browsing actor only echo the id back. A
    /// separate counter from `next_id` keeps the two id spaces distinct even
    /// though they are different types.
    pub fn next_check_id(&mut self) -> EditCheckId {
        let id = EditCheckId(self.next_check_id);
        self.next_check_id += 1;
        id
    }

    pub fn register(&mut self, session: EditSession) -> EditSessionId {
        let id = session.id;
        self.sessions.push(session);
        id
    }

    pub fn get(&self, id: EditSessionId) -> Option<&EditSession> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: EditSessionId) -> Option<&mut EditSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn find_by_transfer(&self, transfer: TransferId) -> Option<&EditSession> {
        self.sessions
            .iter()
            .find(|s| s.active_transfer == Some(transfer))
    }

    pub fn find_by_temp_path(&self, path: &LocalPath) -> Option<&EditSession> {
        self.sessions.iter().find(|s| &s.local_temp_path == path)
    }

    /// Find an active edit session for `(connection_key, remote_path)`, used
    /// to dedup a re-edit of the same file on the same physical connection.
    /// Every stored session is in a live (non-terminal) [`EditPhase`] —
    /// `Downloading`, `Editing`, `UploadingBack`, or `RemoteConflict` —
    /// because failed downloads are removed rather than parked in a terminal
    /// phase. So matching on `(connection_key, remote_path)` alone never
    /// wrongly blocks a re-edit against a dead session.
    pub fn find_active(
        &self,
        connection_key: &ConnectionKey,
        remote_path: &RemotePath,
    ) -> Option<&EditSession> {
        self.sessions
            .iter()
            .find(|s| s.connection_key == *connection_key && &s.remote_path == remote_path)
    }

    pub fn remove(&mut self, id: EditSessionId) -> Option<EditSession> {
        let index = self.sessions.iter().position(|s| s.id == id)?;
        Some(self.sessions.remove(index))
    }

    /// Remove every edit session belonging to `tab_id`, returning them so the
    /// caller can delete their on-disk temp directories. Used when a tab or its
    /// window closes: the session can never make progress once its owning tab is
    /// gone, and leaving it registered would make [`find_active`] block a future
    /// re-edit of the same file forever.
    ///
    /// [`find_active`]: EditSessionStore::find_active
    pub fn remove_for_tab(&mut self, tab_id: TabId) -> Vec<EditSession> {
        let mut removed = Vec::new();
        let mut index = 0;
        while index < self.sessions.len() {
            if self.sessions[index].tab_id == tab_id {
                removed.push(self.sessions.remove(index));
            } else {
                index += 1;
            }
        }
        removed
    }

    /// Refresh the `session_epoch` of every edit session on `tab_id` to
    /// `session_epoch`. A session captures the tab's epoch when it is created,
    /// but a reconnect bumps the tab's epoch; without this the session's
    /// save-back would carry the stale epoch and the runtime would silently drop
    /// it (an epoch mismatch is filtered with no terminal event), stranding the
    /// session in `UploadingBack` forever. Called on every (re)connect after the
    /// tab's epoch is bumped, so a preserved edit survives a reconnect.
    pub fn update_epoch_for_tab(&mut self, tab_id: TabId, session_epoch: u64) {
        for session in self.sessions.iter_mut().filter(|s| s.tab_id == tab_id) {
            // A check in flight at reconnect time can never complete: the actor
            // that owned it is gone. Reset to `Editing` and clear the pending
            // check so the watcher rediscovers the save and issues a fresh
            // authoritative check against the new session. `local_mtime` is
            // preserved as the pre-save baseline so the unchanged save is
            // detected again. `UploadingBack` is left untouched: that phase is
            // owned by the transfer lifecycle, which handles its own reconnect.
            if session.phase == EditPhase::CheckingRemote {
                session.phase = EditPhase::Editing;
                session.pending_check_id = None;
                session.checking_local_mtime = None;
            }
            session.session_epoch = session_epoch;
        }
    }

    pub fn editing_sessions(&self) -> impl Iterator<Item = &EditSession> {
        self.sessions
            .iter()
            .filter(|s| s.phase == EditPhase::Editing)
    }

    /// `Editing`-phase sessions belonging to `tab_id`. Used when a directory
    /// listing (re)loads for a tab: only `Editing` sessions matter, because
    /// that is the phase in which the watcher actively compares the session
    /// baseline against the listing, so it is the phase whose baseline must be
    /// re-synced to absorb sub-second mtime drift. `RemoteConflict` sessions
    /// are deliberately excluded so a real, already-surfaced conflict is never
    /// masked by a refresh.
    pub fn editing_sessions_for_tab(&self, tab_id: TabId) -> impl Iterator<Item = &EditSession> {
        self.sessions
            .iter()
            .filter(move |s| s.phase == EditPhase::Editing && s.tab_id == tab_id)
    }

    pub fn conflict_sessions(&self) -> impl Iterator<Item = &EditSession> {
        self.sessions
            .iter()
            .filter(|s| s.phase == EditPhase::RemoteConflict)
    }

    /// `CheckingRemote`-phase sessions. Used by the edit watcher's lifecycle
    /// pass so a session whose temp file is deleted while an authoritative
    /// remote check is in flight is still reaped. These sessions never
    /// initiate a new check through the polling loop (duplicate dispatch is
    /// prevented by construction: `poll_edit_sessions` only dispatches for
    /// `Editing` sessions), so this iterator is purely for cleanup.
    pub fn checking_sessions(&self) -> impl Iterator<Item = &EditSession> {
        self.sessions
            .iter()
            .filter(|s| s.phase == EditPhase::CheckingRemote)
    }

    /// The distinct `tab_id`s across all registered sessions. Used after a
    /// window closes to find sessions whose owning tab no longer exists in any
    /// surviving window so they can be torn down.
    pub fn session_tab_ids(&self) -> Vec<TabId> {
        let mut ids: Vec<TabId> = Vec::new();
        for session in &self.sessions {
            if !ids.contains(&session.tab_id) {
                ids.push(session.tab_id);
            }
        }
        ids
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
            Self::LocalNetworkPermissionDenied => "local_network_permission_denied",
            Self::PermissionDenied => "permission_denied",
            Self::FileExists => "file_exists",
            Self::NotFound => "not_found",
            Self::UnsupportedSymlink => "unsupported_symlink",
            Self::MetadataPreservationFailed => "metadata_preservation_failed",
            Self::ChannelClosed => "channel_closed",
            Self::NetworkTimeout => "network_timeout",
            Self::NetworkError => "network_error",
            Self::ServerDisconnected => "server_disconnected",
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
        AppState, AuthCredential, AuthFingerprint, AuthMethod, AuthMethodKind, ConflictPolicy,
        ConflictRequest, ConflictRequestId, ConnectionKey, ConnectionPoolIdentity,
        ConnectionProfile, ConnectionSettings, ConnectionState, DisconnectReason, EditCheckId,
        EditPhase, EditSessionId, EditSessionStore, HostKeyMismatch, HostKeyPrompt, LocalPath,
        MetadataPolicy, ModalRequest, ModalRequestId, ProfileId, RemoteEditSnapshotCheckFailed,
        RemoteEditSnapshotChecked, RemoteEditSnapshotDispatchFailed, RemoteEventScope, RemotePath,
        RemoteScoped, RuntimeBridgeConfig, SecretRef, SessionId, TabId, TabState, Timestamp,
        TransferDirection, TransferEndpoint, TransferId, TransferJob, TransferPlan, TransferPlanId,
        TransferPlanState, TransferState, TransferStore, TrustRequest, TrustRequestId,
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
    fn connection_key_uses_explicit_non_secret_identity() {
        let settings = ConnectionSettings {
            host: "example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "never-hashed".into(),
            },
        };

        let first = ConnectionKey::new(&settings, ConnectionPoolIdentity::Ephemeral(SessionId(1)));
        let second = ConnectionKey::new(&settings, ConnectionPoolIdentity::Ephemeral(SessionId(2)));

        assert_ne!(first, second);
        assert!(!format!("{first:?}").contains("never-hashed"));
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
            has_passphrase: true,
            passphrase_ref: Some(SecretRef::new("key-passphrase")),
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
    fn transfer_store_replay_does_not_duplicate_plans_jobs_or_conflicts() {
        let now = Timestamp::from_secs_since_epoch(10);
        let plan_id = TransferPlanId(1);
        let root_job = super::TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source")),
            state: super::TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: super::ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let plan = super::TransferPlan {
            id: plan_id,
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: super::TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: Some(0),
            child_jobs: Vec::new(),
            conflict_policy: super::ConflictPolicy::default(),
        };
        let started = AppEvent::TransferPlanStarted(super::TransferPlanSnapshot { plan, root_job });
        let child_job = super::TransferJob {
            id: TransferId(2),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source/a.txt")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source/a.txt")),
            state: super::TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: super::ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let progress = AppEvent::TransferPlanProgress(super::TransferPlanProgress {
            plan_id,
            child_jobs: vec![child_job.clone()],
            planned_count: 1,
            total_bytes: Some(4),
        });
        let conflict = AppEvent::TransferConflict(super::TransferConflictPrompt {
            request_id: ConflictRequestId(7),
            plan_id,
            transfer_id: child_job.id,
            source: child_job.source.clone(),
            destination: child_job.destination.clone(),
            source_size: Some(4),
            source_modified_at: Some(now),
        });

        let mut store = super::TransferStore::default();
        for event in [
            &started, &progress, &conflict, &started, &progress, &conflict,
        ] {
            store.apply_event(event, now);
        }

        assert_eq!(store.plans.len(), 1);
        assert_eq!(store.jobs.len(), 2);
        assert_eq!(store.plans[0].child_jobs, vec![TransferId(2)]);
        assert_eq!(store.pending_conflicts.len(), 1);
        assert!(matches!(
            store.jobs[1].state,
            super::TransferState::WaitingForConflictDecision { .. }
        ));
    }

    #[test]
    fn transfer_store_terminal_events_finalize_root_plan_idempotently() {
        let now = Timestamp::from_secs_since_epoch(10);
        let plan_id = TransferPlanId(1);
        let root_job = super::TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
            destination: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
            state: super::TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: super::ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let plan = super::TransferPlan {
            id: plan_id,
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: super::TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: Some(0),
            child_jobs: Vec::new(),
            conflict_policy: super::ConflictPolicy::default(),
        };
        let child_job = super::TransferJob {
            id: TransferId(2),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };
        let mut store = super::TransferStore::default();
        store.apply_event(
            &AppEvent::TransferPlanStarted(super::TransferPlanSnapshot { plan, root_job }),
            now,
        );
        store.apply_event(
            &AppEvent::TransferPlanProgress(super::TransferPlanProgress {
                plan_id,
                child_jobs: vec![child_job],
                planned_count: 1,
                total_bytes: Some(4),
            }),
            now,
        );
        store.apply_event(&AppEvent::TransferPlanCompleted { plan_id }, now);
        let completed = AppEvent::TransferCompleted {
            transfer_id: TransferId(2),
        };
        assert!(store.apply_event(&completed, now));
        assert!(!store.apply_event(&completed, now));

        assert_eq!(store.plans[0].state, super::TransferPlanState::Completed);
        assert_eq!(store.jobs[0].state, super::TransferState::Completed);
        assert_eq!(store.jobs[1].state, super::TransferState::Completed);
    }

    #[test]
    fn transfer_plan_failure_terminalizes_every_published_child() {
        let now = Timestamp::from_secs_since_epoch(10);
        let plan_id = TransferPlanId(1);
        let root_job = super::TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source")),
            state: super::TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: super::ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let plan = super::TransferPlan {
            id: plan_id,
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: super::TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: Some(0),
            child_jobs: Vec::new(),
            conflict_policy: super::ConflictPolicy::default(),
        };
        let child_a = super::TransferJob {
            id: TransferId(2),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };
        let child_b = super::TransferJob {
            id: TransferId(3),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };
        let error = super::UserFacingError::new(
            super::ErrorCode::Unknown,
            "Planning failed",
            "Could not read the directory.",
        )
        .with_retryable(true);

        let mut store = TransferStore::default();
        store.apply_event(
            &AppEvent::TransferPlanStarted(super::TransferPlanSnapshot {
                plan: plan.clone(),
                root_job: root_job.clone(),
            }),
            now,
        );
        store.apply_event(
            &AppEvent::TransferPlanProgress(super::TransferPlanProgress {
                plan_id,
                child_jobs: vec![child_a.clone(), child_b.clone()],
                planned_count: 2,
                total_bytes: Some(8),
            }),
            now,
        );

        let applied = store.apply_event(
            &AppEvent::TransferPlanFailed {
                plan_id,
                error: error.clone(),
            },
            now,
        );
        assert!(applied, "a terminal planning event must change state");

        assert_eq!(
            store.plans[0].state,
            super::TransferPlanState::Failed {
                error: error.clone()
            }
        );
        let expected_job = super::TransferState::Failed {
            retryable: error.retryable,
            error: error.clone(),
        };
        assert_eq!(store.jobs[0].state, expected_job, "root must be failed");
        assert_eq!(store.jobs[1].state, expected_job, "child A must be failed");
        assert_eq!(store.jobs[2].state, expected_job, "child B must be failed");

        assert!(
            !store.apply_event(
                &AppEvent::TransferPlanFailed {
                    plan_id,
                    error: error.clone()
                },
                now
            ),
            "replaying a terminal event must be a no-op"
        );
    }

    #[test]
    fn transfer_plan_cancellation_skips_every_published_child() {
        let now = Timestamp::from_secs_since_epoch(10);
        let plan_id = TransferPlanId(1);
        let root_job = super::TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source")),
            state: super::TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: super::ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let plan = super::TransferPlan {
            id: plan_id,
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: super::TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: Some(0),
            child_jobs: Vec::new(),
            conflict_policy: super::ConflictPolicy::default(),
        };
        let child_a = super::TransferJob {
            id: TransferId(2),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };
        let child_b = super::TransferJob {
            id: TransferId(3),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };

        let mut store = TransferStore::default();
        store.apply_event(
            &AppEvent::TransferPlanStarted(super::TransferPlanSnapshot {
                plan: plan.clone(),
                root_job: root_job.clone(),
            }),
            now,
        );
        store.apply_event(
            &AppEvent::TransferPlanProgress(super::TransferPlanProgress {
                plan_id,
                child_jobs: vec![child_a, child_b],
                planned_count: 2,
                total_bytes: Some(8),
            }),
            now,
        );

        let applied = store.apply_event(&AppEvent::TransferPlanCancelled { plan_id }, now);
        assert!(applied, "a terminal planning event must change state");

        assert_eq!(store.plans[0].state, super::TransferPlanState::Cancelled);
        let skipped = super::TransferState::Skipped;
        assert_eq!(store.jobs[0].state, skipped, "root must be skipped");
        assert_eq!(store.jobs[1].state, skipped, "child A must be skipped");
        assert_eq!(store.jobs[2].state, skipped, "child B must be skipped");

        assert!(
            !store.apply_event(&AppEvent::TransferPlanCancelled { plan_id }, now),
            "replaying a terminal event must be a no-op"
        );
    }

    #[test]
    fn transfer_plan_progress_after_terminal_event_is_ignored() {
        let now = Timestamp::from_secs_since_epoch(10);
        let plan_id = TransferPlanId(1);
        let root_job = super::TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source")),
            state: super::TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: super::ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let plan = super::TransferPlan {
            id: plan_id,
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: super::TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: Some(0),
            child_jobs: Vec::new(),
            conflict_policy: super::ConflictPolicy::default(),
        };
        let child_a = super::TransferJob {
            id: TransferId(2),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };
        let late_child = super::TransferJob {
            id: TransferId(99),
            state: super::TransferState::Queued,
            ..root_job.clone()
        };

        let mut store = TransferStore::default();
        store.apply_event(
            &AppEvent::TransferPlanStarted(super::TransferPlanSnapshot {
                plan: plan.clone(),
                root_job: root_job.clone(),
            }),
            now,
        );
        store.apply_event(
            &AppEvent::TransferPlanProgress(super::TransferPlanProgress {
                plan_id,
                child_jobs: vec![child_a],
                planned_count: 1,
                total_bytes: Some(4),
            }),
            now,
        );
        store.apply_event(
            &AppEvent::TransferPlanFailed {
                plan_id,
                error: super::UserFacingError::new(
                    super::ErrorCode::Unknown,
                    "Planning failed",
                    "stop",
                ),
            },
            now,
        );

        let late_changed = store.apply_event(
            &AppEvent::TransferPlanProgress(super::TransferPlanProgress {
                plan_id,
                child_jobs: vec![late_child],
                planned_count: 2,
                total_bytes: Some(8),
            }),
            now,
        );
        assert!(
            !late_changed,
            "late progress on a terminal plan must be ignored"
        );
        assert_eq!(store.plans[0].child_jobs, vec![TransferId(2)]);
        assert_eq!(store.jobs.len(), 2, "late child must not be inserted");
        assert_eq!(store.plans[0].planned_count, 1);
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
    fn remote_scope_extracts_from_host_key_mismatch() {
        let scope = RemoteEventScope::new(TabId(4), SessionId(6), 3);
        let event = AppEvent::HostKeyMismatch(HostKeyMismatch {
            scope: scope.clone(),
            host: "example.com".to_string(),
            port: 22,
            expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
            actual_fingerprint_sha256: "SHA256:actual".to_string(),
        });

        assert_eq!(event.remote_scope(), Some(scope));
        assert!(event.is_remote_scoped());
        assert!(!event.is_transfer_event());
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
    fn app_state_rejects_host_key_mismatch_from_old_session() {
        let mut state = AppState::new();
        state.tabs.open_tab(connected_tab(1, 11, 2));

        let event = AppEvent::HostKeyMismatch(HostKeyMismatch {
            scope: RemoteEventScope::new(TabId(1), SessionId(10), 1),
            host: "example.com".to_string(),
            port: 22,
            expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
            actual_fingerprint_sha256: "SHA256:actual".to_string(),
        });

        assert!(
            !state.should_accept_event(&event),
            "stale host key mismatch must be rejected"
        );
    }

    #[test]
    fn app_state_accepts_host_key_mismatch_from_current_session() {
        let mut state = AppState::new();
        state.tabs.open_tab(connected_tab(1, 11, 2));

        let event = AppEvent::HostKeyMismatch(HostKeyMismatch {
            scope: RemoteEventScope::new(TabId(1), SessionId(11), 2),
            host: "example.com".to_string(),
            port: 22,
            expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
            actual_fingerprint_sha256: "SHA256:actual".to_string(),
        });

        assert!(
            state.should_accept_event(&event),
            "current host key mismatch must be accepted"
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
                has_passphrase: true,
                passphrase_ref: Some(SecretRef::keychain_ref(ProfileId(2), "passphrase")),
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
                has_passphrase: false,
                passphrase_ref: None,
            }
        );
    }

    #[test]
    fn timestamp_serializes_as_unix_seconds() {
        let ts = Timestamp::from_secs_since_epoch(1_234_567_890);
        let json = serde_json::to_string(&ts).expect("serialize");
        assert_eq!(json, "1234567890");
        let restored: Timestamp = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, ts);
    }

    #[test]
    fn edit_phase_and_snapshot_construct() {
        let snap = crate::RemoteSnapshot {
            size: Some(10),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(5)),
        };
        assert_eq!(snap.size, Some(10));
        assert_eq!(crate::EditSessionId(3), crate::EditSessionId(3));
        let phase = crate::EditPhase::Editing;
        assert_eq!(phase, crate::EditPhase::Editing);
    }

    fn sample_edit_session(phase: crate::EditPhase) -> crate::EditSession {
        crate::EditSession {
            id: crate::EditSessionId(1),
            remote_path: crate::RemotePath::new("/srv/a.txt"),
            tab_id: crate::TabId(1),
            session_epoch: 1,
            profile_id: crate::ProfileId(1),
            connection_key: sample_connection_key(),
            local_temp_path: crate::LocalPath::new("/tmp/edits/1/a.txt"),
            phase,
            remote_snapshot: crate::RemoteSnapshot {
                size: Some(10),
                modified_at: Some(crate::Timestamp::from_secs_since_epoch(100)),
            },
            local_mtime: Some(crate::Timestamp::from_secs_since_epoch(200)),
            active_transfer: None,
            pending_check_id: None,
            checking_local_mtime: None,
            missing_ticks: 0,
        }
    }

    #[test]
    fn local_changed_only_in_editing_phase() {
        let newer = Some(crate::Timestamp::from_secs_since_epoch(300));
        assert!(sample_edit_session(crate::EditPhase::Editing).local_changed(newer));
        assert!(!sample_edit_session(crate::EditPhase::Downloading).local_changed(newer));
        assert!(!sample_edit_session(crate::EditPhase::UploadingBack).local_changed(newer));
    }

    #[test]
    fn local_changed_detects_newer_mtime() {
        let s = sample_edit_session(crate::EditPhase::Editing);
        assert!(s.local_changed(Some(crate::Timestamp::from_secs_since_epoch(300))));
        assert!(!s.local_changed(Some(crate::Timestamp::from_secs_since_epoch(200))));
        assert!(!s.local_changed(None));
    }

    #[test]
    fn remote_diverged_on_size_or_mtime_and_false_when_identical() {
        let s = sample_edit_session(crate::EditPhase::Editing);
        assert!(s.remote_diverged(crate::RemoteSnapshot {
            size: Some(11),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(100))
        }));
        assert!(s.remote_diverged(crate::RemoteSnapshot {
            size: Some(10),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(101))
        }));
        assert!(!s.remote_diverged(crate::RemoteSnapshot {
            size: Some(10),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(100))
        }));
    }

    #[test]
    fn timestamp_truncated_to_secs_drops_sub_second_component() {
        let sub_second = crate::Timestamp::from_system_time(
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_234_837),
        );
        let truncated = sub_second.truncated_to_secs();
        assert_eq!(truncated, crate::Timestamp::from_secs_since_epoch(1_234));
        // Already whole-second timestamps are unchanged.
        let whole = crate::Timestamp::from_secs_since_epoch(1_234);
        assert_eq!(whole.truncated_to_secs(), whole);
    }

    #[test]
    fn snapshot_agrees_at_second_granularity_absorbs_sub_second_noise() {
        let sub_second = crate::RemoteSnapshot {
            size: Some(20),
            modified_at: Some(crate::Timestamp::from_system_time(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_234_837),
            )),
        };
        let whole_second = crate::RemoteSnapshot {
            size: Some(20),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(1_234)),
        };
        // Same size + same whole second, differing only by sub-second noise.
        assert!(sub_second.agrees_at_second_granularity(whole_second));
        // truncated_to_secs makes them byte-for-byte equal.
        assert_eq!(sub_second.truncated_to_secs(), whole_second);
        // A different whole second is a real change, not noise.
        let next_second = crate::RemoteSnapshot {
            size: Some(20),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(1_235)),
        };
        assert!(!sub_second.agrees_at_second_granularity(next_second));
        // A different size is a real change even at the same second.
        let bigger = crate::RemoteSnapshot {
            size: Some(21),
            modified_at: Some(crate::Timestamp::from_secs_since_epoch(1_234)),
        };
        assert!(!whole_second.agrees_at_second_granularity(bigger));
    }

    #[test]
    fn editing_sessions_for_tab_excludes_other_tabs_and_non_editing() {
        let mut store = crate::EditSessionStore::new();
        let mut with = |tab: u64, phase: crate::EditPhase| {
            let id = store.next_id();
            let mut session = sample_edit_session(phase);
            session.id = id;
            session.tab_id = crate::TabId(tab);
            store.register(session);
            id
        };
        let editing_tab1 = with(1, crate::EditPhase::Editing);
        let _conflict_tab1 = with(1, crate::EditPhase::RemoteConflict);
        let _editing_tab2 = with(2, crate::EditPhase::Editing);

        let ids: Vec<_> = store
            .editing_sessions_for_tab(crate::TabId(1))
            .map(|s| s.id)
            .collect();
        assert_eq!(
            ids,
            vec![editing_tab1],
            "only the Editing session on tab 1 is yielded"
        );
    }

    #[test]
    fn remote_edit_snapshot_events_are_remote_scoped() {
        let scope = RemoteEventScope::new(TabId(1), SessionId(2), 3);
        let checked = AppEvent::RemoteEditSnapshotChecked(RemoteScoped::new(
            scope.clone(),
            RemoteEditSnapshotChecked {
                edit_session_id: EditSessionId(4),
                check_id: EditCheckId(5),
                path: RemotePath::new("/srv/a.txt"),
                snapshot: crate::RemoteSnapshot {
                    size: Some(10),
                    modified_at: Some(Timestamp::from_secs_since_epoch(100)),
                },
            },
        ));
        let failed = AppEvent::RemoteEditSnapshotCheckFailed(RemoteScoped::new(
            scope.clone(),
            RemoteEditSnapshotCheckFailed {
                edit_session_id: EditSessionId(4),
                check_id: EditCheckId(5),
                path: RemotePath::new("/srv/a.txt"),
                error: crate::UserFacingError::new(
                    crate::ErrorCode::Unknown,
                    "Could not check remote file",
                    "detail",
                ),
            },
        ));
        let dispatch_failed =
            AppEvent::RemoteEditSnapshotDispatchFailed(RemoteEditSnapshotDispatchFailed {
                tab_id: TabId(1),
                session_epoch: 3,
                edit_session_id: EditSessionId(4),
                check_id: EditCheckId(5),
                path: RemotePath::new("/srv/a.txt"),
                error: crate::UserFacingError::new(
                    crate::ErrorCode::Unknown,
                    "Could not check remote file",
                    "detail",
                ),
            });

        // The two actor outcomes carry the live scope and pass the stale guard.
        assert_eq!(checked.remote_scope(), Some(scope.clone()));
        assert!(checked.is_remote_scoped());
        assert_eq!(failed.remote_scope(), Some(scope.clone()));
        assert!(failed.is_remote_scoped());
        // The dispatch failure has no SessionId; it must not be treated as
        // remote-scoped (it is epoch-correlated instead).
        assert_eq!(dispatch_failed.remote_scope(), None);
        assert!(!dispatch_failed.is_remote_scoped());
        // None of the three are transfer events.
        assert!(!checked.is_transfer_event());
        assert!(!failed.is_transfer_event());
        assert!(!dispatch_failed.is_transfer_event());
    }

    #[test]
    fn store_find_active_treats_checking_remote_as_live() {
        let mut store = EditSessionStore::new();
        let mut session = sample_edit_session(EditPhase::CheckingRemote);
        session.id = store.next_id();
        store.register(session);
        // find_active matches on (connection_key, remote_path) alone and every
        // stored session is a live phase, so CheckingRemote must be returned
        // and block a re-edit of the same file.
        assert!(
            store
                .find_active(&sample_connection_key(), &RemotePath::new("/srv/a.txt"))
                .is_some(),
            "a CheckingRemote session is still an active edit"
        );
    }

    #[test]
    fn store_reconnect_resets_checking_remote_for_retry() {
        let mut store = EditSessionStore::new();
        let mut session = sample_edit_session(EditPhase::CheckingRemote);
        session.id = store.next_id();
        session.pending_check_id = Some(EditCheckId(9));
        session.checking_local_mtime = Some(Timestamp::from_secs_since_epoch(200));
        store.register(session.clone());

        store.update_epoch_for_tab(crate::TabId(1), 2);

        let stored = store
            .find_active(&sample_connection_key(), &RemotePath::new("/srv/a.txt"))
            .expect("session survives reconnect");
        assert_eq!(
            stored.phase,
            EditPhase::Editing,
            "a CheckingRemote session reverts to Editing on reconnect"
        );
        assert_eq!(
            stored.pending_check_id, None,
            "pending check id is cleared on reconnect"
        );
        assert_eq!(
            stored.checking_local_mtime, None,
            "checking local mtime is cleared on reconnect"
        );
        assert_eq!(stored.session_epoch, 2, "epoch is bumped on reconnect");
        // local_mtime baseline is preserved so the unchanged save is re-flagged.
        assert_eq!(
            stored.local_mtime,
            Some(Timestamp::from_secs_since_epoch(200)),
            "pre-save baseline is preserved"
        );
    }

    /// The connection identity every edit-session fixture shares; tests that
    /// exercise dedup build variants of it (different host or session).
    fn sample_connection_key() -> ConnectionKey {
        ConnectionKey::new(
            &ConnectionSettings {
                host: "srv.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "unused".into(),
                },
            },
            ConnectionPoolIdentity::Saved(AuthFingerprint {
                method: AuthMethodKind::Password,
                secret_ref: Some(SecretRef::keychain_ref(ProfileId(1), "password")),
                private_key_path_hash: None,
                profile_revision: 1,
            }),
        )
    }

    fn store_session(
        id_hint: u64,
        profile: u64,
        path: &str,
        phase: crate::EditPhase,
        transfer: Option<crate::TransferId>,
    ) -> crate::EditSession {
        let mut session = sample_edit_session(phase);
        session.id = crate::EditSessionId(id_hint);
        session.remote_path = crate::RemotePath::new(path);
        session.profile_id = crate::ProfileId(profile);
        session.local_temp_path = crate::LocalPath::new(format!("/tmp/edits/{id_hint}"));
        session.active_transfer = transfer;
        session
    }

    #[test]
    fn store_register_find_and_remove() {
        let mut store = crate::EditSessionStore::new();
        let id = store.next_id();
        let mut s = store_session(
            id.0,
            1,
            "/srv/a.txt",
            crate::EditPhase::Downloading,
            Some(crate::TransferId(7)),
        );
        s.id = id;
        store.register(s);
        assert!(store.find_by_transfer(crate::TransferId(7)).is_some());
        assert!(
            store
                .find_by_temp_path(&crate::LocalPath::new(format!("/tmp/edits/{}", id.0)))
                .is_some()
        );
        assert!(
            store
                .find_active(&sample_connection_key(), &RemotePath::new("/srv/a.txt"))
                .is_some()
        );
        assert!(store.get(id).is_some());
        assert!(store.remove(id).is_some());
        assert!(store.get(id).is_none());
        assert!(store.find_by_transfer(crate::TransferId(7)).is_none());
    }

    #[test]
    fn store_dedup_by_connection_key_and_remote_path() {
        let mut store = crate::EditSessionStore::new();
        let id1 = store.next_id();
        let mut s1 = store_session(id1.0, 1, "/srv/a.txt", crate::EditPhase::Editing, None);
        s1.id = id1;
        store.register(s1);
        // 同 connection+path 已有活跃会话可被查到；不同 path 或不同物理连接
        // （另一台服务器 / 另一条手工连接）查不到。
        assert!(
            store
                .find_active(&sample_connection_key(), &RemotePath::new("/srv/a.txt"))
                .is_some()
        );
        assert!(
            store
                .find_active(&sample_connection_key(), &RemotePath::new("/srv/b.txt"))
                .is_none()
        );
        // Same profile record re-pointed at another host: the old session must
        // not be hit (its key carries the old host).
        let other_host = ConnectionKey::new(
            &ConnectionSettings {
                host: "other.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "unused".into(),
                },
            },
            ConnectionPoolIdentity::Ephemeral(SessionId(9)),
        );
        assert!(
            store
                .find_active(&other_host, &RemotePath::new("/srv/a.txt"))
                .is_none(),
            "a different physical connection must not reuse another connection's edit"
        );
        // Two manual connections to two servers share ProfileId(0), so only the
        // Ephemeral(SessionId) half of the key can tell them apart.
        let manual_a = ConnectionKey::new(
            &ConnectionSettings {
                host: "a.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "unused".into(),
                },
            },
            ConnectionPoolIdentity::Ephemeral(SessionId(1)),
        );
        let manual_b = ConnectionKey::new(
            &ConnectionSettings {
                host: "b.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "unused".into(),
                },
            },
            ConnectionPoolIdentity::Ephemeral(SessionId(2)),
        );
        let id2 = store.next_id();
        let mut s2 = store_session(id2.0, 0, "/srv/a.txt", crate::EditPhase::Editing, None);
        s2.id = id2;
        s2.connection_key = manual_a.clone();
        store.register(s2);
        assert!(
            store
                .find_active(&manual_a, &RemotePath::new("/srv/a.txt"))
                .is_some()
        );
        assert!(
            store
                .find_active(&manual_b, &RemotePath::new("/srv/a.txt"))
                .is_none(),
            "two manual connections to different servers must not share an edit"
        );
    }

    #[test]
    fn store_find_active_matches_live_phases_only() {
        // Every stored phase is live, so `find_active` must return a session in
        // any of them (blocking a duplicate edit)...
        for phase in [
            crate::EditPhase::Downloading,
            crate::EditPhase::Editing,
            crate::EditPhase::UploadingBack,
            crate::EditPhase::RemoteConflict,
        ] {
            let mut store = crate::EditSessionStore::new();
            let id = store.next_id();
            let mut s = store_session(id.0, 1, "/srv/a.txt", phase.clone(), None);
            s.id = id;
            store.register(s);
            assert!(
                store
                    .find_active(&sample_connection_key(), &RemotePath::new("/srv/a.txt"))
                    .is_some(),
                "a live {phase:?} session must block a re-edit"
            );
            // ...and once a (e.g. failed) session is removed, the same file is
            // free to be edited again.
            store.remove(id);
            assert!(
                store
                    .find_active(&sample_connection_key(), &RemotePath::new("/srv/a.txt"))
                    .is_none(),
                "removing the session must unblock re-editing (was {phase:?})"
            );
        }
    }

    #[test]
    fn store_editing_sessions_filters_phase() {
        let mut store = crate::EditSessionStore::new();
        let a = store.next_id();
        let mut sa = store_session(a.0, 1, "/a", crate::EditPhase::Editing, None);
        sa.id = a;
        store.register(sa);
        let b = store.next_id();
        let mut sb = store_session(b.0, 1, "/b", crate::EditPhase::Downloading, None);
        sb.id = b;
        store.register(sb);
        assert_eq!(store.editing_sessions().count(), 1);
    }

    #[test]
    fn store_conflict_sessions_filters_phase() {
        let mut store = crate::EditSessionStore::new();
        let a = store.next_id();
        let mut sa = store_session(a.0, 1, "/a", crate::EditPhase::RemoteConflict, None);
        sa.id = a;
        store.register(sa);
        let b = store.next_id();
        let mut sb = store_session(b.0, 1, "/b", crate::EditPhase::Editing, None);
        sb.id = b;
        store.register(sb);
        assert_eq!(store.conflict_sessions().count(), 1);
    }

    /// Register a session on an explicit tab so tab-scoped store operations can
    /// be exercised. `store_session` alone always uses `TabId(1)`.
    fn store_session_on_tab(
        store: &mut crate::EditSessionStore,
        tab: u64,
        path: &str,
        epoch: u64,
    ) -> crate::EditSessionId {
        let id = store.next_id();
        let mut session = store_session(id.0, 1, path, crate::EditPhase::Editing, None);
        session.id = id;
        session.tab_id = crate::TabId(tab);
        session.session_epoch = epoch;
        store.register(session);
        id
    }

    #[test]
    fn store_remove_for_tab_removes_only_that_tab_and_returns_them() {
        let mut store = crate::EditSessionStore::new();
        let a = store_session_on_tab(&mut store, 1, "/srv/a.txt", 1);
        let b = store_session_on_tab(&mut store, 1, "/srv/b.txt", 1);
        let other = store_session_on_tab(&mut store, 2, "/srv/c.txt", 1);

        let removed = store.remove_for_tab(crate::TabId(1));

        let removed_ids: std::collections::HashSet<_> = removed.iter().map(|s| s.id).collect();
        assert_eq!(removed_ids, std::collections::HashSet::from([a, b]));
        assert!(store.get(a).is_none(), "tab-1 session a removed");
        assert!(store.get(b).is_none(), "tab-1 session b removed");
        assert!(store.get(other).is_some(), "tab-2 session untouched");
    }

    #[test]
    fn store_update_epoch_for_tab_refreshes_only_that_tab() {
        let mut store = crate::EditSessionStore::new();
        let a = store_session_on_tab(&mut store, 1, "/srv/a.txt", 1);
        let other = store_session_on_tab(&mut store, 2, "/srv/c.txt", 1);

        store.update_epoch_for_tab(crate::TabId(1), 5);

        assert_eq!(
            store.get(a).expect("session a survives").session_epoch,
            5,
            "tab-1 session epoch refreshed to the reconnect epoch"
        );
        assert_eq!(
            store.get(other).expect("session c survives").session_epoch,
            1,
            "tab-2 session epoch left untouched"
        );
    }

    #[test]
    fn clear_terminal_removes_completed_jobs_and_orphaned_plans() {
        let mut store = TransferStore {
            jobs: vec![
                TransferJob {
                    id: TransferId(1),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
                    state: TransferState::Completed,
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
                TransferJob {
                    id: TransferId(2),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/b.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/b.txt")),
                    state: TransferState::Running {
                        bytes_done: 512,
                        bytes_total: Some(1024),
                        started_at: Timestamp::from_secs_since_epoch(0),
                    },
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
            ],
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(1),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferPlanState::Completed,
                planned_count: 1,
                total_bytes: Some(10),
                child_jobs: vec![TransferId(1)],
                conflict_policy: ConflictPolicy::Ask,
            }],
            pending_conflicts: Vec::new(),
        };

        let changed = store.clear_terminal();
        assert!(changed);
        assert_eq!(store.jobs.len(), 1);
        assert_eq!(store.jobs[0].id, TransferId(2));
        assert_eq!(
            store.plans.len(),
            0,
            "orphaned plan should be removed with its last job"
        );
    }

    #[test]
    fn clear_terminal_preserves_plan_with_remaining_jobs() {
        let mut store = TransferStore {
            jobs: vec![
                TransferJob {
                    id: TransferId(1),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
                    state: TransferState::Completed,
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
                TransferJob {
                    id: TransferId(2),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/b.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/b.txt")),
                    state: TransferState::Running {
                        bytes_done: 0,
                        bytes_total: Some(1024),
                        started_at: Timestamp::from_secs_since_epoch(0),
                    },
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
            ],
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(9),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferPlanState::Queued,
                planned_count: 2,
                total_bytes: Some(20),
                child_jobs: vec![TransferId(1), TransferId(2)],
                conflict_policy: ConflictPolicy::Ask,
            }],
            pending_conflicts: Vec::new(),
        };

        store.clear_terminal();
        assert_eq!(store.jobs.len(), 1);
        assert_eq!(store.plans.len(), 1, "plan kept because job 2 still exists");
    }

    #[test]
    fn clear_terminal_returns_false_when_nothing_to_clear() {
        let mut store = TransferStore {
            jobs: vec![TransferJob {
                id: TransferId(1),
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::Ask,
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(0),
            }],
            plans: Vec::new(),
            pending_conflicts: Vec::new(),
        };

        assert!(!store.clear_terminal());
    }

    #[test]
    fn local_read_guard_rejects_stale_epochs_and_paths() {
        let mut tab = TabState::new(TabId(1), "t");
        let path_a = LocalPath::new("/tmp/a");
        let path_b = LocalPath::new("/tmp/b");

        tab.local.path = Some(path_a.clone());
        let first = tab.local.begin_local_read();

        // Matching epoch + matching path applies.
        assert!(tab.local.accepts_local_result(first, &path_a));

        // A newer request invalidates the in-flight result.
        let second = tab.local.begin_local_read();
        assert!(!tab.local.accepts_local_result(first, &path_a));

        // Same epoch but navigated elsewhere does not apply.
        tab.local.path = Some(path_b.clone());
        assert!(!tab.local.accepts_local_result(second, &path_a));
    }

    #[test]
    fn local_read_epoch_dies_with_the_tab() {
        // The guard lives on TabState, so closing a tab cannot leave a stale
        // side-table entry behind for a future tab to collide with.
        let mut store = AppState::default().tabs;
        store.open_tab(TabState::new(TabId(1), "one"));
        let tab_id = store.active_tab_id.expect("tab must exist");
        let epoch = store
            .find_tab_mut(tab_id)
            .expect("tab must exist")
            .local
            .begin_local_read();
        assert_eq!(epoch, 1);

        store.close_tab(tab_id);
        store.open_tab(TabState::new(TabId(2), "two"));
        assert_ne!(
            store.active_tab_id,
            Some(tab_id),
            "new tab must not reuse the closed id"
        );
    }

    #[test]
    fn pane_nav_history_push_back_forward_and_trim() {
        use super::PaneNavHistory as History;

        let mut history = History::default();
        history.push_navigating_from(Some("/a"), "/b");
        assert_eq!(history.back, vec!["/a".to_string()]);
        assert!(history.forward.is_empty());

        // Same-path navigation records nothing.
        history.push_navigating_from(Some("/b"), "/b");
        assert_eq!(history.back.len(), 1);

        // Missing current path records nothing.
        history.push_navigating_from(None, "/z");
        assert_eq!(history.back.len(), 1);

        // Back on an empty stack returns nothing and records no forward.
        let mut empty = History::default();
        assert_eq!(empty.go_back("/here"), None);
        assert!(empty.forward.is_empty());

        assert_eq!(history.go_back("/b"), Some("/a".to_string()));
        assert_eq!(history.forward, vec!["/b".to_string()]);
        assert_eq!(history.go_forward("/a"), Some("/b".to_string()));
        assert!(history.back.contains(&"/a".to_string()));

        // A fresh push clears forward.
        history.push_navigating_from(Some("/b"), "/c");
        assert!(history.forward.is_empty());

        // Back stack trims to MAX.
        for i in 0..(History::MAX + 5) {
            history.push_navigating_from(Some(&format!("/{i}")), &format!("/{}", i + 1));
        }
        assert_eq!(history.back.len(), History::MAX);
    }
}
