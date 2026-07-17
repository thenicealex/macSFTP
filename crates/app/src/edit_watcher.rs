//! Long-lived polling loop that detects saved edits to remote-edit temp
//! files and uploads the changes back.
//!
//! Mirrors [`crate::event_coordinator::AppEventCoordinator::start`]: a
//! `cx.spawn` loop wakes every [`POLL_INTERVAL`] and `stat`s each `Editing`
//! session's temp file. When the local file changed, it compares the file's
//! most recent remote `(size, mtime)` — read from the tab's existing remote
//! listing, no extra SFTP round-trip — against the session's baseline. If the
//! remote is unchanged it uploads the file back (session → `UploadingBack`);
//! if it diverged it flags a conflict (session → `RemoteConflict`, resolved by
//! Task 11's modal).

use std::time::Duration;

use gpui::{App, Global, Task};
use macsftp_core::{AppCommand, EditPhase, RemotePath, RemoteSnapshot, TabId, Timestamp};
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

/// One watch tick: for every `Editing` session, detect a local save and either
/// upload the file back (remote unchanged) or flag a conflict (remote
/// diverged). Pure app logic with no timer, so tests call it directly.
pub(crate) fn poll_edit_sessions(cx: &mut App) {
    // Snapshot the editing sessions as owned tuples first, so the immutable
    // resource borrow is dropped before we touch the filesystem, read windows,
    // or mutate the store below.
    let candidates: Vec<_> = cx
        .resources()
        .edit_sessions
        .editing_sessions()
        .map(|session| {
            (
                session.id,
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

    for (id, temp, remote_path, snapshot, last_mtime, epoch, profile, tab_id) in candidates {
        let metadata = std::fs::metadata(temp.as_str());
        let current = metadata
            .as_ref()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(Timestamp::from_system_time);
        // The temp file is gone (session dir wiped, user deleted it) → drop the
        // session. A readable file whose mtime is merely unavailable is left
        // alone (treated as unchanged).
        if metadata.is_err() {
            cx.resources_mut().edit_sessions.remove(id);
            continue;
        }
        let changed = match (last_mtime, current) {
            (Some(last), Some(now)) => now > last,
            _ => false,
        };
        if !changed {
            continue;
        }
        // The file's current remote (size, mtime) taken from the tab's existing
        // directory listing — no extra SFTP round-trip. `None` means the file
        // is not in the current listing, so divergence cannot be determined:
        // `is_some_and` yields `false` there (matching the brief), so we do NOT
        // flag a false conflict and fall through to upload.
        let remote_now = current_remote_snapshot(cx, tab_id, &remote_path);
        if remote_now.is_some_and(|now| now != snapshot) {
            if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
                session.phase = EditPhase::RemoteConflict;
                // Record this mtime so the conflict is not re-flagged next tick.
                session.local_mtime = current;
            }
            cx.refresh_windows();
            continue;
        }
        // Remote unchanged → upload the edited file back to its origin.
        let command = build_edit_upload_command(&temp, &remote_path, epoch, profile, tab_id);
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
            session.phase = EditPhase::UploadingBack;
            session.local_mtime = current;
        }
        dispatch_edit_command(cx, tab_id, command);
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

/// Send `command` through the runtime client of the window that owns `tab_id`.
/// If the owning window has closed, the command is logged and dropped — the
/// session's temp file remains, so the user can save again once reconnected.
fn dispatch_edit_command(cx: &App, tab_id: TabId, command: AppCommand) {
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
                warn!(error = ?error, "edit upload command could not be dispatched");
            }
        }
        None => {
            warn!(tab_id = ?tab_id, "no window owns the tab for edit upload; command dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use macsftp_core::{
        AppCommand, ConflictPolicy, EditPhase, EditSession, EditSessionId, FileKind, LocalPath,
        ProfileId, RemoteEntry, RemotePath, RemoteSnapshot, RuntimeBridgeConfig, TabId, Timestamp,
        TransferDirection, TransferEndpoint, WindowSessionId,
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
}
