use std::collections::HashMap;
use std::path::Path;

use gpui::{
    App, ClipboardItem, Context, FocusHandle, Focusable, IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*,
};
use macsftp_core::{
    AppCommand, AppState,
    CommandDispatchError, ConnectCommand, ConnectionSettings, ConnectionState,
    LocalPath, ProfileId, TabId, TabState,
};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::AppearancePreference;
use macsftp_ui::{
    ActiveTheme, InputState, Theme, empty_state, text_button,
};
use tracing::warn;

use crate::app_actions::{
    ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, CopyPath, CopyVersionInfo,
    DeleteSelection, DownloadSelection, FocusLocalPane, FocusRemotePane, MinimizeWindow,
    NavigateBack, NavigateForward, NewFolder, NewTab, OpenLogFolder, OpenSelectedEntry,
    OpenSettings, ParentDirectory, ReconnectTab, RefreshPane, RenameEntry, SelectNextEntry,
    SelectPrevEntry, ShowAbout, ShowTransferDrawer, ToggleHiddenFiles, UploadSelection,
    ZoomWindow,
};
use crate::resources::ActiveResources;
use crate::workspace::file_ops::{ContextMenuState, DeleteConfirmState, InlineEditState};
use crate::workspace::nav::{HistoryOp, TabNavState};

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
    /// Session credentials per tab, kept in memory only so Reconnect
    /// works without re-typing. Replaced by Keychain-backed profiles.
    tab_settings: HashMap<TabId, ConnectionSettings>,
    /// Per-tab local/remote back-forward history (session-only).
    tab_nav: HashMap<TabId, TabNavState>,
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
        mut event_receiver: EventReceiver,
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
            log_file,
            connect_form: None,
            connect_form_focus: cx.focus_handle(),
            conflict_rename: InputState::new(),
            conflict_rename_error: None,
            delete_confirm: None,
            inline_edit: None,
            context_menu: None,
            tab_settings: HashMap::new(),
            tab_nav: HashMap::new(),
            transfer_history_flushed: false,
            _appearance_subscription: appearance_subscription,
            _event_drain: event_drain,
        };
        // Persist in-flight transfers to disk when the app quits (plan §18)
        // so they can be shown and retried on next launch.
        cx.on_app_quit(|workspace, cx| {
            workspace.flush_transfer_history(cx);
            async {}
        })
        .detach();
        workspace.open_new_tab(window, cx);
        workspace
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
        self.reset_scroll_positions();
        self.focus_pane(PaneSide::Local, window, cx);
        cx.notify();
    }
    pub(crate) fn close_tab_by_id(&mut self, tab_id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.tabs.close_tab(tab_id).is_none() {
            return;
        }
        self.tab_nav.remove(&tab_id);
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
    pub(crate) fn activate_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        if self.state.tabs.find_tab(tab_id).is_some() {
            self.state.tabs.active_tab_id = Some(tab_id);
            self.reset_scroll_positions();
            cx.notify();
        }
    }
    pub(crate) fn activate_tab_in_direction(&mut self, offset: isize, cx: &mut Context<Self>) {
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
    pub(crate) fn reset_scroll_positions(&mut self) {
        self.local_scroll = UniformListScrollHandle::new();
        self.remote_scroll = UniformListScrollHandle::new();
    }
    pub(crate) fn send_command(&mut self, command: AppCommand, cx: &mut Context<Self>) -> bool {
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
    pub(crate) fn request_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if crate::workspace::helpers::connection_in_flight(&tab.connection) {
            return;
        }
        let tab_id = tab.id;
        match self.tab_settings.get(&tab_id).cloned() {
            Some(settings) => self.connect_with(settings, tab.profile_id, cx),
            None => self.open_connect_form(window, cx),
        }
    }
    pub(crate) fn connect_with(
        &mut self,
        settings: ConnectionSettings,
        profile_id: Option<ProfileId>,
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
            AppearancePreference::Light => Theme::light(),
            AppearancePreference::Dark => Theme::dark(),
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
            .on_action(cx.listener(|workspace, _: &NavigateBack, window, cx| {
                workspace.navigate_focused(HistoryOp::Back, window, cx);
            }))
            .on_action(cx.listener(|workspace, _: &NavigateForward, window, cx| {
                workspace.navigate_focused(HistoryOp::Forward, window, cx);
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
            .on_action(cx.listener(|workspace, _: &ToggleHiddenFiles, _window, cx| {
                workspace.toggle_hidden_files(cx);
            }))
            .on_action(cx.listener(|workspace, _: &OpenSettings, window, cx| {
                if workspace.connect_form.is_none()
                    && workspace.active_host_key_prompt().is_none()
                    && workspace.active_transfer_conflict_prompt().is_none()
                    && workspace.delete_confirm.is_none()
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
                    && workspace.delete_confirm.is_none()
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
            .children(self.render_context_menu(cx))
            .children(self.render_about(cx))
    }
}

mod connect_form;
mod event_handling;
mod file_ops;
mod helpers;
mod modals;
mod nav;
mod panes;
mod render;
#[cfg(test)]
mod tests;
mod transfers;
mod visible_entries;

use connect_form::ConnectForm;