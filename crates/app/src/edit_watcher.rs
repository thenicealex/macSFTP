//! Long-lived polling loop that detects saved edits to remote-edit temp
//! files and uploads the changes back.
//!
//! Mirrors [`crate::event_coordinator::AppEventCoordinator::start`]: a
//! `cx.spawn` loop wakes every [`POLL_INTERVAL`] and `stat`s each `Editing` or
//! `RemoteConflict` session's temp file. When an editing file changed, it
//! compares the file's most recent remote `(size, mtime)` — read from the tab's
//! existing remote listing, no extra SFTP round-trip — against the session's
//! baseline. If the remote is unchanged it uploads the file back (session →
//! `UploadingBack`); if it diverged it flags a conflict (session →
//! `RemoteConflict`, resolved by Task 11's modal).

use std::time::Duration;

use gpui::{App, Global, Task};
use macsftp_core::{
    AppCommand, EditPhase, EditSessionId, RemotePath, RemoteSnapshot, TabId, Timestamp,
};
use tracing::warn;

use crate::event_coordinator::workspace_windows;
use crate::resources::ActiveResources;
use crate::workspace::build_edit_upload_command;

/// How often the watcher wakes to `stat` each editing session's temp file.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Owns the process-wide edit-watch loop for the app's lifetime. Stored as a
/// GPUI global (like [`crate::event_coordinator::AppEventCoordinator`]);
/// dropping it cancels the spawned task.
pub struct EditWatcher {
    _task: Task<()>,
}

impl Global for EditWatcher {}

impl EditWatcher {
    /// Spawn the polling loop on the GPUI foreground executor. The loop ends
    /// when `cx.update` fails, which only happens once the app is shutting
    /// down.
    pub fn start(cx: &mut App) -> Self {
        let task = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if cx.update(poll_edit_sessions).is_err() {
                    break; // app is shutting down
                }
            }
        });
        Self { _task: task }
    }
}

/// One watch tick: reap sessions whose temp files are gone, and for every
/// `Editing` session detect a local save and either upload the file back
/// (remote unchanged) or flag a conflict (remote diverged). Pure app logic
/// with no timer, so tests call it directly.
pub(crate) fn poll_edit_sessions(cx: &mut App) {
    // Snapshot the watched sessions as owned tuples first, so the immutable
    // resource borrow is dropped before we touch the filesystem, read windows,
    // or mutate the store below.
    let candidates: Vec<_> = cx
        .resources()
        .edit_sessions
        .editing_sessions()
        .chain(cx.resources().edit_sessions.conflict_sessions())
        .map(|session| {
            (
                session.id,
                session.phase.clone(),
                session.local_temp_path.clone(),
                session.remote_path.clone(),
                session.remote_snapshot,
                session.local_mtime,
                session.session_epoch,
                session.profile_id,
                session.tab_id,
            )
        })
        .collect();

    for (id, phase, temp, remote_path, snapshot, last_mtime, epoch, profile, tab_id) in candidates {
        let metadata = std::fs::metadata(temp.as_str());
        let current = metadata
            .as_ref()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(Timestamp::from_system_time);
        // A metadata error is NOT proof the file is gone: an editor's atomic
        // save briefly unlinks-then-renames, leaving a sub-second window where
        // the path does not resolve; `EINTR` and mount hiccups do the same.
        // Removing on the first miss would destroy a live session mid-save and
        // silently drop every later save. Instead count consecutive misses and
        // only tear the session down after EDIT_MISSING_TICKS_LIMIT of them; a
        // single successful stat resets the counter.
        if metadata.is_err() {
            let over_limit = cx
                .resources_mut()
                .edit_sessions
                .get_mut(id)
                .map(|session| {
                    session.missing_ticks += 1;
                    session.missing_ticks >= macsftp_core::EDIT_MISSING_TICKS_LIMIT
                })
                .unwrap_or(false);
            if over_limit && let Some(session) = cx.resources_mut().edit_sessions.remove(id) {
                // Delete the now-orphaned per-session temp directory too, not
                // just the store entry, so an empty `<edits>/<id>/` is not left
                // behind until quit (symmetric with advance_downloading's
                // failure cleanup).
                if let Some(parent) =
                    std::path::Path::new(session.local_temp_path.as_str()).parent()
                {
                    let _ = std::fs::remove_dir_all(parent);
                }
                if phase == EditPhase::RemoteConflict {
                    cx.refresh_windows();
                }
            }
            continue;
        }
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
            session.missing_ticks = 0;
        }
        // Conflict sessions only participate in lifecycle cleanup. Their
        // remote comparison and upload behavior remains exclusively driven by
        // the user's modal choice.
        if phase == EditPhase::RemoteConflict {
            continue;
        }
        let changed = match (last_mtime, current) {
            (Some(last), Some(now)) => now > last,
            _ => false,
        };
        if !changed {
            continue;
        }
        // The file's current remote (size, mtime), read from the tab's existing
        // directory listing — no extra SFTP round-trip.
        //
        // Upload back ONLY when the remote is CONFIRMED unchanged: the owning
        // tab is connected, its directory listing is fully loaded, and that
        // listing holds this file at the baseline (size, mtime).
        //
        // - A ready tab whose file is absent or diverged in its listing
        //   → flag a RemoteConflict. Absent means the tab navigated away or the
        //   remote deleted the file: with no proof the remote is unchanged, an
        //   OverwriteAll upload could silently clobber a concurrent remote edit,
        //   so detect-and-warn instead.
        // - A disconnected, reconnecting, refreshing, or unowned tab has no
        //   authoritative listing → leave the session and its old local mtime
        //   untouched so the save is retried once the tab becomes ready.
        if !tab_remote_is_ready(cx, tab_id) {
            continue;
        }
        let remote_now = current_remote_snapshot(cx, tab_id, &remote_path);
        let confirmed_unchanged = remote_now == Some(snapshot);
        if !confirmed_unchanged {
            if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
                session.phase = EditPhase::RemoteConflict;
                // Record this mtime so the conflict is not re-flagged next tick.
                session.local_mtime = current;
                cx.refresh_windows();
            }
            continue;
        }
        // Remote confirmed unchanged → upload the edited file back to its origin.
        // Capture the pre-save baseline so a failed dispatch can restore it
        // (below): poll flips local_mtime to `current` before dispatch, so a
        // revert that only reset the phase would leave the next poll seeing no
        // change and never retry the upload.
        let command = build_edit_upload_command(&temp, &remote_path, epoch, profile, tab_id);
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
            session.phase = EditPhase::UploadingBack;
            session.local_mtime = current;
        }
        dispatch_edit_command(cx, id, tab_id, last_mtime, command);
    }
}

/// The most recent remote `(size, mtime)` for `remote_path`, read from the
/// listing of whichever window owns `tab_id`. `None` when no window owns the
/// tab or the file is not in that tab's current listing.
fn current_remote_snapshot(
    cx: &App,
    tab_id: TabId,
    remote_path: &RemotePath,
) -> Option<RemoteSnapshot> {
    workspace_windows(cx).into_iter().find_map(|window| {
        window
            .read(cx)
            .ok()
            .and_then(|workspace| workspace.remote_entry_snapshot(tab_id, remote_path))
    })
}

/// Whether a live window owns `tab_id` and its remote listing is authoritative
/// enough for edit-conflict detection.
fn tab_remote_is_ready(cx: &App, tab_id: TabId) -> bool {
    workspace_windows(cx).into_iter().any(|window| {
        window
            .read(cx)
            .is_ok_and(|workspace| workspace.tab_remote_is_ready(tab_id))
    })
}

/// Send `command` through the runtime client of the window that owns `tab_id`.
/// On any failure to hand the command off — the owning window has closed, or
/// the command channel rejects the send (full/closed) — revert the session
/// (`session_id`) from `UploadingBack` back to `Editing` and restore its
/// pre-save `local_mtime` (`restore_mtime`) so it re-enters the watch set and
/// the next save re-triggers the upload. Without both the phase and the mtime
/// revert the session would strand: a stuck `UploadingBack` is never polled
/// again and, via [`find_active`], permanently blocks re-editing the file; and
/// a phase-only revert would leave `local_mtime` at the just-saved value so the
/// next poll sees no change and never retries. The temp file is kept either way.
///
/// [`find_active`]: macsftp_core::EditSessionStore::find_active
fn dispatch_edit_command(
    cx: &mut App,
    session_id: EditSessionId,
    tab_id: TabId,
    restore_mtime: Option<Timestamp>,
    command: AppCommand,
) {
    let client = workspace_windows(cx).into_iter().find_map(|window| {
        window
            .read(cx)
            .ok()
            .filter(|workspace| workspace.owns_tab(tab_id))
            .map(|workspace| workspace.runtime_client())
    });
    match client {
        Some(client) => {
            if let Err(error) = client.try_send(command) {
                // The command never entered the channel (full/closed). No
                // transfer is in flight to advance the session, so revert it —
                // symmetric with the window-closed branch below.
                warn!(error = ?error, "edit upload command could not be dispatched; reverting session to Editing");
                revert_stranded_upload(cx, session_id, restore_mtime);
            }
        }
        None => {
            warn!(
                tab_id = ?tab_id,
                "no window owns the tab for edit upload; reverting session to Editing"
            );
            revert_stranded_upload(cx, session_id, restore_mtime);
        }
    }
}

/// Return an `UploadingBack` session to `Editing` and restore its pre-save
/// `local_mtime` so the watcher's next tick re-detects the save and retries.
fn revert_stranded_upload(
    cx: &mut App,
    session_id: EditSessionId,
    restore_mtime: Option<Timestamp>,
) {
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.local_mtime = restore_mtime;
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use macsftp_core::{
        AppCommand, ConflictPolicy, ConnectionState, DisconnectReason, EditPhase, EditSession,
        EditSessionId, FileKind, LocalPath, ProfileId, RemoteEntry, RemotePath, RemoteSnapshot,
        RuntimeBridgeConfig, SessionId, TabId, Timestamp, TransferDirection, TransferEndpoint,
        WindowSessionId,
    };
    use macsftp_platform::AppPaths;
    use macsftp_sftp::{BridgeChannels, RuntimeClient};
    use macsftp_storage::ConfigStore;
    use macsftp_ui::Theme;

    use super::poll_edit_sessions;
    use crate::app_actions;
    use crate::resources::{ActiveResources, AppResources, SharedTransfers};
    use crate::workspace::Workspace;

    const REMOTE_FILE: &str = "/srv/a.txt";

    /// A unique temp `AppPaths` per call so parallel tests never share files.
    fn test_app_paths(label: &str) -> AppPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "macsftp-edit-watcher-{label}-{}-{sequence}",
            std::process::id()
        ));
        AppPaths::from_home_dir(home.to_string_lossy().as_ref())
    }

    fn install_globals(cx: &mut TestAppContext, label: &str) {
        let app_paths = test_app_paths(label);
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
            cx.set_global(AppResources::load_for_test(app_paths, config));
            cx.set_global(SharedTransfers::default());
        });
    }

    /// Open a window with one tab (`TabId(1)`, from the fresh `AppResources`
    /// counter) whose remote listing holds a single entry for `REMOTE_FILE`
    /// with the given `(size, modified_at)`. Returns the window and the
    /// command channel the window's runtime client sends through, so a test
    /// can observe the dispatched upload command.
    fn window_with_remote_entry(
        cx: &mut TestAppContext,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
    ) -> (WindowHandle<Workspace>, BridgeChannels) {
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let window = cx
            .add_window(|window, cx| Workspace::new(client, WindowSessionId(1), None, window, cx));
        window
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("window opens with a tab");
                tab.session_epoch = 1;
                tab.connection = ConnectionState::Connected {
                    session_id: SessionId(1),
                    session_epoch: 1,
                    connected_at: Timestamp::from_secs_since_epoch(1),
                };
                tab.remote.path = Some(RemotePath::new("/srv"));
                tab.remote.entries = vec![RemoteEntry {
                    name: "a.txt".to_string(),
                    path: RemotePath::new(REMOTE_FILE),
                    kind: FileKind::File,
                    size,
                    permissions: Some(0o644),
                    modified_at,
                    link_target: None,
                }];
            })
            .expect("seed the tab's remote listing");
        (window, channels)
    }

    /// Register an `Editing` session pointing at a real temp file on disk.
    fn seed_editing_session(
        cx: &mut TestAppContext,
        label: &str,
        snapshot: RemoteSnapshot,
        local_mtime: Option<Timestamp>,
    ) -> (EditSessionId, LocalPath) {
        let temp_file = std::env::temp_dir().join(format!(
            "macsftp-watch-{label}-{}-{}.txt",
            std::process::id(),
            snapshot.size.unwrap_or(0)
        ));
        std::fs::write(&temp_file, b"edited contents").expect("write edit temp file");
        let temp_path = LocalPath::new(temp_file.to_string_lossy().as_ref());
        let id = cx.update(|cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            cx.resources_mut().edit_sessions.register(EditSession {
                id,
                remote_path: RemotePath::new(REMOTE_FILE),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::Editing,
                remote_snapshot: snapshot,
                local_mtime,
                active_transfer: None,
                missing_ticks: 0,
            });
            id
        });
        (id, temp_path)
    }

    fn file_mtime(path: &LocalPath) -> Option<Timestamp> {
        std::fs::metadata(path.as_str())
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(Timestamp::from_system_time)
    }

    fn session_phase(cx: &mut TestAppContext, id: EditSessionId) -> EditPhase {
        cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session survives poll")
                .phase
                .clone()
        })
    }

    #[gpui::test]
    fn poll_uploads_when_local_file_changed_and_remote_unchanged(cx: &mut TestAppContext) {
        install_globals(cx, "upload");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // Remote entry matches the session baseline exactly → no divergence.
        let (_window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        // Baseline mtime far in the past → the just-written file reads as changed.
        let (id, temp_path) = seed_editing_session(
            cx,
            "upload",
            snapshot,
            Some(Timestamp::from_secs_since_epoch(1)),
        );
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::UploadingBack,
            "changed local + unchanged remote must move the session to UploadingBack"
        );
        let command = channels
            .command_rx
            .try_recv()
            .expect("an upload command must be dispatched");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Upload);
        assert_eq!(command.sources, vec![TransferEndpoint::Local(temp_path)]);
        assert_eq!(
            command.destination,
            TransferEndpoint::Remote(RemotePath::new(REMOTE_FILE))
        );
        // The edit layer runs its own divergence check before uploading, so the
        // pipeline-level existence prompt (Ask) would be redundant against the
        // always-present remote origin. Pin OverwriteAll so the save-back stays
        // silent instead of popping the generic conflict modal.
        assert_eq!(command.conflict_policy, ConflictPolicy::OverwriteAll);
    }

    #[gpui::test]
    fn poll_ignores_unchanged_files(cx: &mut TestAppContext) {
        install_globals(cx, "unchanged");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let (_window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        let (id, temp_path) = seed_editing_session(cx, "unchanged", snapshot, None);
        // Baseline == the file's actual mtime → poll sees no local change.
        let mtime = file_mtime(&temp_path);
        cx.update(|cx| {
            cx.resources_mut()
                .edit_sessions
                .get_mut(id)
                .expect("session exists")
                .local_mtime = mtime;
        });
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::Editing,
            "an unchanged file must leave the session in Editing"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no command may be dispatched when the file is unchanged"
        );
    }

    #[gpui::test]
    fn poll_flags_conflict_when_remote_diverged(cx: &mut TestAppContext) {
        install_globals(cx, "conflict");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // Remote entry has a DIFFERENT size than the baseline → divergence.
        let (_window, channels) = window_with_remote_entry(cx, Some(999), snapshot.modified_at);
        let (id, _temp_path) = seed_editing_session(
            cx,
            "conflict",
            snapshot,
            Some(Timestamp::from_secs_since_epoch(1)),
        );
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::RemoteConflict,
            "a diverged remote must move the session to RemoteConflict"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may be dispatched when the remote diverged"
        );
    }

    #[gpui::test]
    fn poll_flags_conflict_when_file_left_the_listing(cx: &mut TestAppContext) {
        install_globals(cx, "out-of-listing");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // A window owns the tab, but its remote listing does NOT contain the
        // edited file (the tab navigated to another directory). The remote
        // (size, mtime) is therefore indeterminate. The watcher must NOT blindly
        // OverwriteAll — it flags a conflict so a concurrent remote change is not
        // silently clobbered.
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let _window = cx
            .add_window(|window, cx| Workspace::new(client, WindowSessionId(1), None, window, cx));
        _window
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("window opens with a tab");
                tab.session_epoch = 1;
                tab.connection = ConnectionState::Connected {
                    session_id: SessionId(1),
                    session_epoch: 1,
                    connected_at: Timestamp::from_secs_since_epoch(1),
                };
                tab.remote.path = Some(RemotePath::new("/srv"));
            })
            .expect("mark the empty remote listing ready");
        let (id, _temp_path) = seed_editing_session(
            cx,
            "out-of-listing",
            snapshot,
            Some(Timestamp::from_secs_since_epoch(1)),
        );
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::RemoteConflict,
            "a file no longer in the tab's listing must flag a conflict, not blind-overwrite"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may be dispatched when remote state is indeterminate"
        );
    }

    #[gpui::test]
    fn poll_reverts_to_editing_when_owning_window_closed(cx: &mut TestAppContext) {
        install_globals(cx, "no-window");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // No window is opened, so nothing owns the session's tab. A changed
        // local file drives the session toward UploadingBack, but the dispatch
        // finds no owning window and must revert the session to Editing rather
        // than strand it (a stuck UploadingBack would block re-editing forever).
        let (id, _temp_path) = seed_editing_session(
            cx,
            "no-window",
            snapshot,
            Some(Timestamp::from_secs_since_epoch(1)),
        );

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::Editing,
            "a window-less upload dispatch must revert the session to Editing"
        );
    }

    #[gpui::test]
    fn poll_defers_changed_file_until_disconnected_tab_is_ready(cx: &mut TestAppContext) {
        install_globals(cx, "disconnected");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let (window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        let baseline = Timestamp::from_secs_since_epoch(1);
        let (id, _temp_path) = seed_editing_session(cx, "disconnected", snapshot, Some(baseline));
        window
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("active tab");
                tab.connection = ConnectionState::Disconnected {
                    reason: DisconnectReason::ConnectionLost,
                };
                tab.remote.path = None;
                tab.remote.entries.clear();
            })
            .expect("disconnect tab");

        cx.update(poll_edit_sessions);

        assert_eq!(session_phase(cx, id), EditPhase::Editing);
        let deferred_mtime = cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session survives disconnect")
                .local_mtime
        });
        assert_eq!(
            deferred_mtime,
            Some(baseline),
            "deferring must preserve the old mtime so reconnect retries the save"
        );
        assert!(channels.command_rx.try_recv().is_err());

        window
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("active tab");
                tab.connection = ConnectionState::Connected {
                    session_id: SessionId(2),
                    session_epoch: 1,
                    connected_at: Timestamp::from_secs_since_epoch(2),
                };
                tab.remote.path = Some(RemotePath::new("/srv"));
                tab.remote.entries = vec![RemoteEntry {
                    name: "a.txt".to_string(),
                    path: RemotePath::new(REMOTE_FILE),
                    kind: FileKind::File,
                    size: snapshot.size,
                    permissions: Some(0o644),
                    modified_at: snapshot.modified_at,
                    link_target: None,
                }];
            })
            .expect("restore ready remote listing");

        cx.update(poll_edit_sessions);

        assert_eq!(session_phase(cx, id), EditPhase::UploadingBack);
        assert!(
            matches!(
                channels.command_rx.try_recv(),
                Ok(AppCommand::StartTransfer(_))
            ),
            "the deferred save must upload once the tab is ready again"
        );
    }

    #[gpui::test]
    fn poll_reverts_to_editing_when_command_channel_full(cx: &mut TestAppContext) {
        install_globals(cx, "chan-full");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // An owning window exists, but its command channel is saturated so
        // try_send returns ChannelFull. The session must NOT strand in
        // UploadingBack: it reverts to Editing and its pre-save mtime is
        // restored so the next poll re-detects the save and retries.
        let (_window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        let baseline = Timestamp::from_secs_since_epoch(1);
        let (id, _temp_path) = seed_editing_session(cx, "chan-full", snapshot, Some(baseline));
        // Fill the bounded command channel to force ChannelFull on dispatch.
        // The receiver is never drained, so every slot stays occupied.
        loop {
            if channels
                .command_tx
                .try_send(AppCommand::CloseTab { tab_id: TabId(999) })
                .is_err()
            {
                break;
            }
        }

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::Editing,
            "a channel-full upload dispatch must revert the session to Editing, not strand it"
        );
        let restored = cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session survives")
                .local_mtime
        });
        assert_eq!(
            restored,
            Some(baseline),
            "the pre-save mtime must be restored so the next poll retries the upload"
        );
    }

    #[gpui::test]
    fn poll_tolerates_transient_stat_miss_then_reaps_after_limit(cx: &mut TestAppContext) {
        install_globals(cx, "transient-miss");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let (_window, _channels) =
            window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        let (id, temp_path) = seed_editing_session(cx, "transient-miss", snapshot, None);

        // Delete the temp file to simulate the window during an editor's atomic
        // save (unlink-then-rename) where metadata() transiently fails.
        std::fs::remove_file(temp_path.as_str()).expect("remove temp to force a stat miss");

        // The first LIMIT-1 misses must NOT drop the session: a live edit must
        // survive a brief unreadable window.
        for tick in 1..macsftp_core::EDIT_MISSING_TICKS_LIMIT {
            cx.update(poll_edit_sessions);
            let missing = cx.read(|cx| {
                cx.resources()
                    .edit_sessions
                    .get(id)
                    .map(|session| session.missing_ticks)
            });
            assert_eq!(
                missing,
                Some(tick),
                "tick {tick}: session must survive a transient miss and count it"
            );
        }

        // A successful stat before the limit resets the counter, proving the
        // session recovers from a transient miss rather than accumulating
        // forever.
        std::fs::write(temp_path.as_str(), b"back again").expect("recreate temp file");
        cx.update(poll_edit_sessions);
        let after_recovery = cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .map(|session| session.missing_ticks)
        });
        assert_eq!(
            after_recovery,
            Some(0),
            "a successful stat must reset the miss counter"
        );

        // Now a sustained absence (LIMIT consecutive misses) must finally reap
        // the session.
        std::fs::remove_file(temp_path.as_str()).expect("remove temp again");
        for _ in 0..macsftp_core::EDIT_MISSING_TICKS_LIMIT {
            cx.update(poll_edit_sessions);
        }
        assert!(
            cx.read(|cx| cx.resources().edit_sessions.get(id).is_none()),
            "a genuinely deleted temp (LIMIT consecutive misses) must reap the session"
        );
    }

    #[gpui::test]
    fn poll_reaps_remote_conflict_after_temp_file_is_deleted(cx: &mut TestAppContext) {
        install_globals(cx, "conflict-missing");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let (id, temp_path) = seed_editing_session(cx, "conflict-missing", snapshot, None);
        cx.update(|cx| {
            cx.resources_mut()
                .edit_sessions
                .get_mut(id)
                .expect("session exists")
                .phase = EditPhase::RemoteConflict;
        });
        std::fs::remove_file(temp_path.as_str()).expect("remove conflict temp file");

        for _ in 0..macsftp_core::EDIT_MISSING_TICKS_LIMIT {
            cx.update(poll_edit_sessions);
        }

        assert!(
            cx.read(|cx| cx.resources().edit_sessions.get(id).is_none()),
            "a conflict whose temp file is gone must be reaped"
        );
    }

    #[gpui::test]
    fn poll_keeps_present_remote_conflict_unchanged(cx: &mut TestAppContext) {
        install_globals(cx, "conflict-present");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let baseline = Timestamp::from_secs_since_epoch(1);
        let (id, _temp_path) =
            seed_editing_session(cx, "conflict-present", snapshot, Some(baseline));
        cx.update(|cx| {
            cx.resources_mut()
                .edit_sessions
                .get_mut(id)
                .expect("session exists")
                .phase = EditPhase::RemoteConflict;
        });

        cx.update(poll_edit_sessions);

        let (phase, local_mtime) = cx.read(|cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(id)
                .expect("present conflict survives");
            (session.phase.clone(), session.local_mtime)
        });
        assert_eq!(phase, EditPhase::RemoteConflict);
        assert_eq!(local_mtime, Some(baseline));
    }
}
