use gpui::Context;
use macsftp_core::{
    AppCommand, ConflictPolicy, EditPhase, EditSession, LocalPath, MetadataPolicy, ProfileId,
    RemotePath, RemoteSnapshot, StartTransferCommand, TabId, Timestamp, TransferDirection,
    TransferEndpoint,
};

use crate::resources::ActiveResources;
use crate::workspace::Workspace;
use crate::workspace::helpers::connected_transfer_session;

pub(crate) const EDIT_SIZE_WARN_THRESHOLD: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct PendingEdit {
    pub remote_path: RemotePath,
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub tab_id: TabId,
}

impl Workspace {
    #[allow(dead_code)] // Production call sites are wired in Task 12; tests exercise it now.
    pub(crate) fn begin_edit(
        &mut self,
        remote_path: RemotePath,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before editing".into());
            cx.notify();
            return;
        };
        let tab_id = tab.id;
        // 查重：同 profile + 远程路径已有活跃会话 → 复用，不重复下载。
        if cx
            .resources()
            .edit_sessions
            .find_active(profile_id, &remote_path)
            .is_some()
        {
            self.status_message = Some("This file is already open for editing".into());
            cx.notify();
            return;
        }
        let pending = PendingEdit {
            remote_path,
            size,
            modified_at,
            session_epoch,
            profile_id,
            tab_id,
        };
        if pending.size.unwrap_or(0) > EDIT_SIZE_WARN_THRESHOLD {
            self.large_edit_confirm = Some(pending);
            cx.notify();
            return;
        }
        self.start_edit_download(pending, cx);
    }

    #[allow(dead_code)] // Called by begin_edit and (Task 12) the large-file confirm handler.
    pub(crate) fn start_edit_download(&mut self, pending: PendingEdit, cx: &mut Context<Self>) {
        let edits_dir = cx.resources().app_paths.edits_dir.clone();
        let id = cx.resources_mut().edit_sessions.next_id();
        let file_name = std::path::Path::new(pending.remote_path.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let local_temp_path =
            LocalPath::new(format!("{}/{}/{}", edits_dir.as_str(), id.0, file_name));
        // 建会话目录（忽略已存在）。
        if let Some(parent) = std::path::Path::new(local_temp_path.as_str()).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let session = EditSession {
            id,
            remote_path: pending.remote_path.clone(),
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            local_temp_path: local_temp_path.clone(),
            phase: EditPhase::Downloading,
            remote_snapshot: RemoteSnapshot {
                size: pending.size,
                modified_at: pending.modified_at,
            },
            local_mtime: None,
            active_transfer: None,
        };
        cx.resources_mut().edit_sessions.register(session);
        let command = AppCommand::StartTransfer(StartTransferCommand {
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(pending.remote_path)],
            destination: TransferEndpoint::Local(local_temp_path),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
        });
        if self.send_command(command, cx) {
            self.status_message = Some("Opening for edit…".into());
            cx.notify();
        }
    }
}

/// Build the `StartTransfer(Upload)` command that sends an edited temp file
/// back to its remote origin. Mirrors the inline download command in
/// [`Workspace::start_edit_download`]; factored out so the edit watcher (and
/// Task 11's conflict modal) share one construction site rather than
/// duplicating the struct literal.
pub(crate) fn build_edit_upload_command(
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
        conflict_policy: ConflictPolicy::default(),
    })
}
