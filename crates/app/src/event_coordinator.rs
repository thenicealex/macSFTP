use gpui::{App, Global, Task, WindowHandle};
use macsftp_core::{
    AppEvent, ConflictRequest, EditPhase, LocalPath, RemoteSnapshot, Timestamp,
    TransferConflictPrompt, TransferEndpoint,
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
///   editor; failure moves it to [`EditPhase::Failed`] and does not open.
/// - `UploadingBack` (the watcher's save-back, correlated by the job's local
///   *source*): success rebases `remote_snapshot` to the just-uploaded file's
///   own `(size, mtime)` — an honest zero-round-trip approximation of the new
///   remote, corrected on the next directory refresh — and returns to
///   [`EditPhase::Editing`]; failure also returns to `Editing`, keeping the
///   temp file so the user can save again to retry.
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
    // Correlate to the session by the job's local endpoint: a download lands at
    // a local *destination*, an upload-back reads from a local *source*. Either
    // way exactly one side is local. Extract it before mutably borrowing
    // resources below.
    let temp_path = match cx.transfers().find_job(transfer_id) {
        Some(job) => match (&job.source, &job.destination) {
            (_, TransferEndpoint::Local(path)) => path.clone(),
            (TransferEndpoint::Local(path), _) => path.clone(),
            _ => return,
        },
        None => return,
    };
    let (session_id, phase) = match cx.resources().edit_sessions.find_by_temp_path(&temp_path) {
        Some(session)
            if matches!(
                session.phase,
                EditPhase::Downloading | EditPhase::UploadingBack
            ) =>
        {
            (session.id, session.phase.clone())
        }
        _ => return,
    };

    match phase {
        EditPhase::Downloading => advance_downloading(session_id, &temp_path, succeeded, cx),
        EditPhase::UploadingBack => advance_uploading_back(session_id, &temp_path, succeeded, cx),
        _ => {}
    }
}

/// Finish an edit download: open the editor on success, mark failed otherwise.
fn advance_downloading(
    session_id: macsftp_core::EditSessionId,
    temp_path: &LocalPath,
    succeeded: bool,
    cx: &mut App,
) {
    if !succeeded {
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
            session.phase = EditPhase::Failed {
                error: macsftp_core::UserFacingError::new(
                    macsftp_core::ErrorCode::Unknown,
                    "Edit download failed",
                    "The file could not be downloaded for editing.",
                ),
            };
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
/// just wrote.
fn advance_uploading_back(
    session_id: macsftp_core::EditSessionId,
    temp_path: &LocalPath,
    succeeded: bool,
    cx: &mut App,
) {
    // On success the remote now holds our local bytes; take the local file's
    // own (size, mtime) as the new remote baseline. It is an approximation
    // (the server may stamp a different mtime) but a self-consistent one, and
    // the next directory refresh corrects it. On failure we leave the baseline
    // untouched.
    let refreshed = succeeded
        .then(|| std::fs::metadata(temp_path.as_str()).ok())
        .flatten()
        .map(|meta| RemoteSnapshot {
            size: Some(meta.len()),
            modified_at: meta.modified().ok().map(Timestamp::from_system_time),
        });
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.active_transfer = None;
        if let Some(snapshot) = refreshed {
            session.remote_snapshot = snapshot;
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
    use gpui::TestAppContext;
    use macsftp_core::{
        AppEvent, ConflictPolicy, ConflictRequestId, EditPhase, EditSession, LocalPath,
        MetadataPolicy, ProfileId, RemotePath, RemoteSnapshot, RuntimeBridgeConfig, TabId,
        Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferFailure,
        TransferId, TransferJob, TransferPlan, TransferPlanId, TransferPlanProgress,
        TransferPlanSnapshot, TransferPlanState, TransferSnapshot, TransferState, UserFacingError,
        WindowSessionId,
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
        let temp_file = std::env::temp_dir().join(format!(
            "macsftp-edit-{label}-{}-{}.txt",
            std::process::id(),
            transfer_id.0
        ));
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
    fn download_failure_moves_edit_session_to_failed(cx: &mut TestAppContext) {
        install_test_globals(cx, "edit-failed");
        let transfer_id = TransferId(43);
        let (session_id, _temp_path) = seed_downloading_edit(cx, "failed", transfer_id);

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
            let session = cx
                .resources()
                .edit_sessions
                .get(session_id)
                .expect("edit session survives failure");
            assert!(
                matches!(session.phase, EditPhase::Failed { .. }),
                "phase becomes Failed, was {:?}",
                session.phase
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
        let (session_id, temp_path) = seed_uploading_back_edit(cx, "ok", transfer_id, stale);
        let disk_len = std::fs::metadata(temp_path.as_str())
            .expect("temp file exists")
            .len();

        cx.update(|cx| {
            dispatch_event(AppEvent::TransferCompleted { transfer_id }, cx);
        });

        cx.read(|cx| {
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
        });
        assert_eq!(
            OPENER_CALLS.with(|calls| calls.get()),
            0,
            "upload-back must not reopen the editor"
        );
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
