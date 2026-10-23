use gpui_kit::Context;
use macsftp_core::{
    AppCommand, EntryPath, FileKind, LocalPath, RemotePath, TabId, TransferEndpoint, TransferId,
    TransferState,
};

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::helpers::*;

impl crate::workspace::Workspace {
    pub(crate) fn begin_upload(&mut self, sources: Vec<LocalPath>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect to the server before uploading.".into());
            cx.notify();
            return;
        };
        let Some(remote_directory) = tab.remote.path.clone() else {
            self.status_message = Some("Choose a destination folder on the server.".into());
            cx.notify();
            return;
        };
        if sources.is_empty() {
            self.status_message = Some("Select files or folders on your Mac to upload.".into());
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
                        Some("The selected upload item has no usable name.".into());
                    cx.notify();
                    return;
                }
            }
        } else {
            remote_directory
        };
        let tab_id = tab.id;
        let metadata_policy = cx.resources().config.config().metadata_policy.clone();
        let command = AppCommand::StartTransfer(macsftp_core::StartTransferCommand {
            tab_id,
            session_epoch,
            profile_id,
            direction: macsftp_core::TransferDirection::Upload,
            sources: sources.into_iter().map(TransferEndpoint::Local).collect(),
            destination: TransferEndpoint::Remote(destination),
            metadata_policy,
            conflict_policy: Default::default(),
        });
        if self.send_command(command, cx) {
            self.transfer_drawer.open = true;
            self.status_message = Some("Preparing upload…".into());
            cx.notify();
        }
    }
    pub(crate) fn begin_download(&mut self, sources: Vec<RemotePath>, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect to the server before downloading.".into());
            cx.notify();
            return;
        };
        let Some(local_directory) = tab.local.path.clone() else {
            self.status_message = Some("Choose a destination folder on your Mac.".into());
            cx.notify();
            return;
        };
        if sources.is_empty() {
            self.status_message = Some("Select files or folders on the server to download.".into());
            cx.notify();
            return;
        }

        // Every source becomes its own single-source plan, so each plan's
        // destination is the exact local path for that item: one file keeps its
        // own name, and a folder recreates its tree under a matching name.
        let destination_for = |source: &RemotePath| -> Result<LocalPath, &'static str> {
            append_local_name(&local_directory, source)
        };
        let tab_id = tab.id;
        let metadata_policy = cx.resources().config.config().metadata_policy.clone();

        // Download planning is per-source: the remote planner resolves a single
        // remote tree against one local destination. Several selected items
        // therefore become several plans, queued in selection order, where an
        // upload hands the whole list to one plan (local traversal needs no
        // connection). Both shapes reach the drawer as ordinary rows.
        let mut queued = 0;
        for source in sources {
            let destination = match destination_for(&source) {
                Ok(destination) => destination,
                Err(message) => {
                    self.status_message = Some(message.into());
                    cx.notify();
                    return;
                }
            };
            let command = AppCommand::StartTransfer(macsftp_core::StartTransferCommand {
                tab_id,
                session_epoch,
                profile_id,
                direction: macsftp_core::TransferDirection::Download,
                sources: vec![TransferEndpoint::Remote(source)],
                destination: TransferEndpoint::Local(destination),
                metadata_policy: metadata_policy.clone(),
                conflict_policy: Default::default(),
            });
            if !self.send_command(command, cx) {
                break;
            }
            queued += 1;
        }
        if queued > 0 {
            self.transfer_drawer.open = true;
            self.status_message = Some(if queued == 1 {
                "Preparing download…".into()
            } else {
                format!("Preparing {queued} downloads…").into()
            });
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
    pub(crate) fn cancel_transfer(
        &mut self,
        transfer_id: macsftp_core::TransferId,
        cx: &mut Context<Self>,
    ) {
        let plan_id = cx.transfers().plan_id_for_job(transfer_id);
        if self.send_command(
            AppCommand::CancelTransfer {
                transfer_id,
                plan_id,
            },
            cx,
        ) {
            cx.mark_transfer_cancelling(transfer_id);
            cx.notify();
        }
    }
    /// Retries the job's original plan, including the metadata and conflict
    /// policies captured when it started; a Settings change applies only to
    /// new transfers.
    pub(crate) fn retry_transfer(
        &mut self,
        transfer_id: macsftp_core::TransferId,
        cx: &mut Context<Self>,
    ) {
        self.send_command(AppCommand::RetryTransfer { transfer_id }, cx);
    }
    pub(crate) fn clean_remote_residual_temps(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let connection_key = {
            let Some(tab) = self.state.tabs.find_tab(tab_id) else {
                return;
            };
            let Some(profile_id) = tab.profile_id else {
                return;
            };
            let Some(profile) = cx.resources().profiles.find_profile(profile_id) else {
                return;
            };
            format!("{}:{}", profile.host, profile.port)
        };
        let pending: Vec<_> = cx
            .resources()
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
    pub(crate) fn cancel_all_transfers(&mut self, cx: &mut Context<Self>) {
        let job_ids: Vec<TransferId> =
            crate::workspace::transfer_render::visible_transfer_jobs(cx.transfers())
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        TransferState::Queued
                            | TransferState::Running { .. }
                            | TransferState::Planning
                            | TransferState::WaitingForConflictDecision { .. }
                    )
                })
                .map(|job| job.id)
                .collect();
        for id in job_ids {
            self.cancel_transfer(id, cx);
        }
    }
    pub(crate) fn clear_transfer_records(&mut self, cx: &mut Context<Self>) {
        if cx.clear_terminal_transfers() {
            self.transfer_drawer.completed_section_expanded = false;
            self.transfer_drawer.failed_section_expanded = false;
            cx.notify();
        }
    }
}
