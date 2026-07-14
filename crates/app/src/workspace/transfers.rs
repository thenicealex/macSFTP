#![allow(unused_imports)]
use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, Hsla, IntoElement,
    KeyDownEvent, ParentElement, Render, ScrollStrategy, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use macsftp_core::{
    AppCommand, AppEvent, AppState, AuthCredential, AuthMethod, AuthMethodKind,
    CommandDispatchError, ConflictDecision, ConflictDecisionCommand, ConflictRequest,
    ConflictRequestId, ConnectCommand, ConnectionProfile, ConnectionSettings, ConnectionState,
    DisconnectReason, EntryPath, ErrorCode, FileKind, HostKeyDecisionCommand, HostKeyPrompt,
    LocalPath, ModalRequest, ModalRequestId, ProfileId, RemotePath, SecretRef, TabId, TabState,
    Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferJob,
    TransferState, TrustRequestId, UserFacingError, sort_entries,
};
use macsftp_platform::{AppPaths, read_local_directory};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::{
    AppearancePreference, ConfigStore, KeychainError, KeychainStore, ProfileStore,
    ResidualTempStore,
};
use macsftp_ui::{
    ActiveTheme, DragPreview, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel,
    Theme, TransferRow, connection_status, copy_name, empty_state, file_row, file_table_header,
    format_size, format_timestamp, icon, icon_button, section_header_static, tab, text_button,
    text_field, text_tooltip, transfer_row, transfer_title,
};

use tracing::{debug, warn};

use crate::app_actions::*;
use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::connect_form::*;
use crate::workspace::helpers::*;
use crate::workspace::*;

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
        if let Some(job) = cx
            .transfers_mut()
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
    pub(crate) fn retry_transfer(
        &mut self,
        transfer_id: macsftp_core::TransferId,
        cx: &mut Context<Self>,
    ) {
        self.send_command(AppCommand::RetryTransfer { transfer_id }, cx);
    }
    pub(crate) fn finalize_plan(
        &mut self,
        _job_id: macsftp_core::TransferId,
        cx: &mut Context<Self>,
    ) {
        let Some(plan_id) = cx
            .transfers()
            .plans
            .iter()
            .find_map(|plan| plan.child_jobs.contains(&_job_id).then_some(plan.id))
        else {
            return;
        };
        let Some(plan) = cx.transfers().plans.iter().find(|plan| plan.id == plan_id) else {
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
        let child_jobs = plan.child_jobs.clone();
        let root_job_id = plan.root_job_id;
        let mut any_failed = false;
        for child_id in &child_jobs {
            let Some(job) = cx.transfers().jobs.iter().find(|job| job.id == *child_id) else {
                return;
            };
            match job.state {
                TransferState::Completed | TransferState::Skipped => {}
                TransferState::Failed { .. } => any_failed = true,
                _ => return,
            }
        }

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
        if let Some(plan) = cx
            .transfers_mut()
            .plans
            .iter_mut()
            .find(|plan| plan.id == plan_id)
        {
            plan.state = plan_state;
        }
        if let Some(root_job) = cx
            .transfers_mut()
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
