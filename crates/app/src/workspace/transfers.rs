impl Workspace {
    pub(crate) fn begin_upload(&mut self, sources: Vec<LocalPath>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before starting an upload".into());
            cx.notify();
            return;
        };
        let Some(remote_directory) = tab.remote.path.clone() else {
            self.status_message = Some("Choose a remote destination directory".into());
            cx.notify();
            return;
        };
        if sources.is_empty() {
            self.status_message = Some("Select local files or directories to upload".into());
            cx.notify();
            return;
        }

        let destination = if sources.len() == 1
            && !tab
                .local
                .entries
                .iter()
                .any(|entry| entry.path == sources[0] && entry.kind == FileKind::Directory)
        {
            match append_remote_name(&remote_directory, &sources[0]) {
                Some(path) => path,
                None => {
                    self.status_message =
                        Some("Selected upload source has no usable file name".into());
                    cx.notify();
                    return;
                }
            }
        } else {
            remote_directory
        };
        let tab_id = tab.id;
        let command = AppCommand::StartTransfer(macsftp_core::StartTransferCommand {
            tab_id,
            session_epoch,
            profile_id,
            direction: macsftp_core::TransferDirection::Upload,
            sources: sources.into_iter().map(TransferEndpoint::Local).collect(),
            destination: TransferEndpoint::Remote(destination),
            metadata_policy: Default::default(),
            conflict_policy: Default::default(),
        });
        if self.send_command(command, cx) {
            self.drawer_open = true;
            self.status_message = Some("Planning upload…".into());
            cx.notify();
        }
    }
    pub(crate) fn begin_download(&mut self, sources: Vec<RemotePath>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before starting a download".into());
            cx.notify();
            return;
        };
        let Some(local_directory) = tab.local.path.clone() else {
            self.status_message = Some("Choose a local destination directory".into());
            cx.notify();
            return;
        };
        if sources.len() != 1 {
            self.status_message = Some("Select one remote file or directory to download".into());
            cx.notify();
            return;
        }
        let Some(destination) = append_local_name(&local_directory, &sources[0]) else {
            self.status_message = Some("Selected download source has no usable file name".into());
            cx.notify();
            return;
        };
        let tab_id = tab.id;
        let command = AppCommand::StartTransfer(macsftp_core::StartTransferCommand {
            tab_id,
            session_epoch,
            profile_id,
            direction: macsftp_core::TransferDirection::Download,
            sources: sources.into_iter().map(TransferEndpoint::Remote).collect(),
            destination: TransferEndpoint::Local(destination),
            metadata_policy: Default::default(),
            conflict_policy: Default::default(),
        });
        if self.send_command(command, cx) {
            self.drawer_open = true;
            self.status_message = Some("Planning download…".into());
            cx.notify();
        }
    }
    pub(crate) fn upload_selection(&mut self, cx: &mut Context<Self>) {
        let sources: Vec<LocalPath> = self
            .active_tab()
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter_map(|path| match path {
                        EntryPath::Local(path) => Some(path.clone()),
                        EntryPath::Remote(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.begin_upload(sources, cx);
    }
    pub(crate) fn download_selection(&mut self, cx: &mut Context<Self>) {
        let sources: Vec<RemotePath> = self
            .active_tab()
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter_map(|path| match path {
                        EntryPath::Remote(path) => Some(path.clone()),
                        EntryPath::Local(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.begin_download(sources, cx);
    }
    pub(crate) fn cancel_transfer(&mut self, transfer_id: macsftp_core::TransferId, cx: &mut Context<Self>) {
        if let Some(job) = self
            .state
            .transfers
            .jobs
            .iter_mut()
            .find(|job| job.id == transfer_id)
            && matches!(
                job.state,
                TransferState::Queued | TransferState::Running { .. }
            )
        {
            job.state = TransferState::Cancelling;
        }
        self.send_command(AppCommand::CancelTransfer { transfer_id }, cx);
        cx.notify();
    }
    pub(crate) fn retry_transfer(&mut self, transfer_id: macsftp_core::TransferId, cx: &mut Context<Self>) {
        self.send_command(AppCommand::RetryTransfer { transfer_id }, cx);
    }
    pub(crate) fn finalize_plan(&mut self, _job_id: macsftp_core::TransferId) {
        let Some(plan_id) = self
            .state
            .transfers
            .plans
            .iter()
            .find_map(|plan| plan.child_jobs.contains(&_job_id).then_some(plan.id))
        else {
            return;
        };
        let Some(plan) = self
            .state
            .transfers
            .plans
            .iter()
            .find(|plan| plan.id == plan_id)
        else {
            return;
        };
        if matches!(
            plan.state,
            macsftp_core::TransferPlanState::Completed
                | macsftp_core::TransferPlanState::Cancelled
                | macsftp_core::TransferPlanState::Failed { .. }
        ) {
            return;
        }
        let mut any_failed = false;
        for child_id in &plan.child_jobs {
            let Some(job) = self
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == *child_id)
            else {
                return;
            };
            match job.state {
                TransferState::Completed | TransferState::Skipped => {}
                TransferState::Failed { .. } => any_failed = true,
                _ => return,
            }
        }

        let root_job_id = plan.root_job_id;
        let plan_state = if any_failed {
            macsftp_core::TransferPlanState::Failed {
                error: UserFacingError::new(
                    ErrorCode::Unknown,
                    "Transfer finished with failures",
                    "Some files could not be transferred.",
                ),
            }
        } else {
            macsftp_core::TransferPlanState::Completed
        };
        if let Some(plan) = self
            .state
            .transfers
            .plans
            .iter_mut()
            .find(|plan| plan.id == plan_id)
        {
            plan.state = plan_state;
        }
        if let Some(root_job) = self
            .state
            .transfers
            .jobs
            .iter_mut()
            .find(|job| job.id == root_job_id)
        {
            root_job.state = if any_failed {
                TransferState::Failed {
                    retryable: true,
                    error: UserFacingError::new(
                        ErrorCode::Unknown,
                        "Transfer finished with failures",
                        "Some files could not be transferred.",
                    ),
                }
            } else {
                TransferState::Completed
            };
        }
    }
    pub(crate) fn flush_transfer_history(&mut self) {
        if self.transfer_history_flushed {
            return;
        }
        let now = Timestamp(SystemTime::now());
        let mut new_records: Vec<TransferHistoryRecord> = Vec::new();
        for plan in &self.state.transfers.plans {
            let root_job = self.state.transfers.find_job(plan.root_job_id);
            let record = TransferHistoryRecord {
                id: self.transfer_history.allocate_id(),
                profile_id: None,
                direction: root_job
                    .map(|job| job.direction)
                    .unwrap_or(TransferDirection::Upload),
                sources: vec![plan.source_root.clone()],
                destination: plan.destination_root.clone(),
                metadata_policy: root_job
                    .map(|job| job.metadata_policy.clone())
                    .unwrap_or_default(),
                conflict_policy: plan.conflict_policy.clone(),
                status: history_status_for_plan(plan.state.clone(), root_job),
                started_at: root_job.map(|job| job.created_at).unwrap_or(now),
                last_updated: now,
            };
            new_records.push(record);
        }
        self.transfer_history.append(new_records);
        self.transfer_history.prune(now);
        if let Err(error) = self.transfer_history.save() {
            warn!(error = %error, "could not persist transfer history on quit");
        }
        self.transfer_history_flushed = true;
    }
    pub(crate) fn reconcile_local_residual_temps(mut store: ResidualTempStore) -> ResidualTempStore {
        let local: Vec<_> = store.local_records().cloned().collect();
        for record in local {
            match std::fs::remove_file(&record.path) {
                Ok(()) => {
                    store.remove(record.transfer_id, &record.path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    store.remove(record.transfer_id, &record.path);
                }
                Err(error) => {
                    warn!(
                        path = %record.path,
                        error = %error,
                        "local residual temp file could not be removed at launch"
                    );
                }
            }
        }
        if let Err(error) = store.save() {
            warn!(error = %error, "could not persist residual temp store at launch");
        }
        store
    }
    pub(crate) fn clean_remote_residual_temps(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let connection_key = {
            let Some(tab) = self.state.tabs.find_tab(tab_id) else {
                return;
            };
            let Some(profile_id) = tab.profile_id else {
                return;
            };
            let Some(profile) = self.profiles.find_profile(profile_id) else {
                return;
            };
            format!("{}:{}", profile.host, profile.port)
        };
        let pending: Vec<_> = self
            .residual_temps
            .remote_records_for(&connection_key)
            .cloned()
            .collect();
        for record in pending {
            self.send_command(
                AppCommand::RemoveRemoteTempFile {
                    tab_id,
                    transfer_id: record.transfer_id,
                    path: RemotePath::new(record.path.clone()),
                },
                cx,
            );
        }
    }
    pub(crate) fn retry_history_transfer(
        &mut self,
        record_id: TransferHistoryId,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(record) = self.transfer_history.find(record_id).cloned() else {
            return false;
        };
        let Some(tab) = self.active_tab() else {
            self.status_message = Some("Open a connection to retry a transfer".into());
            cx.notify();
            return false;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before retrying a transfer".into());
            cx.notify();
            return false;
        };
        let command = record.to_start_command(tab.id, session_epoch, profile_id);
        let enqueued = self.send_command(AppCommand::StartTransfer(command), cx);
        if enqueued {
            self.transfer_history.remove(record_id);
            if let Err(error) = self.transfer_history.save() {
                warn!(error = %error, "could not update transfer history after retry");
            }
            self.drawer_open = true;
            self.status_message = Some("Retrying transfer…".into());
            cx.notify();
        }
        enqueued
    }
}
