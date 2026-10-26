use gpui_kit::{
    Context, FontWeight, IntoElement, ParentElement, Styled, Window, div, prelude::*, px,
};
use macsftp_core::{
    AppCommand, CheckRemoteEditSnapshotCommand, ConflictPolicy, ConnectionKey, EditPhase,
    EditSession, EditSessionId, EntryPath, LocalPath, MetadataPolicy, ProfileId, RemotePath,
    RemoteSnapshot, StartTransferCommand, TabId, Timestamp, TransferDirection, TransferEndpoint,
    is_bidi_or_control,
};
use macsftp_ui::{ActiveTheme, format_size, text_button};
use tracing::warn;

use crate::event_coordinator::{open_edit_temp, workspace_windows};
use crate::resources::ActiveResources;
use crate::workspace::Workspace;
use crate::workspace::helpers::connected_edit_session;
use crate::workspace::overlay::OverlayFade;

pub(crate) const EDIT_SIZE_WARN_THRESHOLD: u64 = 100 * 1024 * 1024;

pub(crate) fn conflict_remote_path(local_temp_path: &LocalPath) -> LocalPath {
    let path = std::path::Path::new(local_temp_path.as_str());
    let parent = path.parent().unwrap_or(path);
    let ext = path.extension().and_then(|e| e.to_str());
    let file_name = match ext {
        Some(e) if !e.is_empty() && !e.chars().any(is_bidi_or_control) => {
            format!("conflict-remote.{}", e)
        }
        _ => "conflict-remote".to_string(),
    };
    // Keep the snapshot in a subdirectory. Sharing the session directory would
    // collide with a remote file literally named `conflict-remote.<ext>`, and
    // the comparison download would overwrite the user's edited copy. The
    // watcher only understands `<session>/<file>`, so writing this snapshot
    // can never mark the session dirty either.
    LocalPath::new(
        parent
            .join("compare")
            .join(file_name)
            .to_string_lossy()
            .to_string(),
    )
}

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
    /// The path the user opened, as the listing showed it (UI identity).
    pub remote_path: RemotePath,
    /// The write-through path every transfer and check must use: the resolved
    /// target for a file symlink, otherwise `remote_path`.
    pub io_path: RemotePath,
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
    pub session_epoch: u64,
    /// Identity of the physical connection the edit was opened through; used
    /// for dedup only. The transfer commands keep carrying `ProfileId` (the
    /// tab's own value, `ProfileId(0)` when manual).
    pub connection_key: ConnectionKey,
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
            self.show_status(
                match phase {
                    EditPhase::Downloading => "Your copy is still downloading.".into(),
                    EditPhase::CheckingRemote => "The server file is already being checked.".into(),
                    EditPhase::UploadingBack => "Your changes are already uploading.".into(),
                    EditPhase::RemoteConflict => "Review the server file’s changes first.".into(),
                    EditPhase::Editing => "Your copy is ready to edit.".into(),
                },
                cx,
            );
            cx.notify();
            return;
        }

        let Some(session) = cx.resources().edit_sessions.get(session_id).cloned() else {
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
        if connection_key != session.connection_key {
            self.status_message =
                Some("Reconnect to the original server before uploading your changes.".into());
            cx.notify();
            return;
        }

        let checking_local_mtime = match std::fs::metadata(session.local_temp_path.as_str())
            .and_then(|metadata| metadata.modified())
        {
            Ok(modified) => Timestamp::from_system_time(modified),
            Err(error) => {
                warn!(error = %error, "edited temp file is unavailable");
                if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
                    session.phase = EditPhase::Editing;
                    session.pending_check_id = None;
                    session.checking_local_mtime = None;
                }
                self.show_status(
                    "Your edited copy couldn’t be read. Check file permissions and try again."
                        .into(),
                    cx,
                );
                cx.notify();
                return;
            }
        };

        let check_id = cx.resources_mut().edit_sessions.next_check_id();
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
            session.phase = EditPhase::CheckingRemote;
            session.session_epoch = session_epoch;
            session.pending_check_id = Some(check_id);
            session.checking_local_mtime = Some(checking_local_mtime);
        }
        let command = AppCommand::CheckRemoteEditSnapshot(CheckRemoteEditSnapshotCommand {
            tab_id: session.tab_id,
            session_epoch,
            edit_session_id: session_id,
            check_id,
            path: session.io_path,
        });
        if self.send_command(command, cx) {
            self.show_status("Checking the server file before uploading…".into(), cx);
        } else if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
            session.phase = EditPhase::Editing;
            session.pending_check_id = None;
            session.checking_local_mtime = None;
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
        // A link that was re-pointed after its session was created would
        // otherwise produce two live sessions sharing one `remote_path`, and
        // the pane lookup could select either. Bind edit sessions to the target
        // they were opened against and refuse the ambiguous case instead.
        if let Some((existing_tab_id, existing_io_path, existing_phase)) = cx
            .resources()
            .edit_sessions
            .find_active_for_remote_path(&connection_key, &remote_path)
            .map(|session| {
                (
                    session.tab_id,
                    session.io_path.clone(),
                    session.phase.clone(),
                )
            })
            && existing_io_path != io_path
        {
            let owner_is_live = self.owns_tab(existing_tab_id)
                || workspace_windows(cx).into_iter().any(|window| {
                    window
                        .read(cx)
                        .is_ok_and(|workspace| workspace.owns_tab(existing_tab_id))
                });
            if owner_is_live {
                self.show_status(match existing_phase {
                    EditPhase::RemoteConflict => {
                        "The link now points somewhere else. Resolve the open edit before reopening it."
                            .into()
                    }
                    _ => {
                        "The link now points somewhere else. Finish the open edit before reopening it."
                            .into()
                    }
                }, cx);
                cx.notify();
                return;
            }
            // The old session's tab is gone, so it cannot make progress; reap
            // it before binding this link to its new target.
            cleanup_edit_sessions_for_tab(cx, existing_tab_id);
        }
        // Deduplicate a genuinely live edit, but do not let a session whose
        // owning tab disappeared with a closed window block this path forever.
        // Window-close cleanup is the primary lifecycle boundary; this check is
        // the self-healing guard if that boundary was missed by an older build
        // or interrupted lifecycle.
        if let Some((existing_tab_id, existing_phase, existing_temp_path)) = cx
            .resources()
            .edit_sessions
            .find_active(&connection_key, &io_path)
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
                self.show_status(match existing_phase {
                    EditPhase::Editing | EditPhase::UploadingBack => {
                        let config = cx.resources().config.config();
                        let file_name = std::path::Path::new(existing_temp_path.as_str())
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let editor = macsftp_core::resolve_editor_app(
                            &file_name,
                            &config.editor_rules,
                            config.external_editor.as_deref(),
                        );
                        match open_edit_temp(&existing_temp_path, editor) {
                            Ok(()) => "Your copy is open for editing.".into(),
                            Err(error) => {
                                warn!(error = %error, "could not reopen editor for remote edit");
                                "Your copy couldn’t be opened for editing.".into()
                            }
                        }
                    }
                    EditPhase::Downloading => "Your copy is still downloading.".into(),
                    EditPhase::CheckingRemote => {
                        "Checking the server file before uploading…".into()
                    }
                    EditPhase::RemoteConflict => {
                        "Review the server file’s changes before reopening.".into()
                    }
                }, cx);
                cx.notify();
                return;
            }
            cleanup_edit_sessions_for_tab(cx, existing_tab_id);
        }
        let pending = PendingEdit {
            remote_path,
            io_path,
            size,
            modified_at,
            session_epoch,
            connection_key,
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
        let Some(file_name) = std::path::Path::new(pending.remote_path.as_str())
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && !name.chars().any(is_bidi_or_control))
        else {
            self.show_status(
                "The selected file has a name that cannot be saved safely.".into(),
                cx,
            );
            cx.notify();
            return;
        };
        // StartTransferCommand carries the profile id as metadata only —
        // runtime routing keys transfers by (tab_id, session_epoch); only
        // edit dedup uses `connection_key`. Read it from the live tab so a
        // reconnect between arming and confirming the large-file modal
        // refreshes both values.
        let transfer_profile_id = self
            .state
            .tabs
            .find_tab(pending.tab_id)
            .map(|tab| tab.profile_id.unwrap_or(ProfileId(0)));
        let edits_dir = cx.resources().app_paths.edits_dir.clone();
        let edit_run_id = cx.resources().edit_run_id.clone();
        let id = cx.resources_mut().edit_sessions.next_id();
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
            io_path: pending.io_path.clone(),
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: transfer_profile_id.unwrap_or_default(),
            connection_key: pending.connection_key.clone(),
            local_temp_path: local_temp_path.clone(),
            phase: EditPhase::Downloading,
            remote_snapshot: RemoteSnapshot {
                size: pending.size,
                modified_at: pending.modified_at,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        };
        cx.resources_mut().edit_sessions.register(session);
        let command = AppCommand::StartTransfer(StartTransferCommand {
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: transfer_profile_id.unwrap_or(ProfileId(0)),
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(pending.io_path)],
            destination: TransferEndpoint::Local(local_temp_path),
            metadata_policy: MetadataPolicy::default(),
            // An edit temp is always ours to overwrite; a download must never
            // prompt the user (see build_edit_upload_command for the rationale).
            conflict_policy: ConflictPolicy::OverwriteAll,
        });
        if self.send_command(command, cx) {
            self.show_status("Opening your copy…".into(), cx);
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
        self.modal_inputs.reset_conflict_compare();
        match choice {
            ConflictChoice::Overwrite => {
                let command = build_edit_upload_command(
                    &session.local_temp_path,
                    &session.io_path,
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
                    io_path: session.io_path.clone(),
                    size: session.remote_snapshot.size,
                    modified_at: session.remote_snapshot.modified_at,
                    session_epoch: session.session_epoch,
                    connection_key: session.connection_key.clone(),
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

    pub(crate) fn compare_edit_conflict(&mut self, id: EditSessionId, cx: &mut Context<Self>) {
        let Some(session) = cx.resources().edit_sessions.get(id).cloned() else {
            return;
        };
        if session.phase != EditPhase::RemoteConflict {
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
        let compare_path = conflict_remote_path(&session.local_temp_path);

        self.modal_inputs.begin_conflict_compare(id);

        let transfer_profile_id = self
            .state
            .tabs
            .find_tab(session.tab_id)
            .map(|tab| tab.profile_id.unwrap_or(ProfileId(0)))
            .unwrap_or(session.profile_id);

        let command = AppCommand::StartTransfer(StartTransferCommand {
            tab_id: session.tab_id,
            session_epoch: session.session_epoch,
            profile_id: transfer_profile_id,
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(session.io_path)],
            destination: TransferEndpoint::Local(compare_path),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
        });

        if !self.send_command(command, cx) {
            self.modal_inputs.conflict_compare_loading = false;
            self.modal_inputs.conflict_compare_error =
                Some("Could not start comparison download. Try again.".into());
        }
        cx.notify();
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
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.colors.text)
                    .child("Edit Large File?"),
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
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.colors.text)
                    .child("Server File Changed"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child(
                        "The server file changed after you opened it. Replacing it will discard those changes.",
                    ),
            )
            .when(!diff_tool_available, |card| {
                card.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child("No supported diff tool found (/usr/bin/opendiff or code)."),
                )
            })
            .when_some(compare_error, |card, error| {
                card.child(
                    div()
                        .text_size(px(11.0))
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
/// [`find_active`] keys on `(connection_key, io_path)` regardless of tab —
/// permanently block re-editing the same file. Best-effort on the filesystem;
/// a temp dir that was never created is ignored.
///
/// Takes `&mut App` (not `Context<Workspace>`) so it can run from both the
/// tab-close path (where `Context` derefs to `App`) and the window-closed hook.
///
/// [`find_active`]: macsftp_core::EditSessionStore::find_active
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

/// Uploads changes from a remote-edit session. This deliberately keeps the
/// default metadata policy instead of the user's transfer preference:
/// `advance_uploading_back` rebases the remote snapshot from the local temp
/// file's mtime, which is only valid when the upload preserved mtime.
/// Letting the preference disable mtime here would make the next refresh
/// report a spurious conflict.
///
/// `remote_path` must be the session's `io_path` (the resolved target for a
/// symlink), never the listed link path: uploading to the link would replace
/// it with a regular file and break every other reference to the target.
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
        // Keep the default policy (preserve mtime and permissions): remote-edit
        // relies on preserving the local temp file's mtime to construct the
        // baseline for `advance_uploading_back`. If mtime preservation were
        // disabled per user preference, the server would assign its current time,
        // causing subsequent refreshes to report a false conflict.
        metadata_policy: MetadataPolicy::default(),
        // The edit layer already ran its own (size, mtime) divergence check, or
        // the user explicitly chose "Replace Server File". The pipeline-level
        // existence prompt would be redundant (the origin file always exists),
        // so overwrite unconditionally rather than emitting a TransferConflict.
        conflict_policy: ConflictPolicy::OverwriteAll,
    })
}
