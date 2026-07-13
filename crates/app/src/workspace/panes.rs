#![allow(unused_imports)]
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
    DisconnectReason, EntryPath, ErrorCode, FileKind, FileSortField, HostKeyDecisionCommand,
    HostKeyPrompt, LocalPath, ModalRequest, ModalRequestId, ProfileId, RemotePath, SecretRef,
    SortDirection, TabId, TabState, Timestamp, TransferConflictPrompt, TransferDirection,
    TransferEndpoint, TransferHistoryId, TransferHistoryRecord, TransferHistoryStatus,
    TransferJob, TransferState, TrustRequestId, UserFacingError, history_status_for_plan,
    sort_entries,
};
use macsftp_platform::{AppPaths, read_local_directory};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::{
    AppearancePreference, ConfigStore, KeychainError, KeychainStore, ProfileStore,
    ResidualTempStore, TransferHistoryStore,
};
use macsftp_ui::{
    ActiveTheme, DragPreview, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel,
    Theme, TransferRow, connection_status, copy_name, empty_state, file_row, file_table_header,
    format_size, format_timestamp, icon, icon_button, section_header_static, tab, text_button,
    text_field, text_tooltip, transfer_history_detail, transfer_history_title, transfer_row,
    transfer_title,
};

use tracing::{debug, warn};

use crate::app_actions::*;
use crate::workspace::connect_form::*;
use crate::workspace::helpers::*;
use crate::workspace::*;

impl crate::workspace::Workspace {
    pub(crate) fn focus_pane(
        &mut self,
        side: PaneSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_side = side;
        window.focus(self.pane_focus(side));
        cx.notify();
    }
    pub(crate) fn entry_count(&self, side: PaneSide) -> usize {
        match (self.active_tab(), side) {
            (Some(tab), PaneSide::Local) => tab.local.entries.len(),
            (Some(tab), PaneSide::Remote) => tab.remote.entries.len(),
            (None, _) => 0,
        }
    }
    pub(crate) fn entry_path_at(&self, side: PaneSide, index: usize) -> Option<EntryPath> {
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
    pub(crate) fn selected_index(&self, side: PaneSide) -> Option<usize> {
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
    pub(crate) fn select_index(&mut self, side: PaneSide, index: usize, cx: &mut Context<Self>) {
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
    pub(crate) fn move_selection(&mut self, side: PaneSide, offset: isize, cx: &mut Context<Self>) {
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
    pub(crate) fn open_entry_at(
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
    pub(crate) fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        if let Some(index) = self.selected_index(side) {
            self.open_entry_at(side, index, window, cx);
        }
    }
    pub(crate) fn set_local_path(
        &mut self,
        path: LocalPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    pub(crate) fn load_local_directory(path: &LocalPath, tab: &mut TabState) -> Option<String> {
        match read_local_directory(path) {
            Ok(mut entries) => {
                macsftp_core::sort_entries(&mut entries, &tab.sort);
                tab.local.entries = entries;
                tab.local.path = Some(path.clone());
                tab.local.error = None;
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

    pub(crate) fn apply_sort_field(&mut self, field: FileSortField, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.sort.field == field {
            tab.sort.direction = match tab.sort.direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
        } else {
            tab.sort.field = field;
            tab.sort.direction = SortDirection::Ascending;
        }
        let sort = tab.sort.clone();
        macsftp_core::sort_entries(&mut tab.local.entries, &sort);
        macsftp_core::sort_entries(&mut tab.remote.entries, &sort);
        cx.notify();
    }
    pub(crate) fn request_remote_directory(
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
    pub(crate) fn go_to_parent_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(crate) fn refresh_focused_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(crate) fn copy_focused_path(&mut self, cx: &mut Context<Self>) {
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
    pub(crate) fn on_row_clicked(
        &mut self,
        side: PaneSide,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane(side, window, cx);
        if event.is_right_click() {
            self.open_context_menu(side, Some(index), cx);
            return;
        }
        if event.click_count() >= 2 {
            self.open_entry_at(side, index, window, cx);
        } else {
            self.select_index(side, index, cx);
        }
    }

    /// Cancel an in-flight connect/reconnect (or host-key wait) from the
    /// remote empty-state Cancel button.
    pub(crate) fn cancel_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id;
        match &tab.connection {
            ConnectionState::AwaitingHostKey { request_id, .. } => {
                let request_id = *request_id;
                self.reject_host_key(request_id, window, cx);
                return;
            }
            ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. } => {}
            _ => return,
        }
        self.send_command(AppCommand::DisconnectTab { tab_id }, cx);
        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            tab.disconnect(DisconnectReason::UserRequested);
            tab.remote.entries.clear();
            tab.remote.path = None;
            tab.remote.is_refreshing = false;
            tab.remote.error = None;
        }
        self.state.drain_expired_modals();
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
}
