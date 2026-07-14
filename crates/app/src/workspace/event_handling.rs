use gpui::{Context, UniformListScrollHandle, Window};
use macsftp_core::{AppEvent, ErrorCode, ModalRequest, UserFacingError, sort_entries};

use tracing::{debug, warn};

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::*;

impl crate::workspace::Workspace {
    pub(crate) fn handle_app_event(
        &mut self,
        event: AppEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Production transfer events are intercepted by AppEventCoordinator
        // and reduced once process-wide. Keeping this small delegation makes
        // direct Workspace tests exercise the same reducer without restoring
        // per-window event ownership.
        if event.is_transfer_event() {
            let conflict_prompt = match &event {
                AppEvent::TransferConflict(prompt) => Some(prompt.clone()),
                _ => None,
            };
            cx.apply_transfer_event(
                &event,
                macsftp_core::Timestamp(std::time::SystemTime::now()),
            );
            if let Some(prompt) = conflict_prompt {
                self.present_transfer_conflict(prompt, window, cx);
            }
            cx.notify();
            return;
        }

        if !self.state.should_accept_event(&event) {
            // `AppEvent` carries no secrets by invariant (only host/port,
            // key fingerprints, paths, and user-facing errors), so logging
            // it at debug is safe per AGENTS.md §5.
            debug!(?event, "dropped stale event");
            return;
        }

        match event {
            AppEvent::TabConnecting { .. } => {
                // Already reflected optimistically when the command was
                // sent; nothing to update.
            }
            AppEvent::HostKeyUnknown(prompt) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(prompt.tab_id) {
                    tab.await_host_key(prompt.session_id, prompt.request_id);
                    self.state.modals.active.push(ModalRequest::HostKey(prompt));
                    window.focus(&self.modal_focus);
                }
            }
            AppEvent::HostKeyMismatch(mismatch) => {
                // Security block, no override (plan §10): the tab fails
                // with an explanation and the known_hosts file location.
                if let Some(tab) = self.state.tabs.find_tab_mut(mismatch.tab_id) {
                    let mut error = UserFacingError::new(
                        ErrorCode::HostKeyMismatch,
                        "Host key mismatch",
                        format!(
                            "The key presented by {}:{} does not match the stored \
                             key. This can indicate a man-in-the-middle attack. \
                             Connection blocked.",
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
            AppEvent::TabConnected(scoped) => {
                let tab_id = scoped.scope.tab_id;
                let remote_root = scoped.payload.remote_root;
                // Prefer the path restored from the previous session so the
                // user lands where they left off; fall back to the actor's
                // remote_root. Clear the preference after use so reconnect
                // navigates from the live path.
                let navigate_to = {
                    let restored = self
                        .restored_targets
                        .get(&tab_id)
                        .and_then(|target| target.remote_path.clone());
                    restored.unwrap_or(remote_root)
                };
                if let Some(target) = self.restored_targets.get_mut(&tab_id) {
                    target.remote_path = None;
                }
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                    tab.complete_connect(
                        scoped.scope.session_id,
                        macsftp_core::Timestamp(std::time::SystemTime::now()),
                    );
                    tab.remote.entries.clear();
                    tab.remote.path = Some(navigate_to.clone());
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                }
                self.remote_scroll = UniformListScrollHandle::new();
                self.request_remote_directory(tab_id, navigate_to, cx);
                // Reconcile remote residual temp files from a previous run
                // now that a live session to this host exists (plan M5/M6).
                self.clean_remote_residual_temps(tab_id, cx);
                self.record_recent_for_tab(tab_id, cx);
            }
            AppEvent::TabDisconnected(scoped) => {
                let tab_id = scoped.scope.tab_id;
                // Before clearing the live remote path, stash it in
                // restored_targets so build_session_snapshot still records
                // the last browsing location after disconnect-then-quit.
                // TabConnected clears this preference once it is consumed.
                let last_remote_path = self
                    .state
                    .tabs
                    .find_tab(tab_id)
                    .and_then(|tab| tab.remote.path.clone());
                if let Some(remote_path) = last_remote_path {
                    if let Some(target) = self.restored_targets.get_mut(&tab_id) {
                        target.remote_path = Some(remote_path);
                    } else if let Some(settings) = self.tab_settings.get(&tab_id) {
                        let profile_id = self
                            .state
                            .tabs
                            .find_tab(tab_id)
                            .and_then(|tab| tab.profile_id);
                        self.restored_targets.insert(
                            tab_id,
                            RestoredTabTarget {
                                host: settings.host.clone(),
                                port: settings.port,
                                username: settings.username.clone(),
                                profile_id,
                                remote_path: Some(remote_path),
                            },
                        );
                    }
                }
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                    tab.disconnect(scoped.payload.reason);
                    tab.remote.entries.clear();
                    tab.remote.path = None;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                }
                let had_modal = !self.state.drain_expired_modals().is_empty();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            AppEvent::AuthFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id) {
                    tab.fail(scoped.payload.reason);
                }
                let had_modal = !self.state.drain_expired_modals().is_empty();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            AppEvent::RemoteDirLoading(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    tab.remote.is_refreshing = true;
                    tab.remote.error = None;
                }
            }
            AppEvent::RemoteDirLoaded(scoped) => {
                let mut entries = scoped.payload.entries;
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
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
                    self.remote_scroll = UniformListScrollHandle::new();
                }
            }
            AppEvent::RemoteOperationFailed(scoped) => {
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
                self.status_message = Some(
                    format!(
                        "{}: {}",
                        scoped.payload.error.title, scoped.payload.error.message
                    )
                    .into(),
                );
            }
            AppEvent::FsOperationFailed { scope, failure } => {
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
            AppEvent::LocalDirLoaded(snapshot) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(snapshot.tab_id)
                    && tab.local.path.as_ref() == Some(&snapshot.path)
                {
                    let mut entries = snapshot.entries;
                    sort_entries(&mut entries, &tab.sort);
                    tab.local.entries = entries;
                    tab.local.error = None;
                    tab.selection.selected_paths.clear();
                    self.local_scroll = UniformListScrollHandle::new();
                }
            }
            AppEvent::ResidualTempCreated(record) => {
                cx.resources_mut().residual_temps.add(record);
                if let Err(error) = cx.resources_mut().residual_temps.save() {
                    warn!(error = %error, "could not persist residual temp record");
                }
            }
            AppEvent::ResidualTempCleared { transfer_id, path } => {
                cx.resources_mut().residual_temps.remove(transfer_id, &path);
                if let Err(error) = cx.resources_mut().residual_temps.save() {
                    warn!(error = %error, "could not update residual temp store");
                }
            }
            _ => {}
        }
        cx.notify();
    }
}
