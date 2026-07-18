use std::collections::HashMap;

use gpui::{App, Global};
use macsftp_core::{AppCommand, TabId, WindowSessionId};
use macsftp_sftp::RuntimeClient;
use macsftp_storage::{SessionFile, SessionStore, SessionWindowSnapshot, StorageError};
use tracing::warn;

use crate::workspace::Workspace;

/// The sole owner and writer of `session.json` for the process.
pub struct SessionCoordinator {
    store: SessionStore,
    restore_pending: Option<SessionFile>,
    window_order: Vec<WindowSessionId>,
    next_window_id: u64,
    quitting: bool,
    runtime_sessions: HashMap<WindowSessionId, WindowRuntimeSessions>,
}

struct WindowRuntimeSessions {
    runtime_client: RuntimeClient,
    tab_ids: Vec<TabId>,
}

impl Global for SessionCoordinator {}

impl SessionCoordinator {
    pub fn new(store: SessionStore) -> Self {
        if store.initial_error().is_some() {
            warn!("session.json could not be loaded; session writes are blocked until recovery");
        }
        let restore_pending = store.file().clone();
        let next_window_id = restore_pending
            .windows
            .iter()
            .map(|window| window.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        Self {
            store,
            restore_pending: Some(restore_pending),
            window_order: Vec::new(),
            next_window_id,
            quitting: false,
            runtime_sessions: HashMap::new(),
        }
    }

    pub fn take_restore_session(&mut self) -> SessionFile {
        self.restore_pending
            .take()
            .unwrap_or_else(SessionFile::empty)
    }

    pub fn allocate_window_id(&mut self) -> WindowSessionId {
        let id = WindowSessionId(self.next_window_id);
        self.next_window_id = self.next_window_id.saturating_add(1);
        id
    }

    pub fn register_window(&mut self, id: WindowSessionId) {
        if !self.window_order.contains(&id) {
            self.window_order.push(id);
        }
    }

    pub fn register_runtime_tabs(
        &mut self,
        window_id: WindowSessionId,
        runtime_client: RuntimeClient,
        tab_ids: impl IntoIterator<Item = TabId>,
    ) {
        let sessions =
            self.runtime_sessions
                .entry(window_id)
                .or_insert_with(|| WindowRuntimeSessions {
                    runtime_client,
                    tab_ids: Vec::new(),
                });
        for tab_id in tab_ids {
            if !sessions.tab_ids.contains(&tab_id) {
                sessions.tab_ids.push(tab_id);
            }
        }
    }

    pub fn mark_runtime_tab_released(&mut self, window_id: WindowSessionId, tab_id: TabId) {
        if let Some(sessions) = self.runtime_sessions.get_mut(&window_id) {
            sessions.tab_ids.retain(|candidate| *candidate != tab_id);
        }
    }

    fn save(&mut self, snapshot: SessionFile, quitting: bool) -> Result<(), StorageError> {
        if self.quitting {
            return Ok(());
        }
        self.quitting = quitting;
        self.window_order = snapshot.windows.iter().map(|window| window.id).collect();
        self.store.replace(snapshot);
        if self.store.initial_error().is_some() {
            let backup_path = self.store.recover_and_save()?;
            if let Some(backup_path) = backup_path {
                warn!(
                    backup_path = backup_path.as_str(),
                    "recovered session.json after preserving unreadable input"
                );
            }
            Ok(())
        } else {
            self.store.save()
        }
    }
}

fn collect_session(cx: &App) -> SessionFile {
    let active_window = cx.active_window();
    let window_order = cx.global::<SessionCoordinator>().window_order.clone();
    let mut snapshots = cx
        .windows()
        .into_iter()
        .filter_map(|window| {
            let workspace = window.downcast::<Workspace>()?;
            let snapshot = workspace.read(cx).ok()?.build_session_snapshot();
            Some((window, snapshot))
        })
        .collect::<Vec<_>>();

    snapshots.sort_by_key(|(_, snapshot)| {
        window_order
            .iter()
            .position(|id| *id == snapshot.id)
            .unwrap_or(usize::MAX)
    });

    let active_window_id = active_window.and_then(|active| {
        snapshots
            .iter()
            .find(|(window, _)| *window == active)
            .map(|(_, snapshot)| snapshot.id)
    });
    let windows = snapshots
        .into_iter()
        .map(|(_, snapshot)| snapshot)
        .collect::<Vec<SessionWindowSnapshot>>();
    let active_window_index = active_window_id
        .and_then(|id| windows.iter().position(|window| window.id == id))
        .unwrap_or(0);

    SessionFile {
        version: SessionFile::CURRENT_VERSION,
        active_window_index,
        windows,
    }
}

/// Freeze all live windows before GPUI starts tearing them down.
pub fn checkpoint_before_quit(cx: &mut App) {
    if !cx.has_global::<SessionCoordinator>() {
        return;
    }
    let snapshot = collect_session(cx);
    if let Err(error) = cx.global_mut::<SessionCoordinator>().save(snapshot, true) {
        warn!(error = %error, "could not save session.json before quit");
    }
}

/// A normally closed window is already inaccessible here, so the snapshot
/// naturally contains only the surviving windows. After quit starts, the
/// frozen pre-teardown snapshot must not be overwritten.
pub fn checkpoint_after_window_closed(cx: &mut App) {
    if !cx.has_global::<SessionCoordinator>() || cx.global::<SessionCoordinator>().quitting {
        return;
    }
    let snapshot = collect_session(cx);
    release_closed_window_sessions(cx, &snapshot);
    if let Err(error) = cx.global_mut::<SessionCoordinator>().save(snapshot, false) {
        warn!(error = %error, "could not save session.json after window closed");
    }
}

fn release_closed_window_sessions(cx: &mut App, snapshot: &SessionFile) {
    let live_window_ids = snapshot
        .windows
        .iter()
        .map(|window| window.id)
        .collect::<Vec<_>>();
    let closed_window_ids = cx
        .global::<SessionCoordinator>()
        .runtime_sessions
        .keys()
        .filter(|window_id| !live_window_ids.contains(window_id))
        .copied()
        .collect::<Vec<_>>();
    let closed_sessions = {
        let coordinator = cx.global_mut::<SessionCoordinator>();
        closed_window_ids
            .into_iter()
            .filter_map(|window_id| coordinator.runtime_sessions.remove(&window_id))
            .collect::<Vec<_>>()
    };

    for sessions in closed_sessions {
        if sessions.tab_ids.is_empty() {
            continue;
        }
        cx.spawn(async move |_| {
            if let Err(error) = sessions
                .runtime_client
                .send_async(AppCommand::CloseTabs {
                    tab_ids: sessions.tab_ids,
                })
                .await
            {
                warn!(?error, "failed to release closed window browsing sessions");
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowHandle};
    use macsftp_core::{LocalPath, RemotePath, RuntimeBridgeConfig, WindowSessionId};
    use macsftp_platform::AppPaths;
    use macsftp_sftp::{BridgeChannels, RuntimeClient};
    use macsftp_storage::{ConfigStore, SessionFile, SessionStore};
    use macsftp_ui::Theme;

    use super::{SessionCoordinator, checkpoint_after_window_closed, checkpoint_before_quit};
    use crate::app_actions;
    use crate::resources::{AppResources, SharedTransfers};
    use crate::workspace::Workspace;

    fn setup_two_windows(
        cx: &mut TestAppContext,
        label: &str,
    ) -> (AppPaths, WindowHandle<Workspace>, WindowHandle<Workspace>) {
        let app_paths = test_app_paths(label);
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        let mut coordinator =
            SessionCoordinator::new(SessionStore::open_or_empty(app_paths.session_file.clone()));
        coordinator.register_window(WindowSessionId(10));
        coordinator.register_window(WindowSessionId(20));
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
            cx.set_global(coordinator);
            cx.set_global(AppResources::load_for_test(app_paths.clone(), config));
            cx.set_global(SharedTransfers::default());
            cx.on_window_closed(checkpoint_after_window_closed).detach();
        });

        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let first_client = RuntimeClient::new(channels.command_tx.clone());
        let second_client = RuntimeClient::new(channels.command_tx);
        let first = cx.add_window(|window, cx| {
            Workspace::new(first_client, WindowSessionId(10), None, window, cx)
        });
        let second = cx.add_window(|window, cx| {
            Workspace::new(second_client, WindowSessionId(20), None, window, cx)
        });

        first
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("first default tab");
                tab.title = "first.example".into();
                tab.local.path = Some(LocalPath::new("/tmp/first"));
                tab.remote.path = Some(RemotePath::new("/srv/first"));
            })
            .expect("first workspace should be open");
        second
            .update(cx, |workspace, _window, _cx| {
                let tab = workspace.active_tab_mut().expect("second default tab");
                tab.title = "second.example".into();
                tab.local.path = Some(LocalPath::new("/tmp/second"));
                tab.remote.path = Some(RemotePath::new("/srv/second"));
            })
            .expect("second workspace should be open");

        (app_paths, first, second)
    }

    #[gpui::test]
    fn quit_checkpoint_preserves_every_window_and_ignores_late_closes(cx: &mut TestAppContext) {
        let (app_paths, first, second) = setup_two_windows(cx, "quit-freeze");

        second
            .update(cx, |_workspace, window, _cx| window.activate_window())
            .expect("second window should activate");
        cx.update(checkpoint_before_quit);
        let saved = SessionStore::open(app_paths.session_file.clone())
            .expect("quit checkpoint should save session");
        assert_eq!(saved.file().windows.len(), 2);
        assert_eq!(saved.file().active_window_index, 1);
        assert_eq!(saved.file().windows[0].id, WindowSessionId(10));
        assert_eq!(saved.file().windows[0].tabs[0].title, "first.example");
        assert_eq!(
            saved.file().windows[0].tabs[0].remote_path.as_deref(),
            Some("/srv/first")
        );
        assert_eq!(saved.file().windows[1].id, WindowSessionId(20));
        assert_eq!(saved.file().windows[1].tabs[0].title, "second.example");

        first
            .update(cx, |workspace, window, _cx| {
                workspace.active_tab_mut().expect("first tab").title = "late-change".into();
                window.remove_window();
            })
            .expect("first window should close");
        second
            .update(cx, |_workspace, window, _cx| window.remove_window())
            .expect("second window should close");

        let after_closes = SessionStore::open(app_paths.session_file)
            .expect("frozen quit checkpoint should remain readable");
        assert_eq!(after_closes.file().windows.len(), 2);
        assert_eq!(
            after_closes.file().windows[0].tabs[0].title,
            "first.example",
            "window-close callbacks must not overwrite the pre-quit snapshot"
        );
    }

    #[gpui::test]
    fn normal_window_closes_save_only_survivors_then_empty(cx: &mut TestAppContext) {
        let (app_paths, first, second) = setup_two_windows(cx, "normal-close");

        first
            .update(cx, |_workspace, window, _cx| window.remove_window())
            .expect("first window should close");
        let one_window = SessionStore::open(app_paths.session_file.clone())
            .expect("normal close should checkpoint survivors");
        assert_eq!(one_window.file().windows.len(), 1);
        assert_eq!(one_window.file().windows[0].id, WindowSessionId(20));
        assert_eq!(one_window.file().windows[0].tabs[0].title, "second.example");

        second
            .update(cx, |_workspace, window, _cx| window.remove_window())
            .expect("last window should close");
        let empty = SessionStore::open(app_paths.session_file)
            .expect("last close should save an empty session");
        assert!(empty.file().windows.is_empty());
    }

    #[test]
    fn coordinator_recovers_corrupt_session_before_checkpointing() {
        let app_paths = test_app_paths("recover-corrupt");
        std::fs::write(app_paths.session_file.as_str(), "{not json")
            .expect("write corrupt session fixture");
        let mut coordinator =
            SessionCoordinator::new(SessionStore::open_or_empty(app_paths.session_file.clone()));

        coordinator
            .save(SessionFile::empty(), false)
            .expect("checkpoint should recover corrupt session");

        SessionStore::open(app_paths.session_file.clone())
            .expect("checkpoint should replace session.json with valid data");
        assert_eq!(
            std::fs::read_to_string(format!("{}.corrupt", app_paths.session_file.as_str()))
                .expect("read preserved corrupt session"),
            "{not json"
        );
    }

    fn test_app_paths(label: &str) -> AppPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "macsftp-session-coordinator-{label}-{}-{sequence}",
            std::process::id()
        ));
        let app_paths = AppPaths::from_home_dir(home.to_string_lossy().as_ref());
        app_paths
            .ensure_directories()
            .expect("test app directories should be created");
        app_paths
    }
}
