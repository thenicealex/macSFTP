use gpui::{App, Global, Task, WindowHandle};
use macsftp_core::{
    AppEvent, ConflictRequest, EditPhase, LocalPath, Timestamp, TransferConflictPrompt,
    TransferEndpoint,
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

/// Correlate a completed/failed transfer back to the edit session it was
/// downloading for, then advance that session. On success the downloaded
/// file's mtime becomes the watch baseline, the session moves to
/// [`EditPhase::Editing`], and the editor opens. On failure the session moves
/// to [`EditPhase::Failed`] and the editor is not opened. Non-terminal events
/// (and transfers unrelated to any edit) are ignored.
///
/// Runs process-wide, mirroring the transfer reducer, because edit sessions
/// live in the process-global [`AppResources`], not any single window.
fn advance_edit_sessions(event: &AppEvent, cx: &mut App) {
    let (transfer_id, succeeded) = match event {
        AppEvent::TransferCompleted { transfer_id } => (*transfer_id, true),
        AppEvent::TransferFailed(failure) => (failure.transfer_id, false),
        _ => return,
    };
    // The completed job's local destination is the edit temp path (downloads
    // land locally). Extract it as an owned value before mutably borrowing
    // resources below.
    let temp_path = match cx.transfers().find_job(transfer_id) {
        Some(job) => match &job.destination {
            TransferEndpoint::Local(path) => path.clone(),
            TransferEndpoint::Remote(_) => return,
        },
        None => return,
    };
    let session_id = match cx.resources().edit_sessions.find_by_temp_path(&temp_path) {
        Some(session) if session.phase == EditPhase::Downloading => session.id,
        _ => return,
    };

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
    if let Err(error) = edit_opener()(&temp_path, editor.as_deref()) {
        warn!(error = %error, "could not open editor for remote edit");
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

fn workspace_windows(cx: &App) -> Vec<WindowHandle<Workspace>> {
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
