//! One executor for the side effects produced by the remote-edit lifecycle.
//!
//! `macsftp_core::remote_edit` owns every phase transition and returns
//! [`EditEffect`] values. The app executes them on two paths: a user action in
//! one window, and a process-wide event applied by [`crate::event_coordinator`].
//! Both paths share the policy below — dispatch-failure rollback, editor
//! resolution, notice choice, temp cleanup, listing rebase — and differ only in
//! their environment, which the [`EditEffectHost`] seam supplies:
//!
//! - [`crate::workspace::Workspace`] executes effects for its own tab;
//! - [`crate::event_coordinator`] executes effects that arrive process-wide and
//!   must first resolve the window that owns the effect's tab.
//!
//! The seam has two real adapters, so it is not hypothetical: the executor
//! never learns whether a window borrowed itself or was discovered.

use macsftp_core::{
    AppCommand, DispatchRollback, EditEffect, EditNotice, RemotePath, RemoteSnapshot, TabId,
};
use tracing::warn;

use crate::resources::AppResources;

/// The environment one executor run needs from its adapter.
pub(crate) trait EditEffectHost {
    fn resources(&self) -> &AppResources;
    fn resources_mut(&mut self) -> &mut AppResources;
    /// Whether `command` entered the runtime channel.
    fn dispatch(&mut self, tab_id: TabId, command: AppCommand) -> bool;
    /// Surface `notice` in the window that owns `tab_id`.
    fn notify(&mut self, tab_id: TabId, notice: EditNotice);
    /// Rebase the owning tab's listing entry. Returns whether it did.
    fn sync_listing(&mut self, tab_id: TabId, path: &RemotePath, snapshot: RemoteSnapshot) -> bool;
}

/// Execute lifecycle effects through `host`.
///
/// A failed dispatch rolls the session back through the lifecycle (the effect
/// names its rollback), so a command that never entered the channel cannot
/// strand a phase. A rollback can itself schedule cleanup (an aborted
/// download's temp directory); those follow-up effects never dispatch and are
/// executed by the same policy.
pub(crate) fn execute_edit_effects(host: &mut impl EditEffectHost, effects: Vec<EditEffect>) {
    for effect in effects {
        match effect {
            EditEffect::Dispatch {
                tab_id,
                session_id,
                command,
                rollback,
            } => {
                if host.dispatch(tab_id, command) {
                    continue;
                }
                warn!(session_id = ?session_id, "edit command could not be dispatched");
                let file_name = host
                    .resources()
                    .edit_sessions
                    .session(session_id)
                    .map(|session| file_name_of(&session.remote_path))
                    .unwrap_or_default();
                let follow_up = host
                    .resources_mut()
                    .edit_sessions
                    .dispatch_failed(session_id, rollback);
                let notice = match rollback {
                    DispatchRollback::AbortDownload => EditNotice::DownloadFailed { file_name },
                    DispatchRollback::RevertCheck | DispatchRollback::RestoreConflict => {
                        EditNotice::UploadStartFailed
                    }
                };
                host.notify(tab_id, notice);
                execute_edit_effects(host, follow_up);
            }
            EditEffect::OpenEditor { tab_id, temp_path } => {
                let opened = {
                    let config = host.resources().config.config();
                    let file_name = file_name_of_local(&temp_path);
                    let editor = macsftp_core::resolve_editor_app(
                        &file_name,
                        &config.editor_rules,
                        config.external_editor.as_deref(),
                    )
                    .map(str::to_string);
                    crate::event_coordinator::open_edit_temp(&temp_path, editor.as_deref())
                };
                match opened {
                    Ok(()) => host.notify(tab_id, EditNotice::CopyOpen),
                    Err(error) => {
                        warn!(error = %error, "could not open editor for remote edit");
                        host.notify(tab_id, EditNotice::CopyOpenFailed);
                    }
                }
            }
            EditEffect::DeleteTempDir { path } => crate::workspace::cleanup_edit_temp_dir(&path),
            EditEffect::RebaseListing {
                tab_id,
                path,
                snapshot,
            } => {
                host.sync_listing(tab_id, &path, snapshot);
            }
            EditEffect::Notice { tab_id, notice } => host.notify(tab_id, notice),
        }
    }
}

/// The trailing path component of a remote path, for user-facing messages.
fn file_name_of(remote_path: &RemotePath) -> String {
    std::path::Path::new(remote_path.as_str())
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// The trailing path component of a local temp path, for editor matching.
fn file_name_of_local(path: &macsftp_core::LocalPath) -> String {
    std::path::Path::new(path.as_str())
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{
        ConnectionKey, ConnectionPoolIdentity, EditPhase, EditSession, EditSessionId, LocalPath,
        ProfileId, SessionId, Timestamp, TransferId,
    };
    use macsftp_platform::AppPaths;
    use macsftp_storage::ConfigStore;

    struct RecordingHost {
        resources: AppResources,
        dispatch_result: bool,
        dispatched: Vec<(TabId, AppCommand)>,
        notices: Vec<(TabId, EditNotice)>,
        synced: Vec<(TabId, RemotePath, RemoteSnapshot)>,
    }

    impl EditEffectHost for RecordingHost {
        fn resources(&self) -> &AppResources {
            &self.resources
        }

        fn resources_mut(&mut self) -> &mut AppResources {
            &mut self.resources
        }

        fn dispatch(&mut self, tab_id: TabId, command: AppCommand) -> bool {
            self.dispatched.push((tab_id, command));
            self.dispatch_result
        }

        fn notify(&mut self, tab_id: TabId, notice: EditNotice) {
            self.notices.push((tab_id, notice));
        }

        fn sync_listing(
            &mut self,
            tab_id: TabId,
            path: &RemotePath,
            snapshot: RemoteSnapshot,
        ) -> bool {
            self.synced.push((tab_id, path.clone(), snapshot));
            true
        }
    }

    fn test_host(dispatch_result: bool) -> RecordingHost {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let home = format!(
            "{}/macsftp-edit-effects-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            seq
        );
        let app_paths = AppPaths::from_home_dir(&home);
        let _ = app_paths.ensure_directories();
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        RecordingHost {
            resources: AppResources::load_for_test(app_paths, config),
            dispatch_result,
            dispatched: Vec::new(),
            notices: Vec::new(),
            synced: Vec::new(),
        }
    }

    fn snapshot() -> RemoteSnapshot {
        RemoteSnapshot {
            size: Some(10),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
        }
    }

    fn connection_key() -> ConnectionKey {
        ConnectionKey {
            host: "srv.example.com".into(),
            port: 22,
            username: "alex".into(),
            identity: ConnectionPoolIdentity::Ephemeral(SessionId(1)),
        }
    }

    /// Register a session in `phase` with a temp path under the app's edits
    /// directory, and create that directory so cleanup is observable.
    fn seed_session(host: &mut RecordingHost, phase: EditPhase) -> (EditSessionId, LocalPath) {
        let id = host.resources.edit_sessions.next_id();
        let temp_path = LocalPath::new(format!(
            "{}/{}/{}/notes.txt",
            host.resources.app_paths.edits_dir.as_str(),
            host.resources.edit_run_id,
            id.0
        ));
        let parent = std::path::Path::new(temp_path.as_str())
            .parent()
            .expect("edit temp has a parent directory");
        std::fs::create_dir_all(parent).expect("create edit temp directory");
        host.resources.edit_sessions.register(EditSession {
            id,
            remote_path: RemotePath::new("/srv/notes.txt"),
            io_path: RemotePath::new("/srv/notes.txt"),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            connection_key: connection_key(),
            local_temp_path: temp_path.clone(),
            phase,
            remote_snapshot: snapshot(),
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        (id, temp_path)
    }

    fn cancel_command() -> AppCommand {
        AppCommand::CancelTransfer {
            transfer_id: TransferId(1),
            plan_id: None,
        }
    }

    #[test]
    fn failed_download_dispatch_rolls_back_and_cleans_temp_dir() {
        let mut host = test_host(false);
        let (session_id, temp_path) = seed_session(&mut host, EditPhase::Downloading);

        execute_edit_effects(
            &mut host,
            vec![EditEffect::Dispatch {
                tab_id: TabId(1),
                session_id,
                command: cancel_command(),
                rollback: DispatchRollback::AbortDownload,
            }],
        );

        assert_eq!(
            host.notices,
            vec![(
                TabId(1),
                EditNotice::DownloadFailed {
                    file_name: "notes.txt".into()
                }
            )]
        );
        assert!(
            host.resources.edit_sessions.session(session_id).is_none(),
            "an aborted download must remove the session"
        );
        let session_dir = std::path::Path::new(temp_path.as_str())
            .parent()
            .expect("edit temp has a parent directory");
        assert!(
            !session_dir.exists(),
            "the rollback's DeleteTempDir follow-up must run"
        );
    }

    #[test]
    fn failed_upload_check_dispatch_surfaces_upload_start_failed() {
        let mut host = test_host(false);
        let (session_id, _temp_path) = seed_session(&mut host, EditPhase::Editing);

        execute_edit_effects(
            &mut host,
            vec![EditEffect::Dispatch {
                tab_id: TabId(1),
                session_id,
                command: cancel_command(),
                rollback: DispatchRollback::RevertCheck,
            }],
        );

        assert_eq!(
            host.notices,
            vec![(TabId(1), EditNotice::UploadStartFailed)]
        );
        assert!(
            host.resources.edit_sessions.session(session_id).is_some(),
            "a reverted check keeps the session"
        );
    }

    #[test]
    fn editor_launch_failure_surfaces_copy_open_failed() {
        crate::event_coordinator::set_edit_opener(|_temp, _editor| {
            Err(std::io::Error::other("editor unavailable"))
        });
        let mut host = test_host(true);
        let temp_path = LocalPath::new("/tmp/macsftp-edit-effects/notes.txt");

        execute_edit_effects(
            &mut host,
            vec![EditEffect::OpenEditor {
                tab_id: TabId(1),
                temp_path,
            }],
        );

        assert_eq!(host.notices, vec![(TabId(1), EditNotice::CopyOpenFailed)]);
    }

    #[test]
    fn rebase_listing_is_forwarded_to_the_host() {
        let mut host = test_host(true);

        execute_edit_effects(
            &mut host,
            vec![EditEffect::RebaseListing {
                tab_id: TabId(1),
                path: RemotePath::new("/srv/notes.txt"),
                snapshot: snapshot(),
            }],
        );

        assert_eq!(
            host.synced,
            vec![(TabId(1), RemotePath::new("/srv/notes.txt"), snapshot())]
        );
        assert!(host.notices.is_empty());
    }
}
