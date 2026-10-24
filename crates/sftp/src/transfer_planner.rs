use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use macsftp_core::{
    AppEvent, ErrorCode, LocalPath, RemotePath, StartTransferCommand, Timestamp, TransferDirection,
    TransferEndpoint, TransferId, TransferJob, TransferPlan, TransferPlanId, TransferPlanProgress,
    TransferPlanState, TransferState, UserFacingError,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub(crate) const PLAN_PROGRESS_BATCH_SIZE: usize = 128;

/// Plan a local upload from a blocking worker thread.
///
/// Local directory traversal must not run on Tokio's async workers. Each
/// The first discovered child is emitted immediately so the UI updates
/// promptly; later children are emitted in bounded batches so large trees do
/// not consume one structural event per filesystem entry.
pub fn plan_local_upload(
    command: StartTransferCommand,
    plan_id: TransferPlanId,
    root_job: TransferJob,
    next_transfer_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
) -> Option<Vec<TransferJob>> {
    let mut planner = LocalUploadPlanner {
        command,
        plan_id,
        next_transfer_id,
        event_tx,
        cancel,
        planned_count: 0,
        total_bytes: Some(0),
        created_at: root_job.created_at,
        child_jobs: Vec::new(),
        pending_progress: Vec::with_capacity(PLAN_PROGRESS_BATCH_SIZE),
    };

    if planner.command.direction == TransferDirection::Download {
        if let Err(error) = planner.plan_single_download() {
            planner.finish_error(error);
            return None;
        }
        if !planner.finish() {
            return None;
        }
        return Some(planner.child_jobs);
    }
    let TransferEndpoint::Remote(destination_root) = planner.command.destination.clone() else {
        planner.fail(UserFacingError::new(
            ErrorCode::Unknown,
            "Invalid Upload Destination",
            "Choose a destination folder on the server.",
        ));
        return None;
    };
    if planner.command.sources.is_empty() {
        planner.fail(UserFacingError::new(
            ErrorCode::NotFound,
            "Nothing Selected",
            "Select at least one file or folder on your Mac and try again.",
        ));
        return None;
    }

    for source in planner.command.sources.clone() {
        let TransferEndpoint::Local(source_path) = source else {
            planner.fail(UserFacingError::new(
                ErrorCode::Unknown,
                "Invalid Upload Source",
                "Select files or folders on your Mac to upload.",
            ));
            return None;
        };
        if let Err(error) = planner.plan_source(source_path, &destination_root) {
            planner.finish_error(error);
            return None;
        }
    }

    if !planner.finish() {
        return None;
    }
    Some(planner.child_jobs)
}

/// Construct the root plan and root job before moving planning to a
/// blocking task. The initial state is visible to the UI immediately.
pub fn new_plan(
    command: &StartTransferCommand,
    plan_id: TransferPlanId,
    root_job_id: TransferId,
    created_at: Timestamp,
) -> (TransferPlan, TransferJob) {
    let source_root = command
        .sources
        .first()
        .cloned()
        .unwrap_or_else(|| TransferEndpoint::Local(LocalPath::new("")));
    let plan = TransferPlan {
        id: plan_id,
        root_job_id,
        source_root: source_root.clone(),
        destination_root: command.destination.clone(),
        state: TransferPlanState::Planning,
        planned_count: 0,
        total_bytes: Some(0),
        child_jobs: Vec::new(),
        conflict_policy: command.conflict_policy.clone(),
    };
    let root_job = TransferJob {
        id: root_job_id,
        direction: command.direction,
        source: source_root,
        destination: command.destination.clone(),
        state: TransferState::Planning,
        metadata_policy: command.metadata_policy.clone(),
        conflict_policy: command.conflict_policy.clone(),
        warnings: Vec::new(),
        created_at,
    };
    (plan, root_job)
}

struct LocalUploadPlanner {
    command: StartTransferCommand,
    plan_id: TransferPlanId,
    next_transfer_id: Arc<AtomicU64>,
    event_tx: flume::Sender<AppEvent>,
    cancel: CancellationToken,
    planned_count: usize,
    total_bytes: Option<u64>,
    created_at: Timestamp,
    child_jobs: Vec<TransferJob>,
    pending_progress: Vec<TransferJob>,
}

impl LocalUploadPlanner {
    fn plan_single_download(&mut self) -> Result<(), UserFacingError> {
        self.ensure_not_cancelled()?;
        if self.command.sources.len() != 1 {
            return Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Choose one file to download",
                "Multi-file and directory downloads are not available yet.",
            ));
        }
        let Some(source) = self.command.sources.first() else {
            return Err(UserFacingError::new(
                ErrorCode::NotFound,
                "Nothing Selected",
                "Select a server file and try again.",
            ));
        };
        let (TransferEndpoint::Remote(source), TransferEndpoint::Local(destination)) =
            (source, &self.command.destination)
        else {
            return Err(UserFacingError::new(
                ErrorCode::Unknown,
                "Invalid download source or destination",
                "Downloads require a remote source file and local destination file.",
            ));
        };
        let source = source.clone();
        let destination = destination.clone();
        self.planned_count = 1;
        self.total_bytes = None;
        let child_job = TransferJob {
            id: TransferId(self.next_transfer_id.fetch_add(1, Ordering::Relaxed)),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(source),
            destination: TransferEndpoint::Local(destination),
            state: TransferState::Queued,
            metadata_policy: self.command.metadata_policy.clone(),
            conflict_policy: self.command.conflict_policy.clone(),
            warnings: Vec::new(),
            created_at: self.created_at,
        };
        self.queue_progress(child_job)
    }

    fn plan_source(
        &mut self,
        source: LocalPath,
        destination_root: &RemotePath,
    ) -> Result<(), UserFacingError> {
        self.ensure_not_cancelled()?;
        let source_path = PathBuf::from(source.as_str());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| planning_io_error("Upload Item Unavailable", &error))?;
        if metadata.file_type().is_dir() {
            let name = source_path.file_name().ok_or_else(|| {
                UserFacingError::new(
                    ErrorCode::NotFound,
                    "Folder Has No Name",
                    "Choose a folder with a valid name and try again.",
                )
            })?;
            let directory_destination = join_remote_child(destination_root, name);
            self.emit_child(source_path.clone(), directory_destination.clone(), 0)?;
            self.plan_directory(&source_path, &directory_destination)
        } else {
            let destination = if self.command.sources.len() == 1 {
                destination_root.clone()
            } else {
                let name = source_path.file_name().ok_or_else(|| {
                    UserFacingError::new(
                        ErrorCode::NotFound,
                        "Item Has No Name",
                        "Choose a file or folder with a valid name and try again.",
                    )
                })?;
                join_remote_child(destination_root, name)
            };
            let size = if metadata.file_type().is_symlink() {
                0
            } else {
                metadata.len()
            };
            self.emit_child(source_path, destination, size)
        }
    }

    fn plan_directory(
        &mut self,
        root_path: &Path,
        root_destination: &RemotePath,
    ) -> Result<(), UserFacingError> {
        let mut dir_stack = vec![(root_path.to_path_buf(), root_destination.clone())];

        while let Some((current_dir, current_destination)) = dir_stack.pop() {
            self.ensure_not_cancelled()?;
            let entries = fs::read_dir(&current_dir)
                .map_err(|error| planning_io_error("Folder Couldn’t Be Read", &error))?;

            let mut subdirectories = Vec::new();

            for entry in entries {
                self.ensure_not_cancelled()?;
                let entry =
                    entry.map_err(|error| planning_io_error("Item Couldn’t Be Read", &error))?;
                let file_name = entry.file_name();
                let destination = join_remote_child(&current_destination, &file_name);
                let path = entry.path();

                let file_type = match entry.file_type() {
                    Ok(ft) => Ok(ft),
                    Err(_) => fs::symlink_metadata(&path).map(|m| m.file_type()),
                };

                let file_type = file_type
                    .map_err(|error| planning_io_error("Item Couldn’t Be Checked", &error))?;

                if file_type.is_dir() {
                    self.emit_child(path.clone(), destination.clone(), 0)?;
                    subdirectories.push((path, destination));
                } else if file_type.is_symlink() {
                    // Symlinks are transferred as symlinks without following them.
                    // This prevents broken symlinks from failing planning and avoids
                    // counting the dereferenced target file size.
                    self.emit_child(path, destination, 0)?;
                } else {
                    let metadata = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => fs::symlink_metadata(&path)
                            .map_err(|error| planning_io_error("Item Couldn’t Be Checked", &error))?,
                    };
                    self.emit_child(path, destination, metadata.len())?;
                }
            }

            // Push subdirectories in reverse order so the first subdirectory
            // is popped and traversed next, matching depth-first order without
            // holding multiple directory handles or recursive stack frames.
            for sub_dir in subdirectories.into_iter().rev() {
                dir_stack.push(sub_dir);
            }
        }
        Ok(())
    }

    fn emit_child(
        &mut self,
        source: PathBuf,
        destination: RemotePath,
        size: u64,
    ) -> Result<(), UserFacingError> {
        self.ensure_not_cancelled()?;
        self.planned_count += 1;
        self.total_bytes = self.total_bytes.and_then(|total| total.checked_add(size));
        let child_job = TransferJob {
            id: TransferId(self.next_transfer_id.fetch_add(1, Ordering::Relaxed)),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new(source.display().to_string())),
            destination: TransferEndpoint::Remote(destination),
            state: TransferState::Queued,
            metadata_policy: self.command.metadata_policy.clone(),
            conflict_policy: self.command.conflict_policy.clone(),
            warnings: Vec::new(),
            created_at: self.created_at,
        };
        self.queue_progress(child_job)
    }

    fn queue_progress(&mut self, child_job: TransferJob) -> Result<(), UserFacingError> {
        self.child_jobs.push(child_job.clone());
        self.pending_progress.push(child_job);
        if self.planned_count == 1 || self.pending_progress.len() >= PLAN_PROGRESS_BATCH_SIZE {
            self.flush_progress()?;
        }
        Ok(())
    }

    fn flush_progress(&mut self) -> Result<(), UserFacingError> {
        if self.pending_progress.is_empty() {
            return Ok(());
        }
        let child_jobs = std::mem::take(&mut self.pending_progress);
        self.event_tx
            .send(AppEvent::TransferPlanProgress(TransferPlanProgress {
                plan_id: self.plan_id,
                child_jobs,
                planned_count: self.planned_count,
                total_bytes: self.total_bytes,
            }))
            .map_err(|_| {
                UserFacingError::new(
                    ErrorCode::ChannelClosed,
                    "Preparation Stopped",
                    "Transfer preparation updates stopped arriving.",
                )
            })
    }

    fn finish(&mut self) -> bool {
        if self.cancel.is_cancelled() {
            self.cancelled();
            return false;
        }
        if let Err(error) = self.flush_progress() {
            self.finish_error(error);
            return false;
        }
        if self.cancel.is_cancelled() {
            self.cancelled();
            return false;
        }
        self.event_tx
            .send(AppEvent::TransferPlanCompleted {
                plan_id: self.plan_id,
            })
            .is_ok()
    }

    fn fail(&self, error: UserFacingError) {
        if let Err(send_error) = self.event_tx.send(AppEvent::TransferPlanFailed {
            plan_id: self.plan_id,
            error,
        }) {
            warn!(error = %send_error, "transfer planning event dropped");
        }
    }

    fn finish_error(&self, error: UserFacingError) {
        if error.code == ErrorCode::Cancelled {
            self.cancelled();
        } else {
            self.fail(error);
        }
    }

    fn cancelled(&self) {
        if let Err(send_error) = self.event_tx.send(AppEvent::TransferPlanCancelled {
            plan_id: self.plan_id,
        }) {
            warn!(error = %send_error, "transfer planning cancellation event dropped");
        }
    }

    fn ensure_not_cancelled(&self) -> Result<(), UserFacingError> {
        if self.cancel.is_cancelled() {
            Err(UserFacingError::new(
                ErrorCode::Cancelled,
                "Preparation Cancelled",
                "Transfer preparation was cancelled.",
            ))
        } else {
            Ok(())
        }
    }
}

fn join_remote_child(parent: &RemotePath, file_name: &std::ffi::OsStr) -> RemotePath {
    let parent_str = parent.as_str().trim_end_matches('/');
    let name_str = file_name.to_string_lossy();
    let mut path = String::with_capacity(parent_str.len() + 1 + name_str.len());
    path.push_str(parent_str);
    path.push('/');
    path.push_str(&name_str);
    RemotePath::new(path)
}

fn planning_io_error(title: &str, error: &std::io::Error) -> UserFacingError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        _ => ErrorCode::Unknown,
    };
    let mut user_error = UserFacingError::new(
        code,
        title,
        "Check the source files and permissions, then try again.",
    )
    .with_retryable(true);
    user_error.detail = Some(error.to_string());
    user_error
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use macsftp_core::{
        AppEvent, ConflictPolicy, LocalPath, MetadataPolicy, ProfileId, StartTransferCommand,
        TransferDirection, TransferEndpoint, TransferId, TransferPlanId,
    };
    use tokio_util::sync::CancellationToken;

    use super::{new_plan, plan_local_upload};

    fn command(source: LocalPath, destination: &str) -> StartTransferCommand {
        StartTransferCommand {
            tab_id: macsftp_core::TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            direction: TransferDirection::Upload,
            sources: vec![TransferEndpoint::Local(source)],
            destination: TransferEndpoint::Remote(macsftp_core::RemotePath::new(destination)),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
        }
    }

    #[test]
    fn directory_planning_streams_all_children_before_completion() {
        let fixture_root =
            std::env::temp_dir().join(format!("macsftp-transfer-plan-{}", std::process::id()));
        std::fs::create_dir_all(fixture_root.join("nested")).expect("create fixture directory");
        std::fs::write(fixture_root.join("alpha.txt"), b"abc").expect("write alpha fixture");
        std::fs::write(fixture_root.join("nested/beta.txt"), b"defg").expect("write beta fixture");

        let command = command(
            LocalPath::new(fixture_root.display().to_string()),
            "/uploads",
        );
        let (plan, root_job) = new_plan(
            &command,
            TransferPlanId(1),
            TransferId(1),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        assert_eq!(plan.planned_count, 0);
        let (event_tx, event_rx) = flume::bounded(8);
        let planned = plan_local_upload(
            command,
            plan.id,
            root_job,
            std::sync::Arc::new(AtomicU64::new(2)),
            event_tx,
            CancellationToken::new(),
        );
        assert!(planned.is_some(), "planning should complete");

        let mut planned_paths = Vec::new();
        let mut final_count = None;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                AppEvent::TransferPlanProgress(progress) => {
                    final_count = Some(progress.planned_count);
                    planned_paths
                        .extend(progress.child_jobs.into_iter().map(|job| job.destination));
                }
                AppEvent::TransferPlanCompleted { plan_id } => {
                    assert_eq!(plan_id, TransferPlanId(1));
                }
                other => panic!("unexpected planning event: {other:?}"),
            }
        }

        let root_name = fixture_root
            .file_name()
            .expect("fixture root name")
            .to_string_lossy();
        assert_eq!(final_count, Some(4));
        assert!(
            planned_paths.contains(&TransferEndpoint::Remote(macsftp_core::RemotePath::new(
                format!("/uploads/{root_name}/alpha.txt")
            )))
        );
        assert!(
            planned_paths.contains(&TransferEndpoint::Remote(macsftp_core::RemotePath::new(
                format!("/uploads/{root_name}/nested")
            )))
        );
        assert!(
            planned_paths.contains(&TransferEndpoint::Remote(macsftp_core::RemotePath::new(
                format!("/uploads/{root_name}/nested/beta.txt")
            )))
        );

        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }

    #[test]
    fn empty_directory_plans_its_named_remote_directory() {
        let fixture_root = std::env::temp_dir().join(format!(
            "macsftp-transfer-plan-empty-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&fixture_root).expect("create empty fixture directory");
        let command = command(
            LocalPath::new(fixture_root.display().to_string()),
            "/uploads",
        );
        let (_, root_job) = new_plan(
            &command,
            TransferPlanId(4),
            TransferId(4),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, _event_rx) = flume::bounded(4);

        let planned = plan_local_upload(
            command,
            TransferPlanId(4),
            root_job,
            std::sync::Arc::new(AtomicU64::new(5)),
            event_tx,
            CancellationToken::new(),
        )
        .expect("empty directory planning should complete");

        assert_eq!(planned.len(), 1);
        let root_name = fixture_root
            .file_name()
            .expect("fixture root name")
            .to_string_lossy();
        assert_eq!(
            planned[0].destination,
            TransferEndpoint::Remote(macsftp_core::RemotePath::new(format!(
                "/uploads/{root_name}"
            )))
        );

        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }

    #[test]
    fn large_directory_planning_emits_first_child_then_bounded_batches() {
        let fixture_root = std::env::temp_dir().join(format!(
            "macsftp-transfer-plan-batches-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&fixture_root).expect("create fixture directory");
        for index in 0..300 {
            std::fs::write(fixture_root.join(format!("file-{index:03}.txt")), b"x")
                .expect("write fixture file");
        }

        let command = command(
            LocalPath::new(fixture_root.display().to_string()),
            "/uploads",
        );
        let (_, root_job) = new_plan(
            &command,
            TransferPlanId(3),
            TransferId(3),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, event_rx) = flume::bounded(8);
        let planned = plan_local_upload(
            command,
            TransferPlanId(3),
            root_job,
            std::sync::Arc::new(AtomicU64::new(4)),
            event_tx,
            CancellationToken::new(),
        )
        .expect("planning should complete");

        let mut batch_sizes = Vec::new();
        let mut final_count = None;
        while let Ok(event) = event_rx.try_recv() {
            if let AppEvent::TransferPlanProgress(progress) = event {
                batch_sizes.push(progress.child_jobs.len());
                final_count = Some(progress.planned_count);
            }
        }
        assert_eq!(planned.len(), 301);
        assert_eq!(batch_sizes, vec![1, 128, 128, 44]);
        assert_eq!(final_count, Some(301));

        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }

    #[test]
    fn cancelled_planning_emits_cancelled_without_child_jobs() {
        let fixture_root = std::env::temp_dir().join(format!(
            "macsftp-transfer-plan-cancel-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&fixture_root).expect("create fixture directory");
        std::fs::write(fixture_root.join("alpha.txt"), b"abc").expect("write fixture file");
        let command = command(
            LocalPath::new(fixture_root.display().to_string()),
            "/uploads",
        );
        let (_, root_job) = new_plan(
            &command,
            TransferPlanId(2),
            TransferId(2),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, event_rx) = flume::bounded(2);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let planned = plan_local_upload(
            command,
            TransferPlanId(2),
            root_job,
            std::sync::Arc::new(AtomicU64::new(3)),
            event_tx,
            cancel,
        );
        assert!(
            planned.is_none(),
            "cancelled planning must not produce jobs"
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::TransferPlanCancelled {
                plan_id: TransferPlanId(2)
            })
        ));
        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }

    #[test]
    fn local_upload_failure_after_progress_emits_plan_failure() {
        let ok_file =
            std::env::temp_dir().join(format!("macsftp-plan-ok-{}.txt", std::process::id()));
        std::fs::write(&ok_file, b"abc").expect("write ok fixture");
        let missing = LocalPath::new("/nonexistent/macsftp/missing-source-xxxx.txt");

        let command = StartTransferCommand {
            tab_id: macsftp_core::TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            direction: TransferDirection::Upload,
            sources: vec![
                TransferEndpoint::Local(LocalPath::new(ok_file.display().to_string())),
                TransferEndpoint::Local(missing),
            ],
            destination: TransferEndpoint::Remote(macsftp_core::RemotePath::new("/uploads")),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
        };
        let (plan, root_job) = new_plan(
            &command,
            TransferPlanId(1),
            TransferId(1),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, event_rx) = flume::bounded(8);

        let planned = plan_local_upload(
            command,
            plan.id,
            root_job,
            std::sync::Arc::new(AtomicU64::new(2)),
            event_tx,
            CancellationToken::new(),
        );
        assert!(planned.is_none(), "planning must fail after a source error");

        let mut seen_progress = false;
        let mut seen_failed = false;
        let mut order = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            match event {
                AppEvent::TransferPlanProgress(progress) => {
                    seen_progress = true;
                    assert!(
                        !progress.child_jobs.is_empty(),
                        "progress must carry a published child"
                    );
                    order.push("progress");
                }
                AppEvent::TransferPlanFailed { plan_id, .. } => {
                    seen_failed = true;
                    assert_eq!(plan_id, TransferPlanId(1));
                    order.push("failed");
                }
                other => panic!("unexpected planning event: {other:?}"),
            }
        }
        assert!(
            seen_progress,
            "a child must be published before the failure"
        );
        assert!(seen_failed, "the planning failure must be published");
        assert_eq!(
            order,
            vec!["progress", "failed"],
            "the failure must follow the published progress"
        );

        let _ = std::fs::remove_file(&ok_file);
    }

    #[test]
    fn local_upload_cancellation_after_progress_emits_plan_cancelled() {
        let fixture_root =
            std::env::temp_dir().join(format!("macsftp-plan-cancel-after-{}", std::process::id()));
        std::fs::create_dir_all(&fixture_root).expect("create fixture directory");
        for index in 0..500 {
            std::fs::write(fixture_root.join(format!("file-{index:03}.txt")), b"x")
                .expect("write fixture file");
        }

        let command = StartTransferCommand {
            tab_id: macsftp_core::TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            direction: TransferDirection::Upload,
            sources: vec![TransferEndpoint::Local(LocalPath::new(
                fixture_root.display().to_string(),
            ))],
            destination: TransferEndpoint::Remote(macsftp_core::RemotePath::new("/uploads")),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
        };
        let (plan, root_job) = new_plan(
            &command,
            TransferPlanId(1),
            TransferId(1),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, event_rx) = flume::bounded(1);
        let cancel = CancellationToken::new();

        let handle = std::thread::spawn({
            let command = command.clone();
            let plan_id = plan.id;
            let root_job = root_job.clone();
            let cancel = cancel.clone();
            let event_tx = event_tx.clone();
            move || {
                plan_local_upload(
                    command,
                    plan_id,
                    root_job,
                    std::sync::Arc::new(AtomicU64::new(2)),
                    event_tx,
                    cancel,
                )
            }
        });

        // The bounded channel of size 1 guarantees backpressure: the planner flushes the
        // root folder progress event (planned_count == 1), and then blocks attempting to send
        // the next progress batch (at batch size 128) until we receive from event_rx.
        // This ensures the planner is deterministically in flight and cannot complete before cancellation.
        let first_event = event_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("first progress event must arrive");
        match first_event {
            AppEvent::TransferPlanProgress(progress) => {
                assert!(
                    !progress.child_jobs.is_empty(),
                    "first progress must carry a published child"
                );
            }
            other => panic!("expected first event to be TransferPlanProgress, got {other:?}"),
        }
        cancel.cancel();

        // Drain events until cancellation is observed, unblocking the planner thread if it is
        // waiting on the bounded channel.
        let mut seen_cancelled = false;
        let mut seen_completed = false;
        while let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            match event {
                AppEvent::TransferPlanProgress(_) => {}
                AppEvent::TransferPlanCancelled { plan_id } => {
                    seen_cancelled = true;
                    assert_eq!(plan_id, TransferPlanId(1));
                    break;
                }
                AppEvent::TransferPlanCompleted { .. } => {
                    seen_completed = true;
                    break;
                }
                other => panic!("unexpected planning event: {other:?}"),
            }
        }

        let planned = handle.join().expect("planner thread panicked");
        assert!(
            planned.is_none(),
            "cancelled planning must not produce jobs"
        );
        assert!(
            seen_cancelled,
            "cancellation must be published after progress"
        );
        assert!(
            !seen_completed,
            "a cancelled plan must not be marked completed"
        );

        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }

    #[test]
    fn directory_planning_handles_broken_and_valid_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture_root = std::env::temp_dir().join(format!(
            "macsftp-transfer-plan-symlinks-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&fixture_root).expect("create fixture directory");
        std::fs::write(fixture_root.join("alpha.txt"), b"regular content")
            .expect("write regular file");
        symlink("alpha.txt", fixture_root.join("valid_link.txt")).expect("create valid symlink");
        symlink(
            "nonexistent_file.txt",
            fixture_root.join("broken_link.txt"),
        )
        .expect("create broken symlink");

        let command = command(
            LocalPath::new(fixture_root.display().to_string()),
            "/uploads",
        );
        let (plan, root_job) = new_plan(
            &command,
            TransferPlanId(10),
            TransferId(10),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, _event_rx) = flume::bounded(16);
        let planned = plan_local_upload(
            command,
            plan.id,
            root_job,
            std::sync::Arc::new(AtomicU64::new(11)),
            event_tx,
            CancellationToken::new(),
        )
        .expect("planning should succeed even with broken symlinks");

        let root_name = fixture_root
            .file_name()
            .expect("fixture root name")
            .to_string_lossy();
        assert_eq!(planned.len(), 4); // root dir + alpha.txt + valid_link.txt + broken_link.txt

        let valid_link_job = planned
            .iter()
            .find(|job| {
                job.destination
                    == TransferEndpoint::Remote(macsftp_core::RemotePath::new(format!(
                        "/uploads/{root_name}/valid_link.txt"
                    )))
            })
            .expect("valid symlink must be planned");
        assert_eq!(
            valid_link_job.state,
            macsftp_core::TransferState::Queued,
            "symlink must be queued"
        );

        let broken_link_job = planned
            .iter()
            .find(|job| {
                job.destination
                    == TransferEndpoint::Remote(macsftp_core::RemotePath::new(format!(
                        "/uploads/{root_name}/broken_link.txt"
                    )))
            })
            .expect("broken symlink must be planned without failing the directory walk");
        assert_eq!(
            broken_link_job.state,
            macsftp_core::TransferState::Queued,
            "broken symlink must be queued"
        );

        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }

    #[test]
    fn directory_planning_iterative_handles_deeply_nested_directories() {
        let fixture_root = std::env::temp_dir().join(format!(
            "macsftp-transfer-plan-deep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut current = fixture_root.clone();
        for level in 0..30 {
            current = current.join(format!("level-{level:02}"));
        }
        std::fs::create_dir_all(&current).expect("create deeply nested directory");
        std::fs::write(current.join("deep.txt"), b"deep content").expect("write deep file");

        let command = command(
            LocalPath::new(fixture_root.display().to_string()),
            "/uploads",
        );
        let (plan, root_job) = new_plan(
            &command,
            TransferPlanId(20),
            TransferId(20),
            macsftp_core::Timestamp::from_secs_since_epoch(1),
        );
        let (event_tx, _event_rx) = flume::bounded(64);
        let planned = plan_local_upload(
            command,
            plan.id,
            root_job,
            std::sync::Arc::new(AtomicU64::new(21)),
            event_tx,
            CancellationToken::new(),
        )
        .expect("deeply nested directory planning must succeed");

        // root + 30 subdirectories + 1 deep file = 32 items
        assert_eq!(planned.len(), 32);

        std::fs::remove_dir_all(&fixture_root).expect("remove fixture directory");
    }
}
