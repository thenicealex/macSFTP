impl Workspace {
    pub(crate) fn handle_app_event(&mut self, event: AppEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.state.should_accept_event(&event) {
            // `AppEvent` carries no secrets by invariant (only host/port,
            // key fingerprints, paths, and user-facing errors), so logging
            // it at debug is safe per AGENTS.md §5.
            debug!(?event, "dropped stale event");
            return;
        }

        match event {
            AppEvent::TabConnecting { .. } => {
                // Already reflected optimistically when the command was
                // sent; nothing to update.
            }
            AppEvent::HostKeyUnknown(prompt) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(prompt.tab_id) {
                    tab.await_host_key(prompt.session_id, prompt.request_id);
                    self.state.modals.active.push(ModalRequest::HostKey(prompt));
                    window.focus(&self.modal_focus);
                }
            }
            AppEvent::HostKeyMismatch(mismatch) => {
                // Security block, no override (plan §10): the tab fails
                // with an explanation and the known_hosts file location.
                if let Some(tab) = self.state.tabs.find_tab_mut(mismatch.tab_id) {
                    let mut error = UserFacingError::new(
                        ErrorCode::HostKeyMismatch,
                        "Host key mismatch",
                        format!(
                            "The key presented by {}:{} does not match the stored \
                             key. This can indicate a man-in-the-middle attack. \
                             Connection blocked.",
                            mismatch.host, mismatch.port
                        ),
                    );
                    error.detail = Some(format!(
                        "Expected {}, got {}. Stored entries: \
                         ~/Library/Application Support/macSFTP/known_hosts and \
                         ~/.ssh/known_hosts.",
                        mismatch
                            .expected_fingerprint_sha256
                            .as_deref()
                            .unwrap_or("(unknown)"),
                        mismatch.actual_fingerprint_sha256
                    ));
                    tab.fail(error);
                }
            }
            AppEvent::TabConnected(scoped) => {
                let tab_id = scoped.scope.tab_id;
                let remote_root = scoped.payload.remote_root;
                if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
                    tab.complete_connect(
                        scoped.scope.session_id,
                        macsftp_core::Timestamp(std::time::SystemTime::now()),
                    );
                    // Start from the canonical remote root. The actor will
                    // replace this empty first-load state with a real listing.
                    tab.remote.entries.clear();
                    tab.remote.path = Some(remote_root.clone());
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                }
                self.remote_scroll = UniformListScrollHandle::new();
                self.request_remote_directory(tab_id, remote_root, cx);
                // Reconcile remote residual temp files from a previous run
                // now that a live session to this host exists (plan M5/M6).
                self.clean_remote_residual_temps(tab_id, cx);
            }
            AppEvent::TabDisconnected(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id) {
                    tab.disconnect(scoped.payload.reason);
                    tab.remote.entries.clear();
                    tab.remote.path = None;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                }
                let had_modal = !self.state.drain_expired_modals().is_empty();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            AppEvent::AuthFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id) {
                    tab.fail(scoped.payload.reason);
                }
                let had_modal = !self.state.drain_expired_modals().is_empty();
                if had_modal {
                    self.focus_pane(self.focused_side, window, cx);
                }
            }
            AppEvent::RemoteDirLoading(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    tab.remote.is_refreshing = true;
                    tab.remote.error = None;
                }
            }
            AppEvent::RemoteDirLoaded(scoped) => {
                let mut entries = scoped.payload.entries;
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    // A later navigation request can be queued while an
                    // earlier listing is still in flight. Keep the newer
                    // path and ignore the obsolete result.
                    && tab.remote.path.as_ref() == Some(&scoped.payload.path)
                {
                    sort_entries(&mut entries, &tab.sort);
                    tab.remote.entries = entries;
                    tab.remote.is_refreshing = false;
                    tab.remote.error = None;
                    tab.selection.selected_paths.clear();
                    self.remote_scroll = UniformListScrollHandle::new();
                }
            }
            AppEvent::RemoteOperationFailed(scoped) => {
                if let Some(tab) = self.state.tabs.find_tab_mut(scoped.scope.tab_id)
                    && scoped
                        .payload
                        .path
                        .as_ref()
                        .is_none_or(|path| tab.remote.path.as_ref() == Some(path))
                {
                    tab.remote.is_refreshing = false;
                    tab.remote.error = Some(scoped.payload.error.clone());
                }
                self.status_message = Some(
                    format!(
                        "{}: {}",
                        scoped.payload.error.title, scoped.payload.error.message
                    )
                    .into(),
                );
            }
            AppEvent::TransferPlanStarted(snapshot) => {
                self.state.transfers.plans.push(snapshot.plan);
                self.state.transfers.jobs.push(snapshot.root_job);
            }
            AppEvent::TransferConflict(prompt) => {
                let default_rename = copy_name(&prompt.destination);
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == prompt.transfer_id)
                {
                    job.state = TransferState::WaitingForConflictDecision {
                        plan_id: prompt.plan_id,
                        request_id: prompt.request_id,
                    };
                }
                self.state.transfers.pending_conflicts.push(
                    ConflictRequest::new(
                        prompt.request_id,
                        prompt.plan_id,
                        prompt.transfer_id,
                        prompt.source.clone(),
                        prompt.destination.clone(),
                        macsftp_core::Timestamp(std::time::SystemTime::now()),
                    )
                    .with_source_details(prompt.source_size, prompt.source_modified_at),
                );
                self.state
                    .modals
                    .active
                    .push(ModalRequest::TransferConflict(prompt));
                self.conflict_rename.set_value(default_rename);
                self.conflict_rename_error = None;
                window.focus(&self.modal_focus);
            }
            AppEvent::TransferPlanProgress(progress) => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == progress.plan_id)
                {
                    plan.planned_count = progress.planned_count;
                    plan.total_bytes = progress.total_bytes;
                    plan.child_jobs.push(progress.child_job.id);
                }
                self.state.transfers.jobs.push(progress.child_job);
            }
            AppEvent::TransferPlanCompleted { plan_id } => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == plan_id)
                {
                    plan.state = macsftp_core::TransferPlanState::Queued;
                    if let Some(root_job) = self
                        .state
                        .transfers
                        .jobs
                        .iter_mut()
                        .find(|job| job.id == plan.root_job_id)
                    {
                        root_job.state = TransferState::Queued;
                    }
                    if plan.child_jobs.is_empty() {
                        plan.state = macsftp_core::TransferPlanState::Completed;
                        if let Some(root_job) = self
                            .state
                            .transfers
                            .jobs
                            .iter_mut()
                            .find(|job| job.id == plan.root_job_id)
                        {
                            root_job.state = TransferState::Completed;
                        }
                    }
                }
            }
            AppEvent::TransferPlanCancelled { plan_id } => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == plan_id)
                {
                    plan.state = macsftp_core::TransferPlanState::Cancelled;
                    if let Some(root_job) = self
                        .state
                        .transfers
                        .jobs
                        .iter_mut()
                        .find(|job| job.id == plan.root_job_id)
                    {
                        root_job.state = TransferState::Skipped;
                    }
                }
            }
            AppEvent::TransferPlanFailed { plan_id, error } => {
                if let Some(plan) = self
                    .state
                    .transfers
                    .plans
                    .iter_mut()
                    .find(|plan| plan.id == plan_id)
                {
                    plan.state = macsftp_core::TransferPlanState::Failed {
                        error: error.clone(),
                    };
                    if let Some(root_job) = self
                        .state
                        .transfers
                        .jobs
                        .iter_mut()
                        .find(|job| job.id == plan.root_job_id)
                    {
                        root_job.state = TransferState::Failed {
                            retryable: error.retryable,
                            error,
                        };
                    }
                }
            }
            AppEvent::TransferQueued(snapshot) | AppEvent::TransferRunning(snapshot) => {
                if let Some(existing_job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == snapshot.job.id)
                {
                    *existing_job = snapshot.job;
                } else {
                    self.state.transfers.jobs.push(snapshot.job);
                }
            }
            AppEvent::TransferProgress(progress) => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == progress.transfer_id)
                {
                    let started_at = match job.state {
                        TransferState::Running { started_at, .. } => started_at,
                        _ => macsftp_core::Timestamp(std::time::SystemTime::now()),
                    };
                    job.state = TransferState::Running {
                        bytes_done: progress.bytes_done,
                        bytes_total: progress.bytes_total,
                        started_at,
                    };
                }
            }
            AppEvent::TransferWarning(warning) => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == warning.transfer_id)
                {
                    job.warnings.push(warning);
                }
            }
            AppEvent::TransferCompleted { transfer_id } => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == transfer_id)
                {
                    job.state = TransferState::Completed;
                }
                self.finalize_plan(transfer_id);
            }
            AppEvent::TransferSkipped { transfer_id } => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == transfer_id)
                {
                    job.state = TransferState::Skipped;
                }
                self.finalize_plan(transfer_id);
            }
            AppEvent::TransferFailed(failure) => {
                if let Some(job) = self
                    .state
                    .transfers
                    .jobs
                    .iter_mut()
                    .find(|job| job.id == failure.transfer_id)
                {
                    job.state = TransferState::Failed {
                        retryable: failure.error.retryable,
                        error: failure.error,
                    };
                }
                self.finalize_plan(failure.transfer_id);
            }
            AppEvent::ResidualTempCreated(record) => {
                self.residual_temps.add(record);
                if let Err(error) = self.residual_temps.save() {
                    warn!(error = %error, "could not persist residual temp record");
                }
            }
            AppEvent::ResidualTempCleared { transfer_id, path } => {
                self.residual_temps.remove(transfer_id, &path);
                if let Err(error) = self.residual_temps.save() {
                    warn!(error = %error, "could not update residual temp store");
                }
            }
            _ => {}
        }
        cx.notify();
    }
}
