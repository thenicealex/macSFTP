//! Long-lived polling loop that detects saved edits to remote-edit temp
//! files and requests an authoritative remote check before the changes are
//! uploaded back.
//!
//! Mirrors [`crate::event_coordinator::AppEventCoordinator::start`]: a
//! `cx.spawn` loop wakes every [`POLL_INTERVAL`] and `stat`s each `Editing`,
//! `CheckingRemote`, or `RemoteConflict` session's temp file. When an
//! `Editing` session's local file changed, it does **not** trust the cached
//! directory listing to decide whether the remote is still unchanged. Instead
//! it allocates a [`macsftp_core::EditCheckId`], parks the session in
//! [`EditPhase::CheckingRemote`], and dispatches
//! [`AppCommand::CheckRemoteEditSnapshot`] to the live browsing actor. The
//! actor reads the file's current `(size, mtime)` and the process-wide
//! coordinator (Task 5) applies that result exactly once — uploading back only
//! when the remote is confirmed unchanged, or flagging a conflict when it
//! diverged. This closes the gap where a stale UI listing could authorize
//! overwriting a concurrent remote edit.

use std::time::Duration;

use gpui::{App, Global, Task};
use macsftp_core::{
    AppCommand, CheckRemoteEditSnapshotCommand, EditPhase, EditSessionId, TabId, Timestamp,
};
use tracing::warn;

use crate::event_coordinator::workspace_windows;
use crate::resources::ActiveResources;

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
/// `Editing` session detect a local save and dispatch an authoritative remote
/// check (see the module docs). The actual upload-back or conflict decision is
/// made later by the coordinator when the check result arrives — this loop only
/// initiates the check. Pure app logic with no timer, so tests call it
/// directly.
pub(crate) fn poll_edit_sessions(cx: &mut App) {
    // Snapshot the watched sessions as owned tuples first, so the immutable
    // resource borrow is dropped before we touch the filesystem, read windows,
    // or mutate the store below.
    let candidates: Vec<_> = cx
        .resources()
        .edit_sessions
        .editing_sessions()
        .chain(cx.resources().edit_sessions.conflict_sessions())
        .chain(cx.resources().edit_sessions.checking_sessions())
        .map(|session| {
            (
                session.id,
                session.phase.clone(),
                session.local_temp_path.clone(),
                session.remote_path.clone(),
                session.local_mtime,
                session.session_epoch,
                session.tab_id,
            )
        })
        .collect();

    for (id, phase, temp, remote_path, last_mtime, epoch, tab_id) in candidates {
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
                // just the store entry, so an empty `<edits>/<run>/<id>/` is not left
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
        // Lifecycle-only phases do not initiate a new check through the
        // polling loop. `RemoteConflict` sessions are driven exclusively by the
        // user's modal choice. `CheckingRemote` sessions already have an
        // in-flight authoritative check — duplicate dispatch is prevented by
        // construction, because they are excluded from the `Editing` candidate
        // set above — so the loop must not re-dispatch for them. Both phases
        // still pass through the missing-temp reaping earlier in this tick.
        if phase != EditPhase::Editing {
            continue;
        }
        let changed = match (last_mtime, current) {
            (Some(last), Some(now)) => now > last,
            _ => false,
        };
        if !changed {
            continue;
        }
        if !tab_remote_is_ready(cx, tab_id) {
            // Defer: leave the session in `Editing` with its old `local_mtime`
            // so the save is retried once the tab becomes ready. A disconnected,
            // reconnecting, refreshing, or unowned tab has no authoritative
            // listing to authorize an upload against, and the watcher no longer
            // trusts the cached listing to declare the remote unchanged.
            continue;
        }
        // Authoritative check instead of a cached-listing comparison: allocate a
        // check id, park the session in `CheckingRemote` with the exact local
        // save mtime, and ask the live browsing actor for the file's current
        // `(size, mtime)`. The coordinator (Task 5) applies the result exactly
        // once — uploading back only when the remote is confirmed unchanged, or
        // flagging a conflict when it diverged. This closes the gap where a
        // stale UI listing could authorize overwriting a concurrent remote edit.
        let check_id = cx.resources_mut().edit_sessions.next_check_id();
        let command = AppCommand::CheckRemoteEditSnapshot(CheckRemoteEditSnapshotCommand {
            tab_id,
            session_epoch: epoch,
            edit_session_id: id,
            check_id,
            path: remote_path.clone(),
        });
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
            session.phase = EditPhase::CheckingRemote;
            session.pending_check_id = Some(check_id);
            // Capture the local mtime at save time so the coordinator can reject
            // a superseded save (the user saved again while the check was in
            // flight) and the watcher can issue a fresh check for the newer save.
            session.checking_local_mtime = current;
            // `local_mtime` is intentionally left at `last_mtime` so the session
            // reverts cleanly if the dispatch fails or the result is rejected.
        }
        dispatch_edit_command(cx, id, tab_id, last_mtime, command);
    }
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
/// (`session_id`) to `Editing`: restore its pre-dispatch `local_mtime`
/// (`restore_mtime`) and clear any pending-check fields so it re-enters the
/// watch set and the next save re-triggers the dispatch. Without both the phase
/// and the mtime revert the session would strand: a stuck dispatch phase is
/// never polled again and, via [`find_active`], permanently blocks re-editing
/// the file; and a phase-only revert would leave `local_mtime` at the just-saved
/// value so the next poll sees no change and never retries. The temp file is
/// kept either way.
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
                warn!(error = ?error, "edit command could not be dispatched; reverting session to Editing");
                revert_stranded_upload(cx, session_id, restore_mtime);
            }
        }
        None => {
            warn!(
                tab_id = ?tab_id,
                "no window owns the tab for edit command; reverting session to Editing"
            );
            revert_stranded_upload(cx, session_id, restore_mtime);
        }
    }
}

/// Return a session stranded by a failed command dispatch to `Editing`,
/// restore its pre-dispatch `local_mtime` (`restore_mtime`), and clear any
/// pending-check fields so the watcher's next tick re-detects the save and
/// retries cleanly.
///
/// Used for two dispatch paths:
/// - the upload-back handoff (`UploadingBack` → `Editing`); and
/// - the authoritative-check handoff (`CheckingRemote` → `Editing`), where the
///   pending `EditCheckId` and `checking_local_mtime` must be cleared so a
///   stale check id never correlates with a later result.
fn revert_stranded_upload(
    cx: &mut App,
    session_id: EditSessionId,
    restore_mtime: Option<Timestamp>,
) {
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.local_mtime = restore_mtime;
        session.pending_check_id = None;
        session.checking_local_mtime = None;
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use macsftp_core::{
        AppCommand, ConnectionState, DisconnectReason, EditCheckId, EditPhase, EditSession,
        EditSessionId, FileKind, LocalPath, ProfileId, RemoteEntry, RemotePath, RemoteSnapshot,
        RuntimeBridgeConfig, SessionId, TabId, Timestamp, WindowSessionId,
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
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let session_dir = std::env::temp_dir().join(format!(
            "macsftp-watch-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&session_dir).expect("create isolated edit session directory");
        let temp_file = session_dir.join("edited.txt");
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
                pending_check_id: None,
                checking_local_mtime: None,
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
    fn poll_dispatches_remote_check_before_upload(cx: &mut TestAppContext) {
        install_globals(cx, "dispatch-check");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // A ready tab is required to authorize the authoritative check. The
        // watcher no longer trusts the cached listing for the unchanged verdict;
        // it asks the live actor for the file's current (size, mtime).
        let (_window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        // Baseline mtime far in the past → the just-written file reads as changed.
        let (id, _temp_path) = seed_editing_session(
            cx,
            "dispatch-check",
            snapshot,
            Some(Timestamp::from_secs_since_epoch(1)),
        );
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(poll_edit_sessions);

        assert_eq!(
            session_phase(cx, id),
            EditPhase::CheckingRemote,
            "a changed local file with a ready tab must move the session to CheckingRemote"
        );
        let command = channels
            .command_rx
            .try_recv()
            .expect("a check command must be dispatched");
        let AppCommand::CheckRemoteEditSnapshot(command) = command else {
            panic!("expected CheckRemoteEditSnapshot, got {command:?}");
        };
        assert_eq!(command.tab_id, TabId(1));
        assert_eq!(command.session_epoch, 1);
        assert_eq!(command.edit_session_id, id);
        assert_eq!(command.check_id, EditCheckId(1), "first allocated check id");
        assert_eq!(command.path, RemotePath::new(REMOTE_FILE));
    }

    #[gpui::test]
    fn poll_does_not_dispatch_duplicate_check_while_checking(cx: &mut TestAppContext) {
        install_globals(cx, "duplicate");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let (_window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        let baseline = Timestamp::from_secs_since_epoch(1);
        let (id, _temp_path) = seed_editing_session(cx, "duplicate", snapshot, Some(baseline));
        while channels.command_rx.try_recv().is_ok() {}

        // First poll dispatches the single authoritative check and parks the
        // session in CheckingRemote.
        cx.update(poll_edit_sessions);
        assert_eq!(session_phase(cx, id), EditPhase::CheckingRemote);
        let first = channels
            .command_rx
            .try_recv()
            .expect("exactly one check command on the first poll");
        assert!(
            matches!(first, AppCommand::CheckRemoteEditSnapshot(_)),
            "the first poll must dispatch a CheckRemoteEditSnapshot"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "only one check command may be dispatched before a result arrives"
        );

        // Second poll must NOT redispatch: the session is already CheckingRemote,
        // so it is excluded from the Editing candidate set by construction.
        cx.update(poll_edit_sessions);
        assert_eq!(
            session_phase(cx, id),
            EditPhase::CheckingRemote,
            "the session must remain CheckingRemote while the check is in flight"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "a duplicate check must not be dispatched while CheckingRemote"
        );
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

        assert_eq!(
            session_phase(cx, id),
            EditPhase::CheckingRemote,
            "once the tab is ready again the watcher must dispatch the authoritative check"
        );
        assert!(
            matches!(
                channels.command_rx.try_recv(),
                Ok(AppCommand::CheckRemoteEditSnapshot(_))
            ),
            "the deferred save must dispatch a remote check once the tab is ready again"
        );
    }

    #[gpui::test]
    fn poll_reverts_to_editing_when_check_dispatch_fails(cx: &mut TestAppContext) {
        install_globals(cx, "check-chan-full");
        let snapshot = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        // An owning window exists, but its command channel is saturated so
        // try_send returns ChannelFull. The session must NOT strand in
        // CheckingRemote: it reverts to Editing, its pre-save mtime is
        // preserved so the next poll re-detects the save and retries, and the
        // pending-check fields are cleared so a stale check id never correlates
        // with a later result.
        let (_window, channels) = window_with_remote_entry(cx, snapshot.size, snapshot.modified_at);
        let baseline = Timestamp::from_secs_since_epoch(1);
        let (id, _temp_path) =
            seed_editing_session(cx, "check-chan-full", snapshot, Some(baseline));
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
            "a channel-full check dispatch must revert the session to Editing, not strand it"
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
            "the pre-save mtime must be restored so the next poll retries the check"
        );
        let pending = cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .map(|session| (session.pending_check_id, session.checking_local_mtime))
        });
        assert_eq!(
            pending,
            Some((None, None)),
            "the pending check fields must be cleared on a failed dispatch"
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
        let session_dir = std::path::Path::new(temp_path.as_str())
            .parent()
            .expect("edit fixture has a session directory")
            .to_path_buf();
        assert_ne!(
            session_dir,
            std::env::temp_dir(),
            "edit cleanup must never target the process-wide temporary directory"
        );
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
        assert!(
            !session_dir.exists(),
            "reaping the session must remove only its isolated directory"
        );
        assert!(
            std::env::temp_dir().exists(),
            "reaping an edit session must preserve the process-wide temporary directory"
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
