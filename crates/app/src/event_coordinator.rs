use gpui::{App, Global, Task, WindowHandle};
use macsftp_core::{
    AppEvent, ConflictRequest, EditPhase, LocalPath, RemotePath, RemoteSnapshot, TabId, Timestamp,
    TransferConflictPrompt, TransferDirection, TransferEndpoint,
};
use macsftp_sftp::EventReceiver;
use tracing::warn;

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::Workspace;

/// Hook used to open the downloaded temp file in the user's editor. Production
/// always uses [`macsftp_platform::open_in_editor`]; tests swap in a recording
/// stub via [`set_edit_opener`]. A `thread_local` cell keeps this safe on the
/// single-threaded GPUI main loop (and one-thread `#[gpui::test]`) without any
/// `unsafe`.
type EditorOpener = fn(&LocalPath, Option<&str>) -> std::io::Result<()>;

#[cfg(test)]
thread_local! {
    static EDIT_OPENER: std::cell::Cell<Option<EditorOpener>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn edit_opener() -> EditorOpener {
    EDIT_OPENER
        .with(|opener| opener.get())
        .unwrap_or(macsftp_platform::open_in_editor)
}

#[cfg(not(test))]
fn edit_opener() -> EditorOpener {
    macsftp_platform::open_in_editor
}

#[cfg(test)]
pub(crate) fn set_edit_opener(opener: EditorOpener) {
    EDIT_OPENER.with(|cell| cell.set(Some(opener)));
}

/// Owns the sole runtime event consumer for the process.
///
/// Process-wide transfer and persistence events are applied once here.
/// Window-scoped events are then delivered to each workspace, whose core
/// stale-event guard decides whether that window owns the referenced tab.
pub struct AppEventCoordinator {
    _event_drain: Task<()>,
}

impl Global for AppEventCoordinator {}

impl AppEventCoordinator {
    pub fn start(mut event_receiver: EventReceiver, cx: &mut App) -> Self {
        let event_drain = cx.spawn(async move |cx| {
            while let Some(event) = event_receiver.recv().await {
                if cx
                    .update(|cx| {
                        dispatch_event(event, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            _event_drain: event_drain,
        }
    }
}

fn dispatch_event(event: AppEvent, cx: &mut App) {
    if event.is_transfer_event() {
        cx.apply_transfer_event(&event, Timestamp(std::time::SystemTime::now()));
        if matches!(event, AppEvent::TransferConflict(_)) {
            present_orphaned_transfer_conflicts(cx);
        }
        advance_edit_sessions(&event, cx);
        cx.refresh_windows();
        return;
    }

    match event {
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
        window_event => {
            for window in workspace_windows(cx) {
                let event = window_event.clone();
                // A window can close between enumeration and update. The
                // remaining workspaces still receive the event.
                let _closed = window.update(cx, |workspace, window, cx| {
                    workspace.handle_app_event(event, window, cx);
                });
            }
        }
    }
}

/// Correlate a completed/failed transfer back to the edit session it belongs
/// to, then advance that session. Handles two phases:
///
/// - `Downloading` (the initial fetch, correlated by the job's local
///   *destination*): success records the downloaded file's mtime as the watch
///   baseline, moves the session to [`EditPhase::Editing`], and opens the
///   editor; failure removes the session, deletes its temp directory, and
///   surfaces a status message to the owning window so the user can retry.
/// - `UploadingBack` (the watcher's save-back, correlated by the job's local
///   *source*): success rebases `remote_snapshot` to the just-uploaded file's
///   own `(size, mtime)` — an honest zero-round-trip approximation of the new
///   remote, with the mtime truncated to whole seconds so a later directory
///   refresh (which carries the server's whole-second mtime) agrees with it —
///   and returns to [`EditPhase::Editing`]; failure also returns to `Editing`,
///   keeping the temp file so the user can save again to retry.
///
/// Non-terminal events, other phases, and transfers unrelated to any edit are
/// ignored. Runs process-wide, mirroring the transfer reducer, because edit
/// sessions live in the process-global [`AppResources`], not any single window.
fn advance_edit_sessions(event: &AppEvent, cx: &mut App) {
    let (transfer_id, succeeded) = match event {
        AppEvent::TransferCompleted { transfer_id } => (*transfer_id, true),
        AppEvent::TransferFailed(failure) => (failure.transfer_id, false),
        _ => return,
    };
    // Capture the job's full shape (direction + both endpoints) before mutably
    // borrowing resources below. Correlating on the local-path string alone is
    // not enough: an unrelated transfer whose local endpoint happens to equal an
    // edit temp path could otherwise drive — or tear down — the edit session.
    let (direction, source, destination) = match cx.transfers().find_job(transfer_id) {
        Some(job) => (job.direction, job.source.clone(), job.destination.clone()),
        None => return,
    };
    // Exactly one endpoint is local for an edit transfer; that is the temp path.
    let temp_path = match (&source, &destination) {
        (_, TransferEndpoint::Local(path)) => path.clone(),
        (TransferEndpoint::Local(path), _) => path.clone(),
        _ => return,
    };
    let (session_id, phase, tab_id, remote_path) =
        match cx.resources().edit_sessions.find_by_temp_path(&temp_path) {
            Some(session)
                if matches!(
                    session.phase,
                    EditPhase::Downloading | EditPhase::UploadingBack
                ) =>
            {
                (
                    session.id,
                    session.phase.clone(),
                    session.tab_id,
                    session.remote_path.clone(),
                )
            }
            _ => return,
        };

    // Verify the job's direction and remote endpoint match what this session's
    // phase expects. A download lands the remote origin at the local temp
    // (Remote source → Local destination); an upload-back sends the local temp
    // to the remote origin (Local source → Remote destination). A transfer that
    // merely shares the temp path but is the wrong direction or targets a
    // different remote path is NOT this edit's transfer and must be ignored.
    let job_matches_phase = match phase {
        EditPhase::Downloading => {
            direction == TransferDirection::Download
                && matches!(&source, TransferEndpoint::Remote(remote) if remote == &remote_path)
        }
        EditPhase::UploadingBack => {
            direction == TransferDirection::Upload
                && matches!(&destination, TransferEndpoint::Remote(remote) if remote == &remote_path)
        }
        _ => false,
    };
    if !job_matches_phase {
        return;
    }

    match phase {
        EditPhase::Downloading => advance_downloading(session_id, &temp_path, succeeded, cx),
        EditPhase::UploadingBack => {
            advance_uploading_back(session_id, &temp_path, tab_id, &remote_path, succeeded, cx)
        }
        _ => {}
    }
}

/// Finish an edit download: open the editor on success. On failure, tear the
/// session down entirely — remove it from the store, delete its temp directory,
/// and surface a status message to the window owning its tab — so the user gets
/// feedback and can retry the edit (a lingering session would silently block
/// re-editing via [`find_active`]).
///
/// [`find_active`]: macsftp_core::EditSessionStore::find_active
fn advance_downloading(
    session_id: macsftp_core::EditSessionId,
    temp_path: &LocalPath,
    succeeded: bool,
    cx: &mut App,
) {
    if !succeeded {
        // Drop the dead session and remember its tab so we can tell that
        // window's user why the edit did not open.
        let (tab_id, name) = match cx.resources_mut().edit_sessions.remove(session_id) {
            Some(session) => (session.tab_id, file_name_of(session.remote_path.as_str())),
            None => return,
        };
        // Clean up the per-session temp directory (`<edits_dir>/<id>/<file>`);
        // best-effort, so ignore errors (e.g. it was never created).
        if let Some(parent) = std::path::Path::new(temp_path.as_str()).parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        // Surface the failure in the owning window's status bar. Edit sessions
        // are process-global, so the owner is whichever window holds the tab.
        let message = format!("Could not download {name} for editing");
        for window in workspace_windows(cx) {
            let shown = window.update(cx, |workspace, _window, cx| {
                workspace.set_edit_status(tab_id, message.clone(), cx)
            });
            if matches!(shown, Ok(true)) {
                break;
            }
        }
        return;
    }

    // Record the downloaded file's mtime as the baseline the edit watcher
    // compares against to detect local saves.
    let mtime = std::fs::metadata(temp_path.as_str())
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(Timestamp::from_system_time);
    let editor = cx.resources().config.config().external_editor.clone();
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.local_mtime = mtime;
        session.active_transfer = None;
    }
    if let Err(error) = edit_opener()(temp_path, editor.as_deref()) {
        warn!(error = %error, "could not open editor for remote edit");
    }
}

/// Finish an edit upload-back: return to `Editing` either way, rebasing the
/// remote snapshot on success so the next local save is judged against what we
/// just wrote. On success we also rebase the owning tab's directory-listing
/// entry to the same `(size, mtime)` so both baselines the watcher reads — the
/// session snapshot and the listing — stay consistent; otherwise a second save
/// with no manual refresh in between would compare the fresh snapshot against a
/// stale listing and flag a spurious `RemoteConflict`.
fn advance_uploading_back(
    session_id: macsftp_core::EditSessionId,
    temp_path: &LocalPath,
    tab_id: TabId,
    remote_path: &RemotePath,
    succeeded: bool,
    cx: &mut App,
) {
    // On success the remote now holds our local bytes; take the local file's
    // own (size, mtime) as the new remote baseline. It is an approximation
    // (the server may stamp a different mtime) but a self-consistent one. The
    // mtime is TRUNCATED to whole seconds so it matches the granularity the
    // SFTP server — and therefore a future `RemoteDirLoaded` refresh — reports;
    // without this, the sub-second component would make an untouched remote
    // look changed after the next refresh and flag a spurious conflict. On
    // failure we leave the baseline untouched.
    let refreshed = succeeded
        .then(|| std::fs::metadata(temp_path.as_str()).ok())
        .flatten()
        .map(|meta| {
            RemoteSnapshot {
                size: Some(meta.len()),
                modified_at: meta.modified().ok().map(Timestamp::from_system_time),
            }
            .truncated_to_secs()
        });
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.active_transfer = None;
        if let Some(snapshot) = refreshed {
            session.remote_snapshot = snapshot;
        }
    }
    // Keep the listing baseline in step with the rebased snapshot. Done after
    // the resources mutation above so the immutable/mutable resource borrow is
    // dropped before `window.update` re-borrows `cx`. Only one window/tab owns
    // the tab, so we stop after the first entry we update.
    if let Some(snapshot) = refreshed {
        for window in workspace_windows(cx) {
            let synced = window.update(cx, |workspace, _window, _cx| {
                workspace.sync_remote_entry_snapshot(tab_id, remote_path, snapshot)
            });
            if matches!(synced, Ok(true)) {
                break;
            }
        }
    }
    if !succeeded {
        warn!(
            temp = %temp_path.as_str(),
            "edit upload-back failed; session returned to Editing for retry"
        );
    }
}

///
/// Called after a conflict arrives, after a new window opens, and after a
/// window closes. If the owning window disappears, the prompt moves to the
/// active window without duplicating process-wide conflict state.
pub fn present_orphaned_transfer_conflicts(cx: &mut App) {
    let windows = workspace_windows(cx);
    if windows.is_empty() {
        return;
    }
    let pending = cx.transfers().pending_conflicts.clone();
    for conflict in pending {
        if windows.iter().any(|window| {
            window
                .read(cx)
                .is_ok_and(|workspace| workspace.has_transfer_conflict_modal(conflict.id))
        }) {
            continue;
        }
        let target = active_workspace_window(cx).unwrap_or(windows[0]);
        let prompt = conflict_prompt(conflict);
        let _closed = target.update(cx, |workspace, window, cx| {
            workspace.present_transfer_conflict(prompt, window, cx);
        });
    }
}

pub(crate) fn workspace_windows(cx: &App) -> Vec<WindowHandle<Workspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<Workspace>())
        .collect()
}

fn active_workspace_window(cx: &App) -> Option<WindowHandle<Workspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<Workspace>())
}

/// The trailing path component of a remote path, for user-facing messages.
/// Falls back to the whole path when there is no separator.
fn file_name_of(remote_path: &str) -> String {
    remote_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(remote_path)
        .to_string()
}

fn conflict_prompt(conflict: ConflictRequest) -> TransferConflictPrompt {
    TransferConflictPrompt {
        request_id: conflict.id,
        plan_id: conflict.plan_id,
        transfer_id: conflict.transfer_id,
        source: conflict.source,
        destination: conflict.destination,
        source_size: conflict.source_size,
        source_modified_at: conflict.source_modified_at,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use macsftp_core::{
        AppEvent, ConflictPolicy, ConflictRequestId, EditPhase, EditSession, FileKind, LocalPath,
        MetadataPolicy, ProfileId, RemoteEntry, RemotePath, RemoteSnapshot, RuntimeBridgeConfig,
        TabId, Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint,
        TransferFailure, TransferId, TransferJob, TransferPlan, TransferPlanId,
        TransferPlanProgress, TransferPlanSnapshot, TransferPlanState, TransferSnapshot,
        TransferState, UserFacingError, WindowSessionId,
    };
    use macsftp_platform::AppPaths;
    use macsftp_sftp::{BridgeChannels, RuntimeClient};
    use macsftp_storage::ConfigStore;
    use macsftp_ui::Theme;

    use super::{dispatch_event, present_orphaned_transfer_conflicts, set_edit_opener};
    use crate::app_actions;
    use crate::resources::{ActiveResources, ActiveTransfers, AppResources, SharedTransfers};
    use crate::workspace::Workspace;

    thread_local! {
        static OPENER_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    fn mock_edit_opener(_temp: &LocalPath, _editor: Option<&str>) -> std::io::Result<()> {
        OPENER_CALLS.with(|calls| calls.set(calls.get() + 1));
        Ok(())
    }

    /// Register a `Downloading` edit session whose temp path is a real file on
    /// disk (so the success path's mtime read succeeds) and seed a matching
    /// download job (id = `transfer_id`, destination = that temp path) into the
    /// transfer store. Returns the session id and its temp path.
    fn seed_downloading_edit(
        cx: &mut TestAppContext,
        label: &str,
        transfer_id: TransferId,
    ) -> (macsftp_core::EditSessionId, LocalPath) {
        // Mirror production's `<edits_dir>/<session>/<file>` layout: a per-call
        // session directory holding the temp file. The failure path removes
        // this directory, so it must NOT be the shared OS temp dir.
        let session_dir = std::env::temp_dir().join(format!(
            "macsftp-edit-{label}-{}-{}",
            std::process::id(),
            transfer_id.0
        ));
        std::fs::create_dir_all(&session_dir).expect("create edit session dir");
        let temp_file = session_dir.join("a.txt");
        std::fs::write(&temp_file, b"remote contents").expect("write edit temp file");
        let temp_path = LocalPath::new(temp_file.to_string_lossy().as_ref());

        let now = Timestamp::from_secs_since_epoch(10);
        let session_id = cx.update(|cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            let session = EditSession {
                id,
                remote_path: RemotePath::new("/srv/a.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::Downloading,
                remote_snapshot: RemoteSnapshot {
                    size: Some(15),
                    modified_at: Some(now),
                },
                local_mtime: None,
                active_transfer: Some(transfer_id),
                missing_ticks: 0,
            };
            cx.resources_mut().edit_sessions.register(session);
            id
        });

        let job = TransferJob {
            id: transfer_id,
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
            destination: TransferEndpoint::Local(temp_path.clone()),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        cx.update(|cx| {
            dispatch_event(AppEvent::TransferQueued(TransferSnapshot { job }), cx);
        });

        (session_id, temp_path)
    }

    /// Register an `UploadingBack` edit session whose temp path is a real file
    /// on disk, and seed a matching UPLOAD job (id = `transfer_id`, *source* =
    /// that temp path, destination = the remote origin) into the transfer
    /// store. `baseline` becomes the session's `remote_snapshot` so a test can
    /// assert it gets rebased on success. Returns the session id and temp path.
    fn seed_uploading_back_edit(
        cx: &mut TestAppContext,
        label: &str,
        transfer_id: TransferId,
        baseline: RemoteSnapshot,
    ) -> (macsftp_core::EditSessionId, LocalPath) {
        let temp_file = std::env::temp_dir().join(format!(
            "macsftp-upload-{label}-{}-{}.txt",
            std::process::id(),
            transfer_id.0
        ));
        std::fs::write(&temp_file, b"locally edited contents").expect("write edit temp file");
        let temp_path = LocalPath::new(temp_file.to_string_lossy().as_ref());

        let now = Timestamp::from_secs_since_epoch(10);
        let session_id = cx.update(|cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            let session = EditSession {
                id,
                remote_path: RemotePath::new("/srv/a.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::UploadingBack,
                remote_snapshot: baseline,
                local_mtime: Some(now),
                active_transfer: Some(transfer_id),
                missing_ticks: 0,
            };
            cx.resources_mut().edit_sessions.register(session);
            id
        });

        let job = TransferJob {
            id: transfer_id,
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(temp_path.clone()),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        cx.update(|cx| {
            dispatch_event(AppEvent::TransferQueued(TransferSnapshot { job }), cx);
        });

        (session_id, temp_path)
    }

    /// Open a window whose first tab (`TabId(1)`, from the fresh `AppResources`
    /// counter — matching the seeded session's `tab_id`) has a remote listing
    /// holding a single entry for `/srv/a.txt` at the given stale
    /// `(size, modified_at)`. Returns the window so a test can read the entry
    /// back after dispatching the completion.
    fn window_with_stale_remote_entry(
        cx: &mut TestAppContext,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
    ) -> WindowHandle<Workspace> {
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let window = cx
            .add_window(|window, cx| Workspace::new(client, WindowSessionId(1), None, window, cx));
        window
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("window opens with a tab");
                tab.remote.entries = vec![RemoteEntry {
                    name: "a.txt".to_string(),
                    path: RemotePath::new("/srv/a.txt"),
                    kind: FileKind::File,
                    size,
                    permissions: Some(0o644),
                    modified_at,
                    link_target: None,
                }];
            })
            .expect("seed the tab's remote listing");
        window
    }

    fn install_test_globals(cx: &mut TestAppContext, label: &str) {
        let app_paths = test_app_paths(label);
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
            cx.set_global(AppResources::load_for_test(app_paths, config));
            cx.set_global(SharedTransfers::default());
            set_edit_opener(mock_edit_opener);
        });
    }

    #[gpui::test]
    fn download_completion_moves_edit_session_to_editing(cx: &mut TestAppContext) {
        install_test_globals(cx, "edit-editing");
        let transfer_id = TransferId(42);
        let (session_id, _temp_path) = seed_downloading_edit(cx, "editing", transfer_id);

        cx.update(|cx| {
            dispatch_event(AppEvent::TransferCompleted { transfer_id }, cx);
        });

        cx.read(|cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(session_id)
                .expect("edit session survives completion");
            assert_eq!(session.phase, EditPhase::Editing);
            assert!(
                session.local_mtime.is_some(),
                "watch baseline mtime recorded"
            );
            assert_eq!(session.active_transfer, None, "active transfer cleared");
        });
        assert_eq!(
            OPENER_CALLS.with(|calls| calls.get()),
            1,
            "editor opened exactly once on success"
        );
    }

    #[gpui::test]
    fn unrelated_transfer_sharing_temp_path_does_not_drive_edit_session(cx: &mut TestAppContext) {
        install_test_globals(cx, "edit-misattr");
        // A Downloading edit session for /srv/a.txt whose temp path the unrelated
        // transfer below will collide with.
        let edit_transfer = TransferId(60);
        let (session_id, temp_path) = seed_downloading_edit(cx, "misattr", edit_transfer);

        // A SEPARATE transfer that happens to write to the SAME local temp path
        // but is an UPLOAD to a DIFFERENT remote — e.g. a stray user transfer
        // whose destination collided. Its completion must NOT advance the edit
        // session out of Downloading (which would open the editor before the
        // real download landed).
        let now = Timestamp::from_secs_since_epoch(10);
        let unrelated = TransferId(61);
        let job = TransferJob {
            id: unrelated,
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(temp_path.clone()),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/unrelated.txt")),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        cx.update(|cx| {
            dispatch_event(AppEvent::TransferQueued(TransferSnapshot { job }), cx);
            dispatch_event(
                AppEvent::TransferCompleted {
                    transfer_id: unrelated,
                },
                cx,
            );
        });

        cx.read(|cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(session_id)
                .expect("edit session must survive an unrelated transfer");
            assert_eq!(
                session.phase,
                EditPhase::Downloading,
                "an unrelated transfer sharing the temp path must not advance the edit session"
            );
        });
        assert_eq!(
            OPENER_CALLS.with(|calls| calls.get()),
            0,
            "the editor must not open for an unrelated transfer"
        );
    }

    #[gpui::test]
    fn download_failure_removes_session_and_surfaces_error(cx: &mut TestAppContext) {
        install_test_globals(cx, "edit-failed");
        // A window owning the session's tab (`TabId(1)`) so the failure status
        // has somewhere to land.
        let window = window_with_stale_remote_entry(cx, Some(15), None);
        let transfer_id = TransferId(43);
        let (session_id, temp_path) = seed_downloading_edit(cx, "failed", transfer_id);
        // The temp file's parent dir is the per-session directory the fix must
        // clean up on failure.
        let session_dir = std::path::Path::new(temp_path.as_str())
            .parent()
            .expect("temp file has a parent dir")
            .to_path_buf();
        assert!(
            session_dir.exists(),
            "session temp dir exists before failure"
        );

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferFailed(TransferFailure {
                    transfer_id,
                    error: UserFacingError::new(
                        macsftp_core::ErrorCode::Unknown,
                        "boom",
                        "download exploded",
                    ),
                }),
                cx,
            );
        });

        cx.read(|cx| {
            assert!(
                cx.resources().edit_sessions.get(session_id).is_none(),
                "a failed download must remove the session, not park it as Failed"
            );
        });
        assert!(
            !session_dir.exists(),
            "the session's temp directory must be cleaned up on failure"
        );
        cx.read(|cx| {
            let workspace = window.read(cx).expect("window is open");
            let status = workspace
                .status_message_for_test()
                .expect("failure surfaces a status message");
            assert!(
                status.contains("a.txt"),
                "status names the file, was {status:?}"
            );
        });
        assert_eq!(
            OPENER_CALLS.with(|calls| calls.get()),
            0,
            "editor is not opened on failure"
        );
    }

    #[gpui::test]
    fn upload_back_completion_rebases_snapshot_and_returns_to_editing(cx: &mut TestAppContext) {
        install_test_globals(cx, "upload-ok");
        let transfer_id = TransferId(50);
        // A stale baseline that must be overwritten by the uploaded file's own
        // (size, mtime) after the upload completes.
        let stale = RemoteSnapshot {
            size: Some(1),
            modified_at: Some(Timestamp::from_secs_since_epoch(1)),
        };
        // A window whose tab (`TabId(1)`) lists `/srv/a.txt` at the same stale
        // (size, mtime). Its listing baseline must be rebased alongside the
        // session snapshot so a second save is not judged against stale data.
        let window = window_with_stale_remote_entry(cx, stale.size, stale.modified_at);
        let (session_id, temp_path) = seed_uploading_back_edit(cx, "ok", transfer_id, stale);
        let disk_len = std::fs::metadata(temp_path.as_str())
            .expect("temp file exists")
            .len();

        cx.update(|cx| {
            dispatch_event(AppEvent::TransferCompleted { transfer_id }, cx);
        });

        let rebased = cx.read(|cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(session_id)
                .expect("session survives upload completion");
            assert_eq!(
                session.phase,
                EditPhase::Editing,
                "successful upload-back returns to Editing"
            );
            assert_eq!(session.active_transfer, None, "active transfer cleared");
            assert_eq!(
                session.remote_snapshot.size,
                Some(disk_len),
                "remote snapshot is rebased to the uploaded file's size"
            );
            assert!(
                session.remote_snapshot != stale,
                "the stale baseline must be replaced"
            );
            session.remote_snapshot
        });
        // The owning tab's listing entry must be rebased to the SAME values as
        // the session snapshot, so the watcher's next divergence check compares
        // consistent baselines and does not flag a spurious RemoteConflict.
        cx.read(|cx| {
            let workspace = window.read(cx).expect("window is open");
            let synced = workspace
                .remote_entry_snapshot(TabId(1), &RemotePath::new("/srv/a.txt"))
                .expect("listing still holds the edited file");
            assert_eq!(
                synced, rebased,
                "the listing entry must be rebased to match the session snapshot"
            );
            assert!(
                synced != stale,
                "the stale listing baseline must be replaced"
            );
        });
        assert_eq!(
            OPENER_CALLS.with(|calls| calls.get()),
            0,
            "upload-back must not reopen the editor"
        );
    }

    #[gpui::test]
    fn upload_back_truncates_rebased_snapshot_mtime_to_whole_seconds(cx: &mut TestAppContext) {
        install_test_globals(cx, "upload-trunc");
        let transfer_id = TransferId(52);
        let stale = RemoteSnapshot {
            size: Some(1),
            modified_at: Some(Timestamp::from_secs_since_epoch(1)),
        };
        let window = window_with_stale_remote_entry(cx, stale.size, stale.modified_at);
        let (session_id, temp_path) = seed_uploading_back_edit(cx, "trunc", transfer_id, stale);
        // Stamp the temp file with a mtime carrying a sub-second component, so a
        // naive rebase would store that fractional mtime and later disagree with
        // the server's whole-second listing.
        let sub_second = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(1_234)
            + std::time::Duration::from_nanos(837_000_000);
        std::fs::File::open(temp_path.as_str())
            .expect("temp file exists")
            .set_times(std::fs::FileTimes::new().set_modified(sub_second))
            .expect("stamp sub-second mtime");

        cx.update(|cx| {
            dispatch_event(AppEvent::TransferCompleted { transfer_id }, cx);
        });

        cx.read(|cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(session_id)
                .expect("session survives upload completion");
            let modified = session
                .remote_snapshot
                .modified_at
                .expect("rebased snapshot carries a mtime");
            // The rebased mtime must equal its own whole-second truncation:
            // no fractional component survives.
            assert_eq!(
                modified,
                modified.truncated_to_secs(),
                "rebased snapshot mtime must be truncated to whole seconds"
            );
            assert_eq!(
                modified,
                Timestamp::from_secs_since_epoch(1_234),
                "the whole-second part of the local mtime is preserved"
            );
        });
        // The tab's listing entry is rebased to the same truncated value, so a
        // subsequent refresh with the server's whole-second mtime agrees.
        cx.read(|cx| {
            let workspace = window.read(cx).expect("window is open");
            let synced = workspace
                .remote_entry_snapshot(TabId(1), &RemotePath::new("/srv/a.txt"))
                .expect("listing still holds the edited file");
            assert_eq!(
                synced.modified_at,
                Some(Timestamp::from_secs_since_epoch(1_234)),
                "the listing baseline is also whole-second"
            );
        });
    }

    #[gpui::test]
    fn upload_back_failure_returns_to_editing_without_rebasing(cx: &mut TestAppContext) {
        install_test_globals(cx, "upload-fail");
        let transfer_id = TransferId(51);
        let baseline = RemoteSnapshot {
            size: Some(7),
            modified_at: Some(Timestamp::from_secs_since_epoch(70)),
        };
        let (session_id, _temp_path) = seed_uploading_back_edit(cx, "fail", transfer_id, baseline);

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferFailed(TransferFailure {
                    transfer_id,
                    error: UserFacingError::new(
                        macsftp_core::ErrorCode::Unknown,
                        "boom",
                        "upload exploded",
                    ),
                }),
                cx,
            );
        });

        cx.read(|cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(session_id)
                .expect("session survives upload failure");
            assert_eq!(
                session.phase,
                EditPhase::Editing,
                "failed upload-back returns to Editing so the user can retry"
            );
            assert_eq!(
                session.remote_snapshot, baseline,
                "a failed upload must not rebase the remote snapshot"
            );
        });
    }

    #[gpui::test]
    fn transfer_events_are_reduced_once_and_conflict_has_one_window_owner(cx: &mut TestAppContext) {
        let app_paths = test_app_paths("single-owner");
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
            cx.set_global(AppResources::load_for_test(app_paths, config));
            cx.set_global(SharedTransfers::default());
        });

        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let first_client = RuntimeClient::new(channels.command_tx.clone());
        let second_client = RuntimeClient::new(channels.command_tx.clone());
        let first = cx.add_window(|window, cx| {
            Workspace::new(first_client, WindowSessionId(1), None, window, cx)
        });
        let second = cx.add_window(|window, cx| {
            Workspace::new(second_client, WindowSessionId(2), None, window, cx)
        });

        let now = Timestamp::from_secs_since_epoch(10);
        let plan_id = TransferPlanId(1);
        let root_job = TransferJob {
            id: TransferId(1),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source")),
            state: TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: now,
        };
        let plan = TransferPlan {
            id: plan_id,
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: Some(0),
            child_jobs: Vec::new(),
            conflict_policy: ConflictPolicy::default(),
        };
        let child_job = TransferJob {
            id: TransferId(2),
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source/a.txt")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/source/a.txt")),
            state: TransferState::Queued,
            ..root_job.clone()
        };
        let events = [
            AppEvent::TransferPlanStarted(TransferPlanSnapshot { plan, root_job }),
            AppEvent::TransferPlanProgress(TransferPlanProgress {
                plan_id,
                child_jobs: vec![child_job.clone()],
                planned_count: 1,
                total_bytes: Some(4),
            }),
            AppEvent::TransferConflict(TransferConflictPrompt {
                request_id: ConflictRequestId(7),
                plan_id,
                transfer_id: child_job.id,
                source: child_job.source,
                destination: child_job.destination,
                source_size: Some(4),
                source_modified_at: Some(now),
            }),
        ];
        cx.update(|cx| {
            for event in events.iter().chain(events.iter()) {
                dispatch_event(event.clone(), cx);
            }
            assert_eq!(cx.transfers().plans.len(), 1);
            assert_eq!(cx.transfers().jobs.len(), 2);
            assert_eq!(cx.transfers().pending_conflicts.len(), 1);
        });

        let owner_count = cx.read(|cx| {
            [first, second]
                .into_iter()
                .filter(|window| {
                    window.read(cx).is_ok_and(|workspace| {
                        workspace.has_transfer_conflict_modal(ConflictRequestId(7))
                    })
                })
                .count()
        });
        assert_eq!(owner_count, 1);

        let (owner, survivor) = cx.read(|cx| {
            if first
                .read(cx)
                .is_ok_and(|workspace| workspace.has_transfer_conflict_modal(ConflictRequestId(7)))
            {
                (first, second)
            } else {
                (second, first)
            }
        });
        owner
            .update(cx, |_workspace, window, _cx| window.remove_window())
            .expect("owner window should close");
        cx.update(present_orphaned_transfer_conflicts);
        assert!(cx.read(|cx| {
            survivor
                .read(cx)
                .is_ok_and(|workspace| workspace.has_transfer_conflict_modal(ConflictRequestId(7)))
        }));
    }

    fn test_app_paths(label: &str) -> AppPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "macsftp-event-coordinator-{label}-{}-{sequence}",
            std::process::id()
        ));
        AppPaths::from_home_dir(home.to_string_lossy().as_ref())
    }
}
