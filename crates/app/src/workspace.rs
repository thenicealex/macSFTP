use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, Hsla, IntoElement,
    KeyDownEvent, ParentElement, Render, ScrollStrategy, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use macsftp_core::{
    AppCommand, AppEvent, AppState, AuthCredential, AuthMethod, AuthMethodKind,
    CommandDispatchError, ConflictDecision, ConflictDecisionCommand, ConflictRequest,
    ConflictRequestId, ConnectCommand, ConnectionProfile, ConnectionSettings, ConnectionState,
    DisconnectReason, EntryPath, ErrorCode, FileKind, HostKeyDecisionCommand, HostKeyPrompt,
    LocalPath, ModalRequest, ModalRequestId, ProfileId, RemotePath, SecretRef, TabId, TabState,
    Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferHistoryId,
    TransferHistoryRecord, TransferHistoryStatus, TransferJob, TransferState, TrustRequestId,
    UserFacingError, history_status_for_plan, sort_entries,
};
use macsftp_platform::{AppPaths, read_local_directory};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::{
    AppearancePreference, ConfigStore, KeychainError, KeychainStore, ProfileStore,
    ResidualTempStore, TransferHistoryStore,
};
use macsftp_ui::{
    ActiveTheme, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel, Theme,
    TransferRow, empty_state, file_row, file_table_header, format_size, format_timestamp, icon,
    icon_button, tab, text_button, text_field, text_tooltip, transfer_row,
};
use tracing::{debug, warn};

use crate::app_actions::{
    ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, CopyPath, CopyVersionInfo,
    DownloadSelection, FocusLocalPane, FocusRemotePane, MinimizeWindow, NewTab, OpenLogFolder,
    OpenSelectedEntry, OpenSettings, ParentDirectory, ReconnectTab, RefreshPane, SelectNextEntry,
    SelectPrevEntry, ShowAbout, ShowTransferDrawer, UploadSelection, ZoomWindow,
};

/// Which file pane an action targets. Local is the default focus side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSide {
    Local,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceSurface {
    Files,
    Settings,
}

/// One field of the connect form, in Tab-cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectField {
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
    ProfileName,
}

/// Connect form state. Secrets live in the input fields only while the
/// form is open; submitting moves them into a zeroized
/// `ConnectionSettings` and drops the form.
struct ConnectForm {
    host: InputState,
    port: InputState,
    username: InputState,
    auth_method: AuthMethodKind,
    password: InputState,
    key_path: InputState,
    passphrase: InputState,
    focused_field: ConnectField,
    error: Option<SharedString>,
    /// Profile this form was prefilled from, if any. Carried into the
    /// connect command so the tab and the saved profile stay linked, and
    /// so "Save profile" updates the same entry instead of duplicating.
    source_profile_id: Option<ProfileId>,
    /// Name entered for saving the current connection as a profile.
    profile_name: InputState,
}

impl ConnectForm {
    fn empty() -> Self {
        Self {
            host: InputState::new(),
            port: InputState::with_value("22"),
            username: InputState::new(),
            auth_method: AuthMethodKind::Password,
            password: InputState::new(),
            key_path: InputState::new(),
            passphrase: InputState::new(),
            focused_field: ConnectField::Host,
            error: None,
            source_profile_id: None,
            profile_name: InputState::new(),
        }
    }

    fn prefilled(settings: &ConnectionSettings) -> Self {
        let mut form = Self::empty();
        form.host = InputState::with_value(settings.host.clone());
        form.port = InputState::with_value(settings.port.to_string());
        form.username = InputState::with_value(settings.username.clone());
        if let AuthCredential::PrivateKey { key_path, .. } = &settings.auth {
            form.auth_method = AuthMethodKind::PrivateKey;
            form.key_path = InputState::with_value(key_path.clone());
        }
        // Secrets are deliberately not prefilled.
        form
    }

    /// Prefill the non-secret fields from a saved profile. The secret
    /// itself is restored separately by `use_profile`, which reads it
    /// back from the Keychain (plan §11).
    fn from_profile(profile: &ConnectionProfile) -> Self {
        let mut form = Self::empty();
        form.host = InputState::with_value(profile.host.clone());
        form.port = InputState::with_value(profile.port.to_string());
        form.username = InputState::with_value(profile.username.clone());
        form.source_profile_id = Some(profile.id);
        match &profile.auth {
            AuthMethod::Password { .. } => {
                form.auth_method = AuthMethodKind::Password;
            }
            AuthMethod::PrivateKey { key_path, .. } => {
                form.auth_method = AuthMethodKind::PrivateKey;
                form.key_path = InputState::with_value(key_path.as_str().to_string());
            }
        }
        form
    }

    fn field_order(&self) -> &'static [ConnectField] {
        match self.auth_method {
            AuthMethodKind::Password => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::Password,
                ConnectField::ProfileName,
            ],
            AuthMethodKind::PrivateKey => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::KeyPath,
                ConnectField::Passphrase,
                ConnectField::ProfileName,
            ],
        }
    }

    fn field_state_mut(&mut self, field: ConnectField) -> &mut InputState {
        match field {
            ConnectField::Host => &mut self.host,
            ConnectField::Port => &mut self.port,
            ConnectField::Username => &mut self.username,
            ConnectField::Password => &mut self.password,
            ConnectField::KeyPath => &mut self.key_path,
            ConnectField::Passphrase => &mut self.passphrase,
            ConnectField::ProfileName => &mut self.profile_name,
        }
    }

    fn cycle_focus(&mut self, backwards: bool) {
        let order = self.field_order();
        let current_index = order
            .iter()
            .position(|field| *field == self.focused_field)
            .unwrap_or(0);
        let field_count = order.len() as isize;
        let offset: isize = if backwards { -1 } else { 1 };
        let next_index = (current_index as isize + offset).rem_euclid(field_count) as usize;
        self.focused_field = order[next_index];
    }

    fn set_auth_method(&mut self, method: AuthMethodKind) {
        self.auth_method = method;
        if !self.field_order().contains(&self.focused_field) {
            self.focused_field = match method {
                AuthMethodKind::Password => ConnectField::Password,
                AuthMethodKind::PrivateKey => ConnectField::KeyPath,
            };
        }
    }

    /// Validate and build the settings. Returns `Err` with a short,
    /// user-facing message.
    fn build_settings(&self) -> Result<ConnectionSettings, SharedString> {
        let host = self.host.value().trim().to_string();
        if host.is_empty() {
            return Err("Host is required.".into());
        }
        let port_text = self.port.value().trim();
        let port: u16 = if port_text.is_empty() {
            22
        } else {
            match port_text.parse() {
                Ok(port) if port > 0 => port,
                _ => return Err("Port must be a number between 1 and 65535.".into()),
            }
        };
        let username = self.username.value().trim().to_string();
        if username.is_empty() {
            return Err("Username is required.".into());
        }

        let auth = match self.auth_method {
            AuthMethodKind::Password => AuthCredential::Password {
                password: self.password.value().to_string(),
            },
            AuthMethodKind::PrivateKey => {
                let key_path = expand_home(self.key_path.value().trim());
                if key_path.is_empty() {
                    return Err("Private key path is required.".into());
                }
                let passphrase = self.passphrase.value();
                AuthCredential::PrivateKey {
                    key_path,
                    passphrase: if passphrase.is_empty() {
                        None
                    } else {
                        Some(passphrase.to_string())
                    },
                }
            }
        };

        Ok(ConnectionSettings {
            host,
            port,
            username,
            auth,
        })
    }
}

fn connection_in_flight(connection: &ConnectionState) -> bool {
    matches!(
        connection,
        ConnectionState::Connecting { .. }
            | ConnectionState::AwaitingHostKey { .. }
            | ConnectionState::AwaitingCredentials { .. }
            | ConnectionState::Reconnecting { .. }
    )
}

/// Expand a leading `~/` to the user's home directory.
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{}/{rest}", home.trim_end_matches('/'));
    }
    path.to_string()
}

/// `SecretRef`s a fresh save would create for `profile_id` given the
/// in-form `auth`. Mirrors `SecretRef::keychain_ref` (plan §11).
fn secret_refs_for_settings(profile_id: ProfileId, auth: &AuthCredential) -> Vec<SecretRef> {
    match auth {
        AuthCredential::Password { .. } => vec![SecretRef::keychain_ref(profile_id, "password")],
        AuthCredential::PrivateKey { passphrase, .. } => match passphrase {
            Some(_) => vec![SecretRef::keychain_ref(profile_id, "passphrase")],
            None => Vec::new(),
        },
    }
}

/// `SecretRef`s already stored for a saved profile's `auth`.
fn secret_refs_for_profile(auth: &AuthMethod) -> Vec<SecretRef> {
    match auth {
        AuthMethod::Password { secret_ref } => vec![secret_ref.clone()],
        AuthMethod::PrivateKey { passphrase_ref, .. } => match passphrase_ref {
            Some(secret_ref) => vec![secret_ref.clone()],
            None => Vec::new(),
        },
    }
}

fn connected_transfer_session(tab: &TabState) -> Option<(u64, ProfileId)> {
    match tab.connection {
        ConnectionState::Connected { session_epoch, .. } => {
            Some((session_epoch, tab.profile_id.unwrap_or(ProfileId(0))))
        }
        _ => None,
    }
}

fn append_remote_name(directory: &RemotePath, source: &LocalPath) -> Option<RemotePath> {
    let name = Path::new(source.as_str()).file_name()?.to_string_lossy();
    Some(RemotePath::new(format!(
        "{}/{}",
        directory.as_str().trim_end_matches('/'),
        name
    )))
}

fn append_local_name(directory: &LocalPath, source: &RemotePath) -> Option<LocalPath> {
    let name = Path::new(source.as_str()).file_name()?.to_string_lossy();
    Some(LocalPath::new(format!(
        "{}/{}",
        directory.as_str().trim_end_matches('/'),
        name
    )))
}

/// Lightweight ghost rendered under the cursor while a file row is being
/// dragged between the local and remote panes.
struct DragPreview {
    label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.colors.element_selected)
            .text_color(theme.colors.text)
            .shadow_md()
            .text_size(px(12.0))
            .child(self.label.clone())
    }
}

/// Root view: tab bar, local/remote panes, transfer drawer, status bar.
/// Long-lived business state lives in `core::AppState`; this view keeps
/// only UI-local state (focus handles, scroll handles, drawer flags).
pub struct Workspace {
    state: AppState,
    runtime_client: RuntimeClient,
    workspace_focus: FocusHandle,
    local_pane_focus: FocusHandle,
    remote_pane_focus: FocusHandle,
    modal_focus: FocusHandle,
    focused_side: PaneSide,
    surface: WorkspaceSurface,
    about_open: bool,
    drawer_open: bool,
    completed_section_expanded: bool,
    failed_section_expanded: bool,
    local_scroll: UniformListScrollHandle,
    remote_scroll: UniformListScrollHandle,
    default_local_path: LocalPath,
    status_message: Option<SharedString>,
    config_error: Option<SharedString>,
    config: ConfigStore,
    log_file: LocalPath,
    connect_form: Option<ConnectForm>,
    connect_form_focus: FocusHandle,
    /// Draft name for the active transfer-conflict rename decision.
    /// It is view-local because the runtime only receives a decision
    /// after the user submits it.
    conflict_rename: InputState,
    conflict_rename_error: Option<SharedString>,
    /// Session credentials per tab, kept in memory only so Reconnect
    /// works without re-typing. Replaced by Keychain-backed profiles.
    tab_settings: HashMap<TabId, ConnectionSettings>,
    /// Disk-backed saved-connection profiles. Loaded at startup; the
    /// connect dialog reads and writes through this store.
    profiles: ProfileStore,
    /// Credential store (macOS Keychain). Profile secrets are written
    /// here; `profiles.json` only ever holds the `SecretRef` handle.
    keychain: KeychainStore,
    /// Persisted transfer history (plan §18). Loaded at startup so
    /// unfinished transfers from the last app close can be retried.
    transfer_history: TransferHistoryStore,
    /// Residual `.macsftp-part-*` temp files from a previous run,
    /// reconciled on launch (local) and on reconnect (remote). Plan M5/M6.
    residual_temps: ResidualTempStore,
    /// Guards against double-flushing history if the quit hook fires
    /// more than once.
    transfer_history_flushed: bool,
    _appearance_subscription: Subscription,
    /// Keeps the event drain task alive for the workspace's lifetime.
    _event_drain: Task<()>,
}

impl Workspace {
    pub fn new(
        runtime_client: RuntimeClient,
        event_receiver: EventReceiver,
        app_paths: AppPaths,
        config: ConfigStore,
        keychain: KeychainStore,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_local_path =
            LocalPath::new(std::env::var("HOME").unwrap_or_else(|_| "/Users/alex".to_string()));

        let state = AppState::new();
        let config_error = config
            .initial_error()
            .map(|_| SharedString::from("Could not load config.json; using defaults."));

        let appearance_subscription =
            cx.observe_window_appearance(window, |workspace, window, cx| {
                if workspace.config.config().appearance == AppearancePreference::System {
                    cx.set_global(Theme::for_appearance(window.appearance()));
                    cx.notify();
                }
            });

        // ADR-002: the drain task runs on the GPUI executor and only
        // awaits `recv()`; every event is applied inside an entity
        // update, followed by `cx.notify()` in the handler.
        let event_drain = cx.spawn_in(window, async move |workspace, cx| {
            while let Some(event) = event_receiver.recv().await {
                let applied = workspace.update_in(cx, |workspace, window, cx| {
                    workspace.handle_app_event(event, window, cx);
                });
                if applied.is_err() {
                    break; // workspace released — app is shutting down
                }
            }
        });

        let mut workspace = Self {
            state,
            runtime_client,
            workspace_focus: cx.focus_handle(),
            local_pane_focus: cx.focus_handle(),
            remote_pane_focus: cx.focus_handle(),
            modal_focus: cx.focus_handle(),
            focused_side: PaneSide::Local,
            surface: WorkspaceSurface::Files,
            about_open: false,
            drawer_open: true,
            completed_section_expanded: false,
            failed_section_expanded: false,
            local_scroll: UniformListScrollHandle::new(),
            remote_scroll: UniformListScrollHandle::new(),
            default_local_path,
            status_message: None,
            config_error,
            config,
            log_file: app_paths.log_file.clone(),
            connect_form: None,
            connect_form_focus: cx.focus_handle(),
            conflict_rename: InputState::new(),
            conflict_rename_error: None,
            tab_settings: HashMap::new(),
            profiles: ProfileStore::open_or_empty(app_paths.profiles_file.clone()),
            keychain,
            transfer_history: TransferHistoryStore::open_or_empty(
                app_paths.transfer_history_file.clone(),
            ),
            residual_temps: Self::reconcile_local_residual_temps(ResidualTempStore::open_or_empty(
                app_paths.residual_temp_file.clone(),
            )),
            transfer_history_flushed: false,
            _appearance_subscription: appearance_subscription,
            _event_drain: event_drain,
        };
        // Persist in-flight transfers to disk when the app quits (plan §18)
        // so they can be shown and retried on next launch.
        cx.on_app_quit(|workspace, _cx| {
            workspace.flush_transfer_history();
            async {}
        })
        .detach();
        workspace.open_new_tab(window, cx);
        workspace
    }

    fn active_tab(&self) -> Option<&TabState> {
        self.state.tabs.active_tab()
    }

    fn active_tab_mut(&mut self) -> Option<&mut TabState> {
        let active_tab_id = self.state.tabs.active_tab_id?;
        self.state.tabs.find_tab_mut(active_tab_id)
    }

    fn pane_focus(&self, side: PaneSide) -> &FocusHandle {
        match side {
            PaneSide::Local => &self.local_pane_focus,
            PaneSide::Remote => &self.remote_pane_focus,
        }
    }

    fn scroll_handle(&self, side: PaneSide) -> &UniformListScrollHandle {
        match side {
            PaneSide::Local => &self.local_scroll,
            PaneSide::Remote => &self.remote_scroll,
        }
    }

    // ── Tab lifecycle ──────────────────────────────────────────────

    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = self.state.tabs.next_tab_id();
        let mut tab = TabState::new(tab_id, "New Connection");
        if let Some(message) = Self::load_local_directory(&self.default_local_path, &mut tab) {
            self.status_message = Some(message.into());
        }
        self.state.tabs.open_tab(tab);
        self.state.tabs.active_tab_id = Some(tab_id);
        self.reset_scroll_positions();
        self.focus_pane(PaneSide::Local, window, cx);
        cx.notify();
    }

    fn close_tab_by_id(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.tabs.close_tab(tab_id).is_none() {
            return;
        }
        // Tell the runtime to cancel the tab's actor and reject its
        // pending trust requests; late events are dropped by the stale
        // event guard because the tab no longer exists.
        self.send_command(AppCommand::CloseTab { tab_id }, cx);
        // Modals bound to the closed tab's session are now stale; drop
        // them so their confirm buttons can never act (plan §7).
        self.state.drain_expired_modals();
        if self.state.tabs.tabs.is_empty() {
            window.focus(&self.workspace_focus);
        }
        self.reset_scroll_positions();
        cx.notify();
    }

    fn activate_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.state.tabs.find_tab(tab_id).is_some() {
            self.state.tabs.active_tab_id = Some(tab_id);
            self.reset_scroll_positions();
            cx.notify();
        }
    }

    fn activate_tab_in_direction(&mut self, offset: isize, cx: &mut Context<Self>) {
        let tabs = &self.state.tabs.tabs;
        if tabs.is_empty() {
            return;
        }
        let current_index = self
            .state
            .tabs
            .active_tab_id
            .and_then(|active_tab_id| tabs.iter().position(|tab| tab.id == active_tab_id))
            .unwrap_or(0);
        let tab_count = tabs.len() as isize;
        let next_index = (current_index as isize + offset).rem_euclid(tab_count) as usize;
        let next_tab_id = tabs[next_index].id;
        self.activate_tab(next_tab_id, cx);
    }

    fn reset_scroll_positions(&mut self) {
        self.local_scroll = UniformListScrollHandle::new();
        self.remote_scroll = UniformListScrollHandle::new();
    }

    // ── Runtime bridge: commands, connect flow, event handling ─────

    /// Non-blocking command dispatch (ADR-002). On backpressure the
    /// command is dropped and a lightweight warning is shown — the UI
    /// thread never blocks or retries in a loop.
    fn send_command(&mut self, command: AppCommand, cx: &mut Context<Self>) -> bool {
        match self.runtime_client.try_send(command) {
            Ok(()) => true,
            Err(CommandDispatchError::ChannelFull) => {
                self.status_message = Some("Runtime is busy — action dropped, try again".into());
                cx.notify();
                false
            }
            Err(CommandDispatchError::ChannelClosed) => {
                self.status_message = Some("Runtime is unavailable".into());
                cx.notify();
                false
            }
        }
    }

    /// Route a connect request: reuse this tab's session credentials
    /// when we have them, otherwise open the connect form.
    fn request_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if connection_in_flight(&tab.connection) {
            return;
        }
        let tab_id = tab.id;
        match self.tab_settings.get(&tab_id).cloned() {
            Some(settings) => self.connect_with(settings, tab.profile_id, cx),
            None => self.open_connect_form(window, cx),
        }
    }

    /// Start (or restart) the active tab's connection through the
    /// runtime bridge. Session identity is allocated here so every
    /// event from the new session matches the tab's epoch (plan §7).
    ///
    /// State is only mutated after the command is accepted; a failed
    /// send leaves the tab untouched.
    fn connect_with(
        &mut self,
        settings: ConnectionSettings,
        profile_id: Option<ProfileId>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if connection_in_flight(&tab.connection) {
            return;
        }
        let tab_id = tab.id;
        let next_epoch = tab.session_epoch + 1;
        let is_reconnect = matches!(tab.connection, ConnectionState::Connected { .. });

        let session_id = self.state.allocate_session_id();
        let sent = self.send_command(
            AppCommand::ConnectTab(ConnectCommand {
                tab_id,
                session_id,
                session_epoch: next_epoch,
                // Credentials are carried directly on the command; the
                // Keychain holds the persisted copy (plan §11). The
                // profile id links the tab to a saved entry when one
                // was used, so transfers and reconnects stay associated.
                profile_id: profile_id.unwrap_or(ProfileId(0)),
                settings: settings.clone(),
            }),
            cx,
        );
        if !sent {
            return;
        }

        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            tab.title = settings.host.clone();
            tab.profile_id = profile_id;
            if is_reconnect {
                tab.begin_reconnect(session_id);
            } else {
                tab.begin_connect(session_id);
            }
        }
        // Keep the credentials for this tab's lifetime so Reconnect
        // works without re-typing. The persisted copy lives in the
        // Keychain (plan §11); this in-memory copy is dropped with the
        // workspace.
        self.tab_settings.insert(tab_id, settings);
        // The epoch bump invalidates any modal from a previous session.
        self.state.drain_expired_modals();
        cx.notify();
    }

    // ── Connect form ───────────────────────────────────────────────

    fn open_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = self
            .state
            .tabs
            .active_tab_id
            .and_then(|tab_id| self.tab_settings.get(&tab_id))
            .map(ConnectForm::prefilled)
            .unwrap_or_else(ConnectForm::empty);
        self.connect_form = Some(form);
        window.focus(&self.connect_form_focus);
        cx.notify();
    }

    fn close_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_form = None;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    fn submit_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.connect_form else {
            return;
        };
        match form.build_settings() {
            Ok(settings) => {
                let profile_id = form.source_profile_id;
                self.connect_form = None;
                self.focus_pane(self.focused_side, window, cx);
                self.connect_with(settings, profile_id, cx);
            }
            Err(message) => {
                form.error = Some(message);
                cx.notify();
            }
        }
    }

    /// Allocate a fresh profile id: one past the current maximum so
    /// saved profiles never collide even after deletes.
    fn next_profile_id(&self) -> ProfileId {
        let max = self
            .profiles
            .profiles()
            .iter()
            .map(|profile| profile.id.0)
            .max()
            .unwrap_or(0);
        ProfileId(max + 1)
    }

    /// Prefill the form from a saved profile, including the secret read
    /// back from the Keychain (plan §11). If the secret is missing or the
    /// Keychain is unavailable, the form still opens with metadata filled
    /// and the user re-enters the secret — saving is never blocked.
    fn use_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.find_profile(profile_id).cloned() else {
            return;
        };
        let mut form = ConnectForm::from_profile(&profile);
        match &profile.auth {
            AuthMethod::Password { secret_ref } => match self.keychain.load(secret_ref) {
                Ok(Some(password)) => form.password.set_value(password),
                Ok(None) => {
                    self.status_message = Some("Saved password not found — re-enter it.".into());
                }
                Err(error) => {
                    warn!("could not load password from Keychain for {profile_id:?}: {error}");
                    self.status_message =
                        Some("Could not read saved password — re-enter it.".into());
                }
            },
            AuthMethod::PrivateKey { passphrase_ref, .. } => {
                if let Some(secret_ref) = passphrase_ref {
                    match self.keychain.load(secret_ref) {
                        Ok(Some(passphrase)) => form.passphrase.set_value(passphrase),
                        Ok(None) => {
                            self.status_message =
                                Some("Saved passphrase not found — re-enter it.".into());
                        }
                        Err(error) => {
                            warn!(
                                "could not load passphrase from Keychain for {profile_id:?}: {error}"
                            );
                            self.status_message =
                                Some("Could not read saved passphrase — re-enter it.".into());
                        }
                    }
                }
            }
        }
        self.connect_form = Some(form);
        cx.notify();
    }

    /// Persist the current form as a profile. If the form came from an
    /// existing profile, the same id is reused (update); otherwise a new
    /// id is allocated. The secret is mapped to a `SecretRef`, written to
    /// the macOS Keychain, and never written to disk (plan §5/§11). The
    /// profile is only flushed to `profiles.json` after the Keychain
    /// write succeeds, so the two never drift apart.
    fn save_current_profile(&mut self, cx: &mut Context<Self>) {
        let settings = match &self.connect_form {
            Some(form) => match form.build_settings() {
                Ok(settings) => settings,
                Err(message) => {
                    if let Some(form) = self.connect_form.as_mut() {
                        form.error = Some(message);
                    }
                    cx.notify();
                    return;
                }
            },
            None => return,
        };
        let name = self
            .connect_form
            .as_ref()
            .map(|form| form.profile_name.value().trim().to_string())
            .unwrap_or_default();
        let source_profile_id = self
            .connect_form
            .as_ref()
            .and_then(|form| form.source_profile_id);
        let name = if name.is_empty() {
            format!("{}@{}", settings.username, settings.host)
        } else {
            name
        };
        let profile_id = match source_profile_id {
            Some(id) if self.profiles.find_profile(id).is_some() => id,
            _ => self.next_profile_id(),
        };

        // When updating, the previous profile may have used a different
        // auth method; capture it so we can drop its now-orphaned
        // Keychain entries after storing the new ones.
        let previous = source_profile_id.and_then(|id| self.profiles.find_profile(id).cloned());

        // Write the secret to the Keychain first. If this fails we do not
        // persist the profile, so a disk entry never points at a missing
        // secret.
        if let Err(error) = self.store_profile_secrets(profile_id, &settings) {
            self.status_message =
                Some(format!("Could not save credentials to Keychain: {error}").into());
            cx.notify();
            return;
        }

        // Remove Keychain entries from the previous auth method that the
        // new profile no longer references.
        if let Some(previous) = &previous {
            for orphan in secret_refs_for_profile(&previous.auth) {
                let still_used = secret_refs_for_settings(profile_id, &settings.auth)
                    .iter()
                    .any(|current| current == &orphan);
                if !still_used {
                    let _ = self.keychain.delete(&orphan);
                }
            }
        }

        let profile =
            ConnectionProfile::from_connection_settings(profile_id, name.clone(), &settings);
        match self.profiles.save_profile(profile) {
            Ok(saved) => {
                if let Some(form) = self.connect_form.as_mut() {
                    form.source_profile_id = Some(saved.id);
                    form.profile_name = InputState::new();
                    form.error = None;
                }
                self.status_message = Some(format!("Saved profile '{}'.", name).into());
            }
            Err(error) => {
                // The Keychain now holds a secret with no disk profile.
                // Best-effort cleanup so we don't leave an orphan
                // credential behind.
                for secret_ref in secret_refs_for_settings(profile_id, &settings.auth) {
                    let _ = self.keychain.delete(&secret_ref);
                }
                self.status_message = Some(format!("Could not save profile: {error}").into());
            }
        }
        cx.notify();
    }

    /// Write the connection secret(s) for `profile_id` to the Keychain,
    /// mirroring `ConnectionProfile::from_connection_settings` (plan §11).
    fn store_profile_secrets(
        &self,
        profile_id: ProfileId,
        settings: &ConnectionSettings,
    ) -> Result<(), KeychainError> {
        match &settings.auth {
            AuthCredential::Password { password } => {
                let secret_ref = SecretRef::keychain_ref(profile_id, "password");
                self.keychain.store(&secret_ref, password)
            }
            AuthCredential::PrivateKey { passphrase, .. } => match passphrase {
                Some(passphrase) => {
                    let secret_ref = SecretRef::keychain_ref(profile_id, "passphrase");
                    self.keychain.store(&secret_ref, passphrase)
                }
                None => Ok(()),
            },
        }
    }

    /// Remove the Keychain entry (or entries) behind a saved profile.
    fn delete_profile_secrets(&self, profile: &ConnectionProfile) -> Result<(), KeychainError> {
        match &profile.auth {
            AuthMethod::Password { secret_ref } => self.keychain.delete(secret_ref),
            AuthMethod::PrivateKey { passphrase_ref, .. } => match passphrase_ref {
                Some(secret_ref) => self.keychain.delete(secret_ref),
                None => Ok(()),
            },
        }
    }

    /// Remove a saved profile from the store and disk, and best-effort
    /// remove its Keychain entries. Drops the link from any open form
    /// that pointed at it.
    fn delete_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        let previous = self.profiles.find_profile(profile_id).cloned();
        match self.profiles.delete_profile(profile_id) {
            Ok(()) => {
                if let Some(form) = self.connect_form.as_mut()
                    && form.source_profile_id == Some(profile_id)
                {
                    form.source_profile_id = None;
                }
                if let Some(profile) = previous
                    && let Err(error) = self.delete_profile_secrets(&profile)
                {
                    warn!(
                        "could not remove Keychain secret for deleted profile {profile_id:?}: {error}"
                    );
                }
                self.status_message = Some("Deleted profile.".into());
            }
            Err(error) => {
                self.status_message = Some(format!("Could not delete profile: {error}").into());
            }
        }
        cx.notify();
    }

    /// Route keys typed while the connect form has focus to the
    /// focused field. Escape is left to the `CancelActiveModal`
    /// binding; unhandled keys propagate to global bindings.
    fn handle_connect_form_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.connect_form else {
            return;
        };
        let keystroke = &event.keystroke;

        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.submit_connect_form(window, cx);
            return;
        }
        if keystroke.key == "tab" {
            form.cycle_focus(keystroke.modifiers.shift);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let focused_field = form.focused_field;
                form.field_state_mut(focused_field).insert(&text);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        let focused_field = form.focused_field;
        if form
            .field_state_mut(focused_field)
            .handle_keystroke(keystroke)
            == InputKeyResult::Handled
        {
            form.error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The host key prompt shown for the active tab, if any.
    fn active_host_key_prompt(&self) -> Option<&HostKeyPrompt> {
        let active_tab_id = self.state.tabs.active_tab_id?;
        self.state
            .modals
            .active
            .iter()
            .rev()
            .find_map(|modal| match modal {
                ModalRequest::HostKey(prompt) if prompt.tab_id == active_tab_id => Some(prompt),
                _ => None,
            })
    }

    fn active_transfer_conflict_prompt(&self) -> Option<&TransferConflictPrompt> {
        self.state
            .modals
            .active
            .iter()
            .rev()
            .find_map(|modal| match modal {
                ModalRequest::TransferConflict(prompt) => Some(prompt),
                _ => None,
            })
    }

    fn remove_modal(&mut self, request_id: TrustRequestId) {
        self.state
            .modals
            .active
            .retain(|modal| modal.request_id() != Some(ModalRequestId::Trust(request_id)));
    }

    fn remove_conflict_modal(&mut self, request_id: ConflictRequestId) {
        self.state
            .modals
            .active
            .retain(|modal| modal.request_id() != Some(ModalRequestId::Conflict(request_id)));
        self.state
            .transfers
            .pending_conflicts
            .retain(|conflict| conflict.id != request_id);
        self.conflict_rename_error = None;
        if let Some(default_name) = self
            .active_transfer_conflict_prompt()
            .map(|prompt| copy_name(&prompt.destination))
        {
            self.conflict_rename.set_value(default_name);
        } else {
            self.conflict_rename.clear();
        }
    }

    fn resolve_transfer_conflict(
        &mut self,
        prompt: &TransferConflictPrompt,
        decision: ConflictDecision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_conflict_modal(prompt.request_id);
        self.send_command(
            AppCommand::ResolveTransferConflict(ConflictDecisionCommand {
                plan_id: prompt.plan_id,
                request_id: prompt.request_id,
                decision,
            }),
            cx,
        );
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    fn submit_transfer_rename(
        &mut self,
        apply_to_all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.active_transfer_conflict_prompt().cloned() else {
            return;
        };
        let new_name = self.conflict_rename.value().trim().to_string();
        if new_name.is_empty() || new_name == "." || new_name == ".." || new_name.contains('/') {
            self.conflict_rename_error =
                Some("Enter a file name without path separators or parent-directory names.".into());
            cx.notify();
            return;
        }

        self.resolve_transfer_conflict(
            &prompt,
            ConflictDecision::Rename {
                new_name,
                apply_to_all,
            },
            window,
            cx,
        );
    }

    /// Route editing keys for the custom rename input. Escape remains
    /// handled by the global modal-cancel action so it always cancels
    /// the waiting transfer rather than discarding text silently.
    fn handle_transfer_conflict_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_transfer_conflict_prompt().is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.submit_transfer_rename(false, window, cx);
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.conflict_rename.insert(&text);
                self.conflict_rename_error = None;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.conflict_rename.handle_keystroke(keystroke) == InputKeyResult::Handled {
            self.conflict_rename_error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn accept_host_key(
        &mut self,
        request_id: TrustRequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_modal(request_id);
        self.send_command(
            AppCommand::AcceptHostKey(HostKeyDecisionCommand { request_id }),
            cx,
        );
        // Handshake continues; show Connecting until TabConnected lands.
        if let Some(tab) = self.active_tab_mut()
            && let ConnectionState::AwaitingHostKey {
                session_id,
                session_epoch,
                request_id: pending_request,
            } = tab.connection
            && pending_request == request_id
        {
            tab.connection = ConnectionState::Connecting {
                session_id,
                session_epoch,
            };
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    fn reject_host_key(
        &mut self,
        request_id: TrustRequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remove_modal(request_id);
        self.send_command(AppCommand::RejectHostKey { request_id }, cx);
        // The user declined — reflect it immediately. The actor's own
        // TabDisconnected for this session is dropped by the guard once
        // the state no longer matches, which is fine: same outcome.
        if let Some(tab) = self.active_tab_mut()
            && let ConnectionState::AwaitingHostKey {
                request_id: pending_request,
                ..
            } = tab.connection
            && pending_request == request_id
        {
            tab.disconnect(DisconnectReason::UserRequested);
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    fn cancel_active_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.about_open {
            self.about_open = false;
            cx.notify();
            return;
        }
        if self.surface == WorkspaceSurface::Settings {
            self.surface = WorkspaceSurface::Files;
            self.workspace_focus.focus(window);
            cx.notify();
            return;
        }
        if self.connect_form.is_some() {
            self.close_connect_form(window, cx);
            return;
        }
        if let Some(prompt) = self.active_host_key_prompt() {
            let request_id = prompt.request_id;
            self.reject_host_key(request_id, window, cx);
        } else if let Some(prompt) = self.active_transfer_conflict_prompt().cloned() {
            self.resolve_transfer_conflict(&prompt, ConflictDecision::CancelJob, window, cx);
        }
    }

    /// Apply one runtime event to the core state. Every event passes
    /// the centralized stale event guard first (plan §7 / M2b); stale
    /// events are dropped with a debug note and must not mutate state.
    fn handle_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.should_accept_event(&event) {
            // `AppEvent` carries no secrets by invariant (only host/port,
            // key fingerprints, paths, and user-facing errors), so logging
            // it at debug is safe per AGENTS.md §5.
            debug!(?event, "dropped stale event");
            return;
        }

        match event {
            AppEvent::TabConnecting { .. } => {
                // Already reflected optimistically when the command was
                // sent; nothing to update.
            }
            AppEvent::HostKeyUnknown(prompt) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(prompt.tab_id) {
                    tab.await_host_key(prompt.session_id, prompt.request_id);
                    self.state.modals.active.push(ModalRequest::HostKey(prompt));
                    window.focus(&self.modal_focus);
                }
            }
            AppEvent::HostKeyMismatch(mismatch) => {
                // Security block, no override (plan §10): the tab fails
                // with an explanation and the known_hosts file location.
                if let Some(tab) = self.state.tabs.find_tab_mut(mismatch.tab_id) {
                    let mut error = UserFacingError::new(
                        ErrorCode::HostKeyMismatch,
                        "Host key mismatch",
                        format!(
                            "The key presented by {}:{} does not match the stored \
                             key. This can indicate a man-in-the-middle attack. \
                             Connection blocked.",
                            mismatch.host, mismatch.port
                        ),
                    );
                    error.detail = Some(format!(
                        "Expected {}, got {}. Stored entries: \
                         ~/Library/Application Support/macSFTP/known_hosts and \
                         ~/.ssh/known_hosts.",
                        mismatch
                            .expected_fingerprint_sha256
                            .as_deref()
                            .unwrap_or("(unknown)"),
                        mismatch.actual_fingerprint_sha256
                    ));
                    tab.fail(error);
                }
            }
            AppEvent::TabConnected(scoped) => {
                let tab_id = scoped.scope.tab_id;
                let remote_root = scoped.payload.remote_root;
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                    tab.complete_connect(
                        scoped.scope.session_id,
                        macsftp_core::Timestamp(std::time::SystemTime::now()),
                    );
                    // Start from the canonical remote root. The actor will
                    // replace this empty first-load state with a real listing.
                    tab.remote.entries.clear();
                    tab.remote.path = Some(remote_root.clone());
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                }
                self.remote_scroll = UniformListScrollHandle::new();
                self.request_remote_directory(tab_id, remote_root, cx);
                // Reconcile remote residual temp files from a previous run
                // now that a live session to this host exists (plan M5/M6).
                self.clean_remote_residual_temps(tab_id, cx);
            }
            AppEvent::TabDisconnected(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id) {
                    tab.disconnect(scoped.payload.reason);
                    tab.remote.entries.clear();
                    tab.remote.path = None;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                }
                let had_modal = !self.state.drain_expired_modals().is_empty();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            AppEvent::AuthFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id) {
                    tab.fail(scoped.payload.reason);
                }
                let had_modal = !self.state.drain_expired_modals().is_empty();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            AppEvent::RemoteDirLoading(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    tab.remote.is_refreshing = true;
                    tab.remote.error = None;
                }
            }
            AppEvent::RemoteDirLoaded(scoped) => {
                let mut entries = scoped.payload.entries;
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    // A later navigation request can be queued while an
                    // earlier listing is still in flight. Keep the newer
                    // path and ignore the obsolete result.
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    sort_entries(&mut entries, &tab.sort);
                    tab.remote.entries = entries;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                    self.remote_scroll = UniformListScrollHandle::new();
                }
            }
            AppEvent::RemoteOperationFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && scoped
                        .payload
                        .path
                        .as_ref()
                        .is_none_or(|path| tab.remote.path.as_ref() == Some(path))
                {
                    tab.remote.is_refreshing = false;
                    tab.remote.error = Some(scoped.payload.error.clone());
                }
                self.status_message = Some(
                    format!(
                        "{}: {}",
                        scoped.payload.error.title, scoped.payload.error.message
                    )
                    .into(),
                );
            }
            AppEvent::TransferPlanStarted(snapshot) => {
                self.state.transfers.plans.push(snapshot.plan);
                self.state.transfers.jobs.push(snapshot.root_job);
            }
            AppEvent::TransferConflict(prompt) => {
                let default_rename = copy_name(&prompt.destination);
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == prompt.transfer_id)
                {
                    job.state = TransferState::WaitingForConflictDecision {
                        plan_id: prompt.plan_id,
                        request_id: prompt.request_id,
                    };
                }
                self.state.transfers.pending_conflicts.push(
                    ConflictRequest::new(
                        prompt.request_id,
                        prompt.plan_id,
                        prompt.transfer_id,
                        prompt.source.clone(),
                        prompt.destination.clone(),
                        macsftp_core::Timestamp(std::time::SystemTime::now()),
                    )
                    .with_source_details(prompt.source_size, prompt.source_modified_at),
                );
                self.state
                    .modals
                    .active
                    .push(ModalRequest::TransferConflict(prompt));
                self.conflict_rename.set_value(default_rename);
                self.conflict_rename_error = None;
                window.focus(&self.modal_focus);
            }
            AppEvent::TransferPlanProgress(progress) => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == progress.plan_id)
                {
                    plan.planned_count = progress.planned_count;
                    plan.total_bytes = progress.total_bytes;
                    plan.child_jobs.push(progress.child_job.id);
                }
                self.state.transfers.jobs.push(progress.child_job);
            }
            AppEvent::TransferPlanCompleted { plan_id } => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == plan_id)
                {
                    plan.state = macsftp_core::TransferPlanState::Queued;
                    if let Some(root_job) = self
                        .state
                        .transfers
                        .jobs
                        .iter_mut()
                        .find(|job| job.id == plan.root_job_id)
                    {
                        root_job.state = TransferState::Queued;
                    }
                    if plan.child_jobs.is_empty() {
                        plan.state = macsftp_core::TransferPlanState::Completed;
                        if let Some(root_job) = self
                            .state
                            .transfers
                            .jobs
                            .iter_mut()
                            .find(|job| job.id == plan.root_job_id)
                        {
                            root_job.state = TransferState::Completed;
                        }
                    }
                }
            }
            AppEvent::TransferPlanCancelled { plan_id } => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == plan_id)
                {
                    plan.state = macsftp_core::TransferPlanState::Cancelled;
                    if let Some(root_job) = self
                        .state
                        .transfers
                        .jobs
                        .iter_mut()
                        .find(|job| job.id == plan.root_job_id)
                    {
                        root_job.state = TransferState::Skipped;
                    }
                }
            }
            AppEvent::TransferPlanFailed { plan_id, error } => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == plan_id)
                {
                    plan.state = macsftp_core::TransferPlanState::Failed {
                        error: error.clone(),
                    };
                    if let Some(root_job) = self
                        .state
                        .transfers
                        .jobs
                        .iter_mut()
                        .find(|job| job.id == plan.root_job_id)
                    {
                        root_job.state = TransferState::Failed {
                            retryable: error.retryable,
                            error,
                        };
                    }
                }
            }
            AppEvent::TransferQueued(snapshot) | AppEvent::TransferRunning(snapshot) => {
                if let Some(existing_job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == snapshot.job.id)
                {
                    *existing_job = snapshot.job;
                } else {
                    self.state.transfers.jobs.push(snapshot.job);
                }
            }
            AppEvent::TransferProgress(progress) => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == progress.transfer_id)
                {
                    let started_at = match job.state {
                        TransferState::Running { started_at, .. } => started_at,
                        _ => macsftp_core::Timestamp(std::time::SystemTime::now()),
                    };
                    job.state = TransferState::Running {
                        bytes_done: progress.bytes_done,
                        bytes_total: progress.bytes_total,
                        started_at,
                    };
                }
            }
            AppEvent::TransferWarning(warning) => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == warning.transfer_id)
                {
                    job.warnings.push(warning);
                }
            }
            AppEvent::TransferCompleted { transfer_id } => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == transfer_id)
                {
                    job.state = TransferState::Completed;
                }
                self.finalize_plan(transfer_id);
            }
            AppEvent::TransferSkipped { transfer_id } => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == transfer_id)
                {
                    job.state = TransferState::Skipped;
                }
                self.finalize_plan(transfer_id);
            }
            AppEvent::TransferFailed(failure) => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == failure.transfer_id)
                {
                    job.state = TransferState::Failed {
                        retryable: failure.error.retryable,
                        error: failure.error,
                    };
                }
                self.finalize_plan(failure.transfer_id);
            }
            AppEvent::ResidualTempCreated(record) => {
                self.residual_temps.add(record);
                if let Err(error) = self.residual_temps.save() {
                    warn!(error = %error, "could not persist residual temp record");
                }
            }
            AppEvent::ResidualTempCleared { transfer_id, path } => {
                self.residual_temps.remove(transfer_id, &path);
                if let Err(error) = self.residual_temps.save() {
                    warn!(error = %error, "could not update residual temp store");
                }
            }
            _ => {}
        }
        cx.notify();
    }

    // ── Pane focus and navigation ──────────────────────────────────

    fn focus_pane(&mut self, side: PaneSide, window: &mut Window, cx: &mut Context<Self>) {
        self.focused_side = side;
        window.focus(self.pane_focus(side));
        cx.notify();
    }

    fn entry_count(&self, side: PaneSide) -> usize {
        match (self.active_tab(), side) {
            (Some(tab), PaneSide::Local) => tab.local.entries.len(),
            (Some(tab), PaneSide::Remote) => tab.remote.entries.len(),
            (None, _) => 0,
        }
    }

    fn entry_path_at(&self, side: PaneSide, index: usize) -> Option<EntryPath> {
        let tab = self.active_tab()?;
        match side {
            PaneSide::Local => tab
                .local
                .entries
                .get(index)
                .map(|entry| EntryPath::Local(entry.path.clone())),
            PaneSide::Remote => tab
                .remote
                .entries
                .get(index)
                .map(|entry| EntryPath::Remote(entry.path.clone())),
        }
    }

    fn selected_index(&self, side: PaneSide) -> Option<usize> {
        let tab = self.active_tab()?;
        let selected = tab.selection.selected_paths.first()?;
        match (side, selected) {
            (PaneSide::Local, EntryPath::Local(path)) => tab
                .local
                .entries
                .iter()
                .position(|entry| &entry.path == path),
            (PaneSide::Remote, EntryPath::Remote(path)) => tab
                .remote
                .entries
                .iter()
                .position(|entry| &entry.path == path),
            _ => None,
        }
    }

    fn select_index(&mut self, side: PaneSide, index: usize, cx: &mut Context<Self>) {
        let Some(path) = self.entry_path_at(side, index) else {
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.selected_paths = vec![path];
        }
        self.scroll_handle(side)
            .scroll_to_item(index, ScrollStrategy::Top);
        cx.notify();
    }

    fn move_selection(&mut self, side: PaneSide, offset: isize, cx: &mut Context<Self>) {
        let entry_count = self.entry_count(side);
        if entry_count == 0 {
            return;
        }
        let next_index = match self.selected_index(side) {
            Some(current) => {
                (current as isize + offset).clamp(0, entry_count as isize - 1) as usize
            }
            None => 0,
        };
        self.select_index(side, next_index, cx);
    }

    fn open_entry_at(
        &mut self,
        side: PaneSide,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        match side {
            PaneSide::Local => {
                let Some(entry) = tab.local.entries.get(index) else {
                    return;
                };
                if entry.kind == FileKind::Directory {
                    let path = entry.path.clone();
                    self.set_local_path(path, window, cx);
                }
            }
            PaneSide::Remote => {
                let remote_directory = tab
                    .remote
                    .entries
                    .get(index)
                    .filter(|entry| entry.kind == FileKind::Directory)
                    .map(|entry| entry.path.clone());
                if let Some(path) = remote_directory {
                    self.request_remote_directory(tab.id, path, cx);
                }
            }
        }
    }

    fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        if let Some(index) = self.selected_index(side) {
            self.open_entry_at(side, index, window, cx);
        }
    }

    fn set_local_path(&mut self, path: LocalPath, _window: &mut Window, cx: &mut Context<Self>) {
        let message = if let Some(tab) = self.active_tab_mut() {
            let message = Self::load_local_directory(&path, tab);
            tab.selection.selected_paths.clear();
            message
        } else {
            None
        };
        if let Some(message) = message {
            self.status_message = Some(message.into());
        }
        self.local_scroll = UniformListScrollHandle::new();
        cx.notify();
    }

    /// Read the real local filesystem directory at `path` into `tab`'s local
    /// pane (sorted, directories first). Returns an error message when the
    /// directory cannot be opened so the caller can surface it; the path is
    /// still recorded so the breadcrumb reflects where the user tried to go.
    fn load_local_directory(path: &LocalPath, tab: &mut TabState) -> Option<String> {
        match read_local_directory(path) {
            Ok(mut entries) => {
                macsftp_core::sort_entries(&mut entries, &Default::default());
                tab.local.entries = entries;
                tab.local.path = Some(path.clone());
                None
            }
            Err(error) => {
                tab.local.entries = Vec::new();
                tab.local.path = Some(path.clone());
                warn!(error = %error, "could not read local directory {}", path.as_str());
                Some(format!("Cannot open {}: {error}", path.as_str()))
            }
        }
    }

    fn request_remote_directory(
        &mut self,
        tab_id: TabId,
        path: RemotePath,
        cx: &mut Context<Self>,
    ) {
        if !self.send_command(
            AppCommand::ReadRemoteDir {
                tab_id,
                path: path.clone(),
            },
            cx,
        ) {
            return;
        }
        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            // Keep the last listing visible while the actor reads the new
            // path. This supplies immediate feedback before the actor emits
            // RemoteDirLoading and prevents a refresh flash.
            tab.remote.path = Some(path);
            tab.remote.is_refreshing = true;
            tab.remote.error = None;
            tab.selection.selected_paths.clear();
        }
        cx.notify();
    }

    fn go_to_parent_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        match side {
            PaneSide::Local => {
                if let Some(parent) = tab.local.path.as_ref().and_then(LocalPath::parent) {
                    self.set_local_path(parent, window, cx);
                }
            }
            PaneSide::Remote => {
                if let Some(parent) = tab.remote.path.as_ref().and_then(RemotePath::parent) {
                    self.request_remote_directory(tab.id, parent, cx);
                }
            }
        }
    }

    fn refresh_focused_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        match side {
            PaneSide::Local => {
                if let Some(path) = tab.local.path.clone() {
                    self.set_local_path(path, window, cx);
                }
            }
            PaneSide::Remote => {
                if let Some(path) = tab.remote.path.clone() {
                    self.request_remote_directory(tab.id, path, cx);
                }
            }
        }
    }

    fn copy_focused_path(&mut self, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        let path_text = match side {
            PaneSide::Local => tab
                .local
                .path
                .as_ref()
                .map(|path| path.as_str().to_string()),
            PaneSide::Remote => tab
                .remote
                .path
                .as_ref()
                .map(|path| path.as_str().to_string()),
        };
        if let Some(path_text) = path_text {
            cx.write_to_clipboard(ClipboardItem::new_string(path_text.clone()));
            self.status_message = Some(format!("Copied {path_text}").into());
            cx.notify();
        }
    }

    fn on_row_clicked(
        &mut self,
        side: PaneSide,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane(side, window, cx);
        if event.click_count() >= 2 {
            self.open_entry_at(side, index, window, cx);
        } else {
            self.select_index(side, index, cx);
        }
    }

    /// Begin an upload of the given local sources into the active tab's
    /// remote directory. Shared by the `Upload Selection` action and by
    /// dragging a local file onto the remote pane.
    fn begin_upload(&mut self, sources: Vec<LocalPath>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before starting an upload".into());
            cx.notify();
            return;
        };
        let Some(remote_directory) = tab.remote.path.clone() else {
            self.status_message = Some("Choose a remote destination directory".into());
            cx.notify();
            return;
        };
        if sources.is_empty() {
            self.status_message = Some("Select local files or directories to upload".into());
            cx.notify();
            return;
        }

        let destination = if sources.len() == 1
            && !tab
                .local
                .entries
                .iter()
                .any(|entry| entry.path == sources[0] && entry.kind == FileKind::Directory)
        {
            match append_remote_name(&remote_directory, &sources[0]) {
                Some(path) => path,
                None => {
                    self.status_message =
                        Some("Selected upload source has no usable file name".into());
                    cx.notify();
                    return;
                }
            }
        } else {
            remote_directory
        };
        let tab_id = tab.id;
        let command = AppCommand::StartTransfer(macsftp_core::StartTransferCommand {
            tab_id,
            session_epoch,
            profile_id,
            direction: macsftp_core::TransferDirection::Upload,
            sources: sources.into_iter().map(TransferEndpoint::Local).collect(),
            destination: TransferEndpoint::Remote(destination),
            metadata_policy: Default::default(),
            conflict_policy: Default::default(),
        });
        if self.send_command(command, cx) {
            self.drawer_open = true;
            self.status_message = Some("Planning upload…".into());
            cx.notify();
        }
    }

    /// Begin a download of the given remote sources into the active tab's
    /// local directory. Shared by the `Download Selection` action and by
    /// dragging a remote file onto the local pane. Exactly one source is
    /// required (the local destination is derived from its file name).
    fn begin_download(&mut self, sources: Vec<RemotePath>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before starting a download".into());
            cx.notify();
            return;
        };
        let Some(local_directory) = tab.local.path.clone() else {
            self.status_message = Some("Choose a local destination directory".into());
            cx.notify();
            return;
        };
        if sources.len() != 1 {
            self.status_message = Some("Select one remote file or directory to download".into());
            cx.notify();
            return;
        }
        let Some(destination) = append_local_name(&local_directory, &sources[0]) else {
            self.status_message = Some("Selected download source has no usable file name".into());
            cx.notify();
            return;
        };
        let tab_id = tab.id;
        let command = AppCommand::StartTransfer(macsftp_core::StartTransferCommand {
            tab_id,
            session_epoch,
            profile_id,
            direction: macsftp_core::TransferDirection::Download,
            sources: sources.into_iter().map(TransferEndpoint::Remote).collect(),
            destination: TransferEndpoint::Local(destination),
            metadata_policy: Default::default(),
            conflict_policy: Default::default(),
        });
        if self.send_command(command, cx) {
            self.drawer_open = true;
            self.status_message = Some("Planning download…".into());
            cx.notify();
        }
    }

    fn upload_selection(&mut self, cx: &mut Context<Self>) {
        let sources: Vec<LocalPath> = self
            .active_tab()
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter_map(|path| match path {
                        EntryPath::Local(path) => Some(path.clone()),
                        EntryPath::Remote(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.begin_upload(sources, cx);
    }

    fn download_selection(&mut self, cx: &mut Context<Self>) {
        let sources: Vec<RemotePath> = self
            .active_tab()
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter_map(|path| match path {
                        EntryPath::Remote(path) => Some(path.clone()),
                        EntryPath::Local(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.begin_download(sources, cx);
    }

    fn cancel_transfer(&mut self, transfer_id: macsftp_core::TransferId, cx: &mut Context<Self>) {
        if let Some(job) = self
            .state
            .transfers
            .jobs
            .iter_mut()
            .find(|job| job.id == transfer_id)
            && matches!(
                job.state,
                TransferState::Queued | TransferState::Running { .. }
            )
        {
            job.state = TransferState::Cancelling;
        }
        self.send_command(AppCommand::CancelTransfer { transfer_id }, cx);
        cx.notify();
    }

    fn retry_transfer(&mut self, transfer_id: macsftp_core::TransferId, cx: &mut Context<Self>) {
        self.send_command(AppCommand::RetryTransfer { transfer_id }, cx);
    }

    /// Finalize the parent plan and root job once every child job in the
    /// plan has reached a terminal state. This prevents completed root jobs
    /// from staying stuck in `Queued` in the transfer drawer.
    fn finalize_plan(&mut self, _job_id: macsftp_core::TransferId) {
        let Some(plan_id) = self
            .state
            .transfers
            .plans
            .iter()
            .find_map(|plan| plan.child_jobs.contains(&_job_id).then_some(plan.id))
        else {
            return;
        };
        let Some(plan) = self
            .state
            .transfers
            .plans
            .iter()
            .find(|plan| plan.id == plan_id)
        else {
            return;
        };
        if matches!(
            plan.state,
            macsftp_core::TransferPlanState::Completed
                | macsftp_core::TransferPlanState::Cancelled
                | macsftp_core::TransferPlanState::Failed { .. }
        ) {
            return;
        }
        let mut any_failed = false;
        for child_id in &plan.child_jobs {
            let Some(job) = self
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == *child_id)
            else {
                return;
            };
            match job.state {
                TransferState::Completed | TransferState::Skipped => {}
                TransferState::Failed { .. } => any_failed = true,
                _ => return,
            }
        }

        let root_job_id = plan.root_job_id;
        let plan_state = if any_failed {
            macsftp_core::TransferPlanState::Failed {
                error: UserFacingError::new(
                    ErrorCode::Unknown,
                    "Transfer finished with failures",
                    "Some files could not be transferred.",
                ),
            }
        } else {
            macsftp_core::TransferPlanState::Completed
        };
        if let Some(plan) = self
            .state
            .transfers
            .plans
            .iter_mut()
            .find(|plan| plan.id == plan_id)
        {
            plan.state = plan_state;
        }
        if let Some(root_job) = self
            .state
            .transfers
            .jobs
            .iter_mut()
            .find(|job| job.id == root_job_id)
        {
            root_job.state = if any_failed {
                TransferState::Failed {
                    retryable: true,
                    error: UserFacingError::new(
                        ErrorCode::Unknown,
                        "Transfer finished with failures",
                        "Some files could not be transferred.",
                    ),
                }
            } else {
                TransferState::Completed
            };
        }
    }

    /// Persist the current in-flight and finished transfers to disk
    /// (plan §18). Called from the app-quit hook. Running transfers are
    /// recorded as `Unfinished` so they can be retried on next launch.
    fn flush_transfer_history(&mut self) {
        if self.transfer_history_flushed {
            return;
        }
        let now = Timestamp(SystemTime::now());
        let mut new_records: Vec<TransferHistoryRecord> = Vec::new();
        for plan in &self.state.transfers.plans {
            let root_job = self.state.transfers.find_job(plan.root_job_id);
            let record = TransferHistoryRecord {
                id: self.transfer_history.allocate_id(),
                profile_id: None,
                direction: root_job
                    .map(|job| job.direction)
                    .unwrap_or(TransferDirection::Upload),
                sources: vec![plan.source_root.clone()],
                destination: plan.destination_root.clone(),
                metadata_policy: root_job
                    .map(|job| job.metadata_policy.clone())
                    .unwrap_or_default(),
                conflict_policy: plan.conflict_policy.clone(),
                status: history_status_for_plan(plan.state.clone(), root_job),
                started_at: root_job.map(|job| job.created_at).unwrap_or(now),
                last_updated: now,
            };
            new_records.push(record);
        }
        self.transfer_history.append(new_records);
        self.transfer_history.prune(now);
        if let Err(error) = self.transfer_history.save() {
            warn!(error = %error, "could not persist transfer history on quit");
        }
        self.transfer_history_flushed = true;
    }

    /// Best-effort clean local residual `.macsftp-part-*` files left by a
    /// previous run, and drop their records. Local temps need no live
    /// connection, so they are reconciled immediately at launch (plan
    /// M5/M6 residual).
    fn reconcile_local_residual_temps(mut store: ResidualTempStore) -> ResidualTempStore {
        let local: Vec<_> = store.local_records().cloned().collect();
        for record in local {
            match std::fs::remove_file(&record.path) {
                Ok(()) => {
                    store.remove(record.transfer_id, &record.path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    store.remove(record.transfer_id, &record.path);
                }
                Err(error) => {
                    warn!(
                        path = %record.path,
                        error = %error,
                        "local residual temp file could not be removed at launch"
                    );
                }
            }
        }
        if let Err(error) = store.save() {
            warn!(error = %error, "could not persist residual temp store at launch");
        }
        store
    }

    /// Best-effort clean remote residual temp files from a previous run
    /// that targeted this connection's `host:port`. Called on reconnect,
    /// since deleting a remote file requires a live session (plan M5/M6).
    fn clean_remote_residual_temps(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let connection_key = {
            let Some(tab) = self.state.tabs.find_tab(tab_id) else {
                return;
            };
            let Some(profile_id) = tab.profile_id else {
                return;
            };
            let Some(profile) = self.profiles.find_profile(profile_id) else {
                return;
            };
            format!("{}:{}", profile.host, profile.port)
        };
        let pending: Vec<_> = self
            .residual_temps
            .remote_records_for(&connection_key)
            .cloned()
            .collect();
        for record in pending {
            self.send_command(
                AppCommand::RemoveRemoteTempFile {
                    tab_id,
                    transfer_id: record.transfer_id,
                    path: RemotePath::new(record.path.clone()),
                },
                cx,
            );
        }
    }

    /// Retry a persisted transfer against the current connected tab
    /// (plan §18: retry does not restore the closed tab's session).
    /// The record is removed from history once the retry is enqueued.
    fn retry_history_transfer(
        &mut self,
        record_id: TransferHistoryId,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(record) = self.transfer_history.find(record_id).cloned() else {
            return false;
        };
        let Some(tab) = self.active_tab() else {
            self.status_message = Some("Open a connection to retry a transfer".into());
            cx.notify();
            return false;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before retrying a transfer".into());
            cx.notify();
            return false;
        };
        let command = record.to_start_command(tab.id, session_epoch, profile_id);
        let enqueued = self.send_command(AppCommand::StartTransfer(command), cx);
        if enqueued {
            self.transfer_history.remove(record_id);
            if let Err(error) = self.transfer_history.save() {
                warn!(error = %error, "could not update transfer history after retry");
            }
            self.drawer_open = true;
            self.status_message = Some("Retrying transfer…".into());
            cx.notify();
        }
        enqueued
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let active_tab_id = self.state.tabs.active_tab_id;

        let tabs = self.state.tabs.tabs.iter().map(|tab_state| {
            let tab_id = tab_state.id;
            let (status_color, status_label) = connection_status(&tab_state.connection, &theme);
            tab(
                ("tab", tab_id.0),
                tab_state.title.clone(),
                status_color,
                status_label,
            )
            .active(active_tab_id == Some(tab_id))
            .on_activate(cx.listener(move |workspace, _event, _window, cx| {
                workspace.activate_tab(tab_id, cx);
            }))
            .on_close(cx.listener(move |workspace, _event, window, cx| {
                workspace.close_tab_by_id(tab_id, window, cx);
            }))
        });

        div()
            .flex()
            .flex_none()
            .items_center()
            .h(theme.sizes.tab_bar_height)
            .bg(theme.colors.surface)
            .border_b_1()
            .border_color(theme.colors.border)
            .child(
                div()
                    .id("tab-strip")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(div().px_1().child(
                icon_button("new-tab", IconName::Plus, "New Tab (⌘T)").on_click(cx.listener(
                    |workspace, _event, window, cx| {
                        workspace.open_new_tab(window, cx);
                    },
                )),
            ))
    }

    fn render_pane(
        &self,
        side: PaneSide,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let pane_focused = self.pane_focus(side).contains_focused(window, cx);
        let tab_state = self.active_tab();
        let is_remote_refreshing =
            side == PaneSide::Remote && tab_state.is_some_and(|tab| tab.remote.is_refreshing);
        let remote_error = (side == PaneSide::Remote)
            .then(|| tab_state.and_then(|tab| tab.remote.error.clone()))
            .flatten();

        let (path_label, sort) = match (tab_state, side) {
            (Some(tab), PaneSide::Local) => (
                tab.local
                    .path
                    .as_ref()
                    .map(|path| path.as_str().to_string()),
                tab.sort.clone(),
            ),
            (Some(tab), PaneSide::Remote) => (
                tab.remote
                    .path
                    .as_ref()
                    .map(|path| path.as_str().to_string()),
                tab.sort.clone(),
            ),
            (None, _) => (None, Default::default()),
        };

        let (pane_name, up_id, refresh_id, copy_id, list_container_id) = match side {
            PaneSide::Local => (
                "Local",
                "local-up",
                "local-refresh",
                "local-copy-path",
                "local-list-container",
            ),
            PaneSide::Remote => (
                "Remote",
                "remote-up",
                "remote-refresh",
                "remote-copy-path",
                "remote-list-container",
            ),
        };

        // Transfer affordances: enabled only when the tab is connected and
        // the relevant pane has a selection. This surfaces the previously
        // menu-only Upload/Download actions as visible, discoverable buttons.
        let connected = tab_state
            .is_some_and(|tab| matches!(tab.connection, ConnectionState::Connected { .. }));
        let local_selected = tab_state
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter(|path| matches!(path, EntryPath::Local(_)))
                    .count()
            })
            .unwrap_or(0);
        let remote_selected = tab_state
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter(|path| matches!(path, EntryPath::Remote(_)))
                    .count()
            })
            .unwrap_or(0);
        let can_upload = connected && local_selected > 0;
        let can_download = connected && remote_selected > 0;

        let path_bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(theme.sizes.path_bar_height)
            .px_2()
            .bg(theme.colors.surface)
            .border_b_1()
            .border_color(if pane_focused {
                theme.colors.border_focused
            } else {
                theme.colors.border
            })
            .child(
                icon_button(up_id, IconName::ArrowUp, "Parent Directory (⌘↑)").on_click(
                    cx.listener(move |workspace, _event, window, cx| {
                        workspace.focused_side = side;
                        workspace.go_to_parent_directory(window, cx);
                    }),
                ),
            )
            .child(
                icon_button(refresh_id, IconName::Refresh, "Refresh (⌘R)").on_click(cx.listener(
                    move |workspace, _event, window, cx| {
                        workspace.focused_side = side;
                        workspace.refresh_focused_pane(window, cx);
                    },
                )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_1()
                    .text_size(px(12.0))
                    .text_color(if pane_focused {
                        theme.colors.text
                    } else {
                        theme.colors.text_muted
                    })
                    .truncate()
                    .child(path_label.clone().unwrap_or_else(|| pane_name.to_string())),
            )
            .when(is_remote_refreshing, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child("Refreshing…"),
                )
            })
            .child(
                icon_button(copy_id, IconName::Copy, "Copy Path (⌘⇧C)").on_click(cx.listener(
                    move |workspace, _event, _window, cx| {
                        workspace.focused_side = side;
                        workspace.copy_focused_path(cx);
                    },
                )),
            )
            .when(side == PaneSide::Local, |bar| {
                bar.child(
                    icon_button("local-upload", IconName::Upload, "Upload Selection (⌘U)")
                        .disabled(!can_upload)
                        .on_click(cx.listener(move |workspace, _event, _window, cx| {
                            workspace.focused_side = PaneSide::Local;
                            workspace.upload_selection(cx);
                        })),
                )
            })
            .when(side == PaneSide::Remote, |bar| {
                bar.child(
                    icon_button(
                        "remote-download",
                        IconName::Download,
                        "Download Selection (⌘D)",
                    )
                    .disabled(!can_download)
                    .on_click(cx.listener(
                        move |workspace, _event, _window, cx| {
                            workspace.focused_side = PaneSide::Remote;
                            workspace.download_selection(cx);
                        },
                    )),
                )
            });

        // Remote pane visualizes the connection lifecycle; the local
        // pane is always browsable. States per ui-ux-guidelines §6.3.
        let target_host = tab_state.map(|tab| tab.title.clone()).unwrap_or_default();
        let connect_button = |id: &'static str, label: &'static str| {
            text_button(id, label).on_click(cx.listener(|workspace, _event, window, cx| {
                workspace.request_connect(window, cx);
            }))
        };
        let edit_connection_button = |id: &'static str| {
            text_button(id, "Edit Connection…").on_click(cx.listener(
                |workspace, _event, window, cx| {
                    workspace.open_connect_form(window, cx);
                },
            ))
        };
        let retry_directory_button = |id: &'static str| {
            text_button(id, "Refresh & Retry").on_click(cx.listener(
                |workspace, _event, window, cx| {
                    workspace.focused_side = PaneSide::Remote;
                    workspace.refresh_focused_pane(window, cx);
                },
            ))
        };
        let connection_placeholder: Option<gpui::AnyElement> = if side == PaneSide::Remote {
            match tab_state.map(|tab| &tab.connection) {
                Some(ConnectionState::Empty) => Some(
                    empty_state(
                        "Not connected",
                        vec![connect_button("connect-remote", "Connect… (⌘⇧R)")],
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. }) => {
                    Some(
                        empty_state(format!("Connecting to {target_host}…"), vec![], cx)
                            .into_any_element(),
                    )
                }
                Some(ConnectionState::AwaitingHostKey { .. }) => Some(
                    empty_state("Waiting for host key decision…", vec![], cx).into_any_element(),
                ),
                Some(ConnectionState::AwaitingCredentials { .. }) => {
                    Some(empty_state("Waiting for credentials…", vec![], cx).into_any_element())
                }
                Some(ConnectionState::Disconnected { .. }) => Some(
                    empty_state(
                        "Disconnected",
                        vec![
                            connect_button("reconnect-remote", "Reconnect (⌘⇧R)"),
                            edit_connection_button("edit-connection-disconnected"),
                        ],
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(ConnectionState::Failed { error }) => Some(
                    empty_state(
                        format!("{} — {}", error.title, error.message),
                        vec![
                            connect_button("retry-connect-remote", "Retry"),
                            edit_connection_button("edit-connection-failed"),
                        ],
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(ConnectionState::Connected { .. }) => remote_error.as_ref().map(|error| {
                    empty_state(
                        format!("{} — {}", error.title, error.message),
                        vec![retry_directory_button("retry-remote-directory")],
                        cx,
                    )
                    .into_any_element()
                }),
                None => None,
            }
        } else {
            None
        };

        let entry_count = self.entry_count(side);
        let list: gpui::AnyElement = if let Some(placeholder) = connection_placeholder {
            placeholder
        } else if entry_count == 0 && is_remote_refreshing {
            empty_state("Loading directory…", vec![], cx).into_any_element()
        } else if entry_count == 0 {
            empty_state("Empty directory", vec![], cx).into_any_element()
        } else {
            let workspace = cx.entity();
            uniform_list(
                list_container_id,
                entry_count,
                move |visible_range, _window, cx| {
                    let workspace_view = workspace.read(cx);
                    let Some(tab) = workspace_view.state.tabs.active_tab() else {
                        return Vec::new();
                    };
                    let selected_paths = &tab.selection.selected_paths;

                    visible_range
                        .filter_map(|index| {
                            let (model, drag_item) = match side {
                                PaneSide::Local => {
                                    let entry = tab.local.entries.get(index)?;
                                    (
                                        FileRowModel {
                                            name: entry.name.clone().into(),
                                            kind: entry.kind,
                                            is_hidden: entry.name.starts_with('.'),
                                            selected: selected_paths
                                                .contains(&EntryPath::Local(entry.path.clone())),
                                            size_label: format_size(entry.size),
                                            modified_label: format_timestamp(entry.modified_at),
                                        },
                                        EntryPath::Local(entry.path.clone()),
                                    )
                                }
                                PaneSide::Remote => {
                                    let entry = tab.remote.entries.get(index)?;
                                    (
                                        FileRowModel {
                                            name: entry.name.clone().into(),
                                            kind: entry.kind,
                                            is_hidden: entry.name.starts_with('.'),
                                            selected: selected_paths
                                                .contains(&EntryPath::Remote(entry.path.clone())),
                                            size_label: format_size(entry.size),
                                            modified_label: format_timestamp(entry.modified_at),
                                        },
                                        EntryPath::Remote(entry.path.clone()),
                                    )
                                }
                            };
                            let workspace = workspace.clone();
                            Some(
                                file_row(("file-row", index), model, cx)
                                    .on_click(move |event, window, cx| {
                                        workspace.update(cx, |workspace, cx| {
                                            workspace
                                                .on_row_clicked(side, index, event, window, cx);
                                        });
                                    })
                                    .on_drag(
                                        drag_item,
                                        |value: &EntryPath, _position, _window, cx| {
                                            let label = match value {
                                                EntryPath::Local(p) => Path::new(p.as_str())
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().to_string()),
                                                EntryPath::Remote(p) => Path::new(p.as_str())
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().to_string()),
                                            }
                                            .unwrap_or_else(|| "file".to_string());
                                            cx.new(|_| DragPreview {
                                                label: label.into(),
                                            })
                                        },
                                    )
                                    .into_any_element(),
                            )
                        })
                        .collect()
                },
            )
            .track_scroll(self.scroll_handle(side).clone())
            .h_full()
            .w_full()
            .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .key_context("FilePane")
            .track_focus(self.pane_focus(side))
            .when(side == PaneSide::Local, |pane| {
                pane.border_r_1().border_color(theme.colors.border)
            })
            .on_drop(
                cx.listener(
                    move |workspace, path: &EntryPath, _window, cx| match (side, path) {
                        (PaneSide::Remote, EntryPath::Local(local)) => {
                            workspace.begin_upload(vec![local.clone()], cx)
                        }
                        (PaneSide::Local, EntryPath::Remote(remote)) => {
                            workspace.begin_download(vec![remote.clone()], cx)
                        }
                        _ => {}
                    },
                ),
            )
            .child(path_bar)
            .child(file_table_header(&sort, cx))
            .child(div().flex_1().min_h_0().child(list))
    }

    fn render_transfer_drawer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let jobs = &self.state.transfers.jobs;

        let active_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    TransferState::Running { .. }
                        | TransferState::Planning
                        | TransferState::Cancelling
                        | TransferState::WaitingForConflictDecision { .. }
                )
            })
            .collect();
        let queued_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Queued))
            .collect();
        let completed_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Completed | TransferState::Skipped))
            .collect();
        let failed_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Failed { .. }))
            .collect();

        let mut drawer = div()
            .id("transfer-drawer")
            .flex()
            .flex_col()
            .flex_none()
            .max_h(px(240.0))
            .overflow_y_scroll()
            .bg(theme.colors.surface)
            .border_t_1()
            .border_color(theme.colors.border);

        drawer = drawer.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h(px(28.0))
                .px_2()
                .text_size(px(11.0))
                .text_color(theme.colors.text_muted)
                .child(icon(IconName::Transfers, theme.colors.text_muted))
                .child("Transfers")
                .child(div().flex_1())
                .child(format!(
                    "{} active · {} queued · {} done · {} failed",
                    active_jobs.len(),
                    queued_jobs.len(),
                    completed_jobs.len(),
                    failed_jobs.len()
                )),
        );

        if jobs.is_empty() && self.transfer_history.records().is_empty() {
            return drawer.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(60.0))
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child("No transfers"),
            );
        }

        for (label, section_jobs) in [("Active", active_jobs), ("Queued", queued_jobs)] {
            if !section_jobs.is_empty() {
                drawer = drawer
                    .child(section_header_static(label, section_jobs.len(), &theme))
                    .children(
                        section_jobs
                            .into_iter()
                            .map(|job| self.render_transfer_job(job, cx)),
                    );
            }
        }

        for (label, section_jobs, expanded, toggle_id) in [
            (
                "Completed",
                completed_jobs,
                self.completed_section_expanded,
                "toggle-completed",
            ),
            (
                "Failed",
                failed_jobs,
                self.failed_section_expanded,
                "toggle-failed",
            ),
        ] {
            if section_jobs.is_empty() {
                continue;
            }
            let is_completed_section = label == "Completed";
            drawer = drawer.child(
                div()
                    .id(toggle_id)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .h(px(24.0))
                    .px_2()
                    .text_size(px(11.0))
                    .text_color(theme.colors.text_muted)
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        if is_completed_section {
                            workspace.completed_section_expanded =
                                !workspace.completed_section_expanded;
                        } else {
                            workspace.failed_section_expanded = !workspace.failed_section_expanded;
                        }
                        cx.notify();
                    }))
                    .child(icon(
                        if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        theme.colors.text_muted,
                    ))
                    .child(format!("{label} ({})", section_jobs.len())),
            );
            if expanded {
                drawer = drawer.children(
                    section_jobs
                        .into_iter()
                        .map(|job| self.render_transfer_job(job, cx)),
                );
            }
        }

        // History (plan §18): persisted transfers from prior sessions,
        // including unfinished ones captured at the last app close. Only
        // shown when there is something to retry or review.
        let mut history_records: Vec<&TransferHistoryRecord> =
            self.transfer_history.records().iter().collect();
        history_records.sort_by_key(|record| std::cmp::Reverse(record.last_updated));
        if !history_records.is_empty() {
            drawer = drawer.child(section_header_static(
                "History",
                history_records.len(),
                &theme,
            ));
            for record in history_records {
                drawer = drawer.child(self.render_history_record(record, cx));
            }
        }

        drawer
    }

    fn render_history_record(
        &self,
        record: &TransferHistoryRecord,
        cx: &mut Context<Self>,
    ) -> TransferRow {
        let theme = cx.theme().clone();
        let title = transfer_history_title(record);
        let (state_label, state_color): (SharedString, Hsla) = match &record.status {
            TransferHistoryStatus::Unfinished => ("Unfinished".into(), theme.colors.warning),
            TransferHistoryStatus::Completed => ("Completed".into(), theme.colors.success),
            TransferHistoryStatus::Cancelled => ("Cancelled".into(), theme.colors.text_muted),
            TransferHistoryStatus::Failed { .. } => ("Failed".into(), theme.colors.error),
        };
        let connected = self
            .active_tab()
            .and_then(connected_transfer_session)
            .is_some();
        let mut row = transfer_row(
            ("history", record.id.0),
            record.direction,
            title,
            state_label,
            state_color,
        )
        .detail(transfer_history_detail(record));
        // Only offer retry when a live connection exists — the retry
        // rebuilds a transfer against the current connected tab.
        if record.is_retryable() && connected {
            let workspace = cx.entity();
            let record_id = record.id;
            row = row.on_retry(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.retry_history_transfer(record_id, cx);
                });
            });
        }
        row
    }

    fn render_transfer_job(&self, job: &TransferJob, cx: &mut Context<Self>) -> TransferRow {
        let theme = cx.theme().clone();
        let job_id = job.id;
        let title = transfer_title(job);

        let (state_label, state_color): (SharedString, Hsla) = match &job.state {
            TransferState::Queued => ("Queued".into(), theme.colors.text_muted),
            TransferState::Planning => ("Planning…".into(), theme.colors.info),
            TransferState::WaitingForConflictDecision { .. } => {
                ("Waiting for decision".into(), theme.colors.warning)
            }
            TransferState::Running {
                bytes_done,
                bytes_total,
                ..
            } => {
                let label = match bytes_total {
                    Some(total) if *total > 0 => {
                        format!("{}%", bytes_done * 100 / total)
                    }
                    _ => "Running".to_string(),
                };
                (label.into(), theme.colors.accent)
            }
            TransferState::Cancelling => ("Cancelling…".into(), theme.colors.warning),
            TransferState::Completed => ("Completed".into(), theme.colors.success),
            TransferState::Skipped => ("Skipped".into(), theme.colors.text_muted),
            TransferState::Failed { .. } => ("Failed".into(), theme.colors.error),
        };

        let detail: SharedString = match &job.state {
            TransferState::Planning => self
                .state
                .transfers
                .plans
                .iter()
                .find(|plan| plan.root_job_id == job.id)
                .map(|plan| {
                    format!(
                        "{} items · {}",
                        plan.planned_count,
                        format_size(plan.total_bytes)
                    )
                })
                .unwrap_or_else(|| "Planning…".to_string())
                .into(),
            TransferState::Running {
                bytes_done,
                bytes_total,
                ..
            } => format!(
                "{} / {} · — MB/s · ETA —",
                format_size(Some(*bytes_done)),
                bytes_total
                    .map(|total| format_size(Some(total)).to_string())
                    .unwrap_or_else(|| "?".to_string())
            )
            .into(),
            TransferState::Completed if !job.warnings.is_empty() => job
                .warnings
                .last()
                .map(|warning| warning.message.clone().into())
                .unwrap_or_else(|| "Completed with warning".into()),
            TransferState::Failed { error, .. } if !job.warnings.is_empty() => {
                let warning = job
                    .warnings
                    .last()
                    .map(|warning| warning.message.as_str())
                    .unwrap_or("Transfer warning");
                format!("{} · {warning}", error.message).into()
            }
            TransferState::Failed { error, .. } => error.message.clone().into(),
            _ => "".into(),
        };

        let mut row = transfer_row(
            ("transfer", job_id.0),
            job.direction,
            title,
            state_label,
            state_color,
        )
        .detail(detail);

        row = match &job.state {
            TransferState::Running {
                bytes_done,
                bytes_total,
                ..
            } => {
                let fraction = bytes_total
                    .filter(|total| *total > 0)
                    .map(|total| *bytes_done as f32 / total as f32)
                    .unwrap_or(0.0);
                row.progress(fraction, theme.colors.accent)
            }
            TransferState::Completed => row.progress(1.0, theme.colors.success),
            _ => row,
        };

        let is_plan_root = self
            .state
            .transfers
            .plans
            .iter()
            .any(|plan| plan.root_job_id == job_id);
        let workspace = cx.entity();
        if matches!(job.state, TransferState::Planning)
            || (!is_plan_root
                && matches!(
                    job.state,
                    TransferState::Queued | TransferState::Running { .. }
                ))
        {
            let workspace = workspace.clone();
            row = row.on_cancel(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.cancel_transfer(job_id, cx);
                });
            });
        }
        if matches!(job.state, TransferState::Failed { .. }) {
            row = row.on_retry(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.retry_transfer(job_id, cx);
                });
            });
        }

        row
    }

    /// The connect form: host/port/username plus password or private
    /// key credentials. Purely app-local state — no runtime request id.
    fn render_connect_form_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let form = self.connect_form.as_ref()?;
        let theme = cx.theme().clone();

        let field_row = |label: &'static str,
                         field: ConnectField,
                         state: &InputState,
                         placeholder: &'static str,
                         masked: bool,
                         cx: &mut Context<Self>| {
            div()
                .id(("connect-field", field as usize))
                .flex()
                .items_center()
                .gap_2()
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    if let Some(form) = &mut workspace.connect_form {
                        form.focused_field = field;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(text_field(
                    ("connect-input", field as usize),
                    TextFieldModel {
                        state,
                        placeholder,
                        focused: form.focused_field == field,
                        masked,
                    },
                    cx,
                )))
        };

        let auth_toggle = |label: &'static str,
                           method: AuthMethodKind,
                           id: &'static str,
                           cx: &mut Context<Self>| {
            text_button(id, label)
                .primary(form.auth_method == method)
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    if let Some(form) = &mut workspace.connect_form {
                        form.set_auth_method(method);
                        cx.notify();
                    }
                }))
        };

        let mut card = div()
            .key_context("ConnectForm")
            .track_focus(&self.connect_form_focus)
            .on_key_down(cx.listener(Self::handle_connect_form_key))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(460.0))
            .p_4()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded_md()
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.colors.text)
                    .child("Connect to Server"),
            );

        if let Some(error) = &form.error {
            card = card.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.error)
                    .child(error.clone()),
            );
        }

        // Saved profiles: pick one to prefill the form, or delete it.
        let saved_profiles: Vec<(ProfileId, String, String)> = self
            .profiles
            .profiles()
            .iter()
            .map(|profile| {
                (
                    profile.id,
                    profile.name.clone(),
                    format!("{}@{}:{}", profile.username, profile.host, profile.port),
                )
            })
            .collect();
        if !saved_profiles.is_empty() {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Saved profiles"),
                    )
                    .children(
                        saved_profiles
                            .into_iter()
                            .map(|(profile_id, name, summary)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.colors.text)
                                                    .child(name),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.0))
                                                    .text_color(theme.colors.text_muted)
                                                    .child(summary),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .child(
                                                text_button(("use-profile", profile_id.0), "Use")
                                                    .on_click(cx.listener(
                                                        move |workspace, _event, _window, _cx| {
                                                            workspace.use_profile(profile_id, _cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                text_button(
                                                    ("delete-profile", profile_id.0),
                                                    "Delete",
                                                )
                                                .on_click(cx.listener(
                                                    move |workspace, _event, _window, _cx| {
                                                        workspace.delete_profile(profile_id, _cx);
                                                    },
                                                )),
                                            ),
                                    )
                            }),
                    )
                    .child(div().h(px(1.0)).bg(theme.colors.border).my_1()),
            );
        }

        // When prefilled from a saved profile, `use_profile` restores the
        // secret from the Keychain into the form, so the field is usually
        // already filled. If the Keychain entry is missing, the field is
        // blank and the user re-enters it.
        if form.source_profile_id.is_some() {
            card = card.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme.colors.text_muted)
                    .child("Saved profile — credentials are restored from the Keychain."),
            );
        }

        card = card
            .child(field_row(
                "Host",
                ConnectField::Host,
                &form.host,
                "example.com",
                false,
                cx,
            ))
            .child(field_row(
                "Port",
                ConnectField::Port,
                &form.port,
                "22",
                false,
                cx,
            ))
            .child(field_row(
                "Username",
                ConnectField::Username,
                &form.username,
                "user",
                false,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(96.0))
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Auth"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(auth_toggle(
                                "Password",
                                AuthMethodKind::Password,
                                "auth-password",
                                cx,
                            ))
                            .child(auth_toggle(
                                "Private Key",
                                AuthMethodKind::PrivateKey,
                                "auth-private-key",
                                cx,
                            )),
                    ),
            );

        card = match form.auth_method {
            AuthMethodKind::Password => card.child(field_row(
                "Password",
                ConnectField::Password,
                &form.password,
                "",
                true,
                cx,
            )),
            AuthMethodKind::PrivateKey => card
                .child(field_row(
                    "Key path",
                    ConnectField::KeyPath,
                    &form.key_path,
                    "~/.ssh/id_ed25519",
                    false,
                    cx,
                ))
                .child(field_row(
                    "Passphrase",
                    ConnectField::Passphrase,
                    &form.passphrase,
                    "",
                    true,
                    cx,
                )),
        };

        // Save the current connection as a profile. The secret is mapped
        // to a SecretRef stub and never written to disk (plan §5).
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child("Save as"),
                )
                .child(
                    div()
                        .id("profile-name-row")
                        .flex_1()
                        .min_w_0()
                        .on_click(cx.listener(
                            move |workspace, _event: &ClickEvent, _window, cx| {
                                if let Some(form) = &mut workspace.connect_form {
                                    form.focused_field = ConnectField::ProfileName;
                                    cx.notify();
                                }
                            },
                        ))
                        .child(text_field(
                            "profile-name-input",
                            TextFieldModel {
                                state: &form.profile_name,
                                placeholder: "profile name (optional)",
                                focused: form.focused_field == ConnectField::ProfileName,
                                masked: false,
                            },
                            cx,
                        )),
                )
                .child(
                    text_button("save-profile", "Save profile").on_click(cx.listener(
                        |workspace, _event, _window, cx| {
                            workspace.save_current_profile(cx);
                        },
                    )),
                ),
        );

        card = card.child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    text_button("connect-cancel", "Cancel").on_click(cx.listener(
                        |workspace, _event, window, cx| {
                            workspace.close_connect_form(window, cx);
                        },
                    )),
                )
                .child(
                    text_button("connect-submit", "Connect")
                        .primary(true)
                        .on_click(cx.listener(|workspace, _event, window, cx| {
                            workspace.submit_connect_form(window, cx);
                        })),
                ),
        );

        Some(
            div()
                .id("connect-form-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(card)
                .into_any_element(),
        )
    }

    /// Blocking security decision for an unknown host key (ADR-004).
    /// Bound to the prompt's `TrustRequestId`; a stale prompt is
    /// removed by `drain_expired_modals` before it can be confirmed.
    fn render_host_key_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let prompt = self.active_host_key_prompt()?.clone();
        let theme = cx.theme().clone();
        let request_id = prompt.request_id;

        let info_row = |label: &'static str, value: SharedString, mono: bool| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(if mono { px(11.0) } else { px(12.0) })
                        .text_color(theme.colors.text)
                        .when(mono, |value_cell| {
                            value_cell.font_family(theme.fonts.mono_family.clone())
                        })
                        .child(value),
                )
        };

        Some(
            div()
                .id("host-key-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .key_context("HostKeyModal")
                        .track_focus(&self.modal_focus)
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w(px(460.0))
                        .p_4()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text)
                                .child("Unknown Host Key"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child(
                                    "The authenticity of this server can't be established. \
                                     Verify the fingerprint before trusting it.",
                                ),
                        )
                        .child(info_row(
                            "Host",
                            format!("{}:{}", prompt.host, prompt.port).into(),
                            false,
                        ))
                        .child(info_row("Key type", prompt.algorithm.clone().into(), false))
                        .child(info_row(
                            "Fingerprint",
                            prompt.fingerprint_sha256.clone().into(),
                            true,
                        ))
                        .child(info_row(
                            "Saves to",
                            "~/Library/Application Support/macSFTP/known_hosts".into(),
                            true,
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(text_button("host-key-cancel", "Cancel").on_click(
                                    cx.listener(move |workspace, _event, window, cx| {
                                        workspace.reject_host_key(request_id, window, cx);
                                    }),
                                ))
                                .child(
                                    text_button("host-key-trust", "Trust and Save")
                                        .primary(true)
                                        .on_click(cx.listener(
                                            move |workspace, _event, window, cx| {
                                                workspace.accept_host_key(request_id, window, cx);
                                            },
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_transfer_conflict_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let prompt = self.active_transfer_conflict_prompt()?.clone();
        let theme = cx.theme().clone();
        let source = match &prompt.source {
            TransferEndpoint::Local(path) => path.as_str().to_string(),
            TransferEndpoint::Remote(path) => path.as_str().to_string(),
        };
        let destination = match &prompt.destination {
            TransferEndpoint::Local(path) => path.as_str().to_string(),
            TransferEndpoint::Remote(path) => path.as_str().to_string(),
        };
        let detail_row = |label: &'static str, value: SharedString| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text)
                        .child(value),
                )
        };
        let decision_button = |id: &'static str,
                               label: &'static str,
                               decision: ConflictDecision,
                               primary: bool,
                               cx: &mut Context<Self>| {
            let prompt = prompt.clone();
            text_button(id, label)
                .primary(primary)
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.resolve_transfer_conflict(&prompt, decision.clone(), window, cx);
                }))
        };

        Some(
            div()
                .id("transfer-conflict-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .key_context("TransferConflictModal")
                        .track_focus(&self.modal_focus)
                        .on_key_down(cx.listener(Self::handle_transfer_conflict_key))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w(px(460.0))
                        .p_4()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text)
                                .child("File already exists"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child("Choose how to handle the existing destination."),
                        )
                        .child(detail_row("Source", source.into()))
                        .child(detail_row("Destination", destination.into()))
                        .child(detail_row("Size", format_size(prompt.source_size)))
                        .child(detail_row(
                            "Modified",
                            format_timestamp(prompt.source_modified_at),
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child("All applies to remaining conflicts in this transfer."),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child("Enter renames · Esc cancels"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(96.0))
                                        .flex_none()
                                        .text_size(px(11.0))
                                        .text_color(theme.colors.text_muted)
                                        .child("New name"),
                                )
                                .child(div().flex_1().min_w_0().child(text_field(
                                    "conflict-rename-input",
                                    TextFieldModel {
                                        state: &self.conflict_rename,
                                        placeholder: "new file name",
                                        focused: true,
                                        masked: false,
                                    },
                                    cx,
                                ))),
                        )
                        .when_some(self.conflict_rename_error.clone(), |card, error| {
                            card.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.colors.error)
                                    .child(error),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(decision_button(
                                    "conflict-cancel",
                                    "Cancel",
                                    ConflictDecision::CancelJob,
                                    false,
                                    cx,
                                ))
                                .child(decision_button(
                                    "conflict-skip",
                                    "Skip",
                                    ConflictDecision::Skip {
                                        apply_to_all: false,
                                    },
                                    false,
                                    cx,
                                ))
                                .child(decision_button(
                                    "conflict-skip-all",
                                    "Skip All",
                                    ConflictDecision::Skip { apply_to_all: true },
                                    false,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    text_button("conflict-rename", "Rename")
                                        .primary(true)
                                        .on_click(cx.listener(|workspace, _event, window, cx| {
                                            workspace.submit_transfer_rename(false, window, cx);
                                        })),
                                )
                                .child(text_button("conflict-rename-all", "Rename All").on_click(
                                    cx.listener(|workspace, _event, window, cx| {
                                        workspace.submit_transfer_rename(true, window, cx);
                                    }),
                                ))
                                .child(decision_button(
                                    "conflict-overwrite",
                                    "Overwrite",
                                    ConflictDecision::Overwrite {
                                        apply_to_all: false,
                                    },
                                    false,
                                    cx,
                                ))
                                .child(decision_button(
                                    "conflict-overwrite-all",
                                    "Overwrite All",
                                    ConflictDecision::Overwrite { apply_to_all: true },
                                    false,
                                    cx,
                                )),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();

        let (status_color, status_text) = match self.active_tab() {
            Some(tab) => {
                let (color, label) = connection_status(&tab.connection, &theme);
                (color, format!("{label} · {}", tab.title))
            }
            None => (theme.colors.text_disabled, "No connection".to_string()),
        };

        let jobs = &self.state.transfers.jobs;
        let active_count = jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    TransferState::Running { .. }
                        | TransferState::Planning
                        | TransferState::Cancelling
                        | TransferState::WaitingForConflictDecision { .. }
                )
            })
            .count();
        let failed_count = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Failed { .. }))
            .count();

        let mut transfer_summary = format!("{active_count} active");
        if failed_count > 0 {
            transfer_summary.push_str(&format!(" · {failed_count} failed"));
        }

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(theme.sizes.status_bar_height)
            .px_2()
            .bg(theme.colors.surface)
            .border_t_1()
            .border_color(theme.colors.border)
            .text_size(px(11.0))
            .text_color(theme.colors.text_muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .size(px(7.0))
                            .flex_none()
                            .rounded_full()
                            .bg(status_color),
                    )
                    .child(div().truncate().child(status_text))
                    .children(
                        self.status_message
                            .clone()
                            .map(|message| div().truncate().child(format!("— {message}"))),
                    ),
            )
            .child(
                div()
                    .id("status-transfers")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .rounded_sm()
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .tooltip(text_tooltip("Toggle Transfers (⌘J)"))
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        workspace.drawer_open = !workspace.drawer_open;
                        cx.notify();
                    }))
                    .child(icon(
                        IconName::Transfers,
                        if failed_count > 0 {
                            theme.colors.error
                        } else if active_count > 0 {
                            theme.colors.accent
                        } else {
                            theme.colors.text_muted
                        },
                    ))
                    .child(transfer_summary),
            )
    }

    fn set_appearance(
        &mut self,
        appearance: AppearancePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.config.set_appearance(appearance) {
            Ok(()) => self.config_error = None,
            Err(error) => {
                warn!(error = %error, "could not save app config");
                self.config_error =
                    Some("Could not write config.json. Check file permissions.".into());
            }
        }

        let theme = match appearance {
            AppearancePreference::System => Theme::for_appearance(window.appearance()),
            AppearancePreference::Light => Theme::light(),
            AppearancePreference::Dark => Theme::dark(),
        };
        cx.set_global(theme);
        cx.notify();
    }

    fn open_log_folder(&self, cx: &mut Context<Self>) {
        if let Some(directory) = Path::new(self.log_file.as_str()).parent() {
            cx.reveal_path(directory);
        }
    }

    fn copy_version_info(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(format!(
            "macSFTP {} ({})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH
        )));
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let selected_appearance = self.config.config().appearance;
        let appearance_button = |id: &'static str,
                                 label: &'static str,
                                 appearance: AppearancePreference,
                                 cx: &mut Context<Self>| {
            let selected = selected_appearance == appearance;
            div()
                .id(id)
                .flex_1()
                .px_3()
                .py_2()
                .text_center()
                .text_size(px(12.0))
                .rounded_sm()
                .border_1()
                .border_color(if selected {
                    theme.colors.accent
                } else {
                    theme.colors.border
                })
                .bg(if selected {
                    theme.colors.element_selected
                } else {
                    theme.colors.background
                })
                .text_color(if selected {
                    theme.colors.accent
                } else {
                    theme.colors.text
                })
                .hover(|style| style.bg(theme.colors.element_hover))
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.set_appearance(appearance, window, cx);
                }))
                .child(label)
        };

        div()
            .key_context("Settings")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(theme.colors.background)
            .child(
                div()
                    .h(theme.sizes.tab_bar_height)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(theme.colors.surface)
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Settings"),
                    )
                    .child(text_button("settings-done", "Done").on_click(cx.listener(
                        |workspace, _event, window, cx| {
                            workspace.surface = WorkspaceSurface::Files;
                            workspace.workspace_focus.focus(window);
                            cx.notify();
                        },
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(180.0))
                            .flex_none()
                            .p_3()
                            .bg(theme.colors.surface)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(
                                div()
                                    .px_2()
                                    .py_2()
                                    .rounded_sm()
                                    .bg(theme.colors.element_selected)
                                    .text_size(px(12.0))
                                    .text_color(theme.colors.text)
                                    .child("General"),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .p_6()
                            .child(
                                div()
                                    .max_w(px(560.0))
                                    .flex()
                                    .flex_col()
                                    .gap_5()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_size(px(16.0))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("Appearance"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.colors.text_muted)
                                                    .child("Choose how macSFTP follows the macOS appearance."),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap_2()
                                            .child(appearance_button(
                                                "appearance-system",
                                                "System",
                                                AppearancePreference::System,
                                                cx,
                                            ))
                                            .child(appearance_button(
                                                "appearance-light",
                                                "Light",
                                                AppearancePreference::Light,
                                                cx,
                                            ))
                                            .child(appearance_button(
                                                "appearance-dark",
                                                "Dark",
                                                AppearancePreference::Dark,
                                                cx,
                                            )),
                                    )
                                    .children(self.config_error.clone().map(|error| {
                                        div()
                                            .p_3()
                                            .rounded_sm()
                                            .border_1()
                                            .border_color(theme.colors.error)
                                            .text_size(px(12.0))
                                            .text_color(theme.colors.error)
                                            .child(error)
                                    }))
                                    .child(div().h(px(1.0)).bg(theme.colors.border))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .text_size(px(14.0))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("Diagnostics"),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .gap_2()
                                                    .child(text_button("open-log-folder", "Open Log Folder").on_click(
                                                        cx.listener(|workspace, _event, _window, cx| {
                                                            workspace.open_log_folder(cx);
                                                        }),
                                                    ))
                                                    .child(text_button("copy-version", "Copy Version Info").on_click(
                                                        cx.listener(|workspace, _event, _window, cx| {
                                                            workspace.copy_version_info(cx);
                                                        }),
                                                    )),
                                            ),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_about(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.about_open {
            return None;
        }
        let theme = cx.theme().clone();

        let mark = div()
            .size(px(72.0))
            .rounded_md()
            .bg(theme.colors.surface)
            .border_1()
            .border_color(theme.colors.border)
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(22.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.colors.accent)
                    .child("⇄"),
            );

        Some(
            div()
                .id("about-scrim")
                .key_context("About")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .w(px(340.0))
                        .p_5()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .rounded_md()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .child(mark)
                        .child(
                            div()
                                .text_size(px(18.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child("macSFTP"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child("A fast, native SFTP client for macOS"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    text_button("about-copy-version", "Copy Version Info")
                                        .on_click(cx.listener(|workspace, _event, _window, cx| {
                                            workspace.copy_version_info(cx);
                                        })),
                                )
                                .child(text_button("about-close", "Close").primary(true).on_click(
                                    cx.listener(|workspace, _event, _window, cx| {
                                        workspace.about_open = false;
                                        cx.notify();
                                    }),
                                )),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn section_header_static(label: &str, count: usize, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.0))
        .px_2()
        .text_size(px(11.0))
        .text_color(theme.colors.text_muted)
        .child(format!("{label} ({count})"))
}

fn connection_status(connection: &ConnectionState, theme: &Theme) -> (Hsla, SharedString) {
    match connection {
        ConnectionState::Empty => (theme.colors.text_disabled, "Not connected".into()),
        ConnectionState::Connecting { .. } => (theme.colors.warning, "Connecting…".into()),
        ConnectionState::AwaitingHostKey { .. } => {
            (theme.colors.warning, "Awaiting host key".into())
        }
        ConnectionState::AwaitingCredentials { .. } => {
            (theme.colors.warning, "Awaiting credentials".into())
        }
        ConnectionState::Connected { .. } => (theme.colors.success, "Connected".into()),
        ConnectionState::Reconnecting { .. } => (theme.colors.warning, "Reconnecting…".into()),
        ConnectionState::Disconnected { .. } => (theme.colors.text_disabled, "Disconnected".into()),
        ConnectionState::Failed { error } => (
            theme.colors.error,
            format!("Failed — {}", error.title).into(),
        ),
    }
}

fn transfer_title(job: &TransferJob) -> String {
    let endpoint_text = |endpoint: &TransferEndpoint| match endpoint {
        TransferEndpoint::Local(path) => path.as_str().to_string(),
        TransferEndpoint::Remote(path) => path.as_str().to_string(),
    };
    let source = endpoint_text(&job.source);
    let destination = endpoint_text(&job.destination);
    let name = source.rsplit('/').next().unwrap_or(&source).to_string();
    format!("{name} · {source} → {destination}")
}

fn copy_name(destination: &TransferEndpoint) -> String {
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

fn transfer_history_title(record: &TransferHistoryRecord) -> String {
    let endpoint_text = |endpoint: &TransferEndpoint| match endpoint {
        TransferEndpoint::Local(path) => path.as_str().to_string(),
        TransferEndpoint::Remote(path) => path.as_str().to_string(),
    };
    let source = record
        .sources
        .first()
        .map(endpoint_text)
        .unwrap_or_default();
    let destination = endpoint_text(&record.destination);
    let name = source.rsplit('/').next().unwrap_or(&source).to_string();
    format!("{name} · {source} → {destination}")
}

fn transfer_history_detail(record: &TransferHistoryRecord) -> String {
    match &record.status {
        TransferHistoryStatus::Failed { message, .. } => message.clone(),
        TransferHistoryStatus::Unfinished => {
            "Was running when the app closed — retry to resume".to_string()
        }
        TransferHistoryStatus::Cancelled => "Cancelled".to_string(),
        TransferHistoryStatus::Completed => "Completed".to_string(),
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.workspace_focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let has_tabs = !self.state.tabs.tabs.is_empty();

        let main_area: gpui::AnyElement = if has_tabs {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_pane(PaneSide::Local, window, cx))
                .child(self.render_pane(PaneSide::Remote, window, cx))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(empty_state(
                    "No connections",
                    vec![
                        text_button("open-connection", "New Tab (⌘T)").on_click(cx.listener(
                            |workspace, _event, window, cx| {
                                workspace.open_new_tab(window, cx);
                            },
                        )),
                    ],
                    cx,
                ))
                .into_any_element()
        };

        let workspace_content = if self.surface == WorkspaceSurface::Settings {
            self.render_settings(cx)
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(self.render_tab_bar(cx))
                .child(main_area)
                .when(self.drawer_open, |workspace_root| {
                    workspace_root.child(self.render_transfer_drawer(cx))
                })
                .child(self.render_status_bar(cx))
                .into_any_element()
        };

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.workspace_focus)
            .on_action(cx.listener(|workspace, _: &NewTab, window, cx| {
                workspace.open_new_tab(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &CloseTab, window, cx| {
                if let Some(active_tab_id) = workspace.state.tabs.active_tab_id {
                    workspace.close_tab_by_id(active_tab_id, window, cx);
                }
            }))
            .on_action(cx.listener(|workspace, _: &ActivateNextTab, _window, cx| {
                workspace.activate_tab_in_direction(1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &ActivatePrevTab, _window, cx| {
                workspace.activate_tab_in_direction(-1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &FocusLocalPane, window, cx| {
                workspace.focus_pane(PaneSide::Local, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &FocusRemotePane, window, cx| {
                workspace.focus_pane(PaneSide::Remote, window, cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &ShowTransferDrawer, _window, cx| {
                    workspace.drawer_open = !workspace.drawer_open;
                    cx.notify();
                }),
            )
            .on_action(cx.listener(|workspace, _: &RefreshPane, window, cx| {
                workspace.refresh_focused_pane(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &UploadSelection, _window, cx| {
                workspace.upload_selection(cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &DownloadSelection, _window, cx| {
                    workspace.download_selection(cx);
                }),
            )
            .on_action(cx.listener(|workspace, _: &ReconnectTab, window, cx| {
                workspace.request_connect(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &CancelActiveModal, window, cx| {
                workspace.cancel_active_modal(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SelectNextEntry, _window, cx| {
                workspace.move_selection(workspace.focused_side, 1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SelectPrevEntry, _window, cx| {
                workspace.move_selection(workspace.focused_side, -1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &OpenSelectedEntry, window, cx| {
                workspace.open_selected_entry(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &ParentDirectory, window, cx| {
                workspace.go_to_parent_directory(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &CopyPath, _window, cx| {
                workspace.copy_focused_path(cx);
            }))
            .on_action(cx.listener(|workspace, _: &OpenSettings, window, cx| {
                if workspace.connect_form.is_none()
                    && workspace.active_host_key_prompt().is_none()
                    && workspace.active_transfer_conflict_prompt().is_none()
                {
                    workspace.about_open = false;
                    workspace.surface = WorkspaceSurface::Settings;
                    workspace.workspace_focus.focus(window);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|workspace, _: &ShowAbout, _window, cx| {
                if workspace.connect_form.is_none()
                    && workspace.active_host_key_prompt().is_none()
                    && workspace.active_transfer_conflict_prompt().is_none()
                {
                    workspace.about_open = true;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|workspace, _: &OpenLogFolder, _window, cx| {
                workspace.open_log_folder(cx);
            }))
            .on_action(cx.listener(|workspace, _: &CopyVersionInfo, _window, cx| {
                workspace.copy_version_info(cx);
            }))
            .on_action(cx.listener(|_workspace, _: &MinimizeWindow, window, _cx| {
                window.minimize_window();
            }))
            .on_action(cx.listener(|_workspace, _: &ZoomWindow, window, _cx| {
                window.zoom_window();
            }))
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .bg(theme.colors.background)
            .text_color(theme.colors.text)
            .font_family(theme.fonts.ui_family.clone())
            .child(workspace_content)
            .children(self.render_connect_form_modal(cx))
            .children(self.render_host_key_modal(cx))
            .children(self.render_transfer_conflict_modal(cx))
            .children(self.render_about(cx))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use macsftp_core::{
        AppCommand, AppEvent, AuthCredential, AuthMethodKind, ConflictDecision, ConflictPolicy,
        ConflictRequestId, ConnectionSettings, ConnectionState, DisconnectReason, EntryPath,
        ErrorCode, FileKind, HostKeyPrompt, LocalPath, MetadataPolicy, ProfileId,
        RemoteDirSnapshot, RemoteEntry, RemoteEventScope, RemoteOperationFailure, RemotePath,
        RemoteScoped, RuntimeBridgeConfig, SessionId, TabConnected, TabDisconnected, TabId,
        Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint,
        TransferHistoryRecord, TransferHistoryStatus, TransferId, TransferJob, TransferPlanId,
        TransferPlanProgress, TransferPlanSnapshot, TransferPlanState, TransferState,
        TrustRequestId, UserFacingError,
    };
    use macsftp_sftp::{BridgeChannels, EventReceiver, RuntimeClient};
    use macsftp_storage::AppearancePreference;
    use macsftp_ui::{Appearance, Theme};

    use super::{AppPaths, PaneSide, Workspace, WorkspaceSurface};
    use crate::app_actions::{
        self, ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, NewTab, OpenSettings,
        SelectNextEntry, SelectPrevEntry, ShowAbout, ShowTransferDrawer,
    };

    const TEST_REMOTE_ROOT: &str = "/home/tester";

    fn init_workspace(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, VisualTestContext, BridgeChannels) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
        });
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let receiver = EventReceiver::new(channels.event_rx.clone());
        let app_paths = temp_app_paths();
        let config = macsftp_storage::ConfigStore::with_defaults(app_paths.config_file.clone());
        let window = cx.add_window(|window, cx| {
            Workspace::new(
                client,
                receiver,
                app_paths,
                config,
                macsftp_storage::KeychainStore::new_memory(),
                window,
                cx,
            )
        });
        let workspace = window
            .root(cx)
            .expect("workspace root view should be available");
        let visual_cx = VisualTestContext::from_window(window.into(), cx);
        (workspace, visual_cx, channels)
    }

    /// A unique temp `AppPaths` per call so parallel tests never share a
    /// `profiles.json` (plan §9: tests must not clobber each other's files).
    fn temp_app_paths() -> AppPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let home = format!(
            "{}/macsftp-test-home-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            seq
        );
        let app_paths = AppPaths::from_home_dir(&home);
        // Mirror production main.rs: create the app support dirs so the
        // profiles.json write succeeds (otherwise save hits ENOENT).
        let _ = app_paths.ensure_directories();
        app_paths
    }

    fn test_settings() -> ConnectionSettings {
        ConnectionSettings {
            host: "mock.example.com".to_string(),
            port: 22,
            username: "tester".to_string(),
            auth: AuthCredential::Password {
                password: "secret".to_string(),
            },
        }
    }

    /// Drive the mock connect flow up to the host key prompt: send the
    /// connect command and inject the actor's `HostKeyUnknown` event.
    fn connect_and_prompt(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> HostKeyPrompt {
        workspace.update_in(cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let prompt = HostKeyPrompt {
            request_id: TrustRequestId(1),
            tab_id: TabId(1),
            session_id: SessionId(1),
            session_epoch: 1,
            host: "mock.example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:mock".to_string(),
        };
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::HostKeyUnknown(prompt.clone()), window, cx);
        });
        prompt
    }

    #[gpui::test]
    fn new_tab_and_close_tab_actions_manage_tab_store(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 1, "starts with one tab");
            assert_eq!(workspace.state.tabs.active_tab_id, Some(TabId(1)));
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Empty,
                "new tabs start disconnected"
            );
        });

        cx.dispatch_action(NewTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 2);
            assert_eq!(
                workspace.state.tabs.active_tab_id,
                Some(TabId(2)),
                "new tab becomes active"
            );
        });

        cx.dispatch_action(CloseTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 1);
            assert_eq!(
                workspace.state.tabs.active_tab_id,
                Some(TabId(1)),
                "closing the active tab activates the remaining tab"
            );
        });

        cx.dispatch_action(CloseTab);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.tabs.tabs.is_empty());
            assert_eq!(workspace.state.tabs.active_tab_id, None);
        });
    }

    #[gpui::test]
    fn tab_switching_cycles_through_tabs(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        cx.dispatch_action(NewTab); // tabs: 1, 2 — active 2

        cx.dispatch_action(ActivateNextTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.active_tab_id, Some(TabId(1)));
        });

        cx.dispatch_action(ActivatePrevTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.active_tab_id, Some(TabId(2)));
        });
    }

    #[gpui::test]
    fn keyboard_shortcut_opens_new_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.simulate_keystrokes("cmd-t");
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(
                workspace.state.tabs.tabs.len(),
                2,
                "cmd-t must dispatch NewTab via the Workspace key context"
            );
        });
    }

    #[gpui::test]
    fn keyboard_selection_moves_through_local_entries(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        // Point the local pane at a real, deterministic directory so the
        // selection indices are stable (directory-first sort: beta, alpha, gamma).
        let dir = std::env::temp_dir().join("macsftp_sel_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("alpha.txt"), b"a").expect("write alpha.txt");
        std::fs::create_dir(dir.join("beta")).expect("create beta dir");
        std::fs::write(dir.join("gamma.txt"), b"g").expect("write gamma.txt");
        let local_root = LocalPath::new(dir.to_string_lossy().into_owned());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(local_root.clone(), window, cx);
        });

        cx.dispatch_action(SelectNextEntry);
        cx.dispatch_action(SelectNextEntry);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.focused_side, PaneSide::Local);
            let tab = workspace
                .state
                .tabs
                .active_tab()
                .expect("active tab must exist");
            let expected = EntryPath::Local(tab.local.entries[1].path.clone());
            assert_eq!(tab.selection.selected_paths, vec![expected]);
        });

        cx.dispatch_action(SelectPrevEntry);
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace
                .state
                .tabs
                .active_tab()
                .expect("active tab must exist");
            let expected = EntryPath::Local(tab.local.entries[0].path.clone());
            assert_eq!(tab.selection.selected_paths, vec![expected]);
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn transfer_drawer_toggles_via_action(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.drawer_open, "drawer starts open");
        });

        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(!workspace.drawer_open);
        });

        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.drawer_open);
        });
    }

    #[gpui::test]
    fn settings_action_switches_surface_and_persists_appearance(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(OpenSettings);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.surface, WorkspaceSurface::Settings);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_appearance(AppearancePreference::Light, window, cx);
        });
        workspace.read_with(&cx, |workspace, cx| {
            assert_eq!(
                workspace.config.config().appearance,
                AppearancePreference::Light
            );
            assert_eq!(cx.global::<Theme>().appearance, Appearance::Light);
        });

        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.surface, WorkspaceSurface::Files);
        });
    }

    #[gpui::test]
    fn about_action_opens_and_escape_closes_overlay(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(ShowAbout);
        workspace.read_with(&cx, |workspace, _| assert!(workspace.about_open));

        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| assert!(!workspace.about_open));
    }

    // ── Runtime bridge wiring (M2a/M2c app side) ───────────────────

    #[gpui::test]
    fn connect_sends_command_with_ui_allocated_session(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });

        // The command carries the identity the tab now exposes.
        let command = channels
            .command_rx
            .try_recv()
            .expect("connect command must be enqueued");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.tab_id, TabId(1));
                assert_eq!(connect.session_id, SessionId(1));
                assert_eq!(connect.session_epoch, 1);
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.session_epoch, 1);
            assert_eq!(
                tab.connection,
                ConnectionState::Connecting {
                    session_id: SessionId(1),
                    session_epoch: 1,
                }
            );
        });

        // A second connect while in flight must be a no-op.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no duplicate connect while a session attempt is in flight"
        );
    }

    #[gpui::test]
    fn host_key_prompt_opens_modal_and_accept_returns_to_connecting(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = connect_and_prompt(&workspace, &mut cx);
        let _ = channels.command_rx.try_recv(); // drop the ConnectTab command

        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.modals.active.len(), 1, "modal is open");
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::AwaitingHostKey {
                    session_id: SessionId(1),
                    session_epoch: 1,
                    request_id: prompt.request_id,
                }
            );
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.accept_host_key(prompt.request_id, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("accept command must be enqueued");
        assert!(
            matches!(command, AppCommand::AcceptHostKey(decision) if decision.request_id == prompt.request_id),
            "expected AcceptHostKey with the modal's request id"
        );
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.modals.active.is_empty(), "modal is closed");
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Connecting {
                    session_id: SessionId(1),
                    session_epoch: 1,
                }
            );
        });
    }

    #[gpui::test]
    fn reject_host_key_disconnects_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = connect_and_prompt(&workspace, &mut cx);
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.reject_host_key(prompt.request_id, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("reject command must be enqueued");
        assert!(matches!(
            command,
            AppCommand::RejectHostKey { request_id } if request_id == prompt.request_id
        ));
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.modals.active.is_empty());
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Disconnected {
                    reason: DisconnectReason::UserRequested,
                }
            );
        });
    }

    #[gpui::test]
    fn custom_conflict_rename_validates_and_sends_plan_scoped_decision(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = TransferConflictPrompt {
            request_id: ConflictRequestId(7),
            plan_id: TransferPlanId(3),
            transfer_id: TransferId(9),
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source.txt")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/existing.txt")),
            source_size: Some(11),
            source_modified_at: Some(macsftp_core::Timestamp::from_secs_since_epoch(42)),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::TransferConflict(prompt.clone()), window, cx);
            assert_eq!(workspace.conflict_rename.value(), "existing (copy).txt");
            let pending = workspace
                .state
                .transfers
                .pending_conflicts
                .last()
                .expect("conflict should be recorded for the drawer");
            assert_eq!(pending.source_size, Some(11));
            assert_eq!(
                pending.source_modified_at,
                Some(macsftp_core::Timestamp::from_secs_since_epoch(42))
            );

            workspace.conflict_rename.set_value("..");
            workspace.submit_transfer_rename(false, window, cx);
            assert!(workspace.conflict_rename_error.is_some());
            assert!(workspace.active_transfer_conflict_prompt().is_some());

            workspace.conflict_rename.set_value("renamed.txt");
            workspace.submit_transfer_rename(true, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("custom rename decision must be enqueued");
        let AppCommand::ResolveTransferConflict(command) = command else {
            panic!("expected custom rename decision");
        };
        assert_eq!(command.plan_id, TransferPlanId(3));
        assert_eq!(command.request_id, ConflictRequestId(7));
        assert_eq!(
            command.decision,
            ConflictDecision::Rename {
                new_name: "renamed.txt".to_string(),
                apply_to_all: true,
            }
        );
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.modals.active.is_empty());
            assert!(workspace.conflict_rename_error.is_none());
        });
    }

    #[gpui::test]
    fn stale_host_key_prompt_is_dropped(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        // A prompt from a previous epoch (0) must not open a modal.
        let stale_prompt = HostKeyPrompt {
            request_id: TrustRequestId(99),
            tab_id: TabId(1),
            session_id: SessionId(9),
            session_epoch: 0,
            host: "mock.example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:stale".to_string(),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::HostKeyUnknown(stale_prompt), window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.state.modals.active.is_empty(),
                "stale prompt must not open a modal"
            );
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert!(
                matches!(tab.connection, ConnectionState::Connecting { .. }),
                "connection state must be untouched"
            );
        });
    }

    #[gpui::test]
    fn tab_connected_event_requests_the_real_remote_root(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert!(matches!(tab.connection, ConnectionState::Connected { .. }));
            assert!(tab.remote.entries.is_empty());
            assert_eq!(tab.remote.path, Some(RemotePath::new(TEST_REMOTE_ROOT)));
            assert!(tab.remote.is_refreshing);
        });

        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new(TEST_REMOTE_ROOT)
        ));
    }

    #[gpui::test]
    fn drag_local_file_onto_remote_pane_begins_upload(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        // TabConnected also enqueues a ReadRemoteDir; drain it so the next
        // receive is the transfer we trigger.
        while channels.command_rx.try_recv().is_ok() {}

        // The drop handler on the remote pane delegates to begin_upload with
        // the dragged local path.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_upload(vec![LocalPath::new("/tmp/foo.txt")], cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("drag-to-upload must enqueue a StartTransfer");
        match command {
            AppCommand::StartTransfer(cmd) => {
                assert_eq!(cmd.direction, TransferDirection::Upload);
                assert_eq!(
                    cmd.sources,
                    vec![TransferEndpoint::Local(LocalPath::new("/tmp/foo.txt"))]
                );
                assert_eq!(
                    cmd.destination,
                    TransferEndpoint::Remote(RemotePath::new("/home/tester/foo.txt"))
                );
            }
            other => panic!("expected StartTransfer, got {other:?}"),
        }
    }

    #[gpui::test]
    fn drag_remote_file_onto_local_pane_begins_download(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        // TabConnected also enqueues a ReadRemoteDir; drain it so the next
        // receive is the transfer we trigger.
        while channels.command_rx.try_recv().is_ok() {}

        // The drop handler on the local pane delegates to begin_download with
        // the dragged remote path.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_download(vec![RemotePath::new("/home/tester/bar.txt")], cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("drag-to-download must enqueue a StartTransfer");
        match command {
            AppCommand::StartTransfer(cmd) => {
                assert_eq!(cmd.direction, TransferDirection::Download);
                assert_eq!(
                    cmd.sources,
                    vec![TransferEndpoint::Remote(RemotePath::new(
                        "/home/tester/bar.txt"
                    ))]
                );
                assert!(matches!(cmd.destination, TransferEndpoint::Local(_)));
            }
            other => panic!("expected StartTransfer, got {other:?}"),
        }
    }

    #[gpui::test]
    fn directory_selection_starts_a_download_plan(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let source = RemotePath::new("/home/tester/project");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let tab = workspace.active_tab_mut().expect("active tab should exist");
            tab.local.path = Some(LocalPath::new("/tmp/downloads"));
            tab.remote.entries = vec![RemoteEntry {
                name: "project".to_string(),
                path: source.clone(),
                kind: FileKind::Directory,
                size: None,
                permissions: Some(0o755),
                modified_at: None,
                link_target: None,
            }];
            tab.selection.selected_paths = vec![EntryPath::Remote(source.clone())];
            workspace.download_selection(cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("directory download command must be enqueued");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected directory download transfer command");
        };
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Remote(RemotePath::new(
                "/home/tester/project"
            ))]
        );
        assert_eq!(
            command.destination,
            TransferEndpoint::Local(LocalPath::new("/tmp/downloads/project"))
        );
    }

    #[gpui::test]
    fn older_directory_result_does_not_replace_a_newer_requested_path(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let older_path = RemotePath::new("/home/tester/older");
        let newer_path = RemotePath::new("/home/tester/newer");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.request_remote_directory(TabId(1), older_path.clone(), cx);
            workspace.request_remote_directory(TabId(1), newer_path.clone(), cx);
        });
        let _ = channels.command_rx.try_recv();
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope.clone(),
                    RemoteDirSnapshot {
                        path: older_path,
                        entries: vec![RemoteEntry {
                            name: "stale.txt".to_string(),
                            path: RemotePath::new("/home/tester/older/stale.txt"),
                            kind: FileKind::File,
                            size: Some(1),
                            permissions: Some(0o644),
                            modified_at: None,
                            link_target: None,
                        }],
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.remote.path.as_ref(), Some(&newer_path));
            assert!(
                tab.remote.entries.is_empty(),
                "stale listing must be ignored"
            );
            assert!(tab.remote.is_refreshing, "newer request is still loading");
        });
    }

    #[gpui::test]
    fn current_directory_failure_shows_recoverable_pane_error(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let missing_path = RemotePath::new("/home/tester/missing");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.request_remote_directory(TabId(1), missing_path.clone(), cx);
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteOperationFailed(RemoteScoped::new(
                    scope,
                    RemoteOperationFailure {
                        operation: "Read remote directory".to_string(),
                        path: Some(missing_path.clone()),
                        error: UserFacingError::new(
                            ErrorCode::NotFound,
                            "Remote directory not found",
                            "The directory no longer exists. Go to its parent directory or refresh.",
                        )
                        .with_retryable(true),
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.remote.path.as_ref(), Some(&missing_path));
            assert!(!tab.remote.is_refreshing);
            assert_eq!(
                tab.remote.error.as_ref().map(|error| error.code),
                Some(ErrorCode::NotFound)
            );
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Remote;
            workspace.refresh_focused_pane(window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path }) if path == missing_path
        ));
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert!(tab.remote.is_refreshing);
            assert!(tab.remote.error.is_none(), "retry clears the pane error");
        });
    }

    #[gpui::test]
    fn remote_listing_applies_the_tab_sort(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope,
                    RemoteDirSnapshot {
                        path: RemotePath::new(TEST_REMOTE_ROOT),
                        entries: vec![
                            RemoteEntry {
                                name: "zeta.txt".to_string(),
                                path: RemotePath::new("/home/tester/zeta.txt"),
                                kind: FileKind::File,
                                size: Some(1),
                                permissions: Some(0o644),
                                modified_at: None,
                                link_target: None,
                            },
                            RemoteEntry {
                                name: "alpha-dir".to_string(),
                                path: RemotePath::new("/home/tester/alpha-dir"),
                                kind: FileKind::Directory,
                                size: None,
                                permissions: Some(0o755),
                                modified_at: None,
                                link_target: None,
                            },
                            RemoteEntry {
                                name: "alpha.txt".to_string(),
                                path: RemotePath::new("/home/tester/alpha.txt"),
                                kind: FileKind::File,
                                size: Some(1),
                                permissions: Some(0o644),
                                modified_at: None,
                                link_target: None,
                            },
                        ],
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            let names: Vec<&str> = tab
                .remote
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect();
            assert_eq!(names, ["alpha-dir", "alpha.txt", "zeta.txt"]);
        });
    }

    #[gpui::test]
    fn remote_navigation_and_refresh_keep_the_previous_listing_visible(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let child_path = RemotePath::new("/home/tester/child");
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope,
                    RemoteDirSnapshot {
                        path: RemotePath::new(TEST_REMOTE_ROOT),
                        entries: vec![RemoteEntry {
                            name: "child".to_string(),
                            path: child_path.clone(),
                            kind: FileKind::Directory,
                            size: None,
                            permissions: Some(0o755),
                            modified_at: None,
                            link_target: None,
                        }],
                    },
                )),
                window,
                cx,
            );
            workspace.focused_side = PaneSide::Remote;
            workspace.open_entry_at(PaneSide::Remote, 0, window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path }) if path == child_path
        ));
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.remote.path.as_ref(), Some(&child_path));
            assert_eq!(tab.remote.entries.len(), 1);
            assert!(tab.remote.is_refreshing);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.go_to_parent_directory(window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new(TEST_REMOTE_ROOT)
        ));

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.refresh_focused_pane(window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new(TEST_REMOTE_ROOT)
        ));
    }

    #[gpui::test]
    fn disconnect_event_clears_remote_pane_and_expires_modal(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _prompt = connect_and_prompt(&workspace, &mut cx);
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    scope,
                    TabDisconnected {
                        reason: DisconnectReason::ConnectionLost,
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Disconnected {
                    reason: DisconnectReason::ConnectionLost,
                }
            );
            assert!(tab.remote.entries.is_empty());
            assert!(
                workspace.state.modals.active.is_empty(),
                "host key modal must expire when its session disconnects"
            );
        });
    }

    #[gpui::test]
    fn request_connect_opens_form_then_reuses_session_credentials(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        // First connect request: no credentials yet — the form opens
        // and no command is sent.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.request_connect(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.connect_form.is_some(), "form must open");
        });
        assert!(channels.command_rx.try_recv().is_err(), "no command yet");

        // Fill the form and submit.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let form = workspace.connect_form.as_mut().expect("form is open");
            form.host.set_value("server.example.com");
            form.port.set_value("2200");
            form.username.set_value("alex");
            form.password.set_value("secret");
            workspace.submit_connect_form(window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("submit must send ConnectTab");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.settings.host, "server.example.com");
                assert_eq!(connect.settings.port, 2200);
                assert_eq!(connect.settings.username, "alex");
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.connect_form.is_none(), "form closes on submit");
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.title, "server.example.com");
            assert!(matches!(tab.connection, ConnectionState::Connecting { .. }));
        });

        // Simulate a disconnect, then reconnect: credentials are reused
        // without reopening the form.
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    scope,
                    TabDisconnected {
                        reason: DisconnectReason::ConnectionLost,
                    },
                )),
                window,
                cx,
            );
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.request_connect(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.connect_form.is_none(),
                "cached credentials must skip the form"
            );
        });
        let command = channels
            .command_rx
            .try_recv()
            .expect("reconnect must send ConnectTab");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.settings.host, "server.example.com");
                assert_eq!(connect.session_epoch, 2, "reconnect bumps the epoch");
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }
    }

    #[gpui::test]
    fn connect_form_validates_before_sending(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });

        // Empty host: submit is rejected with an inline error.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.submit_connect_form(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            let form = workspace.connect_form.as_ref().expect("form stays open");
            assert!(form.error.is_some(), "validation error must be shown");
        });
        assert!(channels.command_rx.try_recv().is_err());

        // Bad port is rejected too.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let form = workspace.connect_form.as_mut().expect("form is open");
            form.host.set_value("h");
            form.username.set_value("u");
            form.port.set_value("99999");
            workspace.submit_connect_form(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace
                    .connect_form
                    .as_ref()
                    .expect("open")
                    .error
                    .is_some()
            );
        });
        assert!(channels.command_rx.try_recv().is_err());
    }

    #[gpui::test]
    fn escape_closes_connect_form(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });

        cx.simulate_keystrokes("escape");
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.connect_form.is_none(),
                "escape must close the connect form"
            );
        });
    }

    /// Saving, reusing, updating, and deleting profiles through the
    /// connect form must persist to disk and round-trip (plan §5/§22).
    #[gpui::test]
    fn connect_form_save_use_update_and_delete_profile(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });
        // Fill a valid connection (including a password) and save it.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace.connect_form.as_mut().expect("form is open");
            form.host.set_value("example.com");
            form.username.set_value("alex");
            form.password.set_value("hunter2");
            form.profile_name.set_value("My Server");
            workspace.save_current_profile(cx);
        });

        let saved_id = workspace.read_with(&cx, |workspace, _| {
            let profiles = workspace.profiles.profiles();
            assert_eq!(profiles.len(), 1, "one profile saved");
            assert_eq!(profiles[0].name, "My Server");
            assert_eq!(profiles[0].host, "example.com");
            assert_eq!(profiles[0].username, "alex");
            profiles[0].id
        });

        // The save also flushed to disk: reopening the store reads it back.
        let path = workspace.read_with(&cx, |workspace, _| workspace.profiles.path().clone());
        let reloaded =
            macsftp_storage::ProfileStore::open(path).expect("reload saved profiles.json");
        assert_eq!(reloaded.profiles().len(), 1, "profile persisted to disk");

        // The secret was written to the Keychain, not just the profile
        // file (plan §11). The profile on disk holds only the SecretRef.
        let secret_ref = macsftp_core::SecretRef::keychain_ref(saved_id, "password");
        let stored = workspace.read_with(&cx, |workspace, _| {
            workspace.keychain.load(&secret_ref).expect("load secret")
        });
        assert_eq!(
            stored,
            Some("hunter2".to_string()),
            "secret stored in Keychain"
        );

        // "Use" prefills the form (including the restored secret) and
        // remembers the source profile id.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.use_profile(saved_id, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form
                .as_ref()
                .expect("form open after use");
            assert_eq!(form.host.value(), "example.com");
            assert_eq!(form.source_profile_id, Some(saved_id));
            assert_eq!(form.auth_method, AuthMethodKind::Password);
            assert_eq!(
                form.password.value(),
                "hunter2",
                "secret restored from Keychain into the form"
            );
        });

        // Re-saving the used form updates the same entry, not a duplicate.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.save_current_profile(cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(
                workspace.profiles.profiles().len(),
                1,
                "re-save updates, does not duplicate"
            );
        });

        // Deleting removes it from store and disk.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.delete_profile(saved_id, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.profiles.profiles().is_empty(),
                "profile deleted from store"
            );
        });
        let path = workspace.read_with(&cx, |workspace, _| workspace.profiles.path().clone());
        let reloaded = macsftp_storage::ProfileStore::open(path).expect("reload after delete");
        assert!(reloaded.profiles().is_empty(), "profile deleted from disk");
    }

    #[gpui::test]
    fn host_key_mismatch_fails_tab_and_blocks(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::HostKeyMismatch(macsftp_core::HostKeyMismatch {
                    tab_id: TabId(1),
                    host: "mock.example.com".to_string(),
                    port: 22,
                    expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
                    actual_fingerprint_sha256: "SHA256:actual".to_string(),
                }),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            match &tab.connection {
                ConnectionState::Failed { error } => {
                    assert_eq!(error.code, macsftp_core::ErrorCode::HostKeyMismatch);
                    assert!(
                        !error.retryable,
                        "mismatch must not be presented as retryable"
                    );
                }
                other => panic!("expected Failed connection state, got {other:?}"),
            }
            assert!(
                workspace.state.modals.active.is_empty(),
                "mismatch must not open a trust modal"
            );
        });
    }

    #[gpui::test]
    fn close_tab_notifies_runtime(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        cx.dispatch_action(CloseTab);

        let command = channels
            .command_rx
            .try_recv()
            .expect("close command must be enqueued");
        assert!(matches!(command, AppCommand::CloseTab { tab_id: TabId(1) }));
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.tabs.tabs.is_empty());
        });
    }

    #[gpui::test]
    fn flush_persists_running_transfer_as_unfinished_history(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        // Simulate an in-flight upload plan in the live transfer store.
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            let plan_id = macsftp_core::TransferPlanId(7);
            let root_job_id = macsftp_core::TransferId(7);
            let now = macsftp_core::Timestamp::unix_epoch();
            workspace
                .state
                .transfers
                .plans
                .push(macsftp_core::TransferPlan {
                    id: plan_id,
                    root_job_id,
                    source_root: TransferEndpoint::Local(LocalPath::new("/tmp/report.pdf")),
                    destination_root: TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf")),
                    state: macsftp_core::TransferPlanState::Running,
                    planned_count: 1,
                    total_bytes: None,
                    child_jobs: vec![root_job_id],
                    conflict_policy: ConflictPolicy::Ask,
                });
            workspace
                .state
                .transfers
                .jobs
                .push(macsftp_core::TransferJob {
                    id: root_job_id,
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/report.pdf")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf")),
                    state: macsftp_core::TransferState::Running {
                        bytes_done: 0,
                        bytes_total: None,
                        started_at: now,
                    },
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: now,
                });
        });

        // The quit hook path: flush the live transfers to disk.
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace.flush_transfer_history();
        });

        workspace.read_with(&cx, |workspace, _| {
            let records = workspace.transfer_history.records();
            assert_eq!(records.len(), 1, "one in-flight plan persisted");
            assert!(matches!(
                records[0].status,
                TransferHistoryStatus::Unfinished
            ));
            assert_eq!(records[0].direction, TransferDirection::Upload);
            assert_eq!(records[0].sources.len(), 1);
            assert_eq!(
                records[0].destination,
                TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf"))
            );
        });

        // And it survives a reload from disk.
        let reopened = macsftp_storage::TransferHistoryStore::open_or_empty(
            workspace.read_with(&cx, |workspace, _| {
                workspace.transfer_history.path().clone()
            }),
        );
        assert_eq!(reopened.records().len(), 1);
        assert!(matches!(
            reopened.records()[0].status,
            TransferHistoryStatus::Unfinished
        ));
        // A second flush must not duplicate (guarded by the flushed flag).
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace.flush_transfer_history();
        });
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.transfer_history.records().len(), 1);
        });
    }

    #[gpui::test]
    fn retry_history_transfer_rebuilds_command_for_connected_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        // Establish a connected active tab by mutating connection state
        // directly (the host-key handshake is exercised by other tests).
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            let active_tab_id = workspace
                .state
                .tabs
                .active_tab_id
                .expect("active tab exists");
            let tab = workspace
                .state
                .tabs
                .find_tab_mut(active_tab_id)
                .expect("active tab exists");
            tab.profile_id = Some(ProfileId(5));
            tab.connection = ConnectionState::Connected {
                session_id: SessionId(2),
                session_epoch: 3,
                connected_at: Timestamp::unix_epoch(),
            };
        });

        // Seed a retryable (unfinished) history record.
        let record_id = macsftp_core::TransferHistoryId(11);
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace
                .transfer_history
                .append(vec![TransferHistoryRecord {
                    id: record_id,
                    profile_id: None,
                    direction: TransferDirection::Download,
                    sources: vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))],
                    destination: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    status: TransferHistoryStatus::Unfinished,
                    started_at: Timestamp::unix_epoch(),
                    last_updated: Timestamp::unix_epoch(),
                }]);
        });

        let removed = workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.retry_history_transfer(record_id, cx)
        });
        assert!(removed, "retry should enqueue a transfer");

        let command = channels
            .command_rx
            .try_recv()
            .expect("a StartTransfer command must be enqueued");
        match command {
            AppCommand::StartTransfer(cmd) => {
                assert_eq!(cmd.direction, TransferDirection::Download);
                assert_eq!(
                    cmd.sources,
                    vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))]
                );
                assert_eq!(
                    cmd.destination,
                    TransferEndpoint::Local(LocalPath::new("/tmp/a.txt"))
                );
                // Rebuilt against the live connected tab.
                assert_eq!(cmd.tab_id, TabId(1));
                assert_eq!(cmd.session_epoch, 3);
                assert_eq!(cmd.profile_id, ProfileId(5));
            }
            other => panic!("expected StartTransfer, got {other:?}"),
        }

        // The record is removed from history once retried.
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.transfer_history.find(record_id).is_none());
        });
    }

    #[gpui::test]
    fn retry_history_transfer_requires_connected_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        let record_id = macsftp_core::TransferHistoryId(12);
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace
                .transfer_history
                .append(vec![TransferHistoryRecord {
                    id: record_id,
                    profile_id: None,
                    direction: TransferDirection::Download,
                    sources: vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))],
                    destination: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    status: TransferHistoryStatus::Unfinished,
                    started_at: Timestamp::unix_epoch(),
                    last_updated: Timestamp::unix_epoch(),
                }]);
        });

        // No connected tab → retry is refused and nothing is enqueued.
        let enqueued = workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.retry_history_transfer(record_id, cx)
        });
        assert!(!enqueued);
        assert!(channels.command_rx.try_recv().is_err());
        // Record remains so the user can retry later.
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.transfer_history.find(record_id).is_some());
        });
    }

    #[gpui::test]
    fn completed_child_jobs_finalize_root_plan(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let now = Timestamp(std::time::SystemTime::now());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let plan_id = TransferPlanId(1);
            let root_job_id = TransferId(1);
            let _child_job_id = TransferId(2);

            let plan = macsftp_core::TransferPlan {
                id: plan_id,
                root_job_id,
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/src")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/tmp/dest")),
                state: TransferPlanState::Planning,
                planned_count: 0,
                total_bytes: None,
                child_jobs: Vec::new(),
                conflict_policy: ConflictPolicy::default(),
            };
            let root_job = TransferJob {
                id: root_job_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/src")),
                destination: TransferEndpoint::Remote(RemotePath::new("/tmp/dest")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: now,
            };
            workspace.handle_app_event(
                AppEvent::TransferPlanStarted(TransferPlanSnapshot { plan, root_job }),
                window,
                cx,
            );

            workspace.handle_app_event(AppEvent::TransferPlanCompleted { plan_id }, window, cx);

            // Without children, planning completion should finalize the root
            // job immediately so it does not linger in the Queued section.
            let root_job = workspace
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = workspace
                .state
                .transfers
                .plans
                .iter()
                .find(|p| p.id == plan_id)
                .expect("plan exists");
            assert_eq!(plan.state, TransferPlanState::Completed);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let plan_id = TransferPlanId(2);
            let root_job_id = TransferId(10);
            let child_job_id = TransferId(11);

            let plan = macsftp_core::TransferPlan {
                id: plan_id,
                root_job_id,
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/src2")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/tmp/dest2")),
                state: TransferPlanState::Planning,
                planned_count: 0,
                total_bytes: None,
                child_jobs: vec![child_job_id],
                conflict_policy: ConflictPolicy::default(),
            };
            let root_job = TransferJob {
                id: root_job_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/src2")),
                destination: TransferEndpoint::Remote(RemotePath::new("/tmp/dest2")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: now,
            };
            workspace.handle_app_event(
                AppEvent::TransferPlanStarted(TransferPlanSnapshot { plan, root_job }),
                window,
                cx,
            );
            workspace.handle_app_event(AppEvent::TransferPlanCompleted { plan_id }, window, cx);

            // Plan completion with children keeps root in Queued.
            let root_job = workspace
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Queued);

            let child_job = TransferJob {
                id: child_job_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/src2/file.txt")),
                destination: TransferEndpoint::Remote(RemotePath::new("/tmp/dest2/file.txt")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: now,
            };
            workspace.handle_app_event(
                AppEvent::TransferPlanProgress(TransferPlanProgress {
                    plan_id,
                    child_job,
                    planned_count: 1,
                    total_bytes: Some(100),
                }),
                window,
                cx,
            );
            workspace.handle_app_event(
                AppEvent::TransferCompleted {
                    transfer_id: child_job_id,
                },
                window,
                cx,
            );

            // After the last child completes, the root plan/job must finalize.
            let root_job = workspace
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = workspace
                .state
                .transfers
                .plans
                .iter()
                .find(|p| p.id == plan_id)
                .expect("plan exists");
            assert_eq!(plan.state, TransferPlanState::Completed);
        });
    }
}
