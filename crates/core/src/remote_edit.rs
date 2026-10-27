//! Remote-edit lifecycle: sessions, phase transitions, cleanup and effects.
//!
//! Every transition lives here. Callers express intent ([`RemoteEditLifecycle::begin`],
//! [`request_upload`](RemoteEditLifecycle::request_upload),
//! [`on_check_result`](RemoteEditLifecycle::on_check_result), ...) and execute the
//! returned [`EditEffect`]s. The lifecycle performs no filesystem I/O and holds no
//! UI/runtime types; local facts and transfer endpoints are passed in by the app.

use std::time::SystemTime;

use crate::{
    AppCommand, CheckRemoteEditSnapshotCommand, ConflictPolicy, ConnectionKey, EditCheckId,
    EditPhase, EditSession, EditSessionId, EditSessionStore, LocalBaseline, LocalPath,
    MetadataPolicy, ProfileId, RemotePath, RemoteSnapshot, StartTransferCommand, TabId, Timestamp,
    TransferDirection, TransferEndpoint, is_bidi_or_control,
};

/// An edit the user asked to open, before the download starts. Also the payload
/// of the large-file confirmation modal.
#[derive(Debug, Clone)]
pub struct PendingEdit {
    /// The path the user opened, as the listing showed it (UI identity).
    pub remote_path: RemotePath,
    /// The write-through path every transfer and check must use: the resolved
    /// target for a file symlink, otherwise `remote_path`.
    pub io_path: RemotePath,
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
    pub session_epoch: u64,
    /// Identity of the physical connection the edit was opened through; used
    /// for dedup only. The transfer commands carry `profile_id` instead.
    pub connection_key: ConnectionKey,
    pub tab_id: TabId,
    pub profile_id: ProfileId,
}

/// How the user chose to resolve a remote-changed-under-edit conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Force the local temp file back over the (changed) remote.
    Overwrite,
    /// Throw away local edits and re-download the current remote.
    DiscardLocal,
    /// Dismiss for now; the session returns to `Editing`.
    Later,
}

/// User-facing outcome of a lifecycle transition. The app maps these to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditNotice {
    /// `begin` found a live `Downloading` session for this file.
    StillDownloading,
    /// `begin` found a live `CheckingRemote` session for this file.
    CheckingServerFile,
    /// `begin` found a live `RemoteConflict` session for this file.
    ReviewConflictBeforeReopen,
    /// A re-pointed symlink has a live edit bound to its old target.
    LinkRepointed { conflict: bool },
    /// The requested phase transition is not allowed from this phase.
    PhaseBlocked(EditPhase),
    /// The live tab reached a different server than the edit session.
    WrongConnection,
    /// The local temp file could not be stat'ed before a check.
    CopyUnreadable,
    /// The temp file changed while the remote check was in flight.
    LocalChangedDuringCheck,
    /// The authoritative remote check failed or could not be dispatched.
    ServerCheckFailed,
    /// The upload command could not be dispatched.
    UploadStartFailed,
    /// The initial download failed; the session was removed.
    DownloadFailed { file_name: String },
    /// The edited copy is open in an editor.
    CopyOpen,
    /// The editor could not be launched for the edited copy.
    CopyOpenFailed,
    /// An explicit upload completed.
    Uploaded,
    /// An explicit upload failed; the temp copy is kept for retry.
    UploadFailed,
    /// The file name cannot be used for a temp path.
    UnsafeFileName,
}

/// A side effect produced by a transition, executed by `macsftp-app`.
///
/// Returned by an intent and consumed immediately; a few hundred bytes per
/// effect do not justify boxing the command at every construction site.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditEffect {
    /// Send a runtime command. On failure the executor applies `rollback`.
    Dispatch {
        tab_id: TabId,
        session_id: EditSessionId,
        command: AppCommand,
        rollback: DispatchRollback,
    },
    /// Launch the configured editor on the temp copy.
    OpenEditor { tab_id: TabId, temp_path: LocalPath },
    /// Best-effort removal of a session temp directory.
    DeleteTempDir { path: LocalPath },
    /// Keep the owning tab's listing entry in step with a rebased snapshot.
    RebaseListing {
        tab_id: TabId,
        path: RemotePath,
        snapshot: RemoteSnapshot,
    },
    /// Surface a status message in the window owning `tab_id`.
    Notice { tab_id: TabId, notice: EditNotice },
}

/// What to undo when a dispatched command never enters the runtime channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchRollback {
    /// Upload authorization check could not start: back to `Editing`.
    RevertCheck,
    /// Conflict overwrite could not start: back to `RemoteConflict`.
    RestoreConflict,
    /// Download could not start: remove the `Downloading` session and its temp.
    AbortDownload,
}

/// Result of asking to begin (or reopen) an edit.
///
/// Consumed by the caller in the same turn; see [`EditEffect`] on the size.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum BeginEdit {
    /// No live session owns the file. Execute `effects` (cleanup of a stale
    /// session that was reaped), then show the size warning or call
    /// [`RemoteEditLifecycle::start_download`].
    Start {
        pending: PendingEdit,
        effects: Vec<EditEffect>,
    },
    /// The request was handled (reopened editor or blocked); execute the
    /// effects and stop.
    Handled(Vec<EditEffect>),
}

/// A registered `Downloading` session and the command that starts its fetch.
#[derive(Debug, Clone)]
pub struct StartedEdit {
    pub session_id: EditSessionId,
    pub temp_path: LocalPath,
    pub command: AppCommand,
}

/// Result of requesting an explicit upload.
#[derive(Debug, Clone)]
pub enum UploadRequest {
    /// The session is in `CheckingRemote`; send this command.
    Check(CheckRemoteEditSnapshotCommand),
    /// The request was refused; surface the notice.
    Rejected(EditNotice),
}

/// One authoritative remote-metadata check result, decoded by the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditCheckResult {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub outcome: EditCheckOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditCheckOutcome {
    Matched(RemoteSnapshot),
    Failed,
}

/// A finished transfer, correlated against edit sessions by the lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditTransferFinished {
    pub direction: TransferDirection,
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    pub succeeded: bool,
}

/// Local temp-file facts read by the app at the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalFileFacts {
    pub size: u64,
    pub mtime: SystemTime,
}

/// Process-wide owner of remote-edit sessions and every phase transition.
#[derive(Debug)]
pub struct RemoteEditLifecycle {
    store: EditSessionStore,
    edits_dir: LocalPath,
    run_id: String,
}

impl RemoteEditLifecycle {
    pub fn new(edits_dir: LocalPath, run_id: String) -> Self {
        Self {
            store: EditSessionStore::new(),
            edits_dir,
            run_id,
        }
    }

    /// Decide whether a new edit may start for `pending`. Reaps sessions whose
    /// owning tab is gone, refuses a re-pointed symlink that still has a live
    /// edit, reopens an editor for a live session, or hands the edit back for
    /// download.
    pub fn begin(
        &mut self,
        pending: PendingEdit,
        is_tab_live: impl Fn(TabId) -> bool,
    ) -> BeginEdit {
        let mut reaped = Vec::new();
        // A link re-pointed after its session was created would otherwise
        // produce two live sessions sharing one `remote_path`.
        if let Some((existing_tab_id, existing_io_path, existing_phase)) = self
            .store
            .find_active_for_remote_path(&pending.connection_key, &pending.remote_path)
            .map(|session| {
                (
                    session.tab_id,
                    session.io_path.clone(),
                    session.phase.clone(),
                )
            })
            && existing_io_path != pending.io_path
        {
            if is_tab_live(existing_tab_id) {
                return BeginEdit::Handled(vec![EditEffect::Notice {
                    tab_id: pending.tab_id,
                    notice: EditNotice::LinkRepointed {
                        conflict: existing_phase == EditPhase::RemoteConflict,
                    },
                }]);
            }
            // The old session's tab is gone, so it cannot make progress; reap
            // it and let this request continue against the new target.
            reaped.extend(self.reap_tab(existing_tab_id));
        }

        // Deduplicate a genuinely live edit, but never let a session whose
        // owning tab disappeared block this path forever.
        if let Some((existing_tab_id, existing_phase, existing_temp_path)) = self
            .store
            .find_active(&pending.connection_key, &pending.io_path)
            .map(|session| {
                (
                    session.tab_id,
                    session.phase.clone(),
                    session.local_temp_path.clone(),
                )
            })
        {
            if is_tab_live(existing_tab_id) {
                let notice = match existing_phase {
                    EditPhase::Editing | EditPhase::UploadingBack => {
                        return BeginEdit::Handled(vec![EditEffect::OpenEditor {
                            tab_id: existing_tab_id,
                            temp_path: existing_temp_path,
                        }]);
                    }
                    EditPhase::Downloading => EditNotice::StillDownloading,
                    EditPhase::CheckingRemote => EditNotice::CheckingServerFile,
                    EditPhase::RemoteConflict => EditNotice::ReviewConflictBeforeReopen,
                };
                return BeginEdit::Handled(vec![EditEffect::Notice {
                    tab_id: pending.tab_id,
                    notice,
                }]);
            }
            reaped.extend(self.reap_tab(existing_tab_id));
        }

        BeginEdit::Start {
            pending,
            effects: reaped,
        }
    }

    /// Register a `Downloading` session and build its initial fetch command.
    pub fn start_download(&mut self, pending: PendingEdit) -> Result<StartedEdit, EditNotice> {
        let file_name = safe_file_name(&pending.remote_path).ok_or(EditNotice::UnsafeFileName)?;
        let id = self.store.next_id();
        let temp_path = LocalPath::new(format!(
            "{}/{}/{}/{}",
            self.edits_dir.as_str(),
            self.run_id,
            id.0,
            file_name
        ));
        // The download worker creates the parent directory itself and reports
        // creation failures as transfer errors, so no synchronous FS call
        // belongs in this UI path.
        self.store.register(EditSession {
            id,
            remote_path: pending.remote_path.clone(),
            io_path: pending.io_path.clone(),
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            connection_key: pending.connection_key,
            local_temp_path: temp_path.clone(),
            phase: EditPhase::Downloading,
            remote_snapshot: RemoteSnapshot {
                size: pending.size,
                modified_at: pending.modified_at,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        let command = AppCommand::StartTransfer(StartTransferCommand {
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(pending.io_path)],
            destination: TransferEndpoint::Local(temp_path.clone()),
            metadata_policy: MetadataPolicy::default(),
            // An edit temp is always ours to overwrite; a download must never
            // prompt the user.
            conflict_policy: ConflictPolicy::OverwriteAll,
        });
        Ok(StartedEdit {
            session_id: id,
            temp_path,
            command,
        })
    }

    /// Undo a download whose command never entered the channel.
    pub fn abort_download(&mut self, id: EditSessionId) -> Vec<EditEffect> {
        let Some(session) = self.store.get(id) else {
            return Vec::new();
        };
        if session.phase != EditPhase::Downloading {
            return Vec::new();
        }
        let path = session.local_temp_path.clone();
        self.store.remove(id);
        vec![EditEffect::DeleteTempDir { path }]
    }

    /// Move an `Editing` session to `CheckingRemote` and return the check
    /// command. The caller reads the local mtime first and sends the command.
    pub fn request_upload(
        &mut self,
        id: EditSessionId,
        session_epoch: u64,
        connection_key: &ConnectionKey,
        checking_local_mtime: Timestamp,
    ) -> UploadRequest {
        let check_id = self.store.next_check_id();
        let Some(session) = self.store.get_mut(id) else {
            return UploadRequest::Rejected(EditNotice::ServerCheckFailed);
        };
        if session.phase != EditPhase::Editing {
            return UploadRequest::Rejected(EditNotice::PhaseBlocked(session.phase.clone()));
        }
        if session.connection_key != *connection_key {
            return UploadRequest::Rejected(EditNotice::WrongConnection);
        }
        session.phase = EditPhase::CheckingRemote;
        session.session_epoch = session_epoch;
        session.pending_check_id = Some(check_id);
        session.checking_local_mtime = Some(checking_local_mtime);
        UploadRequest::Check(CheckRemoteEditSnapshotCommand {
            tab_id: session.tab_id,
            session_epoch,
            edit_session_id: id,
            check_id,
            path: session.io_path.clone(),
        })
    }

    /// Return a `CheckingRemote` session to `Editing` without uploading.
    pub fn revert_check(&mut self, id: EditSessionId) {
        if let Some(session) = self.store.get_mut(id) {
            session.phase = EditPhase::Editing;
            session.pending_check_id = None;
            session.checking_local_mtime = None;
        }
    }

    /// Apply one authoritative check result. Every correlation is re-validated
    /// here: session, tab, epoch, check id, write-through path, phase, and the
    /// local mtime captured at dispatch (re-read by the caller as
    /// `current_local_mtime`).
    pub fn on_check_result(
        &mut self,
        result: EditCheckResult,
        current_local_mtime: Option<Timestamp>,
    ) -> Vec<EditEffect> {
        let Some(session) = self.store.get(result.session_id) else {
            return Vec::new();
        };
        let applicable = session.phase == EditPhase::CheckingRemote
            && session.pending_check_id == Some(result.check_id)
            && session.checking_local_mtime.is_some()
            && session.io_path == result.path
            && result.tab_id == session.tab_id
            && result.session_epoch == session.session_epoch;
        if !applicable {
            return Vec::new();
        }
        let tab_id = session.tab_id;
        let baseline = session.remote_snapshot;
        let checking_local_mtime = session.checking_local_mtime;

        if current_local_mtime != checking_local_mtime {
            self.revert_check(result.session_id);
            return vec![EditEffect::Notice {
                tab_id,
                notice: EditNotice::LocalChangedDuringCheck,
            }];
        }

        match result.outcome {
            EditCheckOutcome::Matched(snapshot) => {
                if snapshot == baseline {
                    let Some(session) = self.store.get_mut(result.session_id) else {
                        return Vec::new();
                    };
                    let command = build_edit_upload_command(
                        &session.local_temp_path,
                        &session.io_path,
                        session.session_epoch,
                        session.profile_id,
                        session.tab_id,
                    );
                    session.phase = EditPhase::UploadingBack;
                    session.pending_check_id = None;
                    session.checking_local_mtime = None;
                    vec![EditEffect::Dispatch {
                        tab_id,
                        session_id: result.session_id,
                        command,
                        rollback: DispatchRollback::RevertCheck,
                    }]
                } else {
                    if let Some(session) = self.store.get_mut(result.session_id) {
                        session.phase = EditPhase::RemoteConflict;
                        session.pending_check_id = None;
                        session.checking_local_mtime = None;
                    }
                    Vec::new()
                }
            }
            EditCheckOutcome::Failed => {
                self.revert_check(result.session_id);
                vec![EditEffect::Notice {
                    tab_id,
                    notice: EditNotice::ServerCheckFailed,
                }]
            }
        }
    }

    /// Advance a `Downloading` or `UploadingBack` session whose transfer just
    /// finished. `local_facts` is the temp file's `(size, mtime)` where the app
    /// could read it.
    pub fn on_transfer_finished(
        &mut self,
        finished: EditTransferFinished,
        local_facts: Option<LocalFileFacts>,
    ) -> Vec<EditEffect> {
        let temp_path = match (&finished.source, &finished.destination) {
            (_, TransferEndpoint::Local(path)) => path.clone(),
            (TransferEndpoint::Local(path), _) => path.clone(),
            _ => return Vec::new(),
        };
        let Some((session_id, phase, tab_id, io_path, remote_path)) =
            self.store.find_by_temp_path(&temp_path).map(|session| {
                (
                    session.id,
                    session.phase.clone(),
                    session.tab_id,
                    session.io_path.clone(),
                    session.remote_path.clone(),
                )
            })
        else {
            return Vec::new();
        };
        let matches_phase = match phase {
            EditPhase::Downloading => {
                finished.direction == TransferDirection::Download
                    && matches!(&finished.source, TransferEndpoint::Remote(remote) if remote == &io_path)
            }
            EditPhase::UploadingBack => {
                finished.direction == TransferDirection::Upload
                    && matches!(&finished.destination, TransferEndpoint::Remote(remote) if remote == &io_path)
            }
            _ => false,
        };
        if !matches_phase {
            return Vec::new();
        }

        match phase {
            EditPhase::Downloading if !finished.succeeded => {
                self.store.remove(session_id);
                vec![
                    EditEffect::DeleteTempDir { path: temp_path },
                    EditEffect::Notice {
                        tab_id,
                        notice: EditNotice::DownloadFailed {
                            file_name: file_name_of(remote_path.as_str()),
                        },
                    },
                ]
            }
            EditPhase::Downloading => {
                if let Some(session) = self.store.get_mut(session_id) {
                    session.phase = EditPhase::Editing;
                    session.local_modified = false;
                    session.local_baseline = local_facts.map(|facts| LocalBaseline {
                        size: facts.size,
                        mtime: facts.mtime,
                    });
                }
                vec![EditEffect::OpenEditor { tab_id, temp_path }]
            }
            EditPhase::UploadingBack => {
                let refreshed = finished.succeeded.then(|| {
                    local_facts.map(|facts| {
                        RemoteSnapshot {
                            size: Some(facts.size),
                            modified_at: Some(Timestamp::from_system_time(facts.mtime)),
                        }
                        .truncated_to_secs()
                    })
                });
                if let Some(session) = self.store.get_mut(session_id) {
                    session.phase = EditPhase::Editing;
                    if finished.succeeded {
                        session.local_modified = false;
                        session.local_baseline = local_facts.map(|facts| LocalBaseline {
                            size: facts.size,
                            mtime: facts.mtime,
                        });
                    }
                    if let Some(Some(snapshot)) = refreshed {
                        session.remote_snapshot = snapshot;
                    }
                }
                let mut effects = Vec::new();
                if let Some(Some(snapshot)) = refreshed {
                    effects.push(EditEffect::RebaseListing {
                        tab_id,
                        path: io_path,
                        snapshot,
                    });
                }
                effects.push(EditEffect::Notice {
                    tab_id,
                    notice: if finished.succeeded {
                        EditNotice::Uploaded
                    } else {
                        EditNotice::UploadFailed
                    },
                });
                effects
            }
            _ => Vec::new(),
        }
    }

    /// Resolve a `RemoteConflict` session. Double clicks are no-ops because the
    /// phase no longer matches after the first call.
    pub fn resolve_conflict(
        &mut self,
        id: EditSessionId,
        choice: ConflictChoice,
    ) -> Vec<EditEffect> {
        let Some(session) = self.store.get(id) else {
            return Vec::new();
        };
        if session.phase != EditPhase::RemoteConflict {
            return Vec::new();
        }
        match choice {
            ConflictChoice::Overwrite => {
                let command = build_edit_upload_command(
                    &session.local_temp_path,
                    &session.io_path,
                    session.session_epoch,
                    session.profile_id,
                    session.tab_id,
                );
                let tab_id = session.tab_id;
                if let Some(session) = self.store.get_mut(id) {
                    session.phase = EditPhase::UploadingBack;
                }
                vec![EditEffect::Dispatch {
                    tab_id,
                    session_id: id,
                    command,
                    rollback: DispatchRollback::RestoreConflict,
                }]
            }
            ConflictChoice::DiscardLocal => {
                let pending = PendingEdit {
                    remote_path: session.remote_path.clone(),
                    io_path: session.io_path.clone(),
                    size: session.remote_snapshot.size,
                    modified_at: session.remote_snapshot.modified_at,
                    session_epoch: session.session_epoch,
                    connection_key: session.connection_key.clone(),
                    tab_id: session.tab_id,
                    profile_id: session.profile_id,
                };
                let old_temp = session.local_temp_path.clone();
                let tab_id = session.tab_id;
                self.store.remove(id);
                let mut effects = vec![EditEffect::DeleteTempDir { path: old_temp }];
                match self.start_download(pending) {
                    Ok(started) => effects.push(EditEffect::Dispatch {
                        tab_id,
                        session_id: started.session_id,
                        command: started.command,
                        rollback: DispatchRollback::AbortDownload,
                    }),
                    Err(notice) => effects.push(EditEffect::Notice { tab_id, notice }),
                }
                effects
            }
            ConflictChoice::Later => {
                if let Some(session) = self.store.get_mut(id) {
                    session.phase = EditPhase::Editing;
                }
                Vec::new()
            }
        }
    }

    /// Build the always-fresh comparison download for a conflict session.
    /// `profile_id` is the live tab's value, used for transfer metadata.
    pub fn plan_conflict_compare(
        &self,
        id: EditSessionId,
        profile_id: ProfileId,
    ) -> Option<AppCommand> {
        let session = self.store.get(id)?;
        if session.phase != EditPhase::RemoteConflict {
            return None;
        }
        Some(AppCommand::StartTransfer(StartTransferCommand {
            tab_id: session.tab_id,
            session_epoch: session.session_epoch,
            profile_id,
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(session.io_path.clone())],
            destination: TransferEndpoint::Local(conflict_remote_path(&session.local_temp_path)),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
        }))
    }

    /// Undo a dispatched command that never reached the runtime.
    pub fn dispatch_failed(
        &mut self,
        id: EditSessionId,
        rollback: DispatchRollback,
    ) -> Vec<EditEffect> {
        match rollback {
            DispatchRollback::RevertCheck => {
                self.revert_check(id);
                Vec::new()
            }
            DispatchRollback::RestoreConflict => {
                if let Some(session) = self.store.get_mut(id)
                    && session.phase == EditPhase::UploadingBack
                {
                    session.phase = EditPhase::RemoteConflict;
                }
                Vec::new()
            }
            DispatchRollback::AbortDownload => self.abort_download(id),
        }
    }

    /// A watcher event for a temp path: mark the session dirty unless the
    /// metadata still equals the baseline we established from our own writes.
    /// `facts` is `None` when the app could not stat the file; it is then
    /// treated as modified rather than silently suppressed. Returns whether a
    /// redraw is needed.
    pub fn record_local_modified(
        &mut self,
        id: EditSessionId,
        temp_path: &LocalPath,
        facts: Option<LocalFileFacts>,
    ) -> bool {
        let Some(session) = self.store.get_mut(id) else {
            return false;
        };
        if session.phase != EditPhase::Editing || &session.local_temp_path != temp_path {
            return false;
        }
        if let Some(facts) = facts
            && session.local_baseline.as_ref().is_some_and(|baseline| {
                baseline.size == facts.size && baseline.mtime == facts.mtime
            })
        {
            return false;
        }
        session.local_modified = true;
        true
    }

    /// Adopt benign listing metadata for `Editing` sessions on `tab_id`.
    ///
    /// Only sessions whose write-through path equals the listed path adopt,
    /// and only when the listing agrees with the baseline at whole-second
    /// granularity. A real change at that granularity is left for the next
    /// explicit upload check. `RemoteConflict` is never touched.
    pub fn adopt_listing_baselines(
        &mut self,
        tab_id: TabId,
        listing: &[(RemotePath, RemoteSnapshot)],
    ) {
        let adoptions: Vec<(EditSessionId, RemoteSnapshot)> = self
            .store
            .editing_sessions_for_tab(tab_id)
            .filter_map(|session| {
                if session.io_path != session.remote_path {
                    return None;
                }
                let (_, listed) = listing
                    .iter()
                    .find(|(path, _)| path == &session.remote_path)?;
                session
                    .remote_snapshot
                    .agrees_at_second_granularity(*listed)
                    .then_some((session.id, *listed))
            })
            .collect();
        for (id, snapshot) in adoptions {
            if let Some(session) = self.store.get_mut(id) {
                session.remote_snapshot = snapshot;
            }
        }
    }

    /// Refresh the epoch of every session on `tab_id`; a check in flight at
    /// reconnect time can never complete, so it returns to `Editing`.
    pub fn update_epoch_for_tab(&mut self, tab_id: TabId, session_epoch: u64) {
        self.store.update_epoch_for_tab(tab_id, session_epoch);
    }

    /// Remove every session on `tab_id`, returning the removed sessions so the
    /// caller can delete their temp directories.
    pub fn remove_for_tab(&mut self, tab_id: TabId) -> Vec<EditSession> {
        self.store.remove_for_tab(tab_id)
    }

    fn reap_tab(&mut self, tab_id: TabId) -> Vec<EditEffect> {
        self.store
            .remove_for_tab(tab_id)
            .into_iter()
            .map(|session| EditEffect::DeleteTempDir {
                path: session.local_temp_path,
            })
            .collect()
    }

    pub fn session(&self, id: EditSessionId) -> Option<&EditSession> {
        self.store.get(id)
    }

    /// The live session (any phase) bound to this file through `connection_key`.
    pub fn find_active(
        &self,
        connection_key: &ConnectionKey,
        io_path: &RemotePath,
    ) -> Option<&EditSession> {
        self.store.find_active(connection_key, io_path)
    }

    /// Every `Editing` session owned by `tab_id`.
    pub fn editing_sessions_for_tab(&self, tab_id: TabId) -> impl Iterator<Item = &EditSession> {
        self.store.editing_sessions_for_tab(tab_id)
    }

    pub fn find_by_temp_path(&self, path: &LocalPath) -> Option<&EditSession> {
        self.store.find_by_temp_path(path)
    }

    pub fn find_for_tab_path(
        &self,
        tab_id: TabId,
        remote_path: &RemotePath,
    ) -> Option<&EditSession> {
        self.store.find_for_tab_path(tab_id, remote_path)
    }

    pub fn sessions(&self) -> impl Iterator<Item = &EditSession> {
        self.store.sessions()
    }

    pub fn conflict_sessions(&self) -> impl Iterator<Item = &EditSession> {
        self.store.conflict_sessions()
    }

    pub fn session_tab_ids(&self) -> Vec<TabId> {
        self.store.session_tab_ids()
    }
}

/// Seeding and mutation hooks for `macsftp-app` tests that need a session in an
/// arbitrary phase (the production path only reaches phases through intents).
/// Compiled only with `cfg(test)` or the `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
impl RemoteEditLifecycle {
    pub fn next_id(&mut self) -> EditSessionId {
        self.store.next_id()
    }

    pub fn register(&mut self, session: EditSession) {
        self.store.register(session);
    }

    pub fn session_mut(&mut self, id: EditSessionId) -> Option<&mut EditSession> {
        self.store.get_mut(id)
    }

    pub fn remove(&mut self, id: EditSessionId) -> Option<EditSession> {
        self.store.remove(id)
    }
}

/// The comparison copy lives in `<session>/compare/` so it can never collide
/// with the user's edited copy.
pub fn conflict_remote_path(local_temp_path: &LocalPath) -> LocalPath {
    let path = std::path::Path::new(local_temp_path.as_str());
    let parent = path.parent().unwrap_or(path);
    let ext = path.extension().and_then(|e| e.to_str());
    let file_name = match ext {
        Some(e) if !e.is_empty() && !e.chars().any(is_bidi_or_control) => {
            format!("conflict-remote.{}", e)
        }
        _ => "conflict-remote".to_string(),
    };
    LocalPath::new(
        parent
            .join("compare")
            .join(file_name)
            .to_string_lossy()
            .to_string(),
    )
}

/// Build the explicit upload for a session's write-through path.
///
/// Keeps the default metadata policy: the upload must preserve the temp file's
/// mtime, because the lifecycle rebases the remote snapshot from it.
fn build_edit_upload_command(
    temp: &LocalPath,
    remote_path: &RemotePath,
    session_epoch: u64,
    profile_id: ProfileId,
    tab_id: TabId,
) -> AppCommand {
    AppCommand::StartTransfer(StartTransferCommand {
        tab_id,
        session_epoch,
        profile_id,
        direction: TransferDirection::Upload,
        sources: vec![TransferEndpoint::Local(temp.clone())],
        destination: TransferEndpoint::Remote(remote_path.clone()),
        metadata_policy: MetadataPolicy::default(),
        // The edit layer already ran its own divergence check, or the user
        // explicitly chose "Replace Server File".
        conflict_policy: ConflictPolicy::OverwriteAll,
    })
}

fn safe_file_name(path: &RemotePath) -> Option<&str> {
    std::path::Path::new(path.as_str())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.chars().any(is_bidi_or_control))
}

fn file_name_of(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::{
        CheckRemoteEditSnapshotCommand, ConnectionPoolIdentity, EditCheckOutcome, EditCheckResult,
        SessionId,
    };

    fn connection_key() -> ConnectionKey {
        ConnectionKey {
            host: "srv.example.com".into(),
            port: 22,
            username: "alex".into(),
            identity: ConnectionPoolIdentity::Ephemeral(SessionId(1)),
        }
    }

    fn lifecycle() -> RemoteEditLifecycle {
        RemoteEditLifecycle::new(LocalPath::new("/tmp/macsftp-edits"), "run-1".into())
    }

    fn timestamp(secs: u64) -> Timestamp {
        Timestamp::from_secs_since_epoch(secs)
    }

    fn snapshot(size: u64, secs: u64) -> RemoteSnapshot {
        RemoteSnapshot {
            size: Some(size),
            modified_at: Some(timestamp(secs)),
        }
    }

    fn facts(size: u64, millis: u64) -> LocalFileFacts {
        LocalFileFacts {
            size,
            mtime: SystemTime::UNIX_EPOCH + Duration::from_millis(millis),
        }
    }

    fn pending(remote_path: &str) -> PendingEdit {
        pending_with(remote_path, remote_path)
    }

    fn pending_with(remote_path: &str, io_path: &str) -> PendingEdit {
        PendingEdit {
            remote_path: RemotePath::new(remote_path),
            io_path: RemotePath::new(io_path),
            size: Some(10),
            modified_at: Some(timestamp(100)),
            session_epoch: 3,
            connection_key: connection_key(),
            tab_id: TabId(1),
            profile_id: ProfileId(1),
        }
    }

    fn seed(
        edits: &mut RemoteEditLifecycle,
        phase: EditPhase,
        tab_id: u64,
        remote_path: &str,
        io_path: &str,
    ) -> EditSessionId {
        let id = edits.next_id();
        edits.register(EditSession {
            id,
            remote_path: RemotePath::new(remote_path),
            io_path: RemotePath::new(io_path),
            tab_id: TabId(tab_id),
            session_epoch: 3,
            profile_id: ProfileId(1),
            connection_key: connection_key(),
            local_temp_path: LocalPath::new(format!("/tmp/macsftp-edits/run-1/{}/a.txt", id.0)),
            phase,
            remote_snapshot: snapshot(10, 100),
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        id
    }

    fn seed_session(
        edits: &mut RemoteEditLifecycle,
        phase: EditPhase,
        tab_id: u64,
    ) -> EditSessionId {
        seed(edits, phase, tab_id, "/srv/a.txt", "/srv/a.txt")
    }

    /// Seed an `Editing` session and move it to `CheckingRemote` through the
    /// production intent, so the result guards see a real check id.
    fn start_check(
        edits: &mut RemoteEditLifecycle,
    ) -> (EditSessionId, CheckRemoteEditSnapshotCommand) {
        let id = seed_session(edits, EditPhase::Editing, 1);
        let UploadRequest::Check(check) =
            edits.request_upload(id, 3, &connection_key(), timestamp(200))
        else {
            panic!("expected an upload check");
        };
        (id, check)
    }

    fn result_for(
        check: &CheckRemoteEditSnapshotCommand,
        outcome: EditCheckOutcome,
    ) -> EditCheckResult {
        EditCheckResult {
            tab_id: check.tab_id,
            session_epoch: check.session_epoch,
            session_id: check.edit_session_id,
            check_id: check.check_id,
            path: check.path.clone(),
            outcome,
        }
    }

    fn finished_download(session: &EditSession, succeeded: bool) -> EditTransferFinished {
        EditTransferFinished {
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(session.io_path.clone()),
            destination: TransferEndpoint::Local(session.local_temp_path.clone()),
            succeeded,
        }
    }

    fn finished_upload(session: &EditSession, succeeded: bool) -> EditTransferFinished {
        EditTransferFinished {
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(session.local_temp_path.clone()),
            destination: TransferEndpoint::Remote(session.io_path.clone()),
            succeeded,
        }
    }

    #[test]
    fn begin_starts_a_new_edit_when_the_file_has_no_session() {
        let mut edits = lifecycle();
        let BeginEdit::Start {
            pending: start,
            effects,
        } = edits.begin(pending("/srv/a.txt"), |_| true)
        else {
            panic!("expected a fresh edit");
        };
        assert_eq!(start.remote_path, RemotePath::new("/srv/a.txt"));
        assert!(effects.is_empty());
        assert_eq!(edits.sessions().count(), 0, "begin alone registers nothing");
    }

    #[test]
    fn begin_reopens_the_editor_for_a_live_editing_session() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        let temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let BeginEdit::Handled(effects) = edits.begin(pending("/srv/a.txt"), |_| true) else {
            panic!("expected the live session to be reused");
        };
        assert_eq!(
            effects,
            vec![EditEffect::OpenEditor {
                tab_id: TabId(1),
                temp_path: temp,
            }]
        );
        assert_eq!(
            edits.sessions().count(),
            1,
            "no second session is registered"
        );
    }

    #[test]
    fn begin_reopens_the_editor_while_an_upload_is_in_flight() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::UploadingBack, 1);
        let temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let BeginEdit::Handled(effects) = edits.begin(pending("/srv/a.txt"), |_| true) else {
            panic!("expected the uploading session to be reused");
        };
        assert_eq!(
            effects,
            vec![EditEffect::OpenEditor {
                tab_id: TabId(1),
                temp_path: temp,
            }]
        );
    }

    #[test]
    fn begin_reports_the_live_phase_instead_of_starting_a_second_download() {
        for (phase, notice) in [
            (EditPhase::Downloading, EditNotice::StillDownloading),
            (EditPhase::CheckingRemote, EditNotice::CheckingServerFile),
            (
                EditPhase::RemoteConflict,
                EditNotice::ReviewConflictBeforeReopen,
            ),
        ] {
            let mut edits = lifecycle();
            seed_session(&mut edits, phase, 1);
            let BeginEdit::Handled(effects) = edits.begin(pending("/srv/a.txt"), |_| true) else {
                panic!("expected the live phase to block");
            };
            assert_eq!(
                effects,
                vec![EditEffect::Notice {
                    tab_id: TabId(1),
                    notice,
                }]
            );
        }
    }

    #[test]
    fn begin_does_not_dedup_across_physical_connections() {
        let mut edits = lifecycle();
        seed_session(&mut edits, EditPhase::Editing, 1);
        let mut other = pending("/srv/a.txt");
        other.connection_key = ConnectionKey {
            host: "other.example.com".into(),
            ..connection_key()
        };
        assert!(matches!(
            edits.begin(other, |_| true),
            BeginEdit::Start { .. }
        ));
    }

    #[test]
    fn begin_reaps_a_session_whose_tab_is_gone_and_starts_the_edit() {
        let mut edits = lifecycle();
        let stale = seed_session(&mut edits, EditPhase::Editing, 9);
        let stale_temp = edits
            .session(stale)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let BeginEdit::Start { effects, .. } =
            edits.begin(pending("/srv/a.txt"), |tab| tab != TabId(9))
        else {
            panic!("expected the dead tab's session to be reaped");
        };
        assert_eq!(
            effects,
            vec![EditEffect::DeleteTempDir { path: stale_temp }]
        );
        assert!(edits.session(stale).is_none());
    }

    #[test]
    fn begin_refuses_to_rebind_a_repointed_link_with_a_live_owner() {
        for (phase, conflict) in [
            (EditPhase::Editing, false),
            (EditPhase::RemoteConflict, true),
        ] {
            let mut edits = lifecycle();
            seed(&mut edits, phase, 1, "/srv/link", "/srv/target-a");
            let BeginEdit::Handled(effects) =
                edits.begin(pending_with("/srv/link", "/srv/target-b"), |_| true)
            else {
                panic!("expected the re-pointed link to be refused");
            };
            assert_eq!(
                effects,
                vec![EditEffect::Notice {
                    tab_id: TabId(1),
                    notice: EditNotice::LinkRepointed { conflict },
                }]
            );
        }
    }

    #[test]
    fn begin_reaps_a_repointed_link_whose_tab_is_gone() {
        let mut edits = lifecycle();
        let stale = seed(
            &mut edits,
            EditPhase::Editing,
            9,
            "/srv/link",
            "/srv/target-a",
        );
        let stale_temp = edits
            .session(stale)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let BeginEdit::Start { effects, .. } = edits
            .begin(pending_with("/srv/link", "/srv/target-b"), |tab| {
                tab != TabId(9)
            })
        else {
            panic!("expected the dead link session to be reaped");
        };
        assert_eq!(
            effects,
            vec![EditEffect::DeleteTempDir { path: stale_temp }]
        );
        assert!(edits.session(stale).is_none());
    }

    #[test]
    fn start_download_registers_a_downloading_session_and_builds_the_fetch() {
        let mut edits = lifecycle();
        let started = edits
            .start_download(pending("/srv/dir/a.txt"))
            .expect("a safe file name starts the download");
        assert_eq!(
            started.temp_path,
            LocalPath::new(format!(
                "/tmp/macsftp-edits/run-1/{}/a.txt",
                started.session_id.0
            ))
        );
        let session = edits.session(started.session_id).expect("registered");
        assert_eq!(session.phase, EditPhase::Downloading);
        assert_eq!(session.remote_snapshot, snapshot(10, 100));
        assert_eq!(session.session_epoch, 3);

        let AppCommand::StartTransfer(command) = started.command else {
            panic!("expected a transfer command");
        };
        assert_eq!(command.tab_id, TabId(1));
        assert_eq!(command.session_epoch, 3);
        assert_eq!(command.profile_id, ProfileId(1));
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Remote(RemotePath::new("/srv/dir/a.txt"))]
        );
        assert_eq!(
            command.destination,
            TransferEndpoint::Local(started.temp_path.clone())
        );
        assert_eq!(command.metadata_policy, MetadataPolicy::default());
        assert_eq!(
            command.conflict_policy,
            ConflictPolicy::OverwriteAll,
            "an edit temp is always ours to overwrite"
        );
    }

    #[test]
    fn start_download_rejects_a_file_name_that_cannot_live_on_disk() {
        let mut edits = lifecycle();
        assert_eq!(
            edits.start_download(pending("/srv/\u{202e}evil.txt")).err(),
            Some(EditNotice::UnsafeFileName)
        );
        assert_eq!(edits.sessions().count(), 0);
    }

    #[test]
    fn abort_download_removes_the_session_and_its_temp_dir() {
        let mut edits = lifecycle();
        let started = edits
            .start_download(pending("/srv/a.txt"))
            .expect("started");
        assert_eq!(
            edits.abort_download(started.session_id),
            vec![EditEffect::DeleteTempDir {
                path: started.temp_path.clone(),
            }]
        );
        assert!(edits.session(started.session_id).is_none());
    }

    #[test]
    fn abort_download_leaves_a_started_edit_alone() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        assert!(edits.abort_download(id).is_empty());
        assert!(edits.session(id).is_some());
    }

    #[test]
    fn request_upload_transitions_to_checking_and_allocates_a_check_id() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        let UploadRequest::Check(check) =
            edits.request_upload(id, 4, &connection_key(), timestamp(200))
        else {
            panic!("expected an upload check");
        };
        assert_eq!(check.tab_id, TabId(1));
        assert_eq!(check.session_epoch, 4);
        assert_eq!(check.edit_session_id, id);
        assert_eq!(check.path, RemotePath::new("/srv/a.txt"));

        let session = edits.session(id).expect("still registered");
        assert_eq!(session.phase, EditPhase::CheckingRemote);
        assert_eq!(session.session_epoch, 4);
        assert_eq!(session.pending_check_id, Some(check.check_id));
        assert_eq!(session.checking_local_mtime, Some(timestamp(200)));
    }

    #[test]
    fn request_upload_rejects_a_phase_that_is_not_editing() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Downloading, 1);
        let UploadRequest::Rejected(notice) =
            edits.request_upload(id, 3, &connection_key(), timestamp(200))
        else {
            panic!("expected the request to be refused");
        };
        assert_eq!(notice, EditNotice::PhaseBlocked(EditPhase::Downloading));
        assert_eq!(
            edits.session(id).expect("session is registered").phase,
            EditPhase::Downloading,
            "a refused request changes nothing"
        );
    }

    #[test]
    fn request_upload_rejects_a_different_physical_connection() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        let other = ConnectionKey {
            host: "other.example.com".into(),
            ..connection_key()
        };
        let UploadRequest::Rejected(notice) = edits.request_upload(id, 3, &other, timestamp(200))
        else {
            panic!("expected the request to be refused");
        };
        assert_eq!(notice, EditNotice::WrongConnection);
        assert_eq!(
            edits.session(id).expect("session is registered").phase,
            EditPhase::Editing
        );
    }

    #[test]
    fn an_unchanged_remote_starts_the_upload() {
        let mut edits = lifecycle();
        let (id, check) = start_check(&mut edits);
        let effects = edits.on_check_result(
            result_for(&check, EditCheckOutcome::Matched(snapshot(10, 100))),
            Some(timestamp(200)),
        );

        let [
            EditEffect::Dispatch {
                session_id,
                command,
                rollback,
                ..
            },
        ] = effects.as_slice()
        else {
            panic!("expected one upload dispatch, got {effects:?}");
        };
        assert_eq!(*session_id, id);
        assert_eq!(*rollback, DispatchRollback::RevertCheck);
        let AppCommand::StartTransfer(upload) = command else {
            panic!("expected a transfer command");
        };
        assert_eq!(upload.tab_id, TabId(1));
        assert_eq!(upload.direction, TransferDirection::Upload);
        assert_eq!(
            upload.sources,
            vec![TransferEndpoint::Local(
                edits
                    .session(id)
                    .expect("session is registered")
                    .local_temp_path
                    .clone()
            )]
        );
        assert_eq!(
            upload.destination,
            TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))
        );
        assert_eq!(
            upload.metadata_policy,
            MetadataPolicy::default(),
            "the upload must preserve the temp mtime the snapshot is rebased from"
        );
        assert_eq!(upload.conflict_policy, ConflictPolicy::OverwriteAll);

        let session = edits.session(id).expect("still registered");
        assert_eq!(session.phase, EditPhase::UploadingBack);
        assert_eq!(session.pending_check_id, None);
        assert_eq!(session.checking_local_mtime, None);
    }

    #[test]
    fn a_changed_remote_opens_a_conflict_instead_of_uploading() {
        let mut edits = lifecycle();
        let (id, check) = start_check(&mut edits);
        let effects = edits.on_check_result(
            result_for(&check, EditCheckOutcome::Matched(snapshot(11, 100))),
            Some(timestamp(200)),
        );
        assert!(
            effects.is_empty(),
            "the user decides; nothing is dispatched"
        );
        let session = edits.session(id).expect("still registered");
        assert_eq!(session.phase, EditPhase::RemoteConflict);
        assert_eq!(
            session.remote_snapshot,
            snapshot(10, 100),
            "baseline is kept"
        );
        assert_eq!(session.pending_check_id, None);
    }

    #[test]
    fn a_failed_check_returns_to_editing_and_reports() {
        let mut edits = lifecycle();
        let (id, check) = start_check(&mut edits);
        let effects = edits.on_check_result(
            result_for(&check, EditCheckOutcome::Failed),
            Some(timestamp(200)),
        );
        assert_eq!(
            effects,
            vec![EditEffect::Notice {
                tab_id: TabId(1),
                notice: EditNotice::ServerCheckFailed,
            }]
        );
        let session = edits.session(id).expect("still registered");
        assert_eq!(session.phase, EditPhase::Editing);
        assert_eq!(session.pending_check_id, None);
        assert_eq!(session.checking_local_mtime, None);
    }

    #[test]
    fn a_save_during_the_check_invalidates_its_result() {
        for current in [None, Some(timestamp(201))] {
            let mut edits = lifecycle();
            let (id, check) = start_check(&mut edits);
            let effects = edits.on_check_result(
                result_for(&check, EditCheckOutcome::Matched(snapshot(10, 100))),
                current,
            );
            assert_eq!(
                effects,
                vec![EditEffect::Notice {
                    tab_id: TabId(1),
                    notice: EditNotice::LocalChangedDuringCheck,
                }]
            );
            assert_eq!(
                edits.session(id).expect("session is registered").phase,
                EditPhase::Editing,
                "the user must request a fresh upload"
            );
        }
    }

    #[test]
    fn check_results_with_stale_correlations_are_ignored() {
        let mut edits = lifecycle();
        let (id, check) = start_check(&mut edits);

        let mut wrong_check = result_for(&check, EditCheckOutcome::Matched(snapshot(10, 100)));
        wrong_check.check_id = EditCheckId(999);
        let mut wrong_epoch = result_for(&check, EditCheckOutcome::Matched(snapshot(10, 100)));
        wrong_epoch.session_epoch += 1;
        let mut wrong_tab = result_for(&check, EditCheckOutcome::Matched(snapshot(10, 100)));
        wrong_tab.tab_id = TabId(2);
        let mut wrong_path = result_for(&check, EditCheckOutcome::Matched(snapshot(10, 100)));
        wrong_path.path = RemotePath::new("/srv/other.txt");

        for stale in [wrong_check, wrong_epoch, wrong_tab, wrong_path] {
            assert!(
                edits
                    .on_check_result(stale, Some(timestamp(200)))
                    .is_empty()
            );
            assert_eq!(
                edits.session(id).expect("session is registered").phase,
                EditPhase::CheckingRemote,
                "a stale result leaves the live check in flight"
            );
        }

        let mut unknown_session = result_for(&check, EditCheckOutcome::Failed);
        unknown_session.session_id = EditSessionId(999);
        assert!(
            edits
                .on_check_result(unknown_session, Some(timestamp(200)))
                .is_empty()
        );
    }

    #[test]
    fn a_completed_download_opens_the_editor_and_records_the_baseline() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Downloading, 1);
        let session = edits.session(id).expect("session is registered").clone();
        let effects =
            edits.on_transfer_finished(finished_download(&session, true), Some(facts(12, 12_500)));
        assert_eq!(
            effects,
            vec![EditEffect::OpenEditor {
                tab_id: TabId(1),
                temp_path: session.local_temp_path.clone(),
            }]
        );
        let session = edits.session(id).expect("session is registered");
        assert_eq!(session.phase, EditPhase::Editing);
        assert!(!session.local_modified);
        assert_eq!(
            session.local_baseline,
            Some(LocalBaseline {
                size: 12,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_millis(12_500),
            })
        );
    }

    #[test]
    fn a_failed_download_removes_the_session_and_its_temp_dir() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Downloading, 1);
        let session = edits.session(id).expect("session is registered").clone();
        let effects =
            edits.on_transfer_finished(finished_download(&session, false), Some(facts(0, 0)));
        assert_eq!(
            effects,
            vec![
                EditEffect::DeleteTempDir {
                    path: session.local_temp_path.clone(),
                },
                EditEffect::Notice {
                    tab_id: TabId(1),
                    notice: EditNotice::DownloadFailed {
                        file_name: "a.txt".into(),
                    },
                },
            ]
        );
        assert!(edits.session(id).is_none());
    }

    #[test]
    fn transfers_that_do_not_match_the_phase_or_path_are_ignored() {
        let mut edits = lifecycle();
        let downloading = seed_session(&mut edits, EditPhase::Downloading, 1);
        let session = edits
            .session(downloading)
            .expect("session is registered")
            .clone();
        assert!(
            edits
                .on_transfer_finished(finished_upload(&session, true), Some(facts(1, 1)))
                .is_empty(),
            "an upload cannot finish a download session"
        );
        assert_eq!(
            edits
                .session(downloading)
                .expect("session is registered")
                .phase,
            EditPhase::Downloading
        );

        let other_remote = EditTransferFinished {
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/other.txt")),
            destination: TransferEndpoint::Local(session.local_temp_path.clone()),
            succeeded: true,
        };
        assert!(
            edits
                .on_transfer_finished(other_remote, Some(facts(1, 1)))
                .is_empty()
        );

        let unknown_temp = EditTransferFinished {
            direction: TransferDirection::Download,
            source: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
            destination: TransferEndpoint::Local(LocalPath::new("/tmp/unrelated")),
            succeeded: false,
        };
        assert!(
            edits
                .on_transfer_finished(unknown_temp, Some(facts(1, 1)))
                .is_empty()
        );
    }

    #[test]
    fn a_completed_upload_rebases_the_snapshot_and_clears_the_dirty_flag() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::UploadingBack, 1);
        let session = edits.session(id).expect("session is registered").clone();
        let effects =
            edits.on_transfer_finished(finished_upload(&session, true), Some(facts(12, 12_500)));
        let rebased = RemoteSnapshot {
            size: Some(12),
            modified_at: Some(timestamp(12)),
        };
        assert_eq!(
            effects,
            vec![
                EditEffect::RebaseListing {
                    tab_id: TabId(1),
                    path: RemotePath::new("/srv/a.txt"),
                    snapshot: rebased,
                },
                EditEffect::Notice {
                    tab_id: TabId(1),
                    notice: EditNotice::Uploaded,
                },
            ]
        );
        let session = edits.session(id).expect("session is registered");
        assert_eq!(session.phase, EditPhase::Editing);
        assert_eq!(session.remote_snapshot, rebased);
        assert!(!session.local_modified);
        assert_eq!(
            session.local_baseline,
            Some(LocalBaseline {
                size: 12,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_millis(12_500),
            })
        );
    }

    #[test]
    fn a_failed_upload_keeps_the_edited_copy_for_retry() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::UploadingBack, 1);
        edits
            .session_mut(id)
            .expect("session is registered")
            .local_modified = true;
        let session = edits.session(id).expect("session is registered").clone();
        let effects =
            edits.on_transfer_finished(finished_upload(&session, false), Some(facts(12, 12_500)));
        assert_eq!(
            effects,
            vec![EditEffect::Notice {
                tab_id: TabId(1),
                notice: EditNotice::UploadFailed,
            }]
        );
        let session = edits.session(id).expect("kept for retry");
        assert_eq!(session.phase, EditPhase::Editing);
        assert_eq!(session.remote_snapshot, snapshot(10, 100));
        assert!(session.local_modified);
        assert_eq!(session.local_baseline, None);
    }

    #[test]
    fn overwriting_a_conflict_dispatches_the_upload() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::RemoteConflict, 1);
        let effects = edits.resolve_conflict(id, ConflictChoice::Overwrite);
        let [
            EditEffect::Dispatch {
                command, rollback, ..
            },
        ] = effects.as_slice()
        else {
            panic!("expected an upload dispatch, got {effects:?}");
        };
        assert_eq!(*rollback, DispatchRollback::RestoreConflict);
        assert!(matches!(
            command,
            AppCommand::StartTransfer(upload)
                if upload.direction == TransferDirection::Upload
                    && upload.destination
                        == TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))
        ));
        assert_eq!(
            edits.session(id).expect("session is registered").phase,
            EditPhase::UploadingBack
        );
        assert!(
            edits
                .resolve_conflict(id, ConflictChoice::Overwrite)
                .is_empty(),
            "a second click is a no-op"
        );
    }

    #[test]
    fn discarding_the_local_copy_redownloads_the_remote() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::RemoteConflict, 1);
        let old_temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let effects = edits.resolve_conflict(id, ConflictChoice::DiscardLocal);
        let [
            EditEffect::DeleteTempDir { path },
            EditEffect::Dispatch {
                session_id,
                command,
                rollback,
                ..
            },
        ] = effects.as_slice()
        else {
            panic!("expected cleanup then a fresh download, got {effects:?}");
        };
        assert_eq!(path, &old_temp);
        assert_ne!(*session_id, id);
        assert_eq!(*rollback, DispatchRollback::AbortDownload);
        assert!(matches!(
            command,
            AppCommand::StartTransfer(download)
                if download.direction == TransferDirection::Download
                    && download.sources
                        == vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))]
        ));
        assert!(edits.session(id).is_none());
        let replacement = edits.session(*session_id).expect("replacement registered");
        assert_eq!(replacement.phase, EditPhase::Downloading);
        assert_eq!(replacement.tab_id, TabId(1));
    }

    #[test]
    fn deferring_a_conflict_returns_to_editing() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::RemoteConflict, 1);
        assert!(edits.resolve_conflict(id, ConflictChoice::Later).is_empty());
        assert_eq!(
            edits.session(id).expect("session is registered").phase,
            EditPhase::Editing
        );
    }

    #[test]
    fn conflict_choices_are_ignored_outside_the_conflict_phase() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        assert!(
            edits
                .resolve_conflict(id, ConflictChoice::Overwrite)
                .is_empty()
        );
        assert_eq!(
            edits.session(id).expect("session is registered").phase,
            EditPhase::Editing
        );
    }

    #[test]
    fn the_conflict_comparison_downloads_beside_the_edited_copy() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::RemoteConflict, 1);
        let temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let command = edits
            .plan_conflict_compare(id, ProfileId(7))
            .expect("a conflict can be compared");
        let AppCommand::StartTransfer(compare) = command else {
            panic!("expected a transfer command");
        };
        assert_eq!(compare.profile_id, ProfileId(7));
        assert_eq!(compare.direction, TransferDirection::Download);
        assert_eq!(
            compare.destination,
            TransferEndpoint::Local(conflict_remote_path(&temp))
        );

        let editing = seed_session(&mut edits, EditPhase::Editing, 1);
        assert!(edits.plan_conflict_compare(editing, ProfileId(1)).is_none());
    }

    #[test]
    fn dispatch_failure_restores_the_phase_it_came_from() {
        let mut edits = lifecycle();
        let (checking, _) = start_check(&mut edits);
        assert!(
            edits
                .dispatch_failed(checking, DispatchRollback::RevertCheck)
                .is_empty()
        );
        assert_eq!(
            edits
                .session(checking)
                .expect("session is registered")
                .phase,
            EditPhase::Editing
        );

        let mut edits = lifecycle();
        let uploading = seed_session(&mut edits, EditPhase::UploadingBack, 1);
        assert!(
            edits
                .dispatch_failed(uploading, DispatchRollback::RestoreConflict)
                .is_empty()
        );
        assert_eq!(
            edits
                .session(uploading)
                .expect("session is registered")
                .phase,
            EditPhase::RemoteConflict
        );

        let mut edits = lifecycle();
        let started = edits
            .start_download(pending("/srv/a.txt"))
            .expect("started");
        assert_eq!(
            edits.dispatch_failed(started.session_id, DispatchRollback::AbortDownload),
            vec![EditEffect::DeleteTempDir {
                path: started.temp_path.clone(),
            }]
        );
        assert!(edits.session(started.session_id).is_none());
    }

    #[test]
    fn the_watcher_ignores_our_own_write_but_marks_real_edits() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        let temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        edits
            .session_mut(id)
            .expect("session is registered")
            .local_baseline = Some(LocalBaseline { size: 4, mtime });

        assert!(!edits.record_local_modified(id, &temp, Some(LocalFileFacts { size: 4, mtime })));
        assert!(
            !edits
                .session(id)
                .expect("session is registered")
                .local_modified
        );

        assert!(edits.record_local_modified(id, &temp, Some(LocalFileFacts { size: 5, mtime })));
        assert!(
            edits
                .session(id)
                .expect("session is registered")
                .local_modified
        );
    }

    #[test]
    fn an_unreadable_temp_file_counts_as_modified() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        let temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        assert!(edits.record_local_modified(id, &temp, None));
        assert!(
            edits
                .session(id)
                .expect("session is registered")
                .local_modified
        );
    }

    #[test]
    fn watcher_events_for_other_paths_or_phases_are_ignored() {
        let mut edits = lifecycle();
        let id = seed_session(&mut edits, EditPhase::Editing, 1);
        let temp = edits
            .session(id)
            .expect("session is registered")
            .local_temp_path
            .clone();
        assert!(!edits.record_local_modified(
            id,
            &LocalPath::new("/tmp/elsewhere"),
            Some(facts(5, 5))
        ));
        assert!(
            !edits
                .session(id)
                .expect("session is registered")
                .local_modified
        );

        let downloading = seed_session(&mut edits, EditPhase::Downloading, 1);
        let downloading_temp = edits
            .session(downloading)
            .expect("session is registered")
            .local_temp_path
            .clone();
        assert!(!edits.record_local_modified(downloading, &downloading_temp, Some(facts(5, 5))));
        assert!(
            !edits
                .session(downloading)
                .expect("session is registered")
                .local_modified
        );
        assert_ne!(temp, downloading_temp);
    }

    #[test]
    fn a_listing_refresh_adopts_only_benign_editing_metadata() {
        let mut edits = lifecycle();
        let plain = seed_session(&mut edits, EditPhase::Editing, 1);
        let repointed = seed(
            &mut edits,
            EditPhase::Editing,
            1,
            "/srv/link",
            "/srv/target",
        );
        let changed = seed(
            &mut edits,
            EditPhase::Editing,
            1,
            "/srv/changed.txt",
            "/srv/changed.txt",
        );
        let conflict = seed(
            &mut edits,
            EditPhase::RemoteConflict,
            1,
            "/srv/conflict.txt",
            "/srv/conflict.txt",
        );
        let other_tab = seed(
            &mut edits,
            EditPhase::Editing,
            2,
            "/srv/elsewhere.txt",
            "/srv/elsewhere.txt",
        );

        let sub_second = RemoteSnapshot {
            size: Some(10),
            modified_at: Some(Timestamp::from_system_time(
                SystemTime::UNIX_EPOCH + Duration::from_millis(100_500),
            )),
        };
        edits.adopt_listing_baselines(
            TabId(1),
            &[
                (RemotePath::new("/srv/a.txt"), sub_second),
                (RemotePath::new("/srv/link"), snapshot(10, 100)),
                (RemotePath::new("/srv/changed.txt"), snapshot(11, 100)),
                (RemotePath::new("/srv/conflict.txt"), snapshot(10, 100)),
                (RemotePath::new("/srv/elsewhere.txt"), snapshot(10, 100)),
            ],
        );

        assert_eq!(
            edits
                .session(plain)
                .expect("session is registered")
                .remote_snapshot,
            sub_second,
            "a sub-second difference is our own rebase noise"
        );
        assert_eq!(
            edits
                .session(repointed)
                .expect("session is registered")
                .remote_snapshot,
            snapshot(10, 100),
            "a link must keep its write-through baseline"
        );
        assert_eq!(
            edits
                .session(changed)
                .expect("session is registered")
                .remote_snapshot,
            snapshot(10, 100),
            "a real size change is left for the upload check"
        );
        assert_eq!(
            edits
                .session(conflict)
                .expect("session is registered")
                .remote_snapshot,
            snapshot(10, 100),
            "a surfaced conflict is never masked"
        );
        assert_eq!(
            edits
                .session(other_tab)
                .expect("session is registered")
                .remote_snapshot,
            snapshot(10, 100),
            "another tab's listing does not apply"
        );
    }

    #[test]
    fn reconnecting_refreshes_epochs_and_cancels_in_flight_checks() {
        let mut edits = lifecycle();
        let (checking, _) = start_check(&mut edits);
        let other_tab = seed_session(&mut edits, EditPhase::Editing, 2);

        edits.update_epoch_for_tab(TabId(1), 9);

        let session = edits.session(checking).expect("session is registered");
        assert_eq!(session.phase, EditPhase::Editing);
        assert_eq!(session.session_epoch, 9);
        assert_eq!(session.pending_check_id, None);
        assert_eq!(session.checking_local_mtime, None);
        assert_eq!(
            edits
                .session(other_tab)
                .expect("session is registered")
                .session_epoch,
            3
        );
    }

    #[test]
    fn removing_a_tab_returns_its_sessions_for_temp_cleanup() {
        let mut edits = lifecycle();
        seed_session(&mut edits, EditPhase::Editing, 1);
        seed_session(&mut edits, EditPhase::Downloading, 1);
        let kept = seed_session(&mut edits, EditPhase::Editing, 2);

        let removed = edits.remove_for_tab(TabId(1));

        assert_eq!(removed.len(), 2);
        assert_eq!(edits.session_tab_ids(), vec![TabId(2)]);
        assert!(edits.session(kept).is_some());
    }

    #[test]
    fn the_comparison_copy_never_collides_with_the_edited_copy() {
        assert_eq!(
            conflict_remote_path(&LocalPath::new("/tmp/e/1/a.txt")),
            LocalPath::new("/tmp/e/1/compare/conflict-remote.txt")
        );
        assert_eq!(
            conflict_remote_path(&LocalPath::new("/tmp/e/1/Makefile")),
            LocalPath::new("/tmp/e/1/compare/conflict-remote")
        );
    }
}
