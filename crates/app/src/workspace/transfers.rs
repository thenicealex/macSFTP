use gpui::Context;
use macsftp_core::{
    AppCommand, EntryPath, FileKind, LocalPath, RemotePath, TabId, TransferEndpoint,
};

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::helpers::*;

impl crate::workspace::Workspace {
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
    pub(crate) fn cancel_transfer(
        &mut self,
        transfer_id: macsftp_core::TransferId,
        cx: &mut Context<Self>,
    ) {
        cx.mark_transfer_cancelling(transfer_id);
        self.send_command(AppCommand::CancelTransfer { transfer_id }, cx);
        cx.notify();
    }
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
}
