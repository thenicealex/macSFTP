use gpui::{Context, FontWeight, IntoElement, ParentElement, Styled, div, prelude::*, px};
use macsftp_core::{
    AppCommand, ConflictPolicy, EditPhase, EditSession, EditSessionId, EntryPath, FileKind,
    LocalPath, MetadataPolicy, ProfileId, RemotePath, RemoteSnapshot, StartTransferCommand, TabId,
    Timestamp, TransferDirection, TransferEndpoint,
};
use macsftp_ui::{ActiveTheme, format_size, text_button};
use tracing::warn;

use crate::event_coordinator::{open_edit_temp, workspace_windows};
use crate::resources::ActiveResources;
use crate::workspace::Workspace;
use crate::workspace::helpers::connected_transfer_session;

pub(crate) const EDIT_SIZE_WARN_THRESHOLD: u64 = 100 * 1024 * 1024;

/// Best-effort removal of a remote-edit session's temp directory
/// (`<edits>/<run>/<id>/`). NotFound is treated as success; any other failure
/// is logged so a leaked remote-file copy is at least diagnosable instead of
/// silent (audit APP-EDIT-003). Never records the remote path or file name.
pub(crate) fn cleanup_edit_temp_dir(temp_path: &LocalPath) {
    if let Some(parent) = std::path::Path::new(temp_path.as_str()).parent() {
        let session_dir = LocalPath::new(parent.to_string_lossy().to_string());
        if let Err(error) = macsftp_platform::cleanup_edit_session_dir(&session_dir) {
            warn!(error = %error, "could not remove edit session temp directory");
        }
    }
}

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
        // Deduplicate a genuinely live edit, but do not let a session whose
        // owning tab disappeared with a closed window block this path forever.
        // Window-close cleanup is the primary lifecycle boundary; this check is
        // the self-healing guard if that boundary was missed by an older build
        // or interrupted lifecycle.
        if let Some((existing_tab_id, existing_phase, existing_temp_path)) = cx
            .resources()
            .edit_sessions
            .find_active(profile_id, &remote_path)
            .map(|session| {
                (
                    session.tab_id,
                    session.phase.clone(),
                    session.local_temp_path.clone(),
                )
            })
        {
            let owner_is_live = self.owns_tab(existing_tab_id)
                || workspace_windows(cx).into_iter().any(|window| {
                    window
                        .read(cx)
                        .is_ok_and(|workspace| workspace.owns_tab(existing_tab_id))
                });
            if owner_is_live {
                self.status_message = Some(match existing_phase {
                    EditPhase::Editing | EditPhase::UploadingBack => {
                        let editor = cx.resources().config.config().external_editor.clone();
                        match open_edit_temp(&existing_temp_path, editor.as_deref()) {
                            Ok(()) => "Reopened file for editing".into(),
                            Err(error) => {
                                warn!(error = %error, "could not reopen editor for remote edit");
                                "Could not open file for editing".into()
                            }
                        }
                    }
                    EditPhase::Downloading => "File is still downloading for editing".into(),
                    EditPhase::CheckingRemote => "Checking the remote file before saving".into(),
                    EditPhase::RemoteConflict => {
                        "Resolve the remote edit conflict before reopening".into()
                    }
                });
                cx.notify();
                return;
            }
            cleanup_edit_sessions_for_tab(cx, existing_tab_id);
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
            self.modal_inputs.large_edit_confirm = Some(pending);
            cx.notify();
            return;
        }
        self.start_edit_download(pending, cx);
    }

    pub(crate) fn start_edit_download(&mut self, pending: PendingEdit, cx: &mut Context<Self>) {
        let edits_dir = cx.resources().app_paths.edits_dir.clone();
        let edit_run_id = cx.resources().edit_run_id.clone();
        let id = cx.resources_mut().edit_sessions.next_id();
        let file_name = std::path::Path::new(pending.remote_path.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let local_temp_path = LocalPath::new(format!(
            "{}/{}/{}/{}",
            edits_dir.as_str(),
            edit_run_id,
            id.0,
            file_name
        ));
        // The download worker creates the parent directory itself and reports
        // creation failures as transfer errors, so no synchronous FS call
        // belongs in this UI path (audit APP-EDIT-002).
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
            pending_check_id: None,
            checking_local_mtime: None,
            missing_ticks: 0,
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
        } else {
            // The command never entered the channel (full/closed). The
            // Downloading session we just registered has no in-flight transfer
            // to advance it, so it would strand: never polled (not Editing),
            // never rendered (not RemoteConflict), never cleaned up — and
            // find_active would block re-editing this file forever. Roll it back
            // and delete its temp directory. send_command already set a
            // "Busy — try again" status.
            if let Some(session) = cx.resources_mut().edit_sessions.remove(id) {
                cleanup_edit_temp_dir(&session.local_temp_path);
            }
        }
    }

    /// User confirmed the large-file edit warning: proceed with the download.
    /// The `PendingEdit` was captured when the modal opened; the connection may
    /// have changed while it was up (a reconnect bumps the tab's epoch, a
    /// disconnect drops it). Re-validate against the live tab and refresh the
    /// epoch before downloading, so we never dispatch with a stale epoch (which
    /// the runtime silently drops, stranding the session in `Downloading`).
    pub(crate) fn confirm_large_edit(&mut self, cx: &mut Context<Self>) {
        let Some(mut pending) = self.modal_inputs.large_edit_confirm.take() else {
            return;
        };
        let Some(tab) = self.state.tabs.find_tab(pending.tab_id) else {
            // The tab closed while the modal was open; nothing to edit into.
            self.status_message = Some("Connect before editing".into());
            cx.notify();
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before editing".into());
            cx.notify();
            return;
        };
        // Adopt the live epoch/profile; the captured ones may predate a reconnect.
        pending.session_epoch = session_epoch;
        pending.profile_id = profile_id;
        self.start_edit_download(pending, cx);
    }

    /// User dismissed the large-file edit warning: drop the pending edit.
    pub(crate) fn cancel_large_edit(&mut self, cx: &mut Context<Self>) {
        self.modal_inputs.large_edit_confirm = None;
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
        // Only a session actually in RemoteConflict may be resolved. The
        // conflict modal is rendered from a snapshot, so a double-click (two
        // clicks before the modal is torn down) can dispatch this twice; the
        // guard makes the second call a no-op instead of, e.g., firing a second
        // Overwrite upload. Existence alone is not enough — the first click may
        // have already moved the session to UploadingBack/Editing.
        if session.phase != EditPhase::RemoteConflict {
            return;
        }
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
                // If the command never entered the channel, the UploadingBack
                // session has no in-flight transfer to advance it and would
                // strand invisibly. Roll back to RemoteConflict so the modal
                // stays up and the user can retry. send_command already set a
                // "Busy — try again" status.
                if !self.send_command(command, cx)
                    && let Some(s) = cx.resources_mut().edit_sessions.get_mut(id)
                {
                    s.phase = EditPhase::RemoteConflict;
                }
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
                // start_edit_download mints a NEW session id (and a new temp
                // dir), so the discarded session's `<edits>/<run>/<old_id>/` directory
                // would orphan. Delete it now rather than leaking it until quit.
                cleanup_edit_temp_dir(&session.local_temp_path);
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
        let size = self.modal_inputs.large_edit_confirm.as_ref()?.size;
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

/// Remove every edit session belonging to `tab_id` and delete each one's
/// on-disk temp directory. Called when a tab or its window closes (and on
/// disconnect): the session can no longer make progress once its owning tab is
/// gone, and leaving it registered would strand its temp files and — because
/// [`find_active`] keys on `(profile_id, remote_path)` regardless of tab —
/// permanently block re-editing the same file. Best-effort on the filesystem;
/// a temp dir that was never created is ignored.
///
/// Takes `&mut App` (not `Context<Workspace>`) so it can run from both the
/// tab-close path (where `Context` derefs to `App`) and the window-closed hook.
///
/// [`find_active`]: macsftp_core::EditSessionStore::find_active
pub(crate) fn cleanup_edit_sessions_for_tab(cx: &mut gpui::App, tab_id: TabId) {
    let removed = cx.resources_mut().edit_sessions.remove_for_tab(tab_id);
    for session in removed {
        cleanup_edit_temp_dir(&session.local_temp_path);
    }
}

/// Garbage-collect edit sessions orphaned by a window close. A tab-close cleans
/// up its own sessions eagerly, but closing a whole window destroys its tabs
/// without routing each through `close_tab_by_id`; this sweep removes any edit
/// session whose owning tab is no longer held by *any* surviving window. Called
/// from the window-closed hook and again before reopening from zero windows.
/// Enumerating live windows first, then removing the unmatched sessions,
/// avoids a borrow conflict on `cx`.
pub(crate) fn cleanup_orphaned_edit_sessions(cx: &mut gpui::App) {
    let live_tabs: std::collections::HashSet<TabId> =
        crate::event_coordinator::workspace_windows(cx)
            .into_iter()
            .filter_map(|window| window.read(cx).ok().map(|workspace| workspace.tab_ids()))
            .flatten()
            .collect();
    let orphaned: Vec<TabId> = cx
        .resources()
        .edit_sessions
        .session_tab_ids()
        .into_iter()
        .filter(|tab_id| !live_tabs.contains(tab_id))
        .collect();
    for tab_id in orphaned {
        cleanup_edit_sessions_for_tab(cx, tab_id);
    }
}
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
