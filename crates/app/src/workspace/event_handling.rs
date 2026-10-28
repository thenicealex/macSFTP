use gpui_kit::{Context, UniformListScrollHandle, Window};
use macsftp_core::{
    EntryPath, ErrorCode, FileKind, ModalRequest, RemotePath, RemoteSnapshot, RestoredTabTarget,
    TabId, UserFacingError, WindowEvent, sort_entries,
};

use tracing::debug;

use crate::resources::ActiveResources;

impl crate::workspace::Workspace {
    pub(crate) fn handle_window_event(
        &mut self,
        event: WindowEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.should_accept_event(&event) {
            // Events may carry host-key fingerprints, paths, and remote error
            // details. Keep stale-event diagnostics structural rather than
            // formatting the payload.
            debug!("dropped stale event");
            return;
        }

        match event {
            WindowEvent::TabConnecting { .. } => {
                // Already reflected optimistically when the command was
                // sent; nothing to update.
            }
            WindowEvent::HostKeyUnknown(prompt) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(prompt.tab_id) {
                    tab.await_host_key(prompt.session_id, prompt.request_id);
                    self.state.modals.active.push(ModalRequest::HostKey(prompt));
                    window.focus(&self.modal_focus, cx);
                }
            }
            WindowEvent::KeyboardInteractivePrompt(prompt) => {
                self.present_keyboard_interactive(prompt, window, cx);
            }
            WindowEvent::HostKeyMismatch(mismatch) => {
                // Security block, no override (architecture §10): the tab fails
                // with an explanation and the known_hosts file location.
                if let Some(tab) = self.state.tabs.find_tab_mut(mismatch.scope.tab_id) {
                    let mut error = UserFacingError::new(
                        ErrorCode::HostKeyMismatch,
                        "Server Identity Changed",
                        format!(
                            "The identity of {}:{} differs from the saved one. \
                             Someone may be intercepting this connection. Connection blocked.",
                            mismatch.host, mismatch.port
                        ),
                    );
                    error.detail = Some(format!(
                        "Expected {}, got {}. Stored entries: \
                         ~/Library/Application Support/macSFTP/known_hosts and \
                         ~/.ssh/known_hosts.",
                        mismatch
                            .expected_fingerprint_sha256
                            .as_deref()
                            .unwrap_or("(unknown)"),
                        mismatch.actual_fingerprint_sha256
                    ));
                    tab.fail(error);
                }
            }
            WindowEvent::TabConnected(scoped) => {
                let tab_id = scoped.scope.tab_id;
                let remote_root = scoped.payload.remote_root;
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                    // Prefer the path restored from the previous session so the
                    // user lands where they left off; fall back to the actor's
                    // remote_root. Clear the preference after use so reconnect
                    // navigates from the live path.
                    let navigate_to = tab
                        .restored_target
                        .as_ref()
                        .and_then(|target| target.remote_path.clone())
                        .unwrap_or_else(|| remote_root.clone());
                    if let Some(target) = tab.restored_target.as_mut() {
                        target.remote_path = None;
                    }
                    tab.complete_connect(
                        scoped.scope.session_id,
                        macsftp_core::Timestamp(std::time::SystemTime::now()),
                    );
                    tab.remote.entries.clear();
                    tab.remote.path = Some(navigate_to.clone());
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                    self.remote.scroll = UniformListScrollHandle::new();
                    self.request_remote_directory(tab_id, navigate_to, cx);
                    // Reconcile remote residual temp files from a previous run
                    // now that a live session to this host exists (current architecture §8).
                    self.clean_remote_residual_temps(tab_id, cx);
                    self.record_recent_for_tab(tab_id, cx);
                }
            }
            WindowEvent::TabDisconnected(scoped) => {
                let tab_id = scoped.scope.tab_id;
                cx.resources_mut()
                    .edit_sessions
                    .update_epoch_for_tab(tab_id, scoped.scope.session_epoch);
                // Before clearing the live remote path, stash it in the tab's
                // restored_target so build_session_snapshot still records the
                // last browsing location after disconnect-then-quit.
                // TabConnected clears this preference once it is consumed.
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id)
                    && let Some(remote_path) = tab.remote.path.take()
                {
                    match tab.restored_target.as_mut() {
                        Some(target) => target.remote_path = Some(remote_path),
                        None => {
                            if let Some(settings) = tab.connection_settings.as_ref() {
                                let host = settings.host.clone();
                                let port = settings.port;
                                let username = settings.username.clone();
                                let profile_id = tab.profile_id;
                                tab.restored_target = Some(Box::new(RestoredTabTarget {
                                    host,
                                    port,
                                    username,
                                    profile_id,
                                    remote_path: Some(remote_path),
                                }));
                            }
                        }
                    }
                }
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                    tab.disconnect(scoped.payload.reason);
                    tab.remote.entries.clear();
                    tab.remote.path = None;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                }
                let had_modal = self.drain_expired_modals();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            WindowEvent::AuthFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id) {
                    tab.fail(scoped.payload.reason);
                }
                let had_modal = self.drain_expired_modals();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            WindowEvent::RemoteDirLoading(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    tab.remote.is_refreshing = true;
                    tab.remote.error = None;
                }
            }
            WindowEvent::RemoteDirLoaded(scoped) => {
                let tab_id = scoped.scope.tab_id;
                let mut entries = scoped.payload.entries;
                // The (path, snapshot) pairs of the freshly-applied listing,
                // captured while we hold the tab so active edit sessions can be
                // re-synced after that borrow is dropped. `None` when the
                // listing is obsolete (a newer navigation superseded it) and
                // therefore was not applied — no re-sync in that case.
                let mut applied_listing: Option<Vec<(RemotePath, RemoteSnapshot)>> = None;
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id)
                    // A later navigation request can be queued while an
                    // earlier listing is still in flight. Keep the newer
                    // path and ignore the obsolete result.
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    sort_entries(&mut entries, &tab.sort);
                    tab.remote.entries = entries;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                    self.remote.scroll = UniformListScrollHandle::new();
                    applied_listing = Some(
                        tab.remote
                            .entries
                            .iter()
                            .map(|entry| {
                                (
                                    entry.path.clone(),
                                    RemoteSnapshot {
                                        size: entry.size,
                                        modified_at: entry.modified_at,
                                    },
                                )
                            })
                            .collect(),
                    );
                }
                if let Some(listing) = applied_listing {
                    Self::resync_edit_baselines_on_refresh(tab_id, &listing, cx);
                }
            }
            WindowEvent::RemoteEntryResolved(scoped) => {
                // Navigation and editing act on this window's active tab; an
                // event for a tab owned by another window must not steer it.
                let tab_id = scoped.scope.tab_id;
                let session_epoch = scoped.scope.session_epoch;
                let resolution = scoped.payload;
                let entry = EntryPath::Remote(resolution.path.clone());
                let applies = self.state.tabs.active_tab_id() == Some(tab_id)
                    && self
                        .state
                        .tabs
                        .find_tab(tab_id)
                        .and_then(|tab| tab.remote.path.clone())
                        == resolution.path.parent();
                // Clear the pending status even when the answer is dropped, so
                // a late result cannot leave "Checking the link…" behind. The
                // epoch keeps a result from a superseded session from clearing
                // a re-issued check for the same path.
                self.finish_link_check(tab_id, &entry, Some(session_epoch), cx);
                if applies {
                    if let Some(error) = &resolution.error {
                        self.status_message =
                            Some(format!("{}: {}", error.title, error.message).into());
                    } else {
                        match resolution.kind {
                            FileKind::Directory => {
                                // Navigate by the listed path: the server resolves
                                // the link itself for every later operation.
                                self.navigate_pane_remote(
                                    resolution.path,
                                    macsftp_core::HistoryOp::Push,
                                    cx,
                                );
                            }
                            FileKind::File => {
                                let io_path =
                                    resolution.target.unwrap_or_else(|| resolution.path.clone());
                                self.begin_edit_resolved(
                                    resolution.path,
                                    io_path,
                                    resolution.size,
                                    resolution.modified_at,
                                    cx,
                                );
                            }
                            FileKind::Symlink | FileKind::Other | FileKind::Unknown => {
                                self.status_message =
                                    Some("This item cannot be opened in the editor.".into());
                            }
                        }
                    }
                }
            }
            WindowEvent::RemoteEntryResolveFailed(failure) => {
                // Epoch-correlated: accept only while this tab is still the
                // same session. Otherwise the tab reconnected and the pending
                // state belongs to the old one.
                let applies = self
                    .state
                    .tabs
                    .find_tab(failure.tab_id)
                    .is_some_and(|tab| tab.session_epoch == failure.session_epoch);
                self.finish_link_check(
                    failure.tab_id,
                    &EntryPath::Remote(failure.path.clone()),
                    Some(failure.session_epoch),
                    cx,
                );
                if applies {
                    self.status_message =
                        Some(format!("{}: {}", failure.error.title, failure.error.message).into());
                }
            }
            WindowEvent::RemoteOperationFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && scoped
                        .payload
                        .path
                        .as_ref()
                        .is_none_or(|path| tab.remote.path.as_ref() == Some(path))
                {
                    tab.remote.is_refreshing = false;
                    tab.remote.error = Some(scoped.payload.error.clone());
                }
                self.show_status(
                    format!(
                        "{}: {}",
                        scoped.payload.error.title, scoped.payload.error.message
                    )
                    .into(),
                    cx,
                );
            }
            WindowEvent::FsOperationFailed { scope, failure } => {
                // FsScope has no session_id; filter by tab + epoch only.
                // Fs failures stay status-bar-only (listing remains visible);
                // directory-read errors use RemoteOperationFailed + in-pane Retry.
                let accept = match scope {
                    macsftp_core::FsScope::Local { tab_id } => {
                        self.state.tabs.find_tab(tab_id).is_some()
                    }
                    macsftp_core::FsScope::Remote {
                        tab_id,
                        session_epoch,
                    } => self
                        .state
                        .tabs
                        .find_tab(tab_id)
                        .is_some_and(|tab| tab.session_epoch == session_epoch),
                };
                if accept {
                    self.status_message =
                        Some(format!("{}: {}", failure.title, failure.message).into());
                }
            }
        }
        cx.notify();
    }

    /// Re-sync active-edit baselines against a just-loaded directory listing.
    ///
    /// A directory refresh replaces the tab's listing with the server's
    /// ground-truth `(size, mtime)`. The explicit upload flow compares each
    /// session's stored `remote_snapshot` with a later live check, so harmless
    /// listing precision changes must not leave the baseline behind when nobody
    /// touched the remote. They otherwise do: an upload rebases
    /// the snapshot from the LOCAL temp file's mtime, and even after truncating
    /// to whole seconds (Part A) a later refresh re-reads the server's own
    /// mtime — usually identical, but this normalization makes the two byte-for-
    /// byte equal so no residual noise survives.
    ///
    /// The adoption rule is deliberately narrow to preserve real-conflict
    /// detection and lives in
    /// [`macsftp_core::RemoteEditLifecycle::adopt_listing_baselines`]: an
    /// `Editing` session adopts a listing entry only when the two agree at
    /// whole-second granularity, write-through sessions are skipped, and
    /// `RemoteConflict` sessions are never touched.
    fn resync_edit_baselines_on_refresh(
        tab_id: TabId,
        listing: &[(RemotePath, RemoteSnapshot)],
        cx: &mut Context<Self>,
    ) {
        cx.resources_mut()
            .edit_sessions
            .adopt_listing_baselines(tab_id, listing);
    }
}
