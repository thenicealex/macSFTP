use gpui_kit::base::Root;
use gpui_kit::{AnyWindowHandle, App, Context, Global, Task, Window, WindowHandle};
use macsftp_core::{
    AppCommand, AppEvent, ConflictRequest, EditPhase, EditSessionId, LocalPath, ProfileId,
    RemotePath, RemoteSnapshot, TabId, Timestamp, TransferConflictPrompt, TransferDirection,
    TransferEndpoint, TransferId, TransferPlanId,
};
use macsftp_sftp::EventReceiver;
use tracing::warn;

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::Workspace;
use crate::workspace::build_edit_upload_command;

/// Hook used to open the downloaded temp file in the user's editor. Production
/// always uses [`macsftp_platform::open_in_editor`]; tests swap in a recording
/// stub via [`set_edit_opener`]. A `thread_local` cell keeps this safe on the
/// single-threaded GPUI main loop (and one-thread `#[gpui_kit::test]`) without any
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

/// Open an edit-session temp file through the production editor launcher or
/// the test hook. Shared by the initial download-complete path and a later
/// request to reopen the same active edit.
pub(crate) fn open_edit_temp(temp: &LocalPath, editor: Option<&str>) -> std::io::Result<()> {
    edit_opener()(temp, editor)
}

/// Owns the sole runtime event consumer for the process.
///
/// Process-wide transfer and persistence events are applied once here.
/// Window-scoped events are then delivered to each workspace, whose core
/// stale-event guard decides whether that window owns the referenced tab.
pub struct AppEventCoordinator {
    _event_drain: Task<()>,
    _local_event_drain: Task<()>,
    _edit_watcher: Option<macsftp_platform::EditWatcher>,
}

impl Global for AppEventCoordinator {}

const EVENT_BATCH_DRAIN_LIMIT: usize = 64;

impl AppEventCoordinator {
    pub fn start(mut event_receiver: EventReceiver, cx: &mut App) -> Self {
        let event_drain = cx.spawn(async move |cx| {
            let mut batch = Vec::with_capacity(EVENT_BATCH_DRAIN_LIMIT);
            while let Some(event) = event_receiver.recv().await {
                batch.push(event);
                while batch.len() < EVENT_BATCH_DRAIN_LIMIT {
                    match event_receiver.try_recv() {
                        Some(event) => batch.push(event),
                        None => break,
                    }
                }
                cx.update(|cx| {
                    for event in batch.drain(..) {
                        dispatch_event(event, cx);
                    }
                });
            }
        });

        let (local_tx, mut local_rx) = tokio::sync::mpsc::channel::<AppEvent>(64);
        let edits_dir = cx.resources().app_paths.edits_dir.clone();
        let edit_run_id = cx.resources().edit_run_id.clone();
        let watch_root = std::path::PathBuf::from(edits_dir.as_str()).join(edit_run_id);

        let edit_watcher =
            match macsftp_platform::EditWatcher::start(watch_root, move |session_id, temp_path| {
                if let Err(error) = local_tx.try_send(AppEvent::LocalEditModified {
                    session_id,
                    temp_path,
                }) {
                    warn!(error = %error, "could not queue local edit modified event");
                }
            }) {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    warn!(error = %error, "could not start edit watcher");
                    None
                }
            };

        let local_event_drain = cx.spawn(async move |cx| {
            let mut batch = Vec::with_capacity(EVENT_BATCH_DRAIN_LIMIT);
            while let Some(event) = local_rx.recv().await {
                batch.push(event);
                while batch.len() < EVENT_BATCH_DRAIN_LIMIT {
                    match local_rx.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(_) => break,
                    }
                }
                cx.update(|cx| {
                    for event in batch.drain(..) {
                        dispatch_event(event, cx);
                    }
                });
            }
        });

        Self {
            _event_drain: event_drain,
            _local_event_drain: local_event_drain,
            _edit_watcher: edit_watcher,
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
        advance_conflict_compare(&event, cx);
        cx.refresh_windows();
        reload_panes_for_finished_plan(&event, cx);
        quarantine_completed_download(&event, cx);
        return;
    }

    // The three authoritative edit-check results are owned process-wide, not by
    // any window: edit sessions live in the process-global `AppResources`, and
    // broadcasting one result to every window would let them race to apply a
    // single outcome (Task 5). Apply it exactly once here, then return before
    // the ordinary window-broadcast path below.
    match &event {
        AppEvent::RemoteEditSnapshotChecked(_)
        | AppEvent::RemoteEditSnapshotCheckFailed(_)
        | AppEvent::RemoteEditSnapshotDispatchFailed(_) => {
            apply_remote_edit_check_event(&event, cx);
            return;
        }
        AppEvent::LocalEditModified {
            session_id,
            temp_path,
        } => {
            handle_local_edit_modified(*session_id, temp_path, cx);
            return;
        }
        _ => {}
    }

    match event {
        AppEvent::ResidualTempCreated(record) => {
            if let Err(error) = cx.resources_mut().residual_temps.add_and_save(record) {
                warn!(error = %error, "could not persist residual temp record");
            }
        }
        AppEvent::ResidualTempCleared { transfer_id, path } => {
            if let Err(error) = cx
                .resources_mut()
                .residual_temps
                .remove_and_save(transfer_id, &path)
            {
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

/// Mark a just-downloaded executable so macOS asks before running it.
///
/// An SFTP download keeps the remote permission bits, so a file the server
/// marked executable lands on disk executable. Opening such a file hands it to
/// LaunchServices, which runs `.command`-shaped content outright — and a
/// download carries no quarantine flag of its own, because it did not come
/// through a browser. Setting the attribute makes the first launch go through a
/// confirmation instead of executing silently.
///
/// Only executables are marked (see `quarantine_executable_download`), and the
/// work runs on the background executor: it is a `stat` plus at most two small
/// process spawns, none of which belongs on the UI thread.
fn quarantine_completed_download(event: &AppEvent, cx: &mut App) {
    let AppEvent::TransferCompleted { transfer_id } = event else {
        return;
    };
    let Some(job) = cx.transfers().find_job(*transfer_id) else {
        return;
    };
    if job.direction != TransferDirection::Download {
        return;
    }
    let TransferEndpoint::Local(path) = job.destination.clone() else {
        return;
    };
    // Remote-edit downloads land in the app's own edits directory and are
    // opened by the editor flow, not by the user double-clicking a listing.
    // Quarantining them would put a Gatekeeper prompt in front of every
    // "open for editing" of an executable file, for no gain: the file is not
    // reachable from the local pane.
    let edits_dir = std::path::Path::new(cx.resources().app_paths.edits_dir.as_str());
    if std::path::Path::new(path.as_str()).starts_with(edits_dir) {
        return;
    }
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = macsftp_platform::quarantine_executable_download(&path) {
                // The xattr failure never mentions the path, so this stays
                // inside the "no paths in diagnostics" rule.
                warn!(error = %error, "could not mark a downloaded executable as quarantined");
            }
        })
        .detach();
}

/// Reload the panes a finished transfer wrote into.
///
/// The file panes render the last directory listing the runtime delivered, so
/// without this an upload or download completes and the new file is simply
/// absent until the user refreshes by hand. Each upload's remote destination
/// directory and each download's local destination directory is re-read once
/// per finished plan; a pane showing another folder is left alone.
///
/// Reloading is skipped while the plan is still outstanding, and a plan that
/// only touched edit temp files reloads nothing visible: its destination parent
/// is the edits directory, which no pane ever lists.
fn reload_panes_for_finished_plan(event: &AppEvent, cx: &mut App) {
    let plan_id = match event {
        // A plan reaches its terminal state once its last job lands. The
        // reducer records that transition, because re-deriving it here would
        // mean scanning the plan's jobs on every one of thousands of
        // completions — the difference between one directory read at the end of
        // a directory transfer and a quadratic scan per file.
        AppEvent::TransferCompleted { .. } | AppEvent::TransferSkipped { .. } => {
            let Some(plan_id) = cx.transfers().just_finished_plan else {
                return;
            };
            plan_id
        }
        // Plan-level outcomes are once per plan already.
        AppEvent::TransferPlanFailed { plan_id, .. }
        | AppEvent::TransferPlanCancelled { plan_id } => *plan_id,
        _ => return,
    };

    // Read the directories out of the store before any window borrow: an
    // upload and a download of the same plan contribute different sides. One
    // pass over the jobs, with the plan's ids in a set, keeps this linear.
    let (root_job_id, child_ids) = {
        let Some(plan) = cx.transfers().find_plan(plan_id) else {
            return;
        };
        (
            plan.root_job_id,
            plan.child_jobs
                .iter()
                .copied()
                .collect::<std::collections::HashSet<TransferId>>(),
        )
    };
    let mut local_directories = Vec::new();
    let mut remote_directories = Vec::new();
    // The root job is a real transfer row of its own: a download that publishes
    // no children is the root whose destination names the folder that received
    // the file.
    for job in &cx.transfers().jobs {
        if job.id != root_job_id && !child_ids.contains(&job.id) {
            continue;
        }
        match job.destination.parent_directory() {
            Some(TransferEndpoint::Local(path)) => local_directories.push(path),
            Some(TransferEndpoint::Remote(path)) => remote_directories.push(path),
            None => {}
        }
    }
    local_directories.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    local_directories.dedup();
    remote_directories.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    remote_directories.dedup();
    if local_directories.is_empty() && remote_directories.is_empty() {
        return;
    }

    for window in workspace_windows(cx) {
        let local_directories = local_directories.clone();
        let remote_directories = remote_directories.clone();
        let _closed = window.update(cx, |workspace, window, cx| {
            workspace.reload_visible_directories(
                &local_directories,
                &remote_directories,
                window,
                cx,
            );
        });
    }
}

/// Correlate a completed/failed transfer back to the edit session it belongs
/// to, then advance that session. Handles two phases:
///
/// - `Downloading` (the initial fetch, correlated by the job's local
///   *destination*): success moves the session to [`EditPhase::Editing`] and
///   opens the editor; failure removes the session, deletes its temp directory,
///   and surfaces a status message to the owning window so the user can retry.
/// - `UploadingBack` (the explicit upload, correlated by the job's local
///   *source*): success rebases `remote_snapshot` to the just-uploaded file's
///   own `(size, mtime)` — an honest zero-round-trip approximation of the new
///   remote, with the mtime truncated to whole seconds so a later directory
///   refresh (which carries the server's whole-second mtime) agrees with it —
///   and returns to [`EditPhase::Editing`]; failure also returns to `Editing`,
///   keeping the temp file for an explicit retry.
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
                    // Edit transfers always target the write-through path.
                    session.io_path.clone(),
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
        // Clean up the per-session temp directory (`<edits_dir>/<run>/<id>/<file>`);
        // best-effort, so ignore errors (e.g. it was never created).
        crate::workspace::cleanup_edit_temp_dir(temp_path);
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

    let (tab_id, editor) = {
        let resources = cx.resources_mut();
        let config = resources.config.config();
        let file_name = std::path::Path::new(temp_path.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let editor = macsftp_core::resolve_editor_app(
            &file_name,
            &config.editor_rules,
            config.external_editor.as_deref(),
        )
        .map(str::to_string);
        let Some(session) = resources.edit_sessions.get_mut(session_id) else {
            return;
        };
        session.phase = EditPhase::Editing;
        session.local_modified = false;
        if let Ok(meta) = std::fs::metadata(temp_path.as_str())
            && let Ok(mtime) = meta.modified()
        {
            session.local_baseline = Some(macsftp_core::LocalBaseline {
                size: meta.len(),
                mtime,
            });
        }
        (session.tab_id, editor)
    };
    match open_edit_temp(temp_path, editor.as_deref()) {
        Ok(()) => show_edit_status(
            cx,
            tab_id,
            "Your copy is open. After saving, click Upload Changes.",
        ),
        Err(error) => warn!(error = %error, "could not open editor for remote edit"),
    }
}

/// Finish an edit upload-back: return to `Editing` either way, rebasing the
/// remote snapshot on success so a later explicit upload is judged against what
/// we just wrote. The owning tab's directory-listing entry is rebased to the
/// same `(size, mtime)` so a later refresh does not create a false conflict.
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
        if succeeded {
            session.local_modified = false;
            if let Ok(meta) = std::fs::metadata(temp_path.as_str())
                && let Ok(mtime) = meta.modified()
            {
                session.local_baseline = Some(macsftp_core::LocalBaseline {
                    size: meta.len(),
                    mtime,
                });
            }
        }
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
            "explicit edit upload failed; session returned to Editing for retry"
        );
    }
    show_edit_status(
        cx,
        tab_id,
        if succeeded {
            "Changes uploaded"
        } else {
            "Upload failed. Click Upload Changes to try again."
        },
    );
}

/// Apply one authoritative remote-edit check result exactly once. Edit
/// sessions are process-global, so this runs here (not in any window) and
/// returns without broadcasting. The user action allocated the `EditCheckId`,
/// parked the session in `CheckingRemote`, and recorded the local file's mtime;
/// every guard below must re-confirm all of that before an upload or conflict
/// is authorized, or a delayed result from an earlier retry could clobber a
/// concurrent remote change.
fn apply_remote_edit_check_event(event: &AppEvent, cx: &mut App) {
    // Decode the three event shapes into a uniform (scope, tab, epoch, id,
    // check, path, outcome) tuple. The dispatch-failure event carries no
    // scope: routing never reached a live actor, so it is correlated only by
    // epoch/edit-session/check id rather than a remote session. Both scoped and
    // dispatch-failure events still carry the tab/epoch that must match the
    // stored session.
    let (scope, event_tab_id, event_epoch, edit_session_id, check_id, path, outcome) = match event {
        AppEvent::RemoteEditSnapshotChecked(scoped) => (
            Some(scoped.scope.clone()),
            scoped.scope.tab_id,
            scoped.scope.session_epoch,
            scoped.payload.edit_session_id,
            scoped.payload.check_id,
            scoped.payload.path.clone(),
            CheckOutcome::Matched(scoped.payload.snapshot),
        ),
        AppEvent::RemoteEditSnapshotCheckFailed(scoped) => (
            Some(scoped.scope.clone()),
            scoped.scope.tab_id,
            scoped.scope.session_epoch,
            scoped.payload.edit_session_id,
            scoped.payload.check_id,
            scoped.payload.path.clone(),
            CheckOutcome::Failure,
        ),
        AppEvent::RemoteEditSnapshotDispatchFailed(payload) => (
            None,
            payload.tab_id,
            payload.session_epoch,
            payload.edit_session_id,
            payload.check_id,
            payload.path.clone(),
            CheckOutcome::Failure,
        ),
        _ => return,
    };

    // Owner resolution for scoped events: a live window must still own the
    // tab at the scope's epoch. The dispatch-failure event has no scope, so it
    // skips this and relies purely on the epoch/edit-session/check tuple below.
    if let Some(scope) = &scope {
        let owned = workspace_windows(cx).into_iter().any(|window| {
            window
                .read(cx)
                .is_ok_and(|workspace| workspace.accepts_remote_scope(scope))
        });
        if !owned {
            // No live window owns this tab/epoch: the result is stale.
            return;
        }
    }

    // Snapshot the guarded session state in one immutable read, then validate
    // every correlation the user action set up. Any mismatch means the result is
    // stale, superseded, or for a different session, and must be ignored so it
    // cannot authorize an upload.
    let guard = match cx.resources().edit_sessions.get(edit_session_id) {
        Some(session) => {
            let applicable = session.phase == EditPhase::CheckingRemote
                && session.pending_check_id == Some(check_id)
                && session.checking_local_mtime.is_some()
                && session.io_path == path
                && event_tab_id == session.tab_id
                && event_epoch == session.session_epoch;
            if !applicable {
                return;
            }
            CheckGuard {
                tab_id: session.tab_id,
                session_epoch: session.session_epoch,
                profile_id: session.profile_id,
                temp_path: session.local_temp_path.clone(),
                remote_path: session.io_path.clone(),
                baseline: session.remote_snapshot,
                checking_local_mtime: session.checking_local_mtime,
            }
        }
        None => return,
    };

    // The local file selected for upload must remain unchanged while the remote
    // check is in flight. A later save requires another explicit upload.
    let current_temp_mtime = std::fs::metadata(guard.temp_path.as_str())
        .ok()
        .and_then(|meta| meta.modified().ok())
        .map(Timestamp::from_system_time);
    if current_temp_mtime != guard.checking_local_mtime {
        revert_edit_check(cx, edit_session_id);
        show_edit_status(
            cx,
            guard.tab_id,
            "Your copy changed during the check. Click Upload Changes again.",
        );
        return;
    }

    match outcome {
        CheckOutcome::Matched(snapshot) => {
            apply_matched_remote_check(cx, edit_session_id, &guard, &snapshot);
        }
        CheckOutcome::Failure => {
            // Actor stat failure or runtime dispatch failure: return to
            // `Editing` and require another explicit request. Do not claim a
            // conflict and do not upload.
            revert_edit_check(cx, edit_session_id);
            show_edit_status(
                cx,
                guard.tab_id,
                "The server file couldn’t be checked. Click Upload Changes to try again.",
            );
        }
    }
}

/// Snapshot of the guarded edit session state captured before any transition.
struct CheckGuard {
    tab_id: TabId,
    session_epoch: u64,
    profile_id: ProfileId,
    temp_path: LocalPath,
    remote_path: RemotePath,
    baseline: RemoteSnapshot,
    checking_local_mtime: Option<Timestamp>,
}

/// Decoded outcome of a check: either the actor read a live snapshot, or the
/// check failed (actor stat error or runtime dispatch failure).
#[derive(Clone, Copy)]
enum CheckOutcome {
    Matched(RemoteSnapshot),
    Failure,
}

/// Apply a check whose remote snapshot matches the actor's live read. If the
/// remote is still at the baseline, upload the edited file back; if it diverged,
/// flag a conflict. Both branches clear the pending-check fields.
fn apply_matched_remote_check(
    cx: &mut App,
    edit_session_id: EditSessionId,
    guard: &CheckGuard,
    snapshot: &RemoteSnapshot,
) {
    if *snapshot == guard.baseline {
        // Remote confirmed unchanged → upload the edited file back to its
        // origin. The phase leaves `CheckingRemote` before dispatch so a second
        // click cannot create a duplicate upload.
        let command = build_edit_upload_command(
            &guard.temp_path,
            &guard.remote_path,
            guard.session_epoch,
            guard.profile_id,
            guard.tab_id,
        );
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(edit_session_id) {
            session.phase = EditPhase::UploadingBack;
            session.pending_check_id = None;
            session.checking_local_mtime = None;
        }
        dispatch_edit_command(cx, edit_session_id, guard.tab_id, command);
        cx.refresh_windows();
    } else {
        // Remote diverged from the baseline → flag a conflict so the user decides.
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(edit_session_id) {
            session.phase = EditPhase::RemoteConflict;
            session.pending_check_id = None;
            session.checking_local_mtime = None;
        }
        cx.refresh_windows();
    }
}

fn dispatch_edit_command(
    cx: &mut App,
    session_id: EditSessionId,
    tab_id: TabId,
    command: AppCommand,
) {
    let client = workspace_windows(cx).into_iter().find_map(|window| {
        window
            .read(cx)
            .ok()
            .filter(|workspace| workspace.owns_tab(tab_id))
            .map(|workspace| workspace.runtime_client())
    });
    let result = match client {
        Some(client) => client.try_send(command),
        None => {
            warn!(tab_id = ?tab_id, "no window owns the tab for explicit edit upload");
            revert_edit_check(cx, session_id);
            show_edit_status(cx, tab_id, "The upload couldn’t start. Try again.");
            return;
        }
    };
    if let Err(error) = result {
        warn!(error = ?error, "explicit edit upload could not be dispatched");
        revert_edit_check(cx, session_id);
        show_edit_status(cx, tab_id, "The upload couldn’t start. Try again.");
    }
}

fn revert_edit_check(cx: &mut App, session_id: EditSessionId) {
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.pending_check_id = None;
        session.checking_local_mtime = None;
    }
}

/// Surface an edit status message in the window that owns `tab_id`.
fn show_edit_status(cx: &mut App, tab_id: TabId, message: &str) {
    for window in workspace_windows(cx) {
        let shown = window.update(cx, |workspace, _window, cx| {
            workspace.set_edit_status(tab_id, message.to_string(), cx)
        });
        if matches!(shown, Ok(true)) {
            break;
        }
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

/// Routes events to application content even when the Base Root owns the window.
#[derive(Clone, Copy)]
pub(crate) enum WorkspaceWindow {
    Content(WindowHandle<Workspace>),
    Root(WindowHandle<Root>),
}

impl WorkspaceWindow {
    fn from_handle(window: AnyWindowHandle, cx: &App) -> Option<Self> {
        if let Some(content) = window.downcast::<Workspace>() {
            return Some(Self::Content(content));
        }
        let root = window.downcast::<Root>()?;
        root.read(cx)
            .ok()?
            .view()
            .clone()
            .downcast::<Workspace>()
            .ok()?;
        Some(Self::Root(root))
    }

    pub(crate) fn read<'a>(&self, cx: &'a App) -> gpui_kit::Result<&'a Workspace> {
        match self {
            Self::Content(window) => window.read(cx),
            Self::Root(window) => Ok(window
                .read(cx)?
                .view()
                .clone()
                .downcast::<Workspace>()
                .expect("workspace window must contain Workspace content")
                .read(cx)),
        }
    }

    pub(crate) fn update<R>(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
    ) -> gpui_kit::Result<R> {
        match self {
            Self::Content(window) => window.update(cx, update),
            Self::Root(window) => window.update(cx, |root, window, cx| {
                let workspace = root
                    .view()
                    .clone()
                    .downcast::<Workspace>()
                    .expect("workspace window must contain Workspace content");
                workspace.update(cx, |workspace, cx| update(workspace, window, cx))
            }),
        }
    }
}

pub(crate) fn workspace_windows(cx: &App) -> Vec<WorkspaceWindow> {
    cx.windows()
        .into_iter()
        .filter_map(|window| WorkspaceWindow::from_handle(window, cx))
        .collect()
}

fn active_workspace_window(cx: &App) -> Option<WorkspaceWindow> {
    cx.active_window()
        .and_then(|window| WorkspaceWindow::from_handle(window, cx))
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

fn handle_local_edit_modified(session_id: EditSessionId, temp_path: &str, cx: &mut App) {
    {
        let resources = cx.resources_mut();
        let Some(session) = resources.edit_sessions.get_mut(session_id) else {
            return;
        };
        if session.local_temp_path.as_str() != temp_path {
            return;
        }
        if session.phase != EditPhase::Editing {
            return;
        }
        if let Ok(meta) = std::fs::metadata(temp_path) {
            let current_size = meta.len();
            let current_mtime = meta.modified().ok();
            if let (Some(base), Some(current_mtime)) = (session.local_baseline, current_mtime)
                && base.size == current_size
                && base.mtime == current_mtime
            {
                // Exactly matches our baseline (our own write or metadata only) -> suppress!
                return;
            }
        }
        session.local_modified = true;
    }
    cx.refresh_windows();
}

fn advance_conflict_compare(event: &AppEvent, cx: &mut App) {
    let (dest_path, outcome) = match event {
        AppEvent::TransferCompleted { transfer_id } => {
            let Some(dest_path) = compare_destination_for_transfer(*transfer_id, cx) else {
                return;
            };
            (dest_path, Ok(()))
        }
        AppEvent::TransferFailed(failure) => {
            let Some(dest_path) = compare_destination_for_transfer(failure.transfer_id, cx) else {
                return;
            };
            (
                dest_path,
                Err("Download of remote copy for comparison failed.".to_string()),
            )
        }
        // Planning and dispatch failures never reach `TransferFailed`, so a
        // plan that dies before its job runs would otherwise leave the modal
        // stuck on “Comparing…”. The plan's destination root identifies the
        // comparison request.
        AppEvent::TransferPlanFailed { plan_id, error } => {
            let Some(dest_path) = compare_destination_for_plan(*plan_id, cx) else {
                return;
            };
            (
                dest_path,
                Err(format!(
                    "Could not download the remote copy: {}",
                    error.title
                )),
            )
        }
        AppEvent::TransferPlanCancelled { plan_id } => {
            let Some(dest_path) = compare_destination_for_plan(*plan_id, cx) else {
                return;
            };
            (
                dest_path,
                Err("Comparison download was cancelled.".to_string()),
            )
        }
        // A child cancel emits only `TransferSkipped`; cancelling a plan root
        // is reported as `TransferPlanCancelled`. Both mean this comparison
        // download stopped before producing its copy.
        AppEvent::TransferSkipped { transfer_id } => {
            let Some(dest_path) = compare_destination_for_transfer(*transfer_id, cx) else {
                return;
            };
            (
                dest_path,
                Err("Comparison download was cancelled.".to_string()),
            )
        }
        _ => return,
    };
    apply_compare_outcome(&dest_path, outcome, cx);
}

fn compare_destination_for_transfer(transfer_id: TransferId, cx: &App) -> Option<LocalPath> {
    let job = cx.transfers().find_job(transfer_id)?;
    match &job.destination {
        TransferEndpoint::Local(path) => Some(path.clone()),
        _ => None,
    }
}

fn compare_destination_for_plan(plan_id: TransferPlanId, cx: &App) -> Option<LocalPath> {
    let plan = cx.transfers().find_plan(plan_id)?;
    match &plan.destination_root {
        TransferEndpoint::Local(path) => Some(path.clone()),
        _ => None,
    }
}

fn apply_compare_outcome(dest_path: &LocalPath, outcome: Result<(), String>, cx: &mut App) {
    if !dest_path.as_str().contains("conflict-remote") {
        return;
    }
    let matching_session = cx
        .resources()
        .edit_sessions
        .sessions()
        .find(|session| {
            session.phase == EditPhase::RemoteConflict
                && crate::workspace::conflict_remote_path(&session.local_temp_path).as_str()
                    == dest_path.as_str()
        })
        .cloned();

    let Some(session) = matching_session else {
        return;
    };

    for window in workspace_windows(cx) {
        if let Err(error) = window.update(cx, |workspace, _window, cx| {
            if !workspace.owns_tab(session.tab_id) {
                return;
            }
            // Only the workspace that started this session's comparison may
            // consume its result; another tab's modal owns different state.
            if workspace.modal_inputs.conflict_compare_session != Some(session.id) {
                return;
            }
            workspace.modal_inputs.conflict_compare_loading = false;
            match &outcome {
                Ok(()) => {
                    workspace.modal_inputs.conflict_compare_error = None;
                    if let Some(tool) = macsftp_platform::detect_default_diff_tool() {
                        let local = std::path::Path::new(session.local_temp_path.as_str());
                        let remote = std::path::Path::new(dest_path.as_str());
                        if !local.is_file() || !remote.is_file() {
                            workspace.modal_inputs.conflict_compare_error = Some(
                                "The comparison copy is missing. Try Compare Changes again.".into(),
                            );
                        } else if let Err(error) = tool.launch(local, remote) {
                            warn!(error = %error, "could not launch diff tool after download");
                            workspace.modal_inputs.conflict_compare_error =
                                Some(format!("Could not launch {}: {}", tool.name(), error));
                        }
                    } else {
                        workspace.modal_inputs.conflict_compare_error = Some(
                            "No supported diff tool found (/usr/bin/opendiff or code).".into(),
                        );
                    }
                }
                Err(message) => {
                    workspace.modal_inputs.conflict_compare_error = Some(message.clone());
                }
            }
            cx.notify();
        }) {
            warn!(error = %error, "could not update conflict compare state");
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::base::Root;
    use gpui_kit::{AppContext as _, TestAppContext, WindowHandle};
    use macsftp_core::{
        AppCommand, AppEvent, AuthCredential, ConflictPolicy, ConflictRequestId, ConnectionKey,
        ConnectionPoolIdentity, ConnectionSettings, ConnectionState, EditCheckId, EditPhase,
        EditSession, EditSessionId, ErrorCode, FileKind, LocalPath, MetadataPolicy, ProfileId,
        RemoteEditSnapshotCheckFailed, RemoteEditSnapshotChecked, RemoteEditSnapshotDispatchFailed,
        RemoteEntry, RemoteEventScope, RemotePath, RemoteScoped, RemoteSnapshot,
        RuntimeBridgeConfig, SessionId, TabId, Timestamp, TransferConflictPrompt,
        TransferDirection, TransferEndpoint, TransferFailure, TransferId, TransferJob,
        TransferPlan, TransferPlanId, TransferPlanProgress, TransferPlanSnapshot,
        TransferPlanState, TransferState, UserFacingError, WindowSessionId,
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

    /// The saved-profile connection identity shared by every edit fixture in
    /// this module; the coordinator never reads it, but `EditSession` requires
    /// a well-formed key.
    fn test_connection_key() -> ConnectionKey {
        ConnectionKey::new(
            &ConnectionSettings {
                host: "srv.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "unused".into(),
                },
                route: macsftp_core::ResolvedConnectionRoute::Direct,
            },
            ConnectionPoolIdentity::Ephemeral(SessionId(1)),
        )
    }

    fn seed_transfer(cx: &mut TestAppContext, job: TransferJob) {
        let plan = TransferPlan {
            id: TransferPlanId(job.id.0),
            root_job_id: job.id,
            source_root: job.source.clone(),
            destination_root: job.destination.clone(),
            state: TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: None,
            child_jobs: Vec::new(),
            conflict_policy: job.conflict_policy.clone(),
        };
        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot {
                    plan,
                    root_job: job,
                })),
                cx,
            );
        });
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
        // Mirror production's `<edits_dir>/<run>/<session>/<file>` layout: a per-call
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
                io_path: RemotePath::new("/srv/a.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                connection_key: test_connection_key(),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::Downloading,
                remote_snapshot: RemoteSnapshot {
                    size: Some(15),
                    modified_at: Some(now),
                },
                pending_check_id: None,
                checking_local_mtime: None,
                local_modified: false,
                local_baseline: None,
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
        seed_transfer(cx, job);

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
                io_path: RemotePath::new("/srv/a.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                connection_key: test_connection_key(),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::UploadingBack,
                remote_snapshot: baseline,
                pending_check_id: None,
                checking_local_mtime: None,
                local_modified: false,
                local_baseline: None,
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
        seed_transfer(cx, job);

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
            Theme::dark().install(cx);
            app_actions::init(cx);
            cx.set_global(AppResources::load_for_test(app_paths, config));
            cx.set_global(SharedTransfers::default());
            set_edit_opener(mock_edit_opener);
        });
    }

    #[gpui_kit::test]
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
        });
        assert_eq!(
            OPENER_CALLS.with(|calls| calls.get()),
            1,
            "editor opened exactly once on success"
        );
    }

    #[gpui_kit::test]
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
        seed_transfer(cx, job);
        cx.update(|cx| {
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

    #[gpui_kit::test]
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

    #[gpui_kit::test]
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
        // the session snapshot, so the next explicit upload check compares a
        // consistent baseline and does not flag a spurious RemoteConflict.
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

    #[gpui_kit::test]
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

    #[gpui_kit::test]
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

    #[gpui_kit::test]
    fn transfer_events_are_reduced_once_and_conflict_has_one_window_owner(cx: &mut TestAppContext) {
        let app_paths = test_app_paths("single-owner");
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        cx.update(|cx| {
            Theme::dark().install(cx);
            app_actions::init(cx);
            cx.set_global(AppResources::load_for_test(app_paths, config));
            cx.set_global(SharedTransfers::default());
        });

        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let first_client = RuntimeClient::new(channels.command_tx.clone());
        let second_client = RuntimeClient::new(channels.command_tx.clone());
        let first = super::WorkspaceWindow::Root(cx.add_window(|window, cx| {
            let workspace =
                cx.new(|cx| Workspace::new(first_client, WindowSessionId(1), None, window, cx));
            Root::new(workspace, window, cx)
        }));
        let second = super::WorkspaceWindow::Root(cx.add_window(|window, cx| {
            let workspace =
                cx.new(|cx| Workspace::new(second_client, WindowSessionId(2), None, window, cx));
            Root::new(workspace, window, cx)
        }));

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
            AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot { plan, root_job })),
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
        cx.update(|cx| owner.update(cx, |_workspace, window, _cx| window.remove_window()))
            .expect("Base Root owner window should close");
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

    /// Register a `CheckingRemote` edit session whose temp file is a real file
    /// on disk stamped at `checking_mtime`, with `pending_check_id =
    /// Some(check_id)` and `checking_local_mtime = Some(checking_mtime)`. The
    /// session's `remote_snapshot` baseline is `baseline`. Returns the session
    /// id and temp path.
    fn seed_checking_remote_edit(
        cx: &mut TestAppContext,
        label: &str,
        baseline: RemoteSnapshot,
        checking_mtime: Timestamp,
        check_id: EditCheckId,
    ) -> (EditSessionId, LocalPath) {
        let session_dir = std::env::temp_dir().join(format!(
            "macsftp-check-{label}-{}-{}",
            std::process::id(),
            label
        ));
        std::fs::create_dir_all(&session_dir).expect("create edit session dir");
        let temp_file = session_dir.join("a.txt");
        std::fs::write(&temp_file, b"locally edited contents").expect("write edit temp file");
        set_file_mtime(&temp_file, checking_mtime);
        let temp_path = LocalPath::new(temp_file.to_string_lossy().as_ref());

        let session_id = cx.update(|cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            let session = EditSession {
                id,
                remote_path: RemotePath::new("/srv/a.txt"),
                io_path: RemotePath::new("/srv/a.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                connection_key: test_connection_key(),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::CheckingRemote,
                remote_snapshot: baseline,
                pending_check_id: Some(check_id),
                checking_local_mtime: Some(checking_mtime),
                local_modified: false,
                local_baseline: None,
            };
            cx.resources_mut().edit_sessions.register(session);
            id
        });
        (session_id, temp_path)
    }

    /// Open a window whose first tab (`TabId(1)`) owns the session and is
    /// `Connected` at `epoch`, so the coordinator's owner-resolution accepts a
    /// check result scoped to that tab/epoch. Returns the window and its bridge
    /// channels so a test can read commands the coordinator dispatches.
    fn open_owning_window(
        cx: &mut TestAppContext,
        epoch: u64,
    ) -> (WindowHandle<Workspace>, BridgeChannels) {
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let window = cx
            .add_window(|window, cx| Workspace::new(client, WindowSessionId(1), None, window, cx));
        window
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("window opens with a tab");
                tab.session_epoch = epoch;
                tab.connection = ConnectionState::Connected {
                    session_id: SessionId(1),
                    session_epoch: epoch,
                    connected_at: Timestamp::from_secs_since_epoch(1),
                };
            })
            .expect("mark tab owning and connected");
        (window, channels)
    }

    /// Stamp `path`'s modified time to `mtime` so the coordinator's re-stat
    /// guard reads the exact `(size, mtime)` the save recorded.
    fn set_file_mtime(path: &std::path::Path, mtime: Timestamp) {
        std::fs::File::open(path)
            .expect("temp file exists")
            .set_times(std::fs::FileTimes::new().set_modified(mtime.0))
            .expect("stamp temp file mtime");
    }

    fn session_phase(cx: &mut TestAppContext, id: EditSessionId) -> EditPhase {
        cx.update(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session survives")
                .phase
                .clone()
        })
    }

    /// The authoritative check result for a `CheckingRemote` session, scoped to
    /// `tab_id`/`session_id`/`epoch` with `check_id` and the live `snapshot`.
    fn checked_event(
        tab_id: TabId,
        session_id: SessionId,
        epoch: u64,
        edit_session_id: EditSessionId,
        check_id: EditCheckId,
        snapshot: RemoteSnapshot,
    ) -> AppEvent {
        AppEvent::RemoteEditSnapshotChecked(RemoteScoped::new(
            RemoteEventScope::new(tab_id, session_id, epoch),
            RemoteEditSnapshotChecked {
                edit_session_id,
                check_id,
                path: RemotePath::new("/srv/a.txt"),
                snapshot,
            },
        ))
    }

    /// The authoritative check-failure result for a `CheckingRemote` session.
    fn check_failed_event(
        tab_id: TabId,
        session_id: SessionId,
        epoch: u64,
        edit_session_id: EditSessionId,
        check_id: EditCheckId,
    ) -> AppEvent {
        AppEvent::RemoteEditSnapshotCheckFailed(RemoteScoped::new(
            RemoteEventScope::new(tab_id, session_id, epoch),
            RemoteEditSnapshotCheckFailed {
                edit_session_id,
                check_id,
                path: RemotePath::new("/srv/a.txt"),
                error: UserFacingError::new(
                    ErrorCode::Unknown,
                    "Server Check Failed",
                    "The server file couldn’t be checked.",
                )
                .with_retryable(false),
            },
        ))
    }

    /// The runtime dispatch-failure result for a `CheckingRemote` session: no
    /// scope, only the epoch/edit-session/check tuple.
    fn dispatch_failed_event(
        tab_id: TabId,
        epoch: u64,
        edit_session_id: EditSessionId,
        check_id: EditCheckId,
    ) -> AppEvent {
        AppEvent::RemoteEditSnapshotDispatchFailed(RemoteEditSnapshotDispatchFailed {
            tab_id,
            session_epoch: epoch,
            edit_session_id,
            check_id,
            path: RemotePath::new("/srv/a.txt"),
            error: UserFacingError::new(
                ErrorCode::Unknown,
                "Server Check Failed",
                "The server file check couldn’t start.",
            )
            .with_retryable(true),
        })
    }

    #[gpui_kit::test]
    fn matching_remote_check_dispatches_one_upload(cx: &mut TestAppContext) {
        install_test_globals(cx, "matched-upload");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) = seed_checking_remote_edit(
            cx,
            "matched-upload",
            baseline,
            checking_mtime,
            EditCheckId(1),
        );
        let (_window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
        });

        assert_eq!(
            session_phase(cx, id),
            EditPhase::UploadingBack,
            "a matching remote check must move the session to UploadingBack"
        );
        let command = channels
            .command_rx
            .try_recv()
            .expect("exactly one upload must be dispatched");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Upload);
        assert_eq!(
            command.destination,
            TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no second command may be dispatched"
        );
    }

    #[gpui_kit::test]
    fn diverged_remote_check_enters_conflict_without_upload(cx: &mut TestAppContext) {
        install_test_globals(cx, "diverged-conflict");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) = seed_checking_remote_edit(
            cx,
            "diverged-conflict",
            baseline,
            checking_mtime,
            EditCheckId(1),
        );
        let (_window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        // The actor reports a DIFFERENT size than the baseline: the remote was
        // changed by someone else during the edit.
        let diverged = RemoteSnapshot {
            size: Some(999),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), diverged),
                cx,
            );
        });

        assert_eq!(
            session_phase(cx, id),
            EditPhase::RemoteConflict,
            "a diverged remote must flag a conflict, not blind-overwrite"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may be dispatched when the remote diverged"
        );
    }

    #[gpui_kit::test]
    fn failed_remote_check_returns_to_editing_without_upload(cx: &mut TestAppContext) {
        install_test_globals(cx, "failed-editing");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) = seed_checking_remote_edit(
            cx,
            "failed-editing",
            baseline,
            checking_mtime,
            EditCheckId(1),
        );
        let (_window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(|cx| {
            dispatch_event(
                check_failed_event(TabId(1), SessionId(1), 1, id, EditCheckId(1)),
                cx,
            );
        });

        // Returned to Editing so the user can explicitly retry.
        assert_eq!(
            session_phase(cx, id),
            EditPhase::Editing,
            "a failed check must return the session to Editing"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may be dispatched on a failed check"
        );
    }

    #[gpui_kit::test]
    fn stale_remote_check_after_reconnect_is_ignored_and_session_retries(cx: &mut TestAppContext) {
        install_test_globals(cx, "stale-reconnect");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) = seed_checking_remote_edit(
            cx,
            "stale-reconnect",
            baseline,
            checking_mtime,
            EditCheckId(1),
        );
        // Simulate reconnect: the tab's epoch advances and the edit session is
        // reset out of CheckingRemote (as `update_epoch_for_tab` does).
        let (_window, channels) = open_owning_window(cx, 2);
        cx.update(|cx| {
            if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
                session.phase = EditPhase::Editing;
                session.pending_check_id = None;
                session.checking_local_mtime = None;
                session.session_epoch = 2;
            }
        });
        while channels.command_rx.try_recv().is_ok() {}

        // The OLD check result (epoch 1, the stale connection) arrives late.
        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
        });

        // It must NOT apply: the session stays Editing (ready to retry) and no
        // upload is dispatched for the stale result.
        assert_eq!(
            session_phase(cx, id),
            EditPhase::Editing,
            "a stale post-reconnect check result must be ignored"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may follow a stale check result"
        );
    }

    #[gpui_kit::test]
    fn stale_dispatch_failure_after_reconnect_does_not_reset_replacement_check(
        cx: &mut TestAppContext,
    ) {
        install_test_globals(cx, "stale-dispatch");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) = seed_checking_remote_edit(
            cx,
            "stale-dispatch",
            baseline,
            checking_mtime,
            EditCheckId(2),
        );
        // Simulate reconnect: a NEW check (epoch 2, check_id 2) is now in
        // flight. The session is CheckingRemote again with the new epoch/id.
        let (_window, channels) = open_owning_window(cx, 2);
        cx.update(|cx| {
            if let Some(session) = cx.resources_mut().edit_sessions.get_mut(id) {
                session.session_epoch = 2;
                session.pending_check_id = Some(EditCheckId(2));
            }
        });
        while channels.command_rx.try_recv().is_ok() {}

        // A dispatch failure for the OLD check (epoch 1, check_id 1) arrives.
        cx.update(|cx| {
            dispatch_event(dispatch_failed_event(TabId(1), 1, id, EditCheckId(1)), cx);
        });

        // It must NOT reset/revert the replacement check.
        assert_eq!(
            session_phase(cx, id),
            EditPhase::CheckingRemote,
            "a stale dispatch failure must not revert the replacement check"
        );
        let pending = cx.update(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session survives")
                .pending_check_id
        });
        assert_eq!(
            pending,
            Some(EditCheckId(2)),
            "the live replacement check id must be preserved"
        );
    }

    #[gpui_kit::test]
    fn remote_check_event_is_applied_once_with_multiple_windows(cx: &mut TestAppContext) {
        install_test_globals(cx, "multi-window");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) =
            seed_checking_remote_edit(cx, "multi-window", baseline, checking_mtime, EditCheckId(1));
        // Two windows, only the first owns the tab at epoch 1.
        let (_owning, channels) = open_owning_window(cx, 1);
        let other = cx.add_window(|window, cx| {
            Workspace::new(
                RuntimeClient::new(BridgeChannels::new(&RuntimeBridgeConfig::default()).command_tx),
                WindowSessionId(2),
                None,
                window,
                cx,
            )
        });
        other
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("window opens with a tab");
                tab.session_epoch = 1;
                tab.connection = ConnectionState::Connected {
                    session_id: SessionId(1),
                    session_epoch: 1,
                    connected_at: Timestamp::from_secs_since_epoch(1),
                };
            })
            .expect("second window owns a different tab id");
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
        });

        // Exactly one upload, applied by the coordinator (not broadcast).
        assert_eq!(session_phase(cx, id), EditPhase::UploadingBack);
        let command = channels
            .command_rx
            .try_recv()
            .expect("the single owning window receives the upload command");
        assert!(matches!(command, AppCommand::StartTransfer(_)));
        assert!(
            channels.command_rx.try_recv().is_err(),
            "the result is applied exactly once"
        );
    }

    #[gpui_kit::test]
    fn duplicate_remote_check_result_does_not_dispatch_second_upload(cx: &mut TestAppContext) {
        install_test_globals(cx, "duplicate");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) =
            seed_checking_remote_edit(cx, "duplicate", baseline, checking_mtime, EditCheckId(1));
        let (_window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
            // A duplicate of the same result arrives (e.g. at-least-once
            // delivery). The session is no longer CheckingRemote, so it is
            // ignored.
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
        });

        assert!(
            matches!(
                channels.command_rx.try_recv(),
                Ok(AppCommand::StartTransfer(_))
            ),
            "exactly one upload is dispatched for the first result"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "the duplicate result must not dispatch a second upload"
        );
    }

    #[gpui_kit::test]
    fn late_result_from_prior_check_id_does_not_authorize_retry(cx: &mut TestAppContext) {
        install_test_globals(cx, "prior-check-id");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, _temp) = seed_checking_remote_edit(
            cx,
            "prior-check-id",
            baseline,
            checking_mtime,
            EditCheckId(2),
        );
        let (_window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        // A late result for the PREVIOUS check id (1) arrives; the live session
        // is on check id 2. It must be ignored.
        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
        });

        assert_eq!(
            session_phase(cx, id),
            EditPhase::CheckingRemote,
            "a result for a superseded check id must not transition the session"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may follow a superseded check result"
        );
    }

    #[gpui_kit::test]
    fn local_file_changed_during_remote_check_requires_another_upload(cx: &mut TestAppContext) {
        install_test_globals(cx, "changed-during");
        let baseline = RemoteSnapshot {
            size: Some(11),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        };
        let checking_mtime = Timestamp::from_secs_since_epoch(200);
        let (id, temp_path) = seed_checking_remote_edit(
            cx,
            "changed-during",
            baseline,
            checking_mtime,
            EditCheckId(1),
        );
        let (_window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        // The user re-saves while the remote check is in flight, so the temp
        // file's mtime no longer matches `checking_local_mtime`.
        set_file_mtime(
            std::path::Path::new(temp_path.as_str()),
            Timestamp::from_secs_since_epoch(300),
        );

        cx.update(|cx| {
            dispatch_event(
                checked_event(TabId(1), SessionId(1), 1, id, EditCheckId(1), baseline),
                cx,
            );
        });

        // The result does not authorize an upload: the session returns to
        // Editing and the user can request a fresh check for the newer save.
        assert_eq!(
            session_phase(cx, id),
            EditPhase::Editing,
            "a result whose temp mtime no longer matches must not upload"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no upload may be dispatched for a changed local save"
        );
    }

    #[gpui_kit::test]
    fn local_edit_modified_suppresses_baseline_and_marks_dirty_on_change(cx: &mut TestAppContext) {
        install_test_globals(cx, "self-write-suppression");
        let session_dir = std::env::temp_dir().join(format!(
            "macsftp-selfwrite-{}-{}",
            std::process::id(),
            "suppression"
        ));
        std::fs::create_dir_all(&session_dir).expect("create session dir for self-write test");
        let temp_file = session_dir.join("file.txt");
        std::fs::write(&temp_file, b"initial content").expect("write temp file for test");
        let meta = std::fs::metadata(&temp_file).expect("read metadata of temp file");
        let initial_baseline = macsftp_core::LocalBaseline {
            size: meta.len(),
            mtime: meta.modified().expect("read initial mtime"),
        };
        let temp_path = LocalPath::new(temp_file.to_string_lossy().as_ref());

        let id = cx.update(|cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            let session = EditSession {
                id,
                remote_path: RemotePath::new("/srv/file.txt"),
                io_path: RemotePath::new("/srv/file.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                connection_key: test_connection_key(),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::Editing,
                remote_snapshot: RemoteSnapshot {
                    size: Some(initial_baseline.size),
                    modified_at: None,
                },
                pending_check_id: None,
                checking_local_mtime: None,
                local_modified: false,
                local_baseline: Some(initial_baseline),
            };
            cx.resources_mut().edit_sessions.register(session);
            id
        });

        // 1. Dispatch LocalEditModified when file is untouched (matches baseline)
        cx.update(|cx| {
            dispatch_event(
                AppEvent::LocalEditModified {
                    session_id: id,
                    temp_path: temp_path.as_str().to_string(),
                },
                cx,
            );
        });

        let modified_after_suppression = cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session exists")
                .local_modified
        });
        assert!(
            !modified_after_suppression,
            "untouched file matching baseline must be suppressed as self-write"
        );

        // 2. Modify the file on disk
        std::fs::write(&temp_file, b"initial content with modifications")
            .expect("modify temp file");

        cx.update(|cx| {
            dispatch_event(
                AppEvent::LocalEditModified {
                    session_id: id,
                    temp_path: temp_path.as_str().to_string(),
                },
                cx,
            );
        });

        let modified_after_change = cx.read(|cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("session exists")
                .local_modified
        });
        assert!(
            modified_after_change,
            "modified file differing from baseline must be marked dirty"
        );

        std::fs::remove_dir_all(&session_dir).ok();
    }

    #[gpui_kit::test]
    fn conflict_compare_download_lifecycle_updates_workspace_state(cx: &mut TestAppContext) {
        install_test_globals(cx, "conflict-compare-lifecycle");
        let (window, channels) = open_owning_window(cx, 1);
        while channels.command_rx.try_recv().is_ok() {}

        let session_dir = std::env::temp_dir().join(format!(
            "macsftp-conflict-compare-test-{}-{}",
            std::process::id(),
            "lifecycle"
        ));
        std::fs::create_dir_all(&session_dir).expect("create test session dir");
        let temp_file = session_dir.join("conflict.txt");
        std::fs::write(&temp_file, b"local version").expect("write local version");
        let temp_path = LocalPath::new(temp_file.to_string_lossy().as_ref());
        let conflict_path = crate::workspace::conflict_remote_path(&temp_path);

        let _id = cx.update(|cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            let session = EditSession {
                id,
                remote_path: RemotePath::new("/srv/conflict.txt"),
                io_path: RemotePath::new("/srv/conflict.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                connection_key: test_connection_key(),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::RemoteConflict,
                remote_snapshot: RemoteSnapshot {
                    size: Some(20),
                    modified_at: None,
                },
                pending_check_id: None,
                checking_local_mtime: None,
                local_modified: true,
                local_baseline: None,
            };
            cx.resources_mut().edit_sessions.register(session);
            id
        });

        let transfer_id = TransferId(42);
        let job = TransferJob {
            id: transfer_id,
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/conflict.txt")),
            destination: TransferEndpoint::Local(conflict_path.clone()),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(10),
        };
        seed_transfer(cx, job);

        window
            .update(cx, |workspace, _window, _cx| {
                workspace.modal_inputs.begin_conflict_compare(_id);
            })
            .expect("update workspace");

        // Simulate successful download of remote snapshot for comparison
        cx.update(|cx| {
            dispatch_event(AppEvent::TransferCompleted { transfer_id }, cx);
        });

        window
            .read_with(cx, |workspace, _cx| {
                assert!(
                    !workspace.modal_inputs.conflict_compare_loading,
                    "completed compare download must clear loading flag"
                );
                // The test never writes the downloaded copy, so the launch is
                // refused instead of spawning a real diff tool during tests.
                assert!(
                    workspace
                        .modal_inputs
                        .conflict_compare_error
                        .as_deref()
                        .is_some_and(|error| error.contains("comparison copy")),
                    "missing comparison copy must surface an actionable error"
                );
            })
            .expect("read workspace");

        // Now test failure case
        let fail_transfer_id = TransferId(43);
        let fail_job = TransferJob {
            id: fail_transfer_id,
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/conflict.txt")),
            destination: TransferEndpoint::Local(conflict_path),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(10),
        };
        seed_transfer(cx, fail_job);

        window
            .update(cx, |workspace, _window, _cx| {
                workspace.modal_inputs.begin_conflict_compare(_id);
            })
            .expect("update workspace");

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferFailed(TransferFailure {
                    transfer_id: fail_transfer_id,
                    error: UserFacingError::new(
                        ErrorCode::NetworkError,
                        "network error",
                        "download failed",
                    ),
                }),
                cx,
            );
        });

        window
            .read_with(cx, |workspace, _cx| {
                assert!(
                    !workspace.modal_inputs.conflict_compare_loading,
                    "failed compare download must clear loading flag"
                );
                assert!(
                    workspace.modal_inputs.conflict_compare_error.is_some(),
                    "failed compare download must surface error message"
                );
            })
            .expect("read workspace");

        // Cancelling a queued compare job emits only `TransferSkipped`; the
        // modal must not stay on “Comparing…”.
        let skipped_transfer_id = TransferId(45);
        let skipped_job = TransferJob {
            id: skipped_transfer_id,
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/conflict.txt")),
            destination: TransferEndpoint::Local(crate::workspace::conflict_remote_path(
                &temp_path,
            )),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(10),
        };
        seed_transfer(cx, skipped_job);

        window
            .update(cx, |workspace, _window, _cx| {
                workspace.modal_inputs.begin_conflict_compare(_id);
            })
            .expect("update workspace");

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferSkipped {
                    transfer_id: skipped_transfer_id,
                },
                cx,
            );
        });

        window
            .read_with(cx, |workspace, _cx| {
                assert!(
                    !workspace.modal_inputs.conflict_compare_loading,
                    "skipped compare download must clear loading flag"
                );
                assert!(
                    workspace
                        .modal_inputs
                        .conflict_compare_error
                        .as_deref()
                        .is_some_and(|error| error.contains("cancelled")),
                    "skipped compare download must surface the cancellation"
                );
            })
            .expect("read workspace");

        // Planning failures never produce TransferFailed; the plan event must
        // clear the loading state on its own.
        let plan_id = TransferPlanId(7);
        let plan_conflict_path = crate::workspace::conflict_remote_path(&temp_path);
        let plan = TransferPlan {
            id: plan_id,
            root_job_id: TransferId(44),
            source_root: TransferEndpoint::Remote(RemotePath::new("/srv/conflict.txt")),
            destination_root: TransferEndpoint::Local(plan_conflict_path),
            state: TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: None,
            child_jobs: Vec::new(),
            conflict_policy: ConflictPolicy::default(),
        };
        let root_job = TransferJob {
            id: TransferId(44),
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/conflict.txt")),
            destination: plan.destination_root.clone(),
            state: TransferState::Planning,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(10),
        };
        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot { plan, root_job })),
                cx,
            );
        });

        window
            .update(cx, |workspace, _window, _cx| {
                workspace.modal_inputs.begin_conflict_compare(_id);
            })
            .expect("update workspace");

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferPlanFailed {
                    plan_id,
                    error: UserFacingError::new(
                        ErrorCode::NetworkError,
                        "planning failed",
                        "could not reach the server",
                    ),
                },
                cx,
            );
        });

        window
            .read_with(cx, |workspace, _cx| {
                assert!(
                    !workspace.modal_inputs.conflict_compare_loading,
                    "plan failure must not leave the modal stuck on Comparing…"
                );
                assert!(
                    workspace
                        .modal_inputs
                        .conflict_compare_error
                        .as_deref()
                        .is_some_and(|error| error.contains("Could not download the remote copy")),
                    "plan failure must surface the reason"
                );
            })
            .expect("read workspace");

        std::fs::remove_dir_all(&session_dir).ok();
    }
}

#[cfg(test)]
mod reload_tests {
    use super::*;
    use crate::app_actions;
    use crate::resources::{AppResources, SharedTransfers};
    use crate::workspace::Workspace;
    use gpui_kit::TestAppContext;
    use macsftp_core::{
        ConflictPolicy, ConnectionState, LocalPath, MetadataPolicy, RemotePath,
        RuntimeBridgeConfig, SessionId, Timestamp, TransferDirection, TransferEndpoint, TransferId,
        TransferJob, TransferPlan, TransferPlanId, TransferPlanSnapshot, TransferPlanState,
        TransferState, WindowSessionId,
    };
    use macsftp_sftp::{BridgeChannels, RuntimeClient};
    use macsftp_storage::ConfigStore;

    fn install_globals(cx: &mut TestAppContext, label: &str) {
        let home =
            std::env::temp_dir().join(format!("macsftp-reload-{label}-{}", std::process::id()));
        let app_paths = macsftp_platform::AppPaths::from_home_dir(home.to_string_lossy().as_ref());
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        cx.update(|cx| {
            macsftp_ui::Theme::dark().install(cx);
            app_actions::init(cx);
            cx.set_global(AppResources::load_for_test(app_paths, config));
            cx.set_global(SharedTransfers::default());
        });
    }

    /// A connected window showing `/srv` on the server and `/tmp/dest` locally.
    fn connected_window(
        cx: &mut TestAppContext,
    ) -> (gpui_kit::WindowHandle<Workspace>, BridgeChannels) {
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
                tab.local.path = Some(LocalPath::new("/tmp/dest"));
            })
            .expect("seed pane paths");
        (window, channels)
    }

    fn start_plan(cx: &mut TestAppContext, root_job: TransferJob) {
        let plan = TransferPlan {
            id: TransferPlanId(root_job.id.0),
            root_job_id: root_job.id,
            source_root: root_job.source.clone(),
            destination_root: root_job.destination.clone(),
            state: TransferPlanState::Planning,
            planned_count: 0,
            total_bytes: None,
            child_jobs: Vec::new(),
            conflict_policy: root_job.conflict_policy.clone(),
        };
        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot { plan, root_job })),
                cx,
            );
        });
    }

    /// Publish one child job for `plan_id`: the shape a real planner emits
    /// before any bytes move. Returns the child's id.
    fn add_child(
        cx: &mut TestAppContext,
        plan_id: TransferPlanId,
        index: u64,
        direction: TransferDirection,
        source: TransferEndpoint,
        destination: TransferEndpoint,
    ) -> TransferId {
        let child = TransferJob {
            id: TransferId(plan_id.0 * 100 + index),
            direction,
            source,
            destination,
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        };
        let id = child.id;
        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferPlanProgress(macsftp_core::TransferPlanProgress {
                    plan_id,
                    child_jobs: vec![child],
                    planned_count: 1,
                    total_bytes: None,
                }),
                cx,
            );
        });
        id
    }

    fn finished_transfer(cx: &mut TestAppContext, transfer_id: TransferId) {
        cx.update(|cx| dispatch_event(AppEvent::TransferCompleted { transfer_id }, cx));
        cx.run_until_parked();
        cx.run_until_parked();
    }

    /// A finished download reloads the local pane that received the file, so
    /// the new name appears without the user pressing ⌘R.
    #[gpui_kit::test]
    fn a_finished_download_reloads_the_local_pane(cx: &mut TestAppContext) {
        install_globals(cx, "download");
        let destination_dir =
            std::env::temp_dir().join(format!("macsftp-reload-dest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&destination_dir);
        std::fs::create_dir_all(&destination_dir).expect("destination fixture");
        let destination = LocalPath::new(destination_dir.to_string_lossy().into_owned());

        let (window, channels) = connected_window(cx);
        window
            .update(cx, |workspace, _window, _cx| {
                workspace.active_tab_mut().expect("tab").local.path = Some(destination.clone());
            })
            .expect("point the local pane at the fixture");
        start_plan(
            cx,
            TransferJob {
                id: TransferId(1),
                direction: TransferDirection::Download,
                source: TransferEndpoint::Remote(RemotePath::new("/srv/downloaded.txt")),
                destination: TransferEndpoint::Local(destination.join("downloaded.txt")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        // The single-file download plan publishes no children; the file the
        // runtime just wrote is simulated here.
        std::fs::write(destination_dir.join("downloaded.txt"), b"data").expect("write download");
        while channels.command_rx.try_recv().is_ok() {}

        finished_transfer(cx, TransferId(1));

        window
            .read_with(cx, |workspace, _cx| {
                let names: Vec<&str> = workspace
                    .active_tab()
                    .expect("tab")
                    .local
                    .entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect();
                assert_eq!(
                    names,
                    vec!["downloaded.txt"],
                    "the local pane lists the file the transfer wrote"
                );
            })
            .expect("window is open");
        let _ = std::fs::remove_dir_all(&destination_dir);
    }

    /// A finished upload asks the runtime to re-read the remote folder that
    /// received the file.
    #[gpui_kit::test]
    fn a_finished_upload_reloads_the_remote_pane(cx: &mut TestAppContext) {
        install_globals(cx, "upload");
        let (_window, channels) = connected_window(cx);
        let plan_id = TransferPlanId(2);
        start_plan(
            cx,
            TransferJob {
                id: TransferId(2),
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/dest")),
                destination: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        let child = add_child(
            cx,
            plan_id,
            0,
            TransferDirection::Upload,
            TransferEndpoint::Local(LocalPath::new("/tmp/dest/a.txt")),
            TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
        );
        while channels.command_rx.try_recv().is_ok() {}

        finished_transfer(cx, child);

        let mut reads = Vec::new();
        while let Ok(command) = channels.command_rx.try_recv() {
            if let AppCommand::ReadRemoteDir { path, .. } = command {
                reads.push(path);
            }
        }
        assert_eq!(
            reads,
            vec![RemotePath::new("/srv")],
            "the upload's destination folder is re-read exactly once"
        );
    }

    /// A finished download of an executable marks the file so macOS asks
    /// before running it. The download preserved the remote execute bit, and
    /// nothing else would put a quarantine flag on an SFTP download.
    #[gpui_kit::test]
    fn a_downloaded_executable_is_marked_for_launch_services(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;

        install_globals(cx, "quarantine-download");
        let dir =
            std::env::temp_dir().join(format!("macsftp-quarantine-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        let script = dir.join("run.command");
        std::fs::write(&script, b"#!/bin/sh\necho hi\n").expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        let destination = LocalPath::new(script.to_string_lossy().into_owned());

        let plan_id = TransferPlanId(21);
        start_plan(
            cx,
            TransferJob {
                id: TransferId(21),
                direction: TransferDirection::Download,
                source: TransferEndpoint::Remote(RemotePath::new("/srv/run.command")),
                destination: TransferEndpoint::Local(destination.clone()),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        let _ = plan_id;

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferCompleted {
                    transfer_id: TransferId(21),
                },
                cx,
            );
        });
        // The marking runs on the background executor.
        cx.run_until_parked();
        cx.run_until_parked();

        let status = std::process::Command::new("/usr/bin/xattr")
            .arg("-p")
            .arg("com.apple.quarantine")
            .arg(destination.as_str())
            .output()
            .expect("query the attribute");
        assert!(
            status.status.success(),
            "the downloaded executable is quarantined: {:?}",
            String::from_utf8_lossy(&status.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Remote-edit downloads are opened by the editor flow, not by the user
    /// clicking a listing, so they are not quarantined: a Gatekeeper prompt in
    /// front of every "open for editing" would be noise with no safety gain.
    #[gpui_kit::test]
    fn a_remote_edit_download_is_not_quarantined(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;

        install_globals(cx, "quarantine-edit");
        let edits_dir = cx.update(|cx| cx.resources().app_paths.edits_dir.clone());
        let session_dir = std::path::PathBuf::from(edits_dir.as_str()).join("test-edit-run");
        let _ = std::fs::remove_dir_all(&session_dir);
        std::fs::create_dir_all(&session_dir).expect("edit session dir");
        let script = session_dir.join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\n").expect("write script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod script");
        let destination = LocalPath::new(script.to_string_lossy().into_owned());

        start_plan(
            cx,
            TransferJob {
                id: TransferId(31),
                direction: TransferDirection::Download,
                source: TransferEndpoint::Remote(RemotePath::new("/srv/run.sh")),
                destination: TransferEndpoint::Local(destination.clone()),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferCompleted {
                    transfer_id: TransferId(31),
                },
                cx,
            );
        });
        cx.run_until_parked();
        cx.run_until_parked();

        let status = std::process::Command::new("/usr/bin/xattr")
            .arg("-p")
            .arg("com.apple.quarantine")
            .arg(destination.as_str())
            .output()
            .expect("query the attribute");
        assert!(
            !status.status.success(),
            "an edit download keeps no quarantine flag"
        );
        let _ = std::fs::remove_dir_all(&session_dir);
    }

    /// A tab in the background is reloaded too: coming back to a stale listing
    /// after a transfer finished is the exact confusion the reload prevents.
    #[gpui_kit::test]
    fn a_background_tab_showing_the_destination_is_reloaded(cx: &mut TestAppContext) {
        install_globals(cx, "reload-background");
        let dir =
            std::env::temp_dir().join(format!("macsftp-reload-background-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        std::fs::write(dir.join("landed.txt"), b"x").expect("write download");
        let destination_dir = LocalPath::new(dir.to_string_lossy().into_owned());

        let (window, channels) = connected_window(cx);
        // A second tab showing the folder the download lands in, then focus the
        // first tab so the second one is in the background.
        let background_tab = window
            .update(cx, |workspace, _window, cx| {
                let tab_id = cx.resources().next_tab_id();
                let mut tab = macsftp_core::TabState::new(tab_id, "background");
                tab.local.path = Some(destination_dir.clone());
                workspace.open_tab_for_test(tab);
                tab_id
            })
            .expect("add the background tab");

        start_plan(
            cx,
            TransferJob {
                id: TransferId(41),
                direction: TransferDirection::Download,
                source: TransferEndpoint::Remote(RemotePath::new("/srv/landed.txt")),
                destination: TransferEndpoint::Local(destination_dir.join("landed.txt")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        while channels.command_rx.try_recv().is_ok() {}

        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferCompleted {
                    transfer_id: TransferId(41),
                },
                cx,
            );
        });
        cx.run_until_parked();
        cx.run_until_parked();

        window
            .read_with(cx, |workspace, _cx| {
                let tab = workspace
                    .tab_for_test(background_tab)
                    .expect("background tab");
                assert!(
                    tab.local
                        .entries
                        .iter()
                        .any(|entry| entry.name == "landed.txt"),
                    "the background tab lists the file the transfer wrote"
                );
            })
            .expect("window is open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reducer records the plan's terminal transition, and that record is
    /// what the reload keys on: re-deriving it per completion would make a
    /// directory transfer quadratic.
    #[gpui_kit::test]
    fn a_plan_completion_is_recorded_when_its_last_job_lands(cx: &mut TestAppContext) {
        install_globals(cx, "plan-finished");
        let plan_id = TransferPlanId(11);
        start_plan(
            cx,
            TransferJob {
                id: TransferId(11),
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/dest")),
                destination: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        let child = add_child(
            cx,
            plan_id,
            0,
            TransferDirection::Upload,
            TransferEndpoint::Local(LocalPath::new("/tmp/dest/a.txt")),
            TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
        );
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                cx.transfers().just_finished_plan,
                None,
                "publishing a child does not finish the plan"
            );
        });

        cx.update(|cx| {
            dispatch_event(AppEvent::TransferCompleted { transfer_id: child }, cx);
        });
        cx.read(|cx| {
            assert_eq!(
                cx.transfers().just_finished_plan,
                Some(plan_id),
                "the last child finishing is recorded as the plan's completion"
            );
        });
    }

    /// A plan with many children reloads the pane exactly once, when the last
    /// child lands. Reloading on every child's completion would issue one
    /// directory read per transferred file.
    #[gpui_kit::test]
    fn a_multi_file_plan_reloads_the_pane_only_once(cx: &mut TestAppContext) {
        install_globals(cx, "reload-once");
        let (_window, channels) = connected_window(cx);
        let plan_id = TransferPlanId(9);
        start_plan(
            cx,
            TransferJob {
                id: TransferId(9),
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/dest")),
                destination: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        let children: Vec<TransferId> = (0..5)
            .map(|index| {
                add_child(
                    cx,
                    plan_id,
                    index as u64,
                    TransferDirection::Upload,
                    TransferEndpoint::Local(LocalPath::new(format!("/tmp/dest/{index}.txt"))),
                    TransferEndpoint::Remote(RemotePath::new(format!("/srv/{index}.txt"))),
                )
            })
            .collect();
        while channels.command_rx.try_recv().is_ok() {}

        // Four of five children land: the plan is still running, so the pane
        // must not be re-read yet.
        for child in &children[..4] {
            cx.update(|cx| {
                dispatch_event(
                    AppEvent::TransferCompleted {
                        transfer_id: *child,
                    },
                    cx,
                );
            });
        }
        cx.run_until_parked();
        assert!(
            channels.command_rx.try_recv().is_err(),
            "a running plan must not re-read the directory"
        );

        // The last child finishes the plan; now the pane is re-read, once.
        cx.update(|cx| {
            dispatch_event(
                AppEvent::TransferCompleted {
                    transfer_id: children[4],
                },
                cx,
            );
        });
        cx.run_until_parked();
        let mut reads = 0;
        while let Ok(command) = channels.command_rx.try_recv() {
            if matches!(command, AppCommand::ReadRemoteDir { .. }) {
                reads += 1;
            }
        }
        assert_eq!(reads, 1, "the finished plan re-reads the pane exactly once");
    }

    /// A plan for a folder elsewhere must not yank a pane that is showing a
    /// different directory.
    #[gpui_kit::test]
    fn a_finished_upload_leaves_panes_on_other_folders_alone(cx: &mut TestAppContext) {
        install_globals(cx, "elsewhere");
        let (_window, channels) = connected_window(cx);
        let plan_id = TransferPlanId(3);
        start_plan(
            cx,
            TransferJob {
                id: TransferId(3),
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/dest")),
                destination: TransferEndpoint::Remote(RemotePath::new("/elsewhere")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(1),
            },
        );
        let child = add_child(
            cx,
            plan_id,
            0,
            TransferDirection::Upload,
            TransferEndpoint::Local(LocalPath::new("/tmp/dest/a.txt")),
            TransferEndpoint::Remote(RemotePath::new("/elsewhere/a.txt")),
        );
        while channels.command_rx.try_recv().is_ok() {}

        finished_transfer(cx, child);

        assert!(
            channels.command_rx.try_recv().is_err(),
            "the remote pane shows /srv, so /elsewhere is not re-read"
        );
    }
}
