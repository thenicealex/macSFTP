//! Phase 1: create / rename / delete for local + remote panes.

use std::path::Path;

use gpui::{
    Context, FontWeight, IntoElement, KeyDownEvent, ParentElement, SharedString, Styled, Window,
    div, prelude::*, px,
};
use macsftp_core::{
    AppCommand, ConnectionState, EntryPath, FileKind, FsCommand, FsEntryRef, FsOp, FsPath, FsScope,
    TabId, UserFacingError,
};
use macsftp_platform::{create_directory, delete_entry, local_fs_error, rename_entry};
use macsftp_ui::{ActiveTheme, IconName, InputKeyResult, InputState, icon, text_button};
use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::*;

const DELETE_NAME_PREVIEW_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeleteNamePreview {
    name: String,
    is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeleteConfirmPresentation {
    title: String,
    names: Vec<DeleteNamePreview>,
    overflow: usize,
    risk_message: String,
}

fn delete_confirm_presentation(entries: &[FsEntryRef]) -> DeleteConfirmPresentation {
    let count = entries.len();
    let names: Vec<DeleteNamePreview> = entries
        .iter()
        .take(DELETE_NAME_PREVIEW_LIMIT)
        .map(|entry| DeleteNamePreview {
            name: Path::new(entry.path.as_str())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| entry.path.as_str().to_string()),
            is_dir: entry.is_dir,
        })
        .collect();
    let directory_count = entries.iter().filter(|entry| entry.is_dir).count();
    let title = if count == 1 {
        format!(
            "Delete “{}”?",
            names.first().map_or("item", |entry| &entry.name)
        )
    } else {
        format!("Delete {count} items?")
    };
    let risk_message = match (count, directory_count) {
        (1, 1) => "This folder and all of its contents will be permanently deleted.".to_string(),
        (1, _) => "This item will be permanently deleted.".to_string(),
        (_, 0) => "These items will be permanently deleted.".to_string(),
        (_, 1) => {
            "This selection includes 1 folder. All selected items and folder contents will be permanently deleted."
                .to_string()
        }
        (_, directory_count) => format!(
            "This selection includes {directory_count} folders. All selected items and folder contents will be permanently deleted."
        ),
    };

    DeleteConfirmPresentation {
        title,
        overflow: count.saturating_sub(names.len()),
        names,
        risk_message,
    }
}

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

    pub(crate) fn request_delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn begin_rename_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Execute a create/rename/delete. Local filesystem work runs on GPUI's
    /// background executor; remote work is forwarded to the Tokio actor.
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let refresh_path = match op.refresh_path() {
            Some(FsPath::Local(path)) => Some(path),
            _ => None,
        };
        self.status_message = Some("Working…".into());
        cx.notify();
        let operation_task = cx
            .background_executor()
            .spawn(async move { execute_local_fs_operation(&op) });
        cx.spawn(async move |workspace, cx| {
            let result = operation_task.await;
            let _ = workspace.update(cx, |workspace, cx| match result {
                Ok(()) => {
                    if let Some(path) = refresh_path {
                        workspace.request_local_directory(tab_id, path, cx);
                    } else {
                        workspace.status_message = None;
                        cx.notify();
                    }
                }
                Err(failure) => workspace.report_local_fs_failure(tab_id, failure, cx),
            });
        })
        .detach();
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
        let presentation = delete_confirm_presentation(&state.entries);
        let show_dont_ask = cx.resources().config.config().confirm_delete;

        let mut name_list = div()
            .flex()
            .flex_col()
            .min_w_0()
            .rounded_sm()
            .border_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.surface);
        for (index, entry) in presentation.names.iter().enumerate() {
            let name = entry.name.clone();
            let entry_icon = if entry.is_dir {
                IconName::Folder
            } else {
                IconName::File
            };
            name_list = name_list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(theme.colors.border)
                    })
                    .text_size(px(12.0))
                    .text_color(theme.colors.text)
                    .child(icon(entry_icon, theme.colors.text_muted))
                    .child(div().min_w_0().truncate().child(name)),
            );
        }
        if presentation.overflow > 0 {
            name_list = name_list.child(
                div()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(theme.colors.border)
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child(format!("and {} more", presentation.overflow)),
            );
        }

        let mut card = div()
            .key_context("DeleteConfirmModal")
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    workspace.cancel_delete_confirm(window, cx);
                } else if event.keystroke.key == "enter" && event.keystroke.modifiers.platform {
                    // cmd-enter confirms; bare enter stays on Cancel (default).
                    cx.stop_propagation();
                    workspace.confirm_delete(window, cx);
                }
            }))
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(440.0))
            .p_5()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded_md()
            .shadow_sm()
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(30.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(gpui::hsla(0.0, 0.55, 0.45, 0.15))
                            .child(icon(IconName::Trash, theme.colors.error)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.colors.text)
                            .child(presentation.title.clone()),
                    ),
            )
            .child(name_list);

        card = card.child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .mt(px(1.0))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.colors.error)
                        .child("!"),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(12.0))
                        .text_color(theme.colors.text_muted)
                        .child(presentation.risk_message),
                ),
        );

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
                            .child("Don't ask again before deleting items"),
                    ),
            );
        }

        card = card.child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(text_button("delete-cancel", "Cancel").on_click(cx.listener(
                    |workspace, _event, window, cx| {
                        workspace.cancel_delete_confirm(window, cx);
                    },
                )))
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
                .p_4()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(card)
                .into_any_element(),
        )
    }

    pub(crate) fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
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
                text_button("ctx-edit", "Edit")
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        workspace.context_menu = None;
                        workspace.focused_side = PaneSide::Remote;
                        workspace.request_edit_selection(cx);
                    }))
                    .into_any_element(),
            );
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

fn execute_local_fs_operation(op: &FsOp) -> Result<(), UserFacingError> {
    match op {
        FsOp::Delete { entries } => {
            for entry in entries {
                let Some(path) = entry.path.as_local() else {
                    return Err(UserFacingError::new(
                        macsftp_core::ErrorCode::Unknown,
                        "Invalid local path",
                        "Delete paths must be local.",
                    ));
                };
                delete_entry(path, entry.is_dir)
                    .map_err(|error| local_fs_error("Could not delete", &error))?;
            }
            Ok(())
        }
        FsOp::Rename { from, to } => {
            let (Some(from), Some(to)) = (from.as_local(), to.as_local()) else {
                return Err(UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Invalid local path",
                    "Rename paths must be local.",
                ));
            };
            rename_entry(from, to).map_err(|error| local_fs_error("Could not rename", &error))
        }
        FsOp::CreateDirectory { parent, name } => {
            let Some(parent) = parent.as_local() else {
                return Err(UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Invalid local path",
                    "Create-folder parent must be local.",
                ));
            };
            create_directory(parent, name)
                .map(|_| ())
                .map_err(|error| local_fs_error("Could not create folder", &error))
        }
    }
}

#[cfg(test)]
mod delete_confirm_presentation_tests {
    use macsftp_core::{FsEntryRef, FsPath, LocalPath};

    use super::{DELETE_NAME_PREVIEW_LIMIT, delete_confirm_presentation};

    fn local_entry(path: &str, is_dir: bool) -> FsEntryRef {
        FsEntryRef {
            path: FsPath::Local(LocalPath::new(path)),
            is_dir,
        }
    }

    #[test]
    fn single_folder_uses_its_name_and_recursive_delete_warning() {
        let presentation = delete_confirm_presentation(&[local_entry("/tmp/project-assets", true)]);

        assert_eq!(presentation.title, "Delete “project-assets”?");
        assert_eq!(presentation.names.len(), 1);
        assert_eq!(presentation.names[0].name, "project-assets");
        assert!(presentation.names[0].is_dir);
        assert_eq!(presentation.overflow, 0);
        assert_eq!(
            presentation.risk_message,
            "This folder and all of its contents will be permanently deleted."
        );
    }

    #[test]
    fn mixed_selection_reports_folder_count_and_preview_overflow() {
        let entries: Vec<FsEntryRef> = (0..DELETE_NAME_PREVIEW_LIMIT + 2)
            .map(|index| local_entry(&format!("/tmp/item-{index}"), index < 2))
            .collect();
        let presentation = delete_confirm_presentation(&entries);

        assert_eq!(presentation.title, "Delete 10 items?");
        assert_eq!(presentation.names.len(), DELETE_NAME_PREVIEW_LIMIT);
        assert_eq!(presentation.overflow, 2);
        assert_eq!(
            presentation.risk_message,
            "This selection includes 2 folders. All selected items and folder contents will be permanently deleted."
        );
    }
}
