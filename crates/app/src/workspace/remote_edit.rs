use gpui::{Context, FontWeight, IntoElement, ParentElement, Styled, div, prelude::*, px};
use macsftp_core::{
    AppCommand, ConflictPolicy, EditPhase, EditSession, EditSessionId, EntryPath, FileKind,
    LocalPath, MetadataPolicy, ProfileId, RemotePath, RemoteSnapshot, StartTransferCommand, TabId,
    Timestamp, TransferDirection, TransferEndpoint,
};
use macsftp_ui::{ActiveTheme, format_size, text_button};

use crate::resources::ActiveResources;
use crate::workspace::Workspace;
use crate::workspace::helpers::connected_transfer_session;

pub(crate) const EDIT_SIZE_WARN_THRESHOLD: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct PendingEdit {
    pub remote_path: RemotePath,
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub tab_id: TabId,
}

/// How the user chose to resolve a remote-changed-under-edit conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictChoice {
    /// Force the local temp file back over the (changed) remote.
    Overwrite,
    /// Throw away local edits and re-download the current remote.
    DiscardLocal,
    /// Dismiss for now; the session returns to `Editing`.
    Later,
}

impl Workspace {
    /// Resolve the first selected remote file (non-directory) from the active
    /// tab's listing into `(RemotePath, size, modified_at)` and open it for
    /// editing. Mirrors `download_selection`'s selection parsing. A directory
    /// or empty selection is a no-op with a status hint.
    pub(crate) fn request_edit_selection(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        // Extract owned values while borrowing `tab`, so the borrow ends
        // before the `&mut self` call to `begin_edit` below.
        let selected: Option<(RemotePath, Option<u64>, Option<Timestamp>)> = tab
            .selection
            .selected_paths
            .iter()
            .find_map(|path| match path {
                EntryPath::Remote(p) => tab
                    .remote
                    .entries
                    .iter()
                    .find(|entry| &entry.path == p && entry.kind != FileKind::Directory)
                    .map(|entry| (entry.path.clone(), entry.size, entry.modified_at)),
                EntryPath::Local(_) => None,
            });
        let Some((remote_path, size, modified_at)) = selected else {
            self.status_message = Some("Select one remote file to edit".into());
            cx.notify();
            return;
        };
        self.begin_edit(remote_path, size, modified_at, cx);
    }

    pub(crate) fn begin_edit(
        &mut self,
        remote_path: RemotePath,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before editing".into());
            cx.notify();
            return;
        };
        let tab_id = tab.id;
        // 查重：同 profile + 远程路径已有活跃会话 → 复用，不重复下载。
        if cx
            .resources()
            .edit_sessions
            .find_active(profile_id, &remote_path)
            .is_some()
        {
            self.status_message = Some("This file is already open for editing".into());
            cx.notify();
            return;
        }
        let pending = PendingEdit {
            remote_path,
            size,
            modified_at,
            session_epoch,
            profile_id,
            tab_id,
        };
        if pending.size.unwrap_or(0) > EDIT_SIZE_WARN_THRESHOLD {
            self.large_edit_confirm = Some(pending);
            cx.notify();
            return;
        }
        self.start_edit_download(pending, cx);
    }

    pub(crate) fn start_edit_download(&mut self, pending: PendingEdit, cx: &mut Context<Self>) {
        let edits_dir = cx.resources().app_paths.edits_dir.clone();
        let id = cx.resources_mut().edit_sessions.next_id();
        let file_name = std::path::Path::new(pending.remote_path.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let local_temp_path =
            LocalPath::new(format!("{}/{}/{}", edits_dir.as_str(), id.0, file_name));
        // 建会话目录（忽略已存在）。
        if let Some(parent) = std::path::Path::new(local_temp_path.as_str()).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let session = EditSession {
            id,
            remote_path: pending.remote_path.clone(),
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            local_temp_path: local_temp_path.clone(),
            phase: EditPhase::Downloading,
            remote_snapshot: RemoteSnapshot {
                size: pending.size,
                modified_at: pending.modified_at,
            },
            local_mtime: None,
            active_transfer: None,
        };
        cx.resources_mut().edit_sessions.register(session);
        let command = AppCommand::StartTransfer(StartTransferCommand {
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(pending.remote_path)],
            destination: TransferEndpoint::Local(local_temp_path),
            metadata_policy: MetadataPolicy::default(),
            // An edit temp is always ours to overwrite; a download must never
            // prompt the user (see build_edit_upload_command for the rationale).
            conflict_policy: ConflictPolicy::OverwriteAll,
        });
        if self.send_command(command, cx) {
            self.status_message = Some("Opening for edit…".into());
            cx.notify();
        }
    }

    /// User confirmed the large-file edit warning: proceed with the download.
    pub(crate) fn confirm_large_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.large_edit_confirm.take() {
            self.start_edit_download(pending, cx);
        }
    }

    /// User dismissed the large-file edit warning: drop the pending edit.
    pub(crate) fn cancel_large_edit(&mut self, cx: &mut Context<Self>) {
        self.large_edit_confirm = None;
        cx.notify();
    }

    /// Resolve a `RemoteConflict` edit session per the user's `choice`.
    pub(crate) fn resolve_edit_conflict(
        &mut self,
        id: EditSessionId,
        choice: ConflictChoice,
        cx: &mut Context<Self>,
    ) {
        // Clone the session out first so we do not hold a resources borrow
        // across the resources_mut()/send_command() calls below.
        let Some(session) = cx.resources().edit_sessions.get(id).cloned() else {
            return;
        };
        match choice {
            ConflictChoice::Overwrite => {
                let command = build_edit_upload_command(
                    &session.local_temp_path,
                    &session.remote_path,
                    session.session_epoch,
                    session.profile_id,
                    session.tab_id,
                );
                if let Some(s) = cx.resources_mut().edit_sessions.get_mut(id) {
                    s.phase = EditPhase::UploadingBack;
                }
                self.send_command(command, cx);
            }
            ConflictChoice::DiscardLocal => {
                // Re-download the current remote over the local temp; the fresh
                // session starts back at Downloading.
                let pending = PendingEdit {
                    remote_path: session.remote_path.clone(),
                    size: session.remote_snapshot.size,
                    modified_at: session.remote_snapshot.modified_at,
                    session_epoch: session.session_epoch,
                    profile_id: session.profile_id,
                    tab_id: session.tab_id,
                };
                cx.resources_mut().edit_sessions.remove(id);
                self.start_edit_download(pending, cx);
            }
            ConflictChoice::Later => {
                if let Some(s) = cx.resources_mut().edit_sessions.get_mut(id) {
                    s.phase = EditPhase::Editing;
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn render_large_edit_modal(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let size = self.large_edit_confirm.as_ref()?.size;
        let theme = cx.theme().clone();
        let body = match size {
            Some(size) => format!(
                "This file is large ({}). Download it for editing?",
                format_size(Some(size))
            ),
            None => "This file is large. Download it for editing?".to_string(),
        };

        let card = div()
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
                    .child("Edit large file?"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("large-edit-cancel", "Cancel").on_click(cx.listener(
                            |workspace, _event, _window, cx| {
                                workspace.cancel_large_edit(cx);
                            },
                        )),
                    )
                    .child(
                        text_button("large-edit-confirm", "Edit")
                            .primary(true)
                            .on_click(cx.listener(|workspace, _event, _window, cx| {
                                workspace.confirm_large_edit(cx);
                            })),
                    ),
            );

        Some(
            div()
                .id("large-edit-scrim")
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

    pub(crate) fn render_edit_conflict_modal(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let active_tab_id = self.active_tab()?.id;
        let id = cx
            .resources()
            .edit_sessions
            .conflict_sessions()
            .find(|s| s.tab_id == active_tab_id)
            .map(|s| s.id)?;
        let theme = cx.theme().clone();

        let card = div()
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
                    .child("Remote file changed"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child(
                        "The remote file changed since you opened it for editing. \
                         Overwriting will discard the remote changes.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("edit-conflict-later", "Later").on_click(cx.listener(
                            move |workspace, _event, _window, cx| {
                                workspace.resolve_edit_conflict(id, ConflictChoice::Later, cx);
                            },
                        )),
                    )
                    .child(
                        text_button("edit-conflict-discard", "Discard local changes").on_click(
                            cx.listener(move |workspace, _event, _window, cx| {
                                workspace.resolve_edit_conflict(
                                    id,
                                    ConflictChoice::DiscardLocal,
                                    cx,
                                );
                            }),
                        ),
                    )
                    .child(
                        text_button("edit-conflict-overwrite", "Overwrite remote")
                            .danger(true)
                            .on_click(cx.listener(move |workspace, _event, _window, cx| {
                                workspace.resolve_edit_conflict(id, ConflictChoice::Overwrite, cx);
                            })),
                    ),
            );

        Some(
            div()
                .id("edit-conflict-scrim")
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
}

/// Build the `StartTransfer(Upload)` command that sends an edited temp file
/// back to its remote origin. Mirrors the inline download command in
/// [`Workspace::start_edit_download`]; factored out so the edit watcher (and
/// Task 11's conflict modal) share one construction site rather than
/// duplicating the struct literal.
pub(crate) fn build_edit_upload_command(
    temp: &LocalPath,
    remote_path: &RemotePath,
    session_epoch: u64,
    profile_id: ProfileId,
    tab_id: TabId,
) -> AppCommand {
    AppCommand::StartTransfer(StartTransferCommand {
        tab_id,
        session_epoch,
        profile_id,
        direction: TransferDirection::Upload,
        sources: vec![TransferEndpoint::Local(temp.clone())],
        destination: TransferEndpoint::Remote(remote_path.clone()),
        metadata_policy: MetadataPolicy::default(),
        // The edit layer already ran its own (size, mtime) divergence check, or
        // the user explicitly chose "Overwrite remote". The pipeline-level
        // existence prompt would be redundant (the origin file always exists),
        // so overwrite unconditionally rather than emitting a TransferConflict.
        conflict_policy: ConflictPolicy::OverwriteAll,
    })
}
