//! Phase 1: create / rename / delete for local + remote panes.

#![allow(unused_imports)]
use std::path::Path;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, Styled, Window, div, prelude::*, px,
};
use macsftp_core::{
    AppCommand, ConnectionState, EntryPath, FileKind, FsCommand, FsEntryRef, FsOp, FsPath, FsScope,
    LocalPath, RemotePath, TabId, UserFacingError,
};
use macsftp_platform::{create_directory, delete_entry, local_fs_error, rename_entry};
use macsftp_ui::{
    ActiveTheme, InputKeyResult, InputState, TextFieldModel, text_button, text_field,
};
use tracing::warn;

use crate::app_actions::*;
use crate::resources::ActiveResources;
use crate::workspace::*;

const DELETE_NAME_PREVIEW_LIMIT: usize = 8;

/// Pending delete confirmation (view-local; not a core ModalRequest).
#[derive(Debug, Clone)]
pub(crate) struct DeleteConfirmState {
    pub scope: FsScope,
    pub entries: Vec<FsEntryRef>,
    pub dont_ask_again: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum InlineEditKind {
    Rename { entry: FsEntryRef },
    NewFolder { parent: FsPath },
}

#[derive(Debug, Clone)]
pub(crate) struct InlineEditState {
    pub side: PaneSide,
    pub kind: InlineEditKind,
    pub input: InputState,
    pub error: Option<SharedString>,
}

#[derive(Debug, Clone)]
pub(crate) struct ContextMenuState {
    pub side: PaneSide,
    /// `None` = blank-area menu (New Folder / Refresh).
    pub entry_index: Option<usize>,
}

impl crate::workspace::Workspace {
    pub(crate) fn selected_fs_entries(&self, side: PaneSide) -> Vec<FsEntryRef> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let selected = &tab.selection.selected_paths;
        match side {
            PaneSide::Local => tab
                .local
                .entries
                .iter()
                .filter(|entry| selected.contains(&EntryPath::Local(entry.path.clone())))
                .map(|entry| FsEntryRef {
                    path: FsPath::Local(entry.path.clone()),
                    is_dir: entry.kind == FileKind::Directory,
                })
                .collect(),
            PaneSide::Remote => tab
                .remote
                .entries
                .iter()
                .filter(|entry| selected.contains(&EntryPath::Remote(entry.path.clone())))
                .map(|entry| FsEntryRef {
                    path: FsPath::Remote(entry.path.clone()),
                    is_dir: entry.kind == FileKind::Directory,
                })
                .collect(),
        }
    }

    pub(crate) fn fs_scope_for_side(&self, side: PaneSide) -> Option<FsScope> {
        let tab = self.active_tab()?;
        match side {
            PaneSide::Local => Some(FsScope::Local { tab_id: tab.id }),
            PaneSide::Remote => match tab.connection {
                ConnectionState::Connected { session_epoch, .. } => Some(FsScope::Remote {
                    tab_id: tab.id,
                    session_epoch,
                }),
                _ => None,
            },
        }
    }

    pub(crate) fn request_delete_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let side = self.focused_side;
        let entries = self.selected_fs_entries(side);
        if entries.is_empty() {
            self.status_message = Some("Select one or more items to delete".into());
            cx.notify();
            return;
        }
        let Some(scope) = self.fs_scope_for_side(side) else {
            self.status_message = Some("Connect before deleting remote items".into());
            cx.notify();
            return;
        };
        self.context_menu = None;
        if cx.resources().config.config().confirm_delete {
            self.delete_confirm = Some(DeleteConfirmState {
                scope,
                entries,
                dont_ask_again: false,
            });
            window.focus(&self.modal_focus);
            cx.notify();
        } else {
            self.dispatch_fs(
                FsCommand {
                    scope,
                    op: FsOp::Delete { entries },
                },
                window,
                cx,
            );
        }
    }

    pub(crate) fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.delete_confirm.take() else {
            return;
        };
        if state.dont_ask_again {
            match cx.resources_mut().config.set_confirm_delete(false) {
                Ok(()) => self.config_error = None,
                Err(error) => {
                    warn!(error = %error, "could not save confirm_delete");
                    self.config_error =
                        Some("Could not write config.json. Check file permissions.".into());
                }
            }
        }
        self.dispatch_fs(
            FsCommand {
                scope: state.scope,
                op: FsOp::Delete {
                    entries: state.entries,
                },
            },
            window,
            cx,
        );
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn cancel_delete_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_confirm = None;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn begin_rename_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let side = self.focused_side;
        let mut entries = self.selected_fs_entries(side);
        if entries.len() != 1 {
            self.status_message = Some("Select a single item to rename".into());
            cx.notify();
            return;
        }
        if side == PaneSide::Remote && self.fs_scope_for_side(side).is_none() {
            self.status_message = Some("Connect before renaming remote items".into());
            cx.notify();
            return;
        }
        let entry = entries.remove(0);
        let name = Path::new(entry.path.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.context_menu = None;
        self.inline_edit = Some(InlineEditState {
            side,
            kind: InlineEditKind::Rename { entry },
            input: InputState::with_value(name),
            error: None,
        });
        window.focus(self.pane_focus(side));
        cx.notify();
    }

    pub(crate) fn begin_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        let parent = match side {
            PaneSide::Local => tab.local.path.clone().map(FsPath::Local),
            PaneSide::Remote => {
                if self.fs_scope_for_side(side).is_none() {
                    self.status_message = Some("Connect before creating a remote folder".into());
                    cx.notify();
                    return;
                }
                tab.remote.path.clone().map(FsPath::Remote)
            }
        };
        let Some(parent) = parent else {
            self.status_message = Some("No current directory".into());
            cx.notify();
            return;
        };
        self.context_menu = None;
        self.inline_edit = Some(InlineEditState {
            side,
            kind: InlineEditKind::NewFolder { parent },
            input: InputState::with_value("Untitled Folder"),
            error: None,
        });
        window.focus(self.pane_focus(side));
        cx.notify();
    }

    pub(crate) fn cancel_inline_edit(&mut self, cx: &mut Context<Self>) {
        self.inline_edit = None;
        cx.notify();
    }

    pub(crate) fn submit_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.inline_edit.clone() else {
            return;
        };
        let name = edit.input.value().trim().to_string();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            if let Some(edit) = &mut self.inline_edit {
                edit.error = Some("Enter a name without path separators.".into());
            }
            cx.notify();
            return;
        }
        // Sibling name collision check against the current listing.
        if let Some(tab) = self.active_tab() {
            let collision = match edit.side {
                PaneSide::Local => tab.local.entries.iter().any(|e| e.name == name),
                PaneSide::Remote => tab.remote.entries.iter().any(|e| e.name == name),
            };
            if collision {
                if let Some(edit) = &mut self.inline_edit {
                    edit.error = Some("An item with that name already exists.".into());
                }
                cx.notify();
                return;
            }
        }
        let Some(scope) = self.fs_scope_for_side(edit.side) else {
            self.status_message = Some("Not connected".into());
            cx.notify();
            return;
        };
        let op = match edit.kind {
            InlineEditKind::Rename { entry } => {
                let parent = match entry.path.parent() {
                    Some(parent) => parent,
                    None => {
                        if let Some(edit) = &mut self.inline_edit {
                            edit.error = Some("Cannot rename the filesystem root.".into());
                        }
                        cx.notify();
                        return;
                    }
                };
                FsOp::Rename {
                    from: entry.path,
                    to: parent.join(&name),
                }
            }
            InlineEditKind::NewFolder { parent } => FsOp::CreateDirectory { parent, name },
        };
        self.inline_edit = None;
        self.dispatch_fs(FsCommand { scope, op }, window, cx);
    }

    pub(crate) fn handle_inline_edit_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inline_edit.is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.cancel_inline_edit(cx);
            return;
        }
        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.submit_inline_edit(window, cx);
            return;
        }
        if let Some(edit) = &mut self.inline_edit {
            if keystroke.modifiers.platform && keystroke.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    edit.input.insert(&text);
                    edit.error = None;
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            if edit.input.handle_keystroke(keystroke) == InputKeyResult::Handled {
                edit.error = None;
                cx.stop_propagation();
                cx.notify();
            }
        }
    }

    /// Execute a create/rename/delete. Local runs on the UI thread via
    /// platform APIs; remote is forwarded to the Tokio runtime/actor.
    pub(crate) fn dispatch_fs(
        &mut self,
        command: FsCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command.scope {
            FsScope::Local { tab_id } => self.execute_local_fs(tab_id, command.op, window, cx),
            FsScope::Remote { .. } => {
                if !self.send_command(AppCommand::Fs(command), cx) {
                    return;
                }
                self.status_message = Some("Working…".into());
                cx.notify();
            }
        }
    }

    fn execute_local_fs(
        &mut self,
        tab_id: TabId,
        op: FsOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match &op {
            FsOp::Delete { entries } => {
                for entry in entries {
                    let Some(path) = entry.path.as_local() else {
                        continue;
                    };
                    if let Err(error) = delete_entry(path, entry.is_dir) {
                        return self.report_local_fs_failure(
                            tab_id,
                            local_fs_error("Could not delete", &error),
                            cx,
                        );
                    }
                }
                Ok(())
            }
            FsOp::Rename { from, to } => {
                let (Some(from), Some(to)) = (from.as_local(), to.as_local()) else {
                    return self.report_local_fs_failure(
                        tab_id,
                        UserFacingError::new(
                            macsftp_core::ErrorCode::Unknown,
                            "Invalid local path",
                            "Rename paths must be local.",
                        ),
                        cx,
                    );
                };
                rename_entry(from, to)
                    .map_err(|error| local_fs_error("Could not rename", &error))
            }
            FsOp::CreateDirectory { parent, name } => {
                let Some(parent) = parent.as_local() else {
                    return self.report_local_fs_failure(
                        tab_id,
                        UserFacingError::new(
                            macsftp_core::ErrorCode::Unknown,
                            "Invalid local path",
                            "Create-folder parent must be local.",
                        ),
                        cx,
                    );
                };
                create_directory(parent, name)
                    .map(|_| ())
                    .map_err(|error| local_fs_error("Could not create folder", &error))
            }
        };

        match result {
            Ok(()) => {
                if let Some(FsPath::Local(path)) = op.refresh_path() {
                    if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                        if let Some(message) = Self::load_local_directory(&path, tab) {
                            self.status_message = Some(message.into());
                        } else {
                            self.status_message = None;
                        }
                        tab.selection.selected_paths.clear();
                    }
                    self.local_scroll = gpui::UniformListScrollHandle::new();
                }
                cx.notify();
            }
            Err(failure) => self.report_local_fs_failure(tab_id, failure, cx),
        }
        let _ = window;
    }

    fn report_local_fs_failure(
        &mut self,
        _tab_id: TabId,
        failure: UserFacingError,
        cx: &mut Context<Self>,
    ) {
        // Keep the listing visible; phase 1 reports via status bar only.
        self.status_message = Some(format!("{}: {}", failure.title, failure.message).into());
        cx.notify();
    }

    pub(crate) fn open_context_menu(
        &mut self,
        side: PaneSide,
        entry_index: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.focused_side = side;
        if let Some(index) = entry_index {
            self.select_index(side, index, cx);
        }
        self.context_menu = Some(ContextMenuState { side, entry_index });
        cx.notify();
    }

    pub(crate) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        self.context_menu = None;
        cx.notify();
    }

    pub(crate) fn reveal_local_selection(&mut self, cx: &mut Context<Self>) {
        let entries = self.selected_fs_entries(PaneSide::Local);
        let Some(entry) = entries.first() else {
            return;
        };
        if let Some(path) = entry.path.as_local() {
            cx.reveal_path(Path::new(path.as_str()));
        }
    }

    pub(crate) fn render_delete_confirm_modal(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.delete_confirm.as_ref()?;
        let theme = cx.theme().clone();
        let count = state.entries.len();
        let title = if count == 1 {
            "Delete 1 item?".to_string()
        } else {
            format!("Delete {count} items?")
        };
        let names: Vec<String> = state
            .entries
            .iter()
            .take(DELETE_NAME_PREVIEW_LIMIT)
            .map(|entry| {
                Path::new(entry.path.as_str())
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.path.as_str().to_string())
            })
            .collect();
        let overflow = count.saturating_sub(names.len());
        let dir_count = state.entries.iter().filter(|e| e.is_dir).count();
        let show_dont_ask = cx.resources().config.config().confirm_delete;

        let mut name_list = div().flex().flex_col().gap_1().min_w_0();
        for name in &names {
            name_list = name_list.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text)
                    .child(name.clone()),
            );
        }
        if overflow > 0 {
            name_list = name_list.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child(format!("and {overflow} more")),
            );
        }

        let mut card = div()
            .key_context("DeleteConfirmModal")
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    workspace.cancel_delete_confirm(window, cx);
                } else if event.keystroke.key == "enter"
                    && event.keystroke.modifiers.platform
                {
                    // cmd-enter confirms; bare enter stays on Cancel (default).
                    cx.stop_propagation();
                    workspace.confirm_delete(window, cx);
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(420.0))
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
                    .child(title),
            )
            .child(name_list);

        if dir_count > 0 {
            let dir_label = if dir_count == 1 {
                "Includes 1 directory. Recursive delete removes all contents permanently."
                    .to_string()
            } else {
                format!(
                    "Includes {dir_count} directories. Recursive delete removes all contents permanently."
                )
            };
            card = card.child(
                div()
                    .p_2()
                    .rounded_sm()
                    .bg(gpui::hsla(0.0, 0.55, 0.45, 0.15))
                    .border_1()
                    .border_color(theme.colors.error)
                    .text_size(px(12.0))
                    .text_color(theme.colors.error)
                    .child(dir_label),
            );
        }

        if show_dont_ask {
            let checked = state.dont_ask_again;
            card = card.child(
                div()
                    .id("delete-dont-ask")
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        if let Some(state) = &mut workspace.delete_confirm {
                            state.dont_ask_again = !state.dont_ask_again;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .size(px(14.0))
                            .border_1()
                            .border_color(theme.colors.border)
                            .rounded_sm()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.0))
                            .text_color(theme.colors.accent)
                            .child(if checked { "✓" } else { "" }),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.colors.text_muted)
                            .child("Don't ask again"),
                    ),
            );
        }

        card = card.child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    text_button("delete-cancel", "Cancel")
                        .primary(true)
                        .on_click(cx.listener(|workspace, _event, window, cx| {
                            workspace.cancel_delete_confirm(window, cx);
                        })),
                )
                .child(
                    text_button("delete-confirm", "Delete")
                        .danger(true)
                        .on_click(cx.listener(|workspace, _event, window, cx| {
                            workspace.confirm_delete(window, cx);
                        })),
                ),
        );

        Some(
            div()
                .id("delete-confirm-scrim")
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

    pub(crate) fn render_context_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.context_menu.as_ref()?;
        let theme = cx.theme().clone();
        let side = menu.side;
        let is_local = side == PaneSide::Local;
        let has_entry = menu.entry_index.is_some();
        let connected = self
            .active_tab()
            .is_some_and(|tab| matches!(tab.connection, ConnectionState::Connected { .. }));

        let mut items: Vec<gpui::AnyElement> = Vec::new();
        if has_entry {
            items.push(
                text_button("ctx-rename", "Rename")
                    .on_click(cx.listener(move |workspace, _event, window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = side;
                        workspace.begin_rename_selection(window, cx);
                    }))
                    .into_any_element(),
            );
            items.push(
                text_button("ctx-delete", "Delete")
                    .danger(true)
                    .on_click(cx.listener(move |workspace, _event, window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = side;
                        workspace.request_delete_selection(window, cx);
                    }))
                    .into_any_element(),
            );
        }
        items.push(
            text_button("ctx-new-folder", "New Folder")
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.context_menu = None;
                    workspace.focused_side = side;
                    workspace.begin_new_folder(window, cx);
                }))
                .into_any_element(),
        );
        if has_entry && is_local {
            items.push(
                text_button("ctx-upload", "Upload")
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = PaneSide::Local;
                        workspace.upload_selection(cx);
                    }))
                    .into_any_element(),
            );
            items.push(
                text_button("ctx-reveal", "Reveal in Finder")
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        workspace.context_menu = None;
                        workspace.reveal_local_selection(cx);
                    }))
                    .into_any_element(),
            );
        }
        if has_entry && !is_local && connected {
            items.push(
                text_button("ctx-download", "Download")
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = PaneSide::Remote;
                        workspace.download_selection(cx);
                    }))
                    .into_any_element(),
            );
        }
        if has_entry {
            items.push(
                text_button("ctx-copy-path", "Copy Path")
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = side;
                        workspace.copy_focused_path(cx);
                    }))
                    .into_any_element(),
            );
        }
        if !has_entry {
            items.push(
                text_button("ctx-refresh", "Refresh")
                    .on_click(cx.listener(move |workspace, _event, window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = side;
                        workspace.refresh_focused_pane(window, cx);
                    }))
                    .into_any_element(),
            );
        }

        Some(
            div()
                .id("context-menu-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .on_click(cx.listener(|workspace, _event, _window, cx| {
                    workspace.close_context_menu(cx);
                }))
                .child(
                    div()
                        .absolute()
                        .top(px(80.0))
                        .left(if is_local { px(40.0) } else { px(420.0) })
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .min_w(px(180.0))
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .shadow_sm()
                        .children(items),
                )
                .into_any_element(),
        )
    }
}
