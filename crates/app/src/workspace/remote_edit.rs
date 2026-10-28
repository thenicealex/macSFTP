use std::collections::HashSet;

use gpui_kit::{Context, IntoElement, ParentElement, Styled, Window, div, prelude::*, px};
use macsftp_core::{
    AppCommand, BeginEdit, DispatchRollback, EditEffect, EditNotice, EditPhase, EditSessionId,
    EntryPath, LocalPath, RemotePath, TabId, Timestamp, UploadRequest, is_bidi_or_control,
};
use macsftp_ui::{ActiveTheme, TextRoleExt, format_size, text_button};
use tracing::warn;

pub(crate) use macsftp_core::{ConflictChoice, PendingEdit, conflict_remote_path};

use crate::event_coordinator::{open_edit_temp, workspace_windows};
use crate::resources::ActiveResources;
use crate::workspace::Workspace;
use crate::workspace::helpers::connected_edit_session;
use crate::workspace::overlay::OverlayFade;

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

/// Display copy for one lifecycle notice.
pub(crate) fn edit_notice_text(notice: EditNotice) -> String {
    match notice {
        EditNotice::StillDownloading => "Your copy is still downloading.".into(),
        EditNotice::CheckingServerFile => "Checking the server file before uploading…".into(),
        EditNotice::ReviewConflictBeforeReopen => {
            "Review the server file’s changes before reopening.".into()
        }
        EditNotice::LinkRepointed { conflict: true } => {
            "The link now points somewhere else. Resolve the open edit before reopening it.".into()
        }
        EditNotice::LinkRepointed { conflict: false } => {
            "The link now points somewhere else. Finish the open edit before reopening it.".into()
        }
        EditNotice::PhaseBlocked(phase) => match phase {
            EditPhase::Downloading => "Your copy is still downloading.".into(),
            EditPhase::CheckingRemote => "The server file is already being checked.".into(),
            EditPhase::UploadingBack => "Your changes are already uploading.".into(),
            EditPhase::RemoteConflict => "Review the server file’s changes first.".into(),
            EditPhase::Editing => "Your copy is ready to edit.".into(),
        },
        EditNotice::WrongConnection => {
            "Reconnect to the original server before uploading your changes.".into()
        }
        EditNotice::CopyUnreadable => {
            "Your edited copy couldn’t be read. Check file permissions and try again.".into()
        }
        EditNotice::LocalChangedDuringCheck => {
            "Your copy changed during the check. Click Upload Changes again.".into()
        }
        EditNotice::ServerCheckFailed => {
            "The server file couldn’t be checked. Click Upload Changes to try again.".into()
        }
        EditNotice::UploadStartFailed => "The upload couldn’t start. Try again.".into(),
        EditNotice::DownloadFailed { file_name } => {
            format!("Could not download {file_name} for editing")
        }
        EditNotice::CopyOpen => "Your copy is open. After saving, click Upload Changes.".into(),
        EditNotice::CopyOpenFailed => "Your copy couldn’t be opened for editing.".into(),
        EditNotice::Uploaded => "Changes uploaded".into(),
        EditNotice::UploadFailed => "Upload failed. Click Upload Changes to try again.".into(),
        EditNotice::UnsafeFileName => {
            "The selected file has a name that cannot be saved safely.".into()
        }
    }
}

fn file_name_of(path: &RemotePath) -> String {
    std::path::Path::new(path.as_str())
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

impl Workspace {
    pub(crate) fn selected_edit_session(
        &self,
        cx: &gpui_kit::App,
    ) -> Option<(EditSessionId, EditPhase)> {
        let tab = self.active_tab()?;
        let remote_path = tab
            .selection
            .selected_paths
            .iter()
            .find_map(|path| match path {
                EntryPath::Remote(path) => Some(path),
                EntryPath::Local(_) => None,
            })?;
        cx.resources()
            .edit_sessions
            .find_for_tab_path(tab.id, remote_path)
            .map(|session| (session.id, session.phase.clone()))
    }

    pub(crate) fn upload_selected_edit(&mut self, cx: &mut Context<Self>) {
        let Some((session_id, phase)) = self.selected_edit_session(cx) else {
            self.status_message =
                Some("Select a server file you’ve already opened for editing.".into());
            cx.notify();
            return;
        };
        if phase != EditPhase::Editing {
            self.show_edit_notice(EditNotice::PhaseBlocked(phase), cx);
            cx.notify();
            return;
        }

        let Some(session) = cx.resources().edit_sessions.session(session_id) else {
            return;
        };
        let Some(tab) = self.state.tabs.find_tab(session.tab_id) else {
            return;
        };
        let Some((session_epoch, connection_key)) = connected_edit_session(tab) else {
            self.show_status("Reconnect before uploading your changes.".into(), cx);
            cx.notify();
            return;
        };

        let checking_local_mtime = match std::fs::metadata(session.local_temp_path.as_str())
            .and_then(|metadata| metadata.modified())
        {
            Ok(modified) => macsftp_core::Timestamp::from_system_time(modified),
            Err(error) => {
                warn!(error = %error, "edited temp file is unavailable");
                self.show_edit_notice(EditNotice::CopyUnreadable, cx);
                cx.notify();
                return;
            }
        };

        let request = cx.resources_mut().edit_sessions.request_upload(
            session_id,
            session_epoch,
            &connection_key,
            checking_local_mtime,
        );
        match request {
            UploadRequest::Check(command) => {
                if self.send_command(AppCommand::CheckRemoteEditSnapshot(command), cx) {
                    self.show_edit_notice(EditNotice::CheckingServerFile, cx);
                } else {
                    cx.resources_mut().edit_sessions.revert_check(session_id);
                    self.show_edit_notice(EditNotice::UploadStartFailed, cx);
                }
            }
            UploadRequest::Rejected(notice) => self.show_edit_notice(notice, cx),
        }
        cx.notify();
    }

    pub(crate) fn begin_edit(
        &mut self,
        remote_path: RemotePath,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
        cx: &mut Context<Self>,
    ) {
        // Plain entries (and local files) write straight to the listed path;
        // a remote symlink overrides this with its resolved target.
        self.begin_edit_resolved(remote_path.clone(), remote_path, size, modified_at, cx);
    }

    /// Start (or reopen) an edit session for `remote_path`, writing through to
    /// `io_path`. `io_path` differs from `remote_path` only for a file symlink:
    /// keeping the link intact while editing the file it points at.
    pub(crate) fn begin_edit_resolved(
        &mut self,
        remote_path: RemotePath,
        io_path: RemotePath,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, connection_key)) = connected_edit_session(tab) else {
            self.show_status("Connect to the server before editing.".into(), cx);
            cx.notify();
            return;
        };
        let tab_id = tab.id;
        let Some(file_name) = std::path::Path::new(remote_path.as_str())
            .file_name()
            .and_then(|name| name.to_str())
        else {
            self.show_status("The selected file has no usable name.".into(), cx);
            cx.notify();
            return;
        };
        if file_name.is_empty() || file_name.chars().any(is_bidi_or_control) {
            self.show_status(
                "The selected file has a name that cannot be saved safely.".into(),
                cx,
            );
            cx.notify();
            return;
        }
        if remote_path != io_path {
            let Some(io_name) = std::path::Path::new(io_path.as_str())
                .file_name()
                .and_then(|name| name.to_str())
            else {
                self.show_status("The selected file has no usable name.".into(), cx);
                cx.notify();
                return;
            };
            if io_name.is_empty() || io_name.chars().any(is_bidi_or_control) {
                self.show_status(
                    "The selected file has a name that cannot be saved safely.".into(),
                    cx,
                );
                cx.notify();
                return;
            }
        }
        let profile_id = tab.profile_id.unwrap_or_default();
        let mut live_tabs: HashSet<TabId> = self.tab_ids().into_iter().collect();
        for window in workspace_windows(cx) {
            if let Ok(workspace) = window.read(cx) {
                live_tabs.extend(workspace.tab_ids());
            }
        }
        let pending = PendingEdit {
            remote_path,
            io_path,
            size,
            modified_at,
            session_epoch,
            connection_key,
            tab_id,
            profile_id,
        };
        match cx
            .resources_mut()
            .edit_sessions
            .begin(pending, |tab_id| live_tabs.contains(&tab_id))
        {
            BeginEdit::Handled(effects) => {
                self.apply_edit_effects(effects, cx);
                cx.notify();
            }
            BeginEdit::Start { pending, effects } => {
                self.apply_edit_effects(effects, cx);
                if pending.size.unwrap_or(0) > EDIT_SIZE_WARN_THRESHOLD {
                    self.modal_inputs.large_edit_confirm = Some(pending);
                    cx.notify();
                    return;
                }
                self.start_edit_download(pending, cx);
            }
        }
    }

    pub(crate) fn start_edit_download(&mut self, mut pending: PendingEdit, cx: &mut Context<Self>) {
        // StartTransferCommand carries the profile id as metadata only —
        // runtime routing keys transfers by (tab_id, session_epoch); only
        // edit dedup uses `connection_key`. Read it from the live tab so a
        // reconnect between arming and confirming the large-file modal
        // refreshes both values.
        if let Some(tab) = self.state.tabs.find_tab(pending.tab_id) {
            pending.profile_id = tab.profile_id.unwrap_or_default();
        }
        let started = match cx.resources_mut().edit_sessions.start_download(pending) {
            Ok(started) => started,
            Err(notice) => {
                self.show_edit_notice(notice, cx);
                cx.notify();
                return;
            }
        };
        if self.send_command(started.command, cx) {
            self.show_status("Opening your copy…".into(), cx);
        } else {
            // The command never entered the channel (full/closed). The
            // Downloading session has no in-flight transfer to advance it, so
            // it would strand; roll it back and delete its temp directory.
            let effects = cx
                .resources_mut()
                .edit_sessions
                .abort_download(started.session_id);
            self.apply_edit_effects(effects, cx);
        }
        cx.notify();
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
            self.show_status("Connect to the server before editing.".into(), cx);
            cx.notify();
            return;
        };
        let Some((session_epoch, connection_key)) = connected_edit_session(tab) else {
            self.show_status("Connect to the server before editing.".into(), cx);
            cx.notify();
            return;
        };
        // Adopt the live epoch/connection identity; the captured ones may
        // predate a reconnect.
        pending.session_epoch = session_epoch;
        pending.connection_key = connection_key;
        pending.profile_id = tab.profile_id.unwrap_or_default();
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
        let Some(phase) = cx
            .resources()
            .edit_sessions
            .session(id)
            .map(|session| session.phase.clone())
        else {
            return;
        };
        // Only a session actually in RemoteConflict may be resolved. The
        // conflict modal is rendered from a snapshot, so a double-click (two
        // clicks before the modal is torn down) can dispatch this twice; the
        // guard makes the second call a no-op instead of, e.g., firing a second
        // Overwrite upload. Existence alone is not enough — the first click may
        // have already moved the session to UploadingBack/Editing.
        if phase != EditPhase::RemoteConflict {
            return;
        }
        self.modal_inputs.reset_conflict_compare();
        let effects = cx
            .resources_mut()
            .edit_sessions
            .resolve_conflict(id, choice);
        self.apply_edit_effects(effects, cx);
        cx.notify();
    }

    pub(crate) fn compare_edit_conflict(&mut self, id: EditSessionId, cx: &mut Context<Self>) {
        let Some((phase, tab_id, profile_id)) = cx
            .resources()
            .edit_sessions
            .session(id)
            .map(|session| (session.phase.clone(), session.tab_id, session.profile_id))
        else {
            return;
        };
        if phase != EditPhase::RemoteConflict {
            return;
        }
        if macsftp_platform::detect_default_diff_tool().is_none() {
            self.modal_inputs.begin_conflict_compare(id);
            self.modal_inputs.conflict_compare_loading = false;
            self.modal_inputs.conflict_compare_error =
                Some("No supported diff tool found (/usr/bin/opendiff or code).".into());
            cx.notify();
            return;
        }
        // Always re-download. A snapshot from an earlier comparison is stale
        // once the server file changes again, and diffing against it would
        // present old remote content as if it were current.
        self.modal_inputs.begin_conflict_compare(id);

        let live_profile_id = self
            .state
            .tabs
            .find_tab(tab_id)
            .map(|tab| tab.profile_id.unwrap_or_default())
            .unwrap_or(profile_id);

        let Some(command) = cx
            .resources()
            .edit_sessions
            .plan_conflict_compare(id, live_profile_id)
        else {
            return;
        };

        if !self.send_command(command, cx) {
            self.modal_inputs.conflict_compare_loading = false;
            self.modal_inputs.conflict_compare_error =
                Some("Could not start comparison download. Try again.".into());
        }
        cx.notify();
    }

    /// Surface one lifecycle notice in this window's status bar.
    pub(crate) fn show_edit_notice(&mut self, notice: EditNotice, cx: &mut Context<Self>) {
        self.show_status(edit_notice_text(notice).into(), cx);
    }

    /// Execute lifecycle effects produced by a user action in this window.
    ///
    /// A failed dispatch rolls the session back through the lifecycle (the
    /// effect names its rollback), so a command that never entered the channel
    /// cannot strand a phase. The coordinator has its own executor for effects
    /// that arrive process-wide.
    pub(crate) fn apply_edit_effects(&mut self, effects: Vec<EditEffect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                EditEffect::Dispatch {
                    session_id,
                    command,
                    rollback,
                    ..
                } => {
                    if self.send_command(command, cx) {
                        continue;
                    }
                    let file_name = cx
                        .resources()
                        .edit_sessions
                        .session(session_id)
                        .map(|session| file_name_of(&session.remote_path))
                        .unwrap_or_default();
                    let follow_up = cx
                        .resources_mut()
                        .edit_sessions
                        .dispatch_failed(session_id, rollback);
                    match rollback {
                        DispatchRollback::AbortDownload => {
                            self.show_edit_notice(EditNotice::DownloadFailed { file_name }, cx)
                        }
                        DispatchRollback::RevertCheck | DispatchRollback::RestoreConflict => {
                            self.show_edit_notice(EditNotice::UploadStartFailed, cx)
                        }
                    }
                    // A rollback can itself schedule cleanup (an aborted
                    // download's temp directory); those effects never dispatch.
                    self.apply_edit_effects(follow_up, cx);
                }
                EditEffect::OpenEditor { temp_path, .. } => {
                    let config = cx.resources().config.config();
                    let file_name = std::path::Path::new(temp_path.as_str())
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let editor = macsftp_core::resolve_editor_app(
                        &file_name,
                        &config.editor_rules,
                        config.external_editor.as_deref(),
                    );
                    match open_edit_temp(&temp_path, editor) {
                        Ok(()) => self.show_edit_notice(EditNotice::CopyOpen, cx),
                        Err(error) => {
                            warn!(error = %error, "could not open editor for remote edit");
                            self.show_edit_notice(EditNotice::CopyOpenFailed, cx);
                        }
                    }
                }
                EditEffect::DeleteTempDir { path } => cleanup_edit_temp_dir(&path),
                EditEffect::RebaseListing {
                    tab_id,
                    path,
                    snapshot,
                } => {
                    self.sync_remote_entry_snapshot(tab_id, &path, snapshot);
                }
                EditEffect::Notice { notice, .. } => self.show_edit_notice(notice, cx),
            }
        }
    }

    pub(crate) fn render_large_edit_modal(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let open = self.modal_inputs.large_edit_confirm.is_some();
        let fade = OverlayFade::sample("large-edit", open, window, cx);
        let size = self.modal_inputs.large_edit_confirm.as_ref()?.size;
        let theme = cx.theme().clone();
        let body = match size {
            Some(size) => format!(
                "This is a large file ({}). Download a copy to edit?",
                format_size(Some(size))
            ),
            None => "This is a large file. Download a copy to edit?".to_string(),
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
            .rounded(theme.sizes.modal_radius)
            .shadow(theme.shadow_modal())
            .child(
                div()
                    .text_role(&theme, theme.typography.title)
                    .text_color(theme.colors.text)
                    .child("Edit Large File?"),
            )
            .child(
                div()
                    .text_role(&theme, theme.typography.secondary)
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
                        text_button("large-edit-confirm", "Open for Editing")
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
                .bg(theme.scrim())
                .opacity(fade.opacity())
                .child(card)
                .into_any_element(),
        )
    }

    pub(crate) fn render_edit_conflict_modal(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        // One lookup feeds both the fade and the body, so the two can never
        // disagree about whether a conflict is up.
        let conflict = self.active_tab().and_then(|tab| {
            cx.resources()
                .edit_sessions
                .conflict_sessions()
                .find(|session| session.tab_id == tab.id)
                .map(|session| session.id)
        });
        let fade = OverlayFade::sample("edit-conflict", conflict.is_some(), window, cx);
        let id = conflict?;
        let theme = cx.theme().clone();

        // Probe once per conflict session, not once per frame: detection hits
        // the filesystem and this modal can stay open for a long time.
        let diff_tool_available = match self.modal_inputs.conflict_diff_tool {
            Some((cached_id, available)) if cached_id == id => available,
            _ => {
                let available = macsftp_platform::detect_default_diff_tool().is_some();
                self.modal_inputs.conflict_diff_tool = Some((id, available));
                available
            }
        };
        let (is_loading, compare_error) = self
            .modal_inputs
            .conflict_compare_for(id)
            .unwrap_or((false, None));

        let card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(460.0))
            .p_4()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded(theme.sizes.modal_radius)
            .shadow(theme.shadow_modal())
            .child(
                div()
                    .text_role(&theme, theme.typography.title)
                    .text_color(theme.colors.text)
                    .child("Server File Changed"),
            )
            .child(
                div()
                    .text_role(&theme, theme.typography.secondary)
                    .text_color(theme.colors.text_muted)
                    .child(
                        "The server file changed after you opened it. Replacing it will discard those changes.",
                    ),
            )
            .when(!diff_tool_available, |card| {
                card.child(
                    div()
                        .text_role(&theme, theme.typography.secondary)
                        .text_color(theme.colors.text_muted)
                        .child("No supported diff tool found (/usr/bin/opendiff or code)."),
                )
            })
            .when_some(compare_error, |card, error| {
                card.child(
                    div()
                        .text_role(&theme, theme.typography.secondary)
                        .text_color(theme.colors.error)
                        .child(error.to_string()),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child({
                        let is_disabled = !diff_tool_available || is_loading;
                        let mut compare_button = text_button(
                            "edit-conflict-compare",
                            if is_loading {
                                "Comparing…"
                            } else {
                                "Compare Changes…"
                            },
                        );
                        if !is_disabled {
                            compare_button = compare_button.on_click(cx.listener(move |workspace, _event, _window, cx| {
                                workspace.compare_edit_conflict(id, cx);
                            }));
                        }
                        div()
                            .when(is_disabled, |this| this.opacity(0.5))
                            .child(compare_button)
                    })
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                text_button("edit-conflict-later", "Decide Later").on_click(
                                    cx.listener(move |workspace, _event, _window, cx| {
                                        workspace.resolve_edit_conflict(
                                            id,
                                            ConflictChoice::Later,
                                            cx,
                                        );
                                    }),
                                ),
                            )
                            .child(
                                text_button("edit-conflict-discard", "Discard My Changes")
                                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                                        workspace.resolve_edit_conflict(
                                            id,
                                            ConflictChoice::DiscardLocal,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                text_button("edit-conflict-overwrite", "Replace Server File")
                                    .danger(true)
                                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                                        workspace.resolve_edit_conflict(
                                            id,
                                            ConflictChoice::Overwrite,
                                            cx,
                                        );
                                    })),
                            ),
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
                .bg(theme.scrim())
                .opacity(fade.opacity())
                .child(card)
                .into_any_element(),
        )
    }
}

/// Remove every edit session belonging to `tab_id` and delete each one's
/// on-disk temp directory. Called when a tab or its window closes (and on
/// disconnect): the session can no longer make progress once its owning tab is
/// gone, and leaving it registered would strand its temp files and — because
/// sessions are deduplicated by `(connection_key, io_path)` regardless of tab —
/// permanently block re-editing the same file. Best-effort on the filesystem;
/// a temp dir that was never created is ignored.
///
/// Takes `&mut App` (not `Context<Workspace>`) so it can run from both the
/// tab-close path (where `Context` derefs to `App`) and the window-closed hook.
pub(crate) fn cleanup_edit_sessions_for_tab(cx: &mut gpui_kit::App, tab_id: TabId) {
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
pub(crate) fn cleanup_orphaned_edit_sessions(cx: &mut gpui_kit::App) {
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
