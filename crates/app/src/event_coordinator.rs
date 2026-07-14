use gpui::{App, Global, Task, WindowHandle};
use macsftp_core::{AppEvent, ConflictRequest, Timestamp, TransferConflictPrompt};
use macsftp_sftp::EventReceiver;
use tracing::warn;

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::Workspace;

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

/// Ensure each pending conflict is presented by exactly one live window.
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
        AppEvent, ConflictPolicy, ConflictRequestId, LocalPath, MetadataPolicy, RemotePath,
        RuntimeBridgeConfig, Timestamp, TransferConflictPrompt, TransferDirection,
        TransferEndpoint, TransferId, TransferJob, TransferPlan, TransferPlanId,
        TransferPlanProgress, TransferPlanSnapshot, TransferPlanState, TransferState,
        WindowSessionId,
    };
    use macsftp_platform::AppPaths;
    use macsftp_sftp::{BridgeChannels, RuntimeClient};
    use macsftp_storage::ConfigStore;
    use macsftp_ui::Theme;

    use super::{dispatch_event, present_orphaned_transfer_conflicts};
    use crate::app_actions;
    use crate::resources::{ActiveTransfers, AppResources, SharedTransfers};
    use crate::workspace::Workspace;

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
