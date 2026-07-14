use std::collections::HashMap;
use std::path::Path;

use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, Subscription, UniformListScrollHandle, Window, div, prelude::*,
};
use macsftp_core::{
    AppCommand, AppState, CommandDispatchError, ConnectCommand, ConnectionSettings,
    ConnectionState, EntryPath, LocalPath, ProfileId, RemotePath, TabId, TabState, WindowSessionId,
};
use macsftp_sftp::RuntimeClient;
use macsftp_storage::{
    AppearancePreference, RecentEntryInput, SessionTabSnapshot, SessionWindowSnapshot,
};
use macsftp_ui::{ActiveTheme, InputState, Theme, empty_state, text_button};
use tracing::warn;

use crate::app_actions::{
    ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, ConfirmTabSwitcher, CopyPath,
    CopyVersionInfo, DeleteSelection, DownloadSelection, FilterPane, FocusLocalPane,
    FocusRemotePane, GoToPath, MinimizeWindow, NavigateBack, NavigateForward, NewFolder, NewTab,
    OpenCommandPalette, OpenLogFolder, OpenProfiles, OpenSelectedEntry, OpenSettings, PageDown,
    PageUp, ParentDirectory, ReconnectTab, RefreshPane, RenameEntry, SelectAllEntries,
    SelectFirstEntry, SelectLastEntry, SelectNextEntry, SelectNextEntryExtend, SelectPrevEntry,
    SelectPrevEntryExtend, ShowAbout, ShowTransferDrawer, TabSwitcherNext, TabSwitcherPrev,
    ToggleHiddenFiles, UploadSelection, ZoomWindow,
};
use crate::resources::ActiveResources;
use crate::workspace::file_ops::{ContextMenuState, DeleteConfirmState, InlineEditState};
use crate::workspace::nav::{HistoryOp, TabNavState};
use crate::workspace::profiles::{ProfileEditorState, SettingsSection};

/// Status bar copy when the command channel is full (non-blocking drop).
pub(crate) const STATUS_BUSY_TRY_AGAIN: &str = "Busy — try again in a moment.";
/// Status bar copy when the connection service can no longer accept commands.
pub(crate) const STATUS_CONNECTION_SERVICE_UNAVAILABLE: &str = "Connection service is unavailable.";

/// Which file pane an action targets. Local is the default focus side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSide {
    Local,
    Remote,
}

/// Per-pane type-to-filter / cmd-f state (view-only; not persisted).
#[derive(Debug, Clone, Default)]
pub(crate) struct PaneFilter {
    pub(crate) query: String,
    pub(crate) input: InputState,
    /// `true` after `cmd-f`: key events prefer the filter input.
    pub(crate) explicit_focus: bool,
}

impl PaneFilter {
    pub(crate) fn is_active(&self) -> bool {
        !self.query.is_empty() || self.explicit_focus
    }

    pub(crate) fn clear(&mut self) {
        self.query.clear();
        self.input.clear();
        self.explicit_focus = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceSurface {
    Files,
    Settings,
}

/// Non-secret connection target restored from `session.json` for form
/// prefill and post-connect remote path navigation.
#[derive(Debug, Clone)]
pub(crate) struct RestoredTabTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_id: Option<ProfileId>,
    pub remote_path: Option<RemotePath>,
}

pub struct Workspace {
    window_session_id: WindowSessionId,
    state: AppState,
    runtime_client: RuntimeClient,
    workspace_focus: FocusHandle,
    local_pane_focus: FocusHandle,
    remote_pane_focus: FocusHandle,
    modal_focus: FocusHandle,
    focused_side: PaneSide,
    surface: WorkspaceSurface,
    /// Active sidebar section when `surface == Settings`.
    settings_section: SettingsSection,
    /// Free-text filter for the Settings → Profiles list.
    profile_filter: InputState,
    /// `true` while the Profiles filter field owns key events.
    profile_filter_focused: bool,
    /// Selected profile in Settings → Profiles (None when the list is empty).
    selected_profile_id: Option<ProfileId>,
    /// Draft for New / Edit profile in Settings → Profiles.
    profile_editor: Option<ProfileEditorState>,
    /// Pending profile delete confirmation (Settings / Connect). View-local.
    profile_delete_confirm: Option<ProfileId>,
    about_open: bool,
    drawer_open: bool,
    drawer_height: Pixels,
    /// Active transfer-drawer resize drag (session-only; cleared on mouse up).
    drawer_resize: Option<drawer_height::TransferDrawerResize>,
    completed_section_expanded: bool,
    failed_section_expanded: bool,
    local_scroll: UniformListScrollHandle,
    remote_scroll: UniformListScrollHandle,
    default_local_path: LocalPath,
    status_message: Option<SharedString>,
    config_error: Option<SharedString>,
    log_file: LocalPath,
    connect_form: Option<ConnectForm>,
    connect_form_focus: FocusHandle,
    /// Draft name for the active transfer-conflict rename decision.
    /// It is view-local because the runtime only receives a decision
    /// after the user submits it.
    conflict_rename: InputState,
    conflict_rename_error: Option<SharedString>,
    /// Phase 1 delete confirmation modal (view-local).
    delete_confirm: Option<DeleteConfirmState>,
    /// Phase 1 inline rename / new-folder editor.
    inline_edit: Option<InlineEditState>,
    /// Phase 1 context menu for the focused file pane.
    context_menu: Option<ContextMenuState>,
    /// Go to Path modal (`cmd-shift-g`).
    go_to_path_open: bool,
    go_to_path_input: InputState,
    go_to_path_error: Option<SharedString>,
    /// Command palette (`cmd-shift-p`).
    palette_open: bool,
    palette_input: InputState,
    palette_selected: usize,
    /// MRU tab order for ctrl-tab switcher only (front = most recent).
    /// `cmd-shift-[/]` still walk creation order in `state.tabs.tabs`.
    tab_mru: Vec<TabId>,
    tab_switcher_open: bool,
    tab_switcher_index: usize,
    /// Active-tab type-to-filter state (cleared on tab switch / navigate).
    local_filter: PaneFilter,
    remote_filter: PaneFilter,
    /// Anchor path for shift-range multi-select (view-local; single-select resets it).
    selection_anchor: Option<EntryPath>,
    /// Session credentials per tab, kept in memory only so Reconnect
    /// works without re-typing. Replaced by Keychain-backed profiles.
    tab_settings: HashMap<TabId, ConnectionSettings>,
    /// Per-tab local/remote back-forward history (session-only).
    tab_nav: HashMap<TabId, TabNavState>,
    /// Connection meta restored from `session.json` (form prefill / path).
    restored_targets: HashMap<TabId, RestoredTabTarget>,
    _appearance_subscription: Subscription,
}

impl Workspace {
    pub fn new(
        runtime_client: RuntimeClient,
        window_session_id: WindowSessionId,
        restore_session: Option<SessionWindowSnapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_local_path =
            LocalPath::new(std::env::var("HOME").unwrap_or_else(|_| "/Users/alex".to_string()));

        let state = AppState::new();
        let config_error = cx
            .resources()
            .config
            .initial_error()
            .map(|_| SharedString::from("Could not load config.json; using defaults."));
        let log_file = cx.resources().app_paths.log_file.clone();

        let appearance_subscription =
            cx.observe_window_appearance(window, |_workspace, window, cx| {
                if cx.resources().config.config().appearance == AppearancePreference::System {
                    cx.set_global(Theme::for_appearance(window.appearance()));
                    cx.notify();
                }
            });

        let mut workspace = Self {
            window_session_id,
            state,
            runtime_client,
            workspace_focus: cx.focus_handle(),
            local_pane_focus: cx.focus_handle(),
            remote_pane_focus: cx.focus_handle(),
            modal_focus: cx.focus_handle(),
            focused_side: PaneSide::Local,
            surface: WorkspaceSurface::Files,
            settings_section: SettingsSection::General,
            profile_filter: InputState::new(),
            profile_filter_focused: false,
            selected_profile_id: None,
            profile_editor: None,
            profile_delete_confirm: None,
            about_open: false,
            drawer_open: true,
            drawer_height: drawer_height::DEFAULT_DRAWER_HEIGHT,
            drawer_resize: None,
            completed_section_expanded: false,
            failed_section_expanded: false,
            local_scroll: UniformListScrollHandle::new(),
            remote_scroll: UniformListScrollHandle::new(),
            default_local_path,
            status_message: None,
            config_error,
            log_file,
            connect_form: None,
            connect_form_focus: cx.focus_handle(),
            conflict_rename: InputState::new(),
            conflict_rename_error: None,
            delete_confirm: None,
            inline_edit: None,
            context_menu: None,
            go_to_path_open: false,
            go_to_path_input: InputState::new(),
            go_to_path_error: None,
            palette_open: false,
            palette_input: InputState::new(),
            palette_selected: 0,
            tab_mru: Vec::new(),
            tab_switcher_open: false,
            tab_switcher_index: 0,
            local_filter: PaneFilter::default(),
            remote_filter: PaneFilter::default(),
            selection_anchor: None,
            tab_settings: HashMap::new(),
            tab_nav: HashMap::new(),
            restored_targets: HashMap::new(),
            _appearance_subscription: appearance_subscription,
        };
        // Restored tabs remain disconnected until the user explicitly connects.
        if let Some(snapshot) = restore_session.as_ref() {
            workspace.restore_session_tabs(snapshot, window, cx);
        }
        if workspace.state.tabs.tabs.is_empty() {
            workspace.open_new_tab(window, cx);
        }
        workspace.update_window_title(window);
        workspace
    }

    /// Pure format helper for the window title (unit-testable without GPUI).
    pub(crate) fn window_title_for_active_tab(tab_title: Option<&str>) -> String {
        match tab_title {
            Some(title) if !title.is_empty() => format!("{title} — macSFTP"),
            _ => "macSFTP".to_string(),
        }
    }

    pub(crate) fn update_window_title(&self, window: &mut Window) {
        let title =
            Self::window_title_for_active_tab(self.active_tab().map(|tab| tab.title.as_str()));
        window.set_window_title(&title);
    }

    pub(crate) fn set_drawer_height(&mut self, height: Pixels, viewport_height: Pixels) {
        let content = drawer_height::content_area_height_from_viewport(viewport_height);
        self.drawer_height = drawer_height::clamp_drawer_height(height, content);
    }

    pub(crate) fn reset_drawer_height(&mut self, viewport_height: Pixels) {
        self.set_drawer_height(drawer_height::DEFAULT_DRAWER_HEIGHT, viewport_height);
    }

    pub(crate) fn reclamp_drawer_height(&mut self, viewport_height: Pixels) {
        self.set_drawer_height(self.drawer_height, viewport_height);
    }

    /// Rebuild disconnected tabs from one window snapshot. Does not send ConnectTab.
    fn restore_session_tabs(
        &mut self,
        snapshot: &SessionWindowSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if snapshot.tabs.is_empty() {
            return;
        }
        let mut restored_ids = Vec::new();
        for snap in &snapshot.tabs {
            let tab_id = cx.resources().next_tab_id();
            let title = if snap.title.is_empty() {
                snap.host.clone()
            } else {
                snap.title.clone()
            };
            let mut tab = TabState::new(tab_id, title);
            tab.profile_id = snap.profile_id.map(ProfileId);
            tab.connection = ConnectionState::Empty;
            if let Some(local) = &snap.local_path {
                let path = LocalPath::new(local.clone());
                if let Some(message) = Self::load_local_directory(&path, &mut tab) {
                    self.status_message = Some(message.into());
                }
            } else if let Some(message) =
                Self::load_local_directory(&self.default_local_path, &mut tab)
            {
                self.status_message = Some(message.into());
            }
            if let Some(remote) = &snap.remote_path {
                tab.remote.path = Some(RemotePath::new(remote.clone()));
                tab.remote.entries.clear();
            }
            self.restored_targets.insert(
                tab_id,
                RestoredTabTarget {
                    host: snap.host.clone(),
                    port: snap.port,
                    username: snap.username.clone(),
                    profile_id: snap.profile_id.map(ProfileId),
                    remote_path: snap
                        .remote_path
                        .as_ref()
                        .map(|path| RemotePath::new(path.clone())),
                },
            );
            self.state.tabs.open_tab(tab);
            restored_ids.push(tab_id);
            self.touch_mru(tab_id);
        }
        let active_index = snapshot
            .active_tab_index
            .min(restored_ids.len().saturating_sub(1));
        if let Some(active_id) = restored_ids.get(active_index).copied() {
            self.state.tabs.active_tab_id = Some(active_id);
            self.touch_mru(active_id);
        }
        self.clear_filters();
        self.reset_scroll_positions();
        self.focus_pane(PaneSide::Local, window, cx);
        cx.notify();
    }

    pub(crate) fn build_session_snapshot(&self) -> SessionWindowSnapshot {
        let tabs: Vec<SessionTabSnapshot> = self
            .state
            .tabs
            .tabs
            .iter()
            .map(|tab| {
                let settings = self.tab_settings.get(&tab.id);
                let restored = self.restored_targets.get(&tab.id);
                SessionTabSnapshot {
                    title: tab.title.clone(),
                    profile_id: tab.profile_id.map(|id| id.0),
                    host: settings
                        .map(|s| s.host.clone())
                        .or_else(|| restored.map(|r| r.host.clone()))
                        .unwrap_or_else(|| tab.title.clone()),
                    port: settings
                        .map(|s| s.port)
                        .or_else(|| restored.map(|r| r.port))
                        .unwrap_or(22),
                    username: settings
                        .map(|s| s.username.clone())
                        .or_else(|| restored.map(|r| r.username.clone()))
                        .unwrap_or_default(),
                    local_path: tab.local.path.as_ref().map(|p| p.as_str().to_string()),
                    remote_path: tab
                        .remote
                        .path
                        .as_ref()
                        .map(|p| p.as_str().to_string())
                        .or_else(|| {
                            restored.and_then(|r| {
                                r.remote_path.as_ref().map(|path| path.as_str().to_string())
                            })
                        }),
                }
            })
            .collect();
        let active_tab_index = self
            .state
            .tabs
            .active_tab_id
            .and_then(|id| self.state.tabs.tabs.iter().position(|t| t.id == id))
            .unwrap_or(0);
        SessionWindowSnapshot {
            id: self.window_session_id,
            active_tab_index,
            tabs,
        }
    }
    pub(crate) fn active_tab(&self) -> Option<&TabState> {
        self.state.tabs.active_tab()
    }
    pub(crate) fn active_tab_mut(&mut self) -> Option<&mut TabState> {
        let active_tab_id = self.state.tabs.active_tab_id?;
        self.state.tabs.find_tab_mut(active_tab_id)
    }
    pub(crate) fn pane_focus(&self, side: PaneSide) -> &FocusHandle {
        match side {
            PaneSide::Local => &self.local_pane_focus,
            PaneSide::Remote => &self.remote_pane_focus,
        }
    }
    pub(crate) fn scroll_handle(&self, side: PaneSide) -> &UniformListScrollHandle {
        match side {
            PaneSide::Local => &self.local_scroll,
            PaneSide::Remote => &self.remote_scroll,
        }
    }
    pub(crate) fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab_id = cx.resources().next_tab_id();
        let mut tab = TabState::new(tab_id, "New Connection");
        if let Some(message) = Self::load_local_directory(&self.default_local_path, &mut tab) {
            self.status_message = Some(message.into());
        }
        self.state.tabs.open_tab(tab);
        self.state.tabs.active_tab_id = Some(tab_id);
        self.touch_mru(tab_id);
        self.clear_filters();
        self.reset_scroll_positions();
        self.focus_pane(PaneSide::Local, window, cx);
        self.update_window_title(window);
        cx.notify();
    }
    pub(crate) fn close_tab_by_id(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.tabs.close_tab(tab_id).is_none() {
            return;
        }
        self.tab_mru.retain(|id| *id != tab_id);
        self.tab_nav.remove(&tab_id);
        self.restored_targets.remove(&tab_id);
        // Keep MRU front aligned with the newly active tab after close
        // (TabStore promotes another tab without going through activate_tab).
        if let Some(active_id) = self.state.tabs.active_tab_id {
            self.touch_mru(active_id);
        }
        if self.tab_switcher_open {
            if self.tab_mru.is_empty() {
                self.tab_switcher_open = false;
                self.tab_switcher_index = 0;
            } else {
                self.tab_switcher_index = self.tab_switcher_index.min(self.tab_mru.len() - 1);
            }
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
        self.update_window_title(window);
        cx.notify();
    }
    pub(crate) fn activate_tab(
        &mut self,
        tab_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.tabs.find_tab(tab_id).is_some() {
            self.state.tabs.active_tab_id = Some(tab_id);
            self.touch_mru(tab_id);
            if self.tab_switcher_open {
                self.tab_switcher_open = false;
                self.tab_switcher_index = 0;
            }
            self.clear_filters();
            self.selection_anchor = None;
            self.reset_scroll_positions();
            self.update_window_title(window);
            cx.notify();
        }
    }
    /// Move `tab_id` to the front of the MRU list (most recently used).
    pub(crate) fn touch_mru(&mut self, tab_id: TabId) {
        self.tab_mru.retain(|id| *id != tab_id);
        self.tab_mru.insert(0, tab_id);
    }
    /// Creation-order cycle for `cmd-shift-[` / `]`. Does **not** walk MRU.
    pub(crate) fn activate_tab_in_direction(
        &mut self,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.activate_tab(next_tab_id, window, cx);
    }
    pub(crate) fn tab_switcher_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_mru.is_empty() {
            return;
        }
        if !self.tab_switcher_open {
            self.tab_switcher_open = true;
            // First press targets the previous tab (MRU[1]), matching OS switchers.
            self.tab_switcher_index = if self.tab_mru.len() > 1 { 1 } else { 0 };
            window.focus(&self.modal_focus);
        } else {
            let count = self.tab_mru.len();
            self.tab_switcher_index = (self.tab_switcher_index + 1) % count;
        }
        cx.notify();
    }
    pub(crate) fn tab_switcher_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_mru.is_empty() {
            return;
        }
        if !self.tab_switcher_open {
            self.tab_switcher_open = true;
            self.tab_switcher_index = self.tab_mru.len() - 1;
            window.focus(&self.modal_focus);
        } else {
            let count = self.tab_mru.len() as isize;
            self.tab_switcher_index =
                (self.tab_switcher_index as isize - 1).rem_euclid(count) as usize;
        }
        cx.notify();
    }
    pub(crate) fn close_tab_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tab_switcher_open {
            return;
        }
        self.tab_switcher_open = false;
        self.tab_switcher_index = 0;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
    pub(crate) fn confirm_tab_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tab_switcher_open {
            return;
        }
        let tab_id = self.tab_mru.get(self.tab_switcher_index).copied();
        self.tab_switcher_open = false;
        self.tab_switcher_index = 0;
        if let Some(tab_id) = tab_id {
            // activate_tab also touch_mru's and would no-op the open flag.
            self.activate_tab(tab_id, window, cx);
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
    pub(crate) fn reset_scroll_positions(&mut self) {
        self.local_scroll = UniformListScrollHandle::new();
        self.remote_scroll = UniformListScrollHandle::new();
    }
    pub(crate) fn send_command(&mut self, command: AppCommand, cx: &mut Context<Self>) -> bool {
        match self.runtime_client.try_send(command) {
            Ok(()) => true,
            Err(CommandDispatchError::ChannelFull) => {
                self.status_message = Some(STATUS_BUSY_TRY_AGAIN.into());
                cx.notify();
                false
            }
            Err(CommandDispatchError::ChannelClosed) => {
                self.status_message = Some(STATUS_CONNECTION_SERVICE_UNAVAILABLE.into());
                cx.notify();
                false
            }
        }
    }
    pub(crate) fn request_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if crate::workspace::helpers::connection_in_flight(&tab.connection) {
            return;
        }
        let tab_id = tab.id;
        match self.tab_settings.get(&tab_id).cloned() {
            Some(settings) => self.connect_with(settings, tab.profile_id, window, cx),
            None => self.open_connect_form(window, cx),
        }
    }
    pub(crate) fn connect_with(
        &mut self,
        settings: ConnectionSettings,
        profile_id: Option<ProfileId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if crate::workspace::helpers::connection_in_flight(&tab.connection) {
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
        self.update_window_title(window);
        cx.notify();
    }
    pub(crate) fn set_appearance(
        &mut self,
        appearance: AppearancePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match cx.resources_mut().config.set_appearance(appearance) {
            Ok(()) => self.config_error = None,
            Err(error) => {
                warn!(error = %error, "could not save app config");
                self.config_error =
                    Some("Could not write config.json. Check file permissions.".into());
            }
        }

        let theme = match appearance {
            AppearancePreference::System => Theme::for_appearance(window.appearance()),
            AppearancePreference::Light => Theme::one_light(),
            AppearancePreference::Dark => Theme::one_dark(),
        };
        cx.set_global(theme);
        cx.notify();
    }
    pub(crate) fn open_log_folder(&self, cx: &mut Context<Self>) {
        if let Some(directory) = Path::new(self.log_file.as_str()).parent() {
            cx.reveal_path(directory);
        }
    }
    pub(crate) fn copy_version_info(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(format!(
            "macSFTP {} ({})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH
        )));
    }

    /// Record a successful connection in `recents.json` (deduped by host/port/user/profile).
    pub(crate) fn record_recent_for_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(tab) = self.state.tabs.find_tab(tab_id) else {
            return;
        };
        let settings = self.tab_settings.get(&tab_id);
        let restored = self.restored_targets.get(&tab_id);
        let host = settings
            .map(|s| s.host.clone())
            .or_else(|| restored.map(|r| r.host.clone()))
            .unwrap_or_else(|| tab.title.clone());
        let port = settings
            .map(|s| s.port)
            .or_else(|| restored.map(|r| r.port))
            .unwrap_or(22);
        let username = settings
            .map(|s| s.username.clone())
            .or_else(|| restored.map(|r| r.username.clone()))
            .unwrap_or_default();
        if host.is_empty() || username.is_empty() {
            return;
        }
        let profile_id = tab.profile_id.map(|id| id.0);
        let display_name = profile_id.and_then(|id| {
            cx.resources()
                .profiles
                .find_profile(ProfileId(id))
                .map(|profile| profile.name.clone())
        });
        let last_remote_path = tab
            .remote
            .path
            .as_ref()
            .map(|path| path.as_str().to_string());
        let last_connected_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        if let Err(error) = cx.resources_mut().recents.upsert(RecentEntryInput {
            host,
            port,
            username,
            profile_id,
            display_name,
            last_remote_path,
            last_connected_at,
        }) {
            warn!(error = %error, "could not save recents.json");
        }
    }

    /// Open a recent connection: remember path for post-connect navigation.
    /// With a live profile and Keychain secret, auto-connects; otherwise opens
    /// the connect form prefilled (profile metadata or host/user/port).
    pub(crate) fn open_recent_connection(
        &mut self,
        recent_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = cx
            .resources()
            .recents
            .entries()
            .iter()
            .find(|entry| entry.id == recent_id)
            .cloned()
        else {
            return;
        };

        if let Some(tab_id) = self.state.tabs.active_tab_id {
            let remote_path = entry
                .last_remote_path
                .as_ref()
                .map(|path| RemotePath::new(path.clone()));
            self.restored_targets.insert(
                tab_id,
                RestoredTabTarget {
                    host: entry.host.clone(),
                    port: entry.port,
                    username: entry.username.clone(),
                    profile_id: entry.profile_id.map(ProfileId),
                    remote_path: remote_path.clone(),
                },
            );
            if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                tab.profile_id = entry.profile_id.map(ProfileId);
                if let Some(path) = remote_path {
                    tab.remote.path = Some(path);
                }
            }
        }

        // Profile still exists → use_profile (Keychain) then auto-connect only when
        // required credentials are present (password non-empty, or private-key
        // path set). Missing Keychain password must leave the form open — not
        // auto-connect with an empty password. Manual connect still allows
        // empty password via submit_connect_form.
        // Missing/deleted profile falls through to host-meta prefill only.
        if let Some(profile_id) = entry.profile_id.map(ProfileId)
            && cx.resources().profiles.find_profile(profile_id).is_some()
        {
            self.use_profile(profile_id, cx);
            if self
                .connect_form
                .as_ref()
                .is_some_and(|form| form.can_auto_connect())
            {
                self.submit_connect_form(window, cx);
                return;
            }
            window.focus(&self.connect_form_focus);
            cx.notify();
            return;
        }

        self.open_connect_form(window, cx);
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
                    workspace_root.child(self.render_transfer_drawer(window, cx))
                })
                .child(self.render_status_bar(cx))
                .into_any_element()
        };

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.workspace_focus)
            // Always attach so clear works even before the tree re-renders
            // after drag start (drawer_resize may still be None on last paint).
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|workspace, _event, _window, cx| {
                    if workspace.drawer_resize.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(|workspace, _: &NewTab, window, cx| {
                workspace.open_new_tab(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &CloseTab, window, cx| {
                if let Some(active_tab_id) = workspace.state.tabs.active_tab_id {
                    workspace.close_tab_by_id(active_tab_id, window, cx);
                }
            }))
            .on_action(cx.listener(|workspace, _: &ActivateNextTab, window, cx| {
                workspace.activate_tab_in_direction(1, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &ActivatePrevTab, window, cx| {
                workspace.activate_tab_in_direction(-1, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &TabSwitcherNext, window, cx| {
                workspace.tab_switcher_next(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &TabSwitcherPrev, window, cx| {
                workspace.tab_switcher_prev(window, cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &ConfirmTabSwitcher, window, cx| {
                    workspace.confirm_tab_switcher(window, cx);
                }),
            )
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
            .on_action(
                cx.listener(|workspace, _: &OpenCommandPalette, window, cx| {
                    workspace.open_command_palette(window, cx);
                }),
            )
            .on_action(cx.listener(|workspace, _: &CancelActiveModal, window, cx| {
                workspace.cancel_active_modal(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SelectNextEntry, _window, cx| {
                workspace.move_selection(workspace.focused_side, 1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SelectPrevEntry, _window, cx| {
                workspace.move_selection(workspace.focused_side, -1, cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &SelectNextEntryExtend, _window, cx| {
                    workspace.move_selection_extend(workspace.focused_side, 1, cx);
                }),
            )
            .on_action(
                cx.listener(|workspace, _: &SelectPrevEntryExtend, _window, cx| {
                    workspace.move_selection_extend(workspace.focused_side, -1, cx);
                }),
            )
            .on_action(cx.listener(|workspace, _: &PageDown, _window, cx| {
                workspace.page_selection(workspace.focused_side, 1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &PageUp, _window, cx| {
                workspace.page_selection(workspace.focused_side, -1, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SelectFirstEntry, _window, cx| {
                let side = workspace.focused_side;
                workspace.select_index(side, 0, cx);
            }))
            .on_action(cx.listener(|workspace, _: &SelectLastEntry, _window, cx| {
                let side = workspace.focused_side;
                let n = workspace.entry_count(side, cx);
                if n > 0 {
                    workspace.select_index(side, n - 1, cx);
                }
            }))
            .on_action(cx.listener(|workspace, _: &SelectAllEntries, _window, cx| {
                workspace.select_all_visible(workspace.focused_side, cx);
            }))
            .on_action(cx.listener(|workspace, _: &OpenSelectedEntry, window, cx| {
                workspace.open_selected_entry(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &ParentDirectory, window, cx| {
                workspace.go_to_parent_directory(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &NavigateBack, window, cx| {
                workspace.navigate_focused(HistoryOp::Back, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &NavigateForward, window, cx| {
                workspace.navigate_focused(HistoryOp::Forward, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &GoToPath, window, cx| {
                workspace.open_go_to_path(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &FilterPane, window, cx| {
                workspace.open_filter_pane(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &CopyPath, _window, cx| {
                workspace.copy_focused_path(cx);
            }))
            .on_action(cx.listener(|workspace, _: &DeleteSelection, window, cx| {
                workspace.request_delete_selection(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &RenameEntry, window, cx| {
                workspace.begin_rename_selection(window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &NewFolder, window, cx| {
                workspace.begin_new_folder(window, cx);
            }))
            .on_action(
                cx.listener(|workspace, _: &ToggleHiddenFiles, _window, cx| {
                    workspace.toggle_hidden_files(cx);
                }),
            )
            .on_action(cx.listener(|workspace, _: &OpenSettings, window, cx| {
                if workspace.connect_form.is_none()
                    && workspace.active_host_key_prompt().is_none()
                    && workspace.active_transfer_conflict_prompt().is_none()
                    && workspace.delete_confirm.is_none()
                    && !workspace.go_to_path_open
                {
                    workspace.about_open = false;
                    workspace.surface = WorkspaceSurface::Settings;
                    workspace.settings_section = SettingsSection::General;
                    workspace.workspace_focus.focus(window);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|workspace, _: &OpenProfiles, window, cx| {
                if workspace.connect_form.is_none()
                    && workspace.active_host_key_prompt().is_none()
                    && workspace.active_transfer_conflict_prompt().is_none()
                    && workspace.delete_confirm.is_none()
                    && !workspace.go_to_path_open
                {
                    workspace.about_open = false;
                    workspace.surface = WorkspaceSurface::Settings;
                    workspace.set_settings_section(SettingsSection::Profiles, cx);
                    workspace.workspace_focus.focus(window);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|workspace, _: &ShowAbout, _window, cx| {
                if workspace.connect_form.is_none()
                    && workspace.active_host_key_prompt().is_none()
                    && workspace.active_transfer_conflict_prompt().is_none()
                    && workspace.delete_confirm.is_none()
                    && !workspace.go_to_path_open
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
            .children(self.render_delete_confirm_modal(cx))
            .children(self.render_profile_delete_confirm_modal(cx))
            .children(self.render_go_to_path_modal(cx))
            .children(self.render_context_menu(cx))
            .children(self.render_about(cx))
            .children(self.render_tab_switcher(cx))
            .children(self.render_command_palette(cx))
    }
}

mod command_palette;
mod connect_form;
mod drawer_height;
mod event_handling;
mod file_ops;
mod helpers;
mod modals;
mod nav;
mod panes;
mod profiles;
mod render;
mod settings_render;
#[cfg(test)]
mod tests;
mod transfer_render;
mod transfers;
mod visible_entries;

use connect_form::ConnectForm;
