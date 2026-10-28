//! Phase 1: create / rename / delete for local + remote panes.

use std::path::Path;

use gpui_kit::{
    Context, IntoElement, KeyDownEvent, ParentElement, SharedString, Styled, Window, div,
    prelude::*, px,
};
use macsftp_core::{
    AppCommand, ConnectionState, EntryPath, FileKind, FsCommand, FsEntryRef, FsOp, FsPath, FsScope,
    TabId, UserFacingError, is_bidi_or_control,
};
use macsftp_platform::{create_directory, delete_entry, local_fs_error, rename_entry};
use macsftp_ui::{ActiveTheme, IconName, InputState, TextRoleExt, icon, text_button};

use crate::resources::ActiveResources;
use crate::workspace::overlay::OverlayFade;
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
        "Delete This Item?".to_string()
    } else {
        format!("Delete {count} items?")
    };
    let risk_message = match (count, directory_count) {
        (1, 1) => "This folder and everything inside it will be permanently deleted.".to_string(),
        (1, _) => "This item will be permanently deleted. You can’t undo this.".to_string(),
        (_, 0) => "These items will be permanently deleted. You can’t undo this.".to_string(),
        (_, 1) => {
            "Selected items and everything inside the selected folder will be permanently deleted."
                .to_string()
        }
        (_, directory_count) => format!(
            "Selected items and everything inside {directory_count} selected folders will be permanently deleted."
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
            self.show_status("Select files or folders to delete.".into(), cx);
            cx.notify();
            return;
        }
        let Some(scope) = self.fs_scope_for_side(side) else {
            self.show_status("Connect to the server before deleting files.".into(), cx);
            cx.notify();
            return;
        };
        if cx.resources().config.config().confirm_delete {
            self.modal_inputs.delete_confirm = Some(DeleteConfirmState {
                scope,
                entries,
                dont_ask_again: false,
            });
            window.focus(&self.modal_focus, cx);
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
        let Some(state) = self.modal_inputs.delete_confirm.take() else {
            return;
        };
        if state.dont_ask_again {
            let result = cx.resources_mut().config.set_confirm_delete(false);
            self.finish_config_change(result, "confirm_delete", cx);
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
        self.modal_inputs.delete_confirm = None;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn begin_rename_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let mut entries = self.selected_fs_entries(side);
        if entries.len() != 1 {
            self.show_status("Select one file or folder to rename.".into(), cx);
            cx.notify();
            return;
        }
        if side == PaneSide::Remote && self.fs_scope_for_side(side).is_none() {
            self.show_status("Connect to the server before renaming files.".into(), cx);
            cx.notify();
            return;
        }
        let entry = entries.remove(0);
        let name = Path::new(entry.path.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.text_inputs.remove(&TextInputTarget::InlineEdit);
        self.modal_inputs.inline_edit = Some(InlineEditState {
            side,
            kind: InlineEditKind::Rename { entry },
            input: InputState::with_value(name),
            error: None,
        });
        window.focus(self.pane_focus(side), cx);
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
                    self.status_message =
                        Some("Connect to the server before creating a folder.".into());
                    cx.notify();
                    return;
                }
                tab.remote.path.clone().map(FsPath::Remote)
            }
        };
        let Some(parent) = parent else {
            self.show_status("No folder is open.".into(), cx);
            cx.notify();
            return;
        };
        self.text_inputs.remove(&TextInputTarget::InlineEdit);
        self.modal_inputs.inline_edit = Some(InlineEditState {
            side,
            kind: InlineEditKind::NewFolder { parent },
            input: InputState::with_value("Untitled Folder"),
            error: None,
        });
        window.focus(self.pane_focus(side), cx);
        cx.notify();
    }

    pub(crate) fn cancel_inline_edit(&mut self, cx: &mut Context<Self>) {
        self.modal_inputs.inline_edit = None;
        cx.notify();
    }

    pub(crate) fn submit_inline_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.modal_inputs.inline_edit.clone() else {
            return;
        };
        let name = edit.input.value().trim().to_string();
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\0')
        {
            if let Some(edit) = &mut self.modal_inputs.inline_edit {
                edit.error = Some("Enter a name without slashes.".into());
            }
            cx.notify();
            return;
        }
        if name.chars().any(is_bidi_or_control) {
            if let Some(edit) = &mut self.modal_inputs.inline_edit {
                edit.error = Some("The name contains invalid control characters.".into());
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
                if let Some(edit) = &mut self.modal_inputs.inline_edit {
                    edit.error = Some("A file or folder with this name already exists.".into());
                }
                cx.notify();
                return;
            }
        }
        let Some(scope) = self.fs_scope_for_side(edit.side) else {
            self.show_status("Not Connected".into(), cx);
            cx.notify();
            return;
        };
        let op = match edit.kind {
            InlineEditKind::Rename { entry } => {
                let parent = match entry.path.parent() {
                    Some(parent) => parent,
                    None => {
                        if let Some(edit) = &mut self.modal_inputs.inline_edit {
                            edit.error = Some("The top-level folder can’t be renamed.".into());
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
        self.modal_inputs.inline_edit = None;
        self.dispatch_fs(FsCommand { scope, op }, window, cx);
    }

    pub(crate) fn handle_inline_edit_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_inputs.inline_edit.is_none() {
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
                self.show_status("Working…".into(), cx);
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
        self.show_status("Working…".into(), cx);
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
        self.show_status(format!("{}: {}", failure.title, failure.message).into(), cx);
        cx.notify();
    }

    pub(crate) fn reveal_local_selection(&mut self, cx: &mut Context<Self>) {
        let entries = self.selected_fs_entries(PaneSide::Local);
        let Some(entry) = entries.first() else {
            // Reachable from the palette with the server pane focused, so say
            // what the command needs instead of doing nothing (§8).
            self.show_status("Select one file or folder to show in Finder.".into(), cx);
            cx.notify();
            return;
        };
        if let Some(path) = entry.path.as_local() {
            crate::workspace::reveal_local_path(cx, Path::new(path.as_str()));
        }
    }

    pub(crate) fn render_delete_confirm_modal(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let open = self.modal_inputs.delete_confirm.is_some();
        let fade = OverlayFade::sample("delete-confirm", open, window, cx);
        let state = self.modal_inputs.delete_confirm.as_ref()?;
        let theme = cx.theme().clone();
        let presentation = delete_confirm_presentation(&state.entries);
        let show_dont_ask = cx.resources().config.config().confirm_delete;

        let mut name_list = div()
            .flex()
            .flex_col()
            .min_w_0()
            .rounded(theme.sizes.input_radius)
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
                    .text_role(&theme, theme.typography.body)
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
                    .text_role(&theme, theme.typography.secondary)
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
            .p_4()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded(theme.sizes.modal_radius)
            .shadow(theme.shadow_modal())
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
                            .bg(gpui_kit::Hsla {
                                a: 0.15,
                                ..theme.colors.error
                            })
                            .child(icon(IconName::Trash, theme.colors.error)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_role(&theme, theme.typography.title)
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
                        .text_role(&theme, theme.typography.heading)
                        .text_color(theme.colors.error)
                        .child("!"),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_role(&theme, theme.typography.secondary)
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
                        if let Some(state) = &mut workspace.modal_inputs.delete_confirm {
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
                            .text_role(&theme, theme.typography.label)
                            .text_color(theme.colors.accent)
                            .child(if checked { "✓" } else { "" }),
                    )
                    .child(
                        div()
                            .text_role(&theme, theme.typography.secondary)
                            .text_color(theme.colors.text_muted)
                            .child("Don’t ask before deleting"),
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
                    text_button("delete-confirm", "Delete Permanently")
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
                .bg(theme.scrim())
                .opacity(fade.opacity())
                .child(card)
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
                let std_path = std::path::Path::new(path.as_str());
                if path.as_str().is_empty()
                    || path.as_str() == "/"
                    || path.as_str() == "."
                    || path.as_str() == ".."
                    || std_path.parent().is_none()
                {
                    return Err(UserFacingError::new(
                        macsftp_core::ErrorCode::Unknown,
                        "Invalid Path",
                        "The root directory cannot be deleted.",
                    ));
                }
                delete_entry(path, entry.is_dir)
                    .map_err(|error| local_fs_error("The item couldn’t be deleted.", &error))?;
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
            let from_path = std::path::Path::new(from.as_str());
            let to_path = std::path::Path::new(to.as_str());
            if from.as_str().is_empty()
                || from.as_str() == "/"
                || from_path.parent().is_none()
                || to.as_str().is_empty()
                || to.as_str() == "/"
                || to_path.parent().is_none()
            {
                return Err(UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Invalid Path",
                    "The root directory cannot be renamed.",
                ));
            }
            if to.as_str().contains('\0') || to.as_str().chars().any(is_bidi_or_control) {
                return Err(UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Invalid name",
                    "The destination name is invalid or contains forbidden characters.",
                ));
            }
            rename_entry(from, to)
                .map_err(|error| local_fs_error("The item couldn’t be renamed.", &error))
        }
        FsOp::CreateDirectory { parent, name } => {
            let Some(parent) = parent.as_local() else {
                return Err(UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Invalid local path",
                    "Create-folder parent must be local.",
                ));
            };
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\0')
                || name.chars().any(is_bidi_or_control)
            {
                return Err(UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Invalid folder name",
                    "The folder name is invalid or contains forbidden characters.",
                ));
            }
            create_directory(parent, name)
                .map(|_| ())
                .map_err(|error| local_fs_error("The folder couldn’t be created.", &error))
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

        assert_eq!(presentation.title, "Delete This Item?");
        assert_eq!(presentation.names.len(), 1);
        assert_eq!(presentation.names[0].name, "project-assets");
        assert!(presentation.names[0].is_dir);
        assert_eq!(presentation.overflow, 0);
        assert_eq!(
            presentation.risk_message,
            "This folder and everything inside it will be permanently deleted."
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
            "Selected items and everything inside 2 selected folders will be permanently deleted."
        );
    }

    #[test]
    fn local_fs_op_rejects_unsafe_names() {
        use super::execute_local_fs_operation;
        use macsftp_core::FsOp;

        let parent = FsPath::Local(LocalPath::new("/tmp"));
        let op = FsOp::CreateDirectory {
            parent: parent.clone(),
            name: "unsafe\u{202E}dir".to_string(),
        };
        assert!(execute_local_fs_operation(&op).is_err());

        let op = FsOp::CreateDirectory {
            parent: parent.clone(),
            name: "../escape".to_string(),
        };
        assert!(execute_local_fs_operation(&op).is_err());

        let op = FsOp::Rename {
            from: FsPath::Local(LocalPath::new("/tmp/old.txt")),
            to: FsPath::Local(LocalPath::new("/tmp/unsafe\u{202E}new.txt")),
        };
        assert!(execute_local_fs_operation(&op).is_err());

        let op = FsOp::Delete {
            entries: vec![macsftp_core::FsEntryRef {
                path: FsPath::Local(LocalPath::new("/")),
                is_dir: true,
            }],
        };
        assert!(execute_local_fs_operation(&op).is_err());

        let op = FsOp::Rename {
            from: FsPath::Local(LocalPath::new("/")),
            to: FsPath::Local(LocalPath::new("/tmp/renamed")),
        };
        assert!(execute_local_fs_operation(&op).is_err());
    }
}
