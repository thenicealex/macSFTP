#![allow(unused_imports)]
#![allow(clippy::module_inception)]
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
    Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferHistoryId,
    TransferHistoryRecord, TransferHistoryStatus, TransferJob, TransferState, TrustRequestId,
    UserFacingError, history_status_for_plan, sort_entries,
};
use macsftp_platform::{AppPaths, read_local_directory};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::{
    AppearancePreference, ConfigStore, KeychainError, KeychainStore, ProfileStore,
    ResidualTempStore, TransferHistoryStore,
};
use macsftp_ui::{
    ActiveTheme, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel, Theme,
    DragPreview, connection_status, copy_name, section_header_static, transfer_history_detail,
    transfer_history_title, transfer_title,
    TransferRow, empty_state, file_row, file_table_header, format_size, format_timestamp, icon,
    icon_button, tab, text_button, text_field, text_tooltip, transfer_row,
};

use tracing::{debug, warn};

use crate::app_actions::*;
use crate::workspace::*;
use crate::workspace::helpers::*;
use crate::workspace::connect_form::*;

#[cfg(test)]
mod tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use macsftp_core::{
        AppCommand, AppEvent, AuthCredential, AuthMethodKind, ConflictDecision, ConflictPolicy,
        ConflictRequestId, ConnectionSettings, ConnectionState, DisconnectReason, EntryPath,
        ErrorCode, FileKind, HostKeyPrompt, LocalPath, MetadataPolicy, ProfileId,
        RemoteDirSnapshot, RemoteEntry, RemoteEventScope, RemoteOperationFailure, RemotePath,
        RemoteScoped, RuntimeBridgeConfig, SessionId, TabConnected, TabDisconnected, TabId,
        Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint,
        TransferHistoryRecord, TransferHistoryStatus, TransferId, TransferJob, TransferPlanId,
        TransferPlanProgress, TransferPlanSnapshot, TransferPlanState, TransferState,
        TrustRequestId, UserFacingError,
    };
    use macsftp_sftp::{BridgeChannels, EventReceiver, RuntimeClient};
    use macsftp_storage::AppearancePreference;
    use macsftp_ui::{Appearance, Theme};

    use super::{AppPaths, PaneSide, Workspace, WorkspaceSurface};
    use crate::app_actions::{
        self, ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, NewTab, OpenSettings,
        SelectNextEntry, SelectPrevEntry, ShowAbout, ShowTransferDrawer,
    };

    const TEST_REMOTE_ROOT: &str = "/home/tester";

    fn init_workspace(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, VisualTestContext, BridgeChannels) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
        });
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let receiver = EventReceiver::new(channels.event_rx.clone());
        let app_paths = temp_app_paths();
        let config = macsftp_storage::ConfigStore::with_defaults(app_paths.config_file.clone());
        let window = cx.add_window(|window, cx| {
            Workspace::new(
                client,
                receiver,
                app_paths,
                config,
                macsftp_storage::KeychainStore::new_memory(),
                window,
                cx,
            )
        });
        let workspace = window
            .root(cx)
            .expect("workspace root view should be available");
        let visual_cx = VisualTestContext::from_window(window.into(), cx);
        (workspace, visual_cx, channels)
    }

    /// A unique temp `AppPaths` per call so parallel tests never share a
    /// `profiles.json` (plan §9: tests must not clobber each other's files).
    fn temp_app_paths() -> AppPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let home = format!(
            "{}/macsftp-test-home-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            seq
        );
        let app_paths = AppPaths::from_home_dir(&home);
        // Mirror production main.rs: create the app support dirs so the
        // profiles.json write succeeds (otherwise save hits ENOENT).
        let _ = app_paths.ensure_directories();
        app_paths
    }

    fn test_settings() -> ConnectionSettings {
        ConnectionSettings {
            host: "mock.example.com".to_string(),
            port: 22,
            username: "tester".to_string(),
            auth: AuthCredential::Password {
                password: "secret".to_string(),
            },
        }
    }

    /// Drive the mock connect flow up to the host key prompt: send the
    /// connect command and inject the actor's `HostKeyUnknown` event.
    fn connect_and_prompt(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> HostKeyPrompt {
        workspace.update_in(cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let prompt = HostKeyPrompt {
            request_id: TrustRequestId(1),
            tab_id: TabId(1),
            session_id: SessionId(1),
            session_epoch: 1,
            host: "mock.example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:mock".to_string(),
        };
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::HostKeyUnknown(prompt.clone()), window, cx);
        });
        prompt
    }

    #[gpui::test]
    fn new_tab_and_close_tab_actions_manage_tab_store(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 1, "starts with one tab");
            assert_eq!(workspace.state.tabs.active_tab_id, Some(TabId(1)));
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Empty,
                "new tabs start disconnected"
            );
        });

        cx.dispatch_action(NewTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 2);
            assert_eq!(
                workspace.state.tabs.active_tab_id,
                Some(TabId(2)),
                "new tab becomes active"
            );
        });

        cx.dispatch_action(CloseTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 1);
            assert_eq!(
                workspace.state.tabs.active_tab_id,
                Some(TabId(1)),
                "closing the active tab activates the remaining tab"
            );
        });

        cx.dispatch_action(CloseTab);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.tabs.tabs.is_empty());
            assert_eq!(workspace.state.tabs.active_tab_id, None);
        });
    }

    #[gpui::test]
    fn tab_switching_cycles_through_tabs(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        cx.dispatch_action(NewTab); // tabs: 1, 2 — active 2

        cx.dispatch_action(ActivateNextTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.active_tab_id, Some(TabId(1)));
        });

        cx.dispatch_action(ActivatePrevTab);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.active_tab_id, Some(TabId(2)));
        });
    }

    #[gpui::test]
    fn keyboard_shortcut_opens_new_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.simulate_keystrokes("cmd-t");
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(
                workspace.state.tabs.tabs.len(),
                2,
                "cmd-t must dispatch NewTab via the Workspace key context"
            );
        });
    }

    #[gpui::test]
    fn keyboard_selection_moves_through_local_entries(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        // Point the local pane at a real, deterministic directory so the
        // selection indices are stable (directory-first sort: beta, alpha, gamma).
        let dir = std::env::temp_dir().join("macsftp_sel_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("alpha.txt"), b"a").expect("write alpha.txt");
        std::fs::create_dir(dir.join("beta")).expect("create beta dir");
        std::fs::write(dir.join("gamma.txt"), b"g").expect("write gamma.txt");
        let local_root = LocalPath::new(dir.to_string_lossy().into_owned());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(local_root.clone(), window, cx);
        });

        cx.dispatch_action(SelectNextEntry);
        cx.dispatch_action(SelectNextEntry);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.focused_side, PaneSide::Local);
            let tab = workspace
                .state
                .tabs
                .active_tab()
                .expect("active tab must exist");
            let expected = EntryPath::Local(tab.local.entries[1].path.clone());
            assert_eq!(tab.selection.selected_paths, vec![expected]);
        });

        cx.dispatch_action(SelectPrevEntry);
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace
                .state
                .tabs
                .active_tab()
                .expect("active tab must exist");
            let expected = EntryPath::Local(tab.local.entries[0].path.clone());
            assert_eq!(tab.selection.selected_paths, vec![expected]);
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn transfer_drawer_toggles_via_action(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.drawer_open, "drawer starts open");
        });

        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(!workspace.drawer_open);
        });

        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.drawer_open);
        });
    }

    #[gpui::test]
    fn settings_action_switches_surface_and_persists_appearance(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(OpenSettings);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.surface, WorkspaceSurface::Settings);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_appearance(AppearancePreference::Light, window, cx);
        });
        workspace.read_with(&cx, |workspace, cx| {
            assert_eq!(
                workspace.config.config().appearance,
                AppearancePreference::Light
            );
            assert_eq!(cx.global::<Theme>().appearance, Appearance::Light);
        });

        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.surface, WorkspaceSurface::Files);
        });
    }

    #[gpui::test]
    fn about_action_opens_and_escape_closes_overlay(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(ShowAbout);
        workspace.read_with(&cx, |workspace, _| assert!(workspace.about_open));

        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| assert!(!workspace.about_open));
    }

    // ── Runtime bridge wiring (M2a/M2c app side) ───────────────────

    #[gpui::test]
    fn connect_sends_command_with_ui_allocated_session(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });

        // The command carries the identity the tab now exposes.
        let command = channels
            .command_rx
            .try_recv()
            .expect("connect command must be enqueued");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.tab_id, TabId(1));
                assert_eq!(connect.session_id, SessionId(1));
                assert_eq!(connect.session_epoch, 1);
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.session_epoch, 1);
            assert_eq!(
                tab.connection,
                ConnectionState::Connecting {
                    session_id: SessionId(1),
                    session_epoch: 1,
                }
            );
        });

        // A second connect while in flight must be a no-op.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no duplicate connect while a session attempt is in flight"
        );
    }

    #[gpui::test]
    fn host_key_prompt_opens_modal_and_accept_returns_to_connecting(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = connect_and_prompt(&workspace, &mut cx);
        let _ = channels.command_rx.try_recv(); // drop the ConnectTab command

        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.modals.active.len(), 1, "modal is open");
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::AwaitingHostKey {
                    session_id: SessionId(1),
                    session_epoch: 1,
                    request_id: prompt.request_id,
                }
            );
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.accept_host_key(prompt.request_id, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("accept command must be enqueued");
        assert!(
            matches!(command, AppCommand::AcceptHostKey(decision) if decision.request_id == prompt.request_id),
            "expected AcceptHostKey with the modal's request id"
        );
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.modals.active.is_empty(), "modal is closed");
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Connecting {
                    session_id: SessionId(1),
                    session_epoch: 1,
                }
            );
        });
    }

    #[gpui::test]
    fn reject_host_key_disconnects_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = connect_and_prompt(&workspace, &mut cx);
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.reject_host_key(prompt.request_id, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("reject command must be enqueued");
        assert!(matches!(
            command,
            AppCommand::RejectHostKey { request_id } if request_id == prompt.request_id
        ));
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.modals.active.is_empty());
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Disconnected {
                    reason: DisconnectReason::UserRequested,
                }
            );
        });
    }

    #[gpui::test]
    fn custom_conflict_rename_validates_and_sends_plan_scoped_decision(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = TransferConflictPrompt {
            request_id: ConflictRequestId(7),
            plan_id: TransferPlanId(3),
            transfer_id: TransferId(9),
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source.txt")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/existing.txt")),
            source_size: Some(11),
            source_modified_at: Some(macsftp_core::Timestamp::from_secs_since_epoch(42)),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::TransferConflict(prompt.clone()), window, cx);
            assert_eq!(workspace.conflict_rename.value(), "existing (copy).txt");
            let pending = workspace
                .state
                .transfers
                .pending_conflicts
                .last()
                .expect("conflict should be recorded for the drawer");
            assert_eq!(pending.source_size, Some(11));
            assert_eq!(
                pending.source_modified_at,
                Some(macsftp_core::Timestamp::from_secs_since_epoch(42))
            );

            workspace.conflict_rename.set_value("..");
            workspace.submit_transfer_rename(false, window, cx);
            assert!(workspace.conflict_rename_error.is_some());
            assert!(workspace.active_transfer_conflict_prompt().is_some());

            workspace.conflict_rename.set_value("renamed.txt");
            workspace.submit_transfer_rename(true, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("custom rename decision must be enqueued");
        let AppCommand::ResolveTransferConflict(command) = command else {
            panic!("expected custom rename decision");
        };
        assert_eq!(command.plan_id, TransferPlanId(3));
        assert_eq!(command.request_id, ConflictRequestId(7));
        assert_eq!(
            command.decision,
            ConflictDecision::Rename {
                new_name: "renamed.txt".to_string(),
                apply_to_all: true,
            }
        );
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.modals.active.is_empty());
            assert!(workspace.conflict_rename_error.is_none());
        });
    }

    #[gpui::test]
    fn stale_host_key_prompt_is_dropped(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        // A prompt from a previous epoch (0) must not open a modal.
        let stale_prompt = HostKeyPrompt {
            request_id: TrustRequestId(99),
            tab_id: TabId(1),
            session_id: SessionId(9),
            session_epoch: 0,
            host: "mock.example.com".to_string(),
            port: 22,
            algorithm: "ssh-ed25519".to_string(),
            fingerprint_sha256: "SHA256:stale".to_string(),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::HostKeyUnknown(stale_prompt), window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.state.modals.active.is_empty(),
                "stale prompt must not open a modal"
            );
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert!(
                matches!(tab.connection, ConnectionState::Connecting { .. }),
                "connection state must be untouched"
            );
        });
    }

    #[gpui::test]
    fn tab_connected_event_requests_the_real_remote_root(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert!(matches!(tab.connection, ConnectionState::Connected { .. }));
            assert!(tab.remote.entries.is_empty());
            assert_eq!(tab.remote.path, Some(RemotePath::new(TEST_REMOTE_ROOT)));
            assert!(tab.remote.is_refreshing);
        });

        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new(TEST_REMOTE_ROOT)
        ));
    }

    #[gpui::test]
    fn drag_local_file_onto_remote_pane_begins_upload(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        // TabConnected also enqueues a ReadRemoteDir; drain it so the next
        // receive is the transfer we trigger.
        while channels.command_rx.try_recv().is_ok() {}

        // The drop handler on the remote pane delegates to begin_upload with
        // the dragged local path.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_upload(vec![LocalPath::new("/tmp/foo.txt")], cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("drag-to-upload must enqueue a StartTransfer");
        match command {
            AppCommand::StartTransfer(cmd) => {
                assert_eq!(cmd.direction, TransferDirection::Upload);
                assert_eq!(
                    cmd.sources,
                    vec![TransferEndpoint::Local(LocalPath::new("/tmp/foo.txt"))]
                );
                assert_eq!(
                    cmd.destination,
                    TransferEndpoint::Remote(RemotePath::new("/home/tester/foo.txt"))
                );
            }
            other => panic!("expected StartTransfer, got {other:?}"),
        }
    }

    #[gpui::test]
    fn drag_remote_file_onto_local_pane_begins_download(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        // TabConnected also enqueues a ReadRemoteDir; drain it so the next
        // receive is the transfer we trigger.
        while channels.command_rx.try_recv().is_ok() {}

        // The drop handler on the local pane delegates to begin_download with
        // the dragged remote path.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_download(vec![RemotePath::new("/home/tester/bar.txt")], cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("drag-to-download must enqueue a StartTransfer");
        match command {
            AppCommand::StartTransfer(cmd) => {
                assert_eq!(cmd.direction, TransferDirection::Download);
                assert_eq!(
                    cmd.sources,
                    vec![TransferEndpoint::Remote(RemotePath::new(
                        "/home/tester/bar.txt"
                    ))]
                );
                assert!(matches!(cmd.destination, TransferEndpoint::Local(_)));
            }
            other => panic!("expected StartTransfer, got {other:?}"),
        }
    }

    #[gpui::test]
    fn directory_selection_starts_a_download_plan(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let source = RemotePath::new("/home/tester/project");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let tab = workspace.active_tab_mut().expect("active tab should exist");
            tab.local.path = Some(LocalPath::new("/tmp/downloads"));
            tab.remote.entries = vec![RemoteEntry {
                name: "project".to_string(),
                path: source.clone(),
                kind: FileKind::Directory,
                size: None,
                permissions: Some(0o755),
                modified_at: None,
                link_target: None,
            }];
            tab.selection.selected_paths = vec![EntryPath::Remote(source.clone())];
            workspace.download_selection(cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("directory download command must be enqueued");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected directory download transfer command");
        };
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Remote(RemotePath::new(
                "/home/tester/project"
            ))]
        );
        assert_eq!(
            command.destination,
            TransferEndpoint::Local(LocalPath::new("/tmp/downloads/project"))
        );
    }

    #[gpui::test]
    fn older_directory_result_does_not_replace_a_newer_requested_path(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let older_path = RemotePath::new("/home/tester/older");
        let newer_path = RemotePath::new("/home/tester/newer");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.request_remote_directory(TabId(1), older_path.clone(), cx);
            workspace.request_remote_directory(TabId(1), newer_path.clone(), cx);
        });
        let _ = channels.command_rx.try_recv();
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope.clone(),
                    RemoteDirSnapshot {
                        path: older_path,
                        entries: vec![RemoteEntry {
                            name: "stale.txt".to_string(),
                            path: RemotePath::new("/home/tester/older/stale.txt"),
                            kind: FileKind::File,
                            size: Some(1),
                            permissions: Some(0o644),
                            modified_at: None,
                            link_target: None,
                        }],
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.remote.path.as_ref(), Some(&newer_path));
            assert!(
                tab.remote.entries.is_empty(),
                "stale listing must be ignored"
            );
            assert!(tab.remote.is_refreshing, "newer request is still loading");
        });
    }

    #[gpui::test]
    fn current_directory_failure_shows_recoverable_pane_error(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let missing_path = RemotePath::new("/home/tester/missing");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.request_remote_directory(TabId(1), missing_path.clone(), cx);
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteOperationFailed(RemoteScoped::new(
                    scope,
                    RemoteOperationFailure {
                        operation: "Read remote directory".to_string(),
                        path: Some(missing_path.clone()),
                        error: UserFacingError::new(
                            ErrorCode::NotFound,
                            "Remote directory not found",
                            "The directory no longer exists. Go to its parent directory or refresh.",
                        )
                        .with_retryable(true),
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.remote.path.as_ref(), Some(&missing_path));
            assert!(!tab.remote.is_refreshing);
            assert_eq!(
                tab.remote.error.as_ref().map(|error| error.code),
                Some(ErrorCode::NotFound)
            );
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Remote;
            workspace.refresh_focused_pane(window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path }) if path == missing_path
        ));
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert!(tab.remote.is_refreshing);
            assert!(tab.remote.error.is_none(), "retry clears the pane error");
        });
    }

    #[gpui::test]
    fn remote_listing_applies_the_tab_sort(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope,
                    RemoteDirSnapshot {
                        path: RemotePath::new(TEST_REMOTE_ROOT),
                        entries: vec![
                            RemoteEntry {
                                name: "zeta.txt".to_string(),
                                path: RemotePath::new("/home/tester/zeta.txt"),
                                kind: FileKind::File,
                                size: Some(1),
                                permissions: Some(0o644),
                                modified_at: None,
                                link_target: None,
                            },
                            RemoteEntry {
                                name: "alpha-dir".to_string(),
                                path: RemotePath::new("/home/tester/alpha-dir"),
                                kind: FileKind::Directory,
                                size: None,
                                permissions: Some(0o755),
                                modified_at: None,
                                link_target: None,
                            },
                            RemoteEntry {
                                name: "alpha.txt".to_string(),
                                path: RemotePath::new("/home/tester/alpha.txt"),
                                kind: FileKind::File,
                                size: Some(1),
                                permissions: Some(0o644),
                                modified_at: None,
                                link_target: None,
                            },
                        ],
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            let names: Vec<&str> = tab
                .remote
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect();
            assert_eq!(names, ["alpha-dir", "alpha.txt", "zeta.txt"]);
        });
    }

    #[gpui::test]
    fn remote_navigation_and_refresh_keep_the_previous_listing_visible(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope.clone(),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        let _ = channels.command_rx.try_recv();

        let child_path = RemotePath::new("/home/tester/child");
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope,
                    RemoteDirSnapshot {
                        path: RemotePath::new(TEST_REMOTE_ROOT),
                        entries: vec![RemoteEntry {
                            name: "child".to_string(),
                            path: child_path.clone(),
                            kind: FileKind::Directory,
                            size: None,
                            permissions: Some(0o755),
                            modified_at: None,
                            link_target: None,
                        }],
                    },
                )),
                window,
                cx,
            );
            workspace.focused_side = PaneSide::Remote;
            workspace.open_entry_at(PaneSide::Remote, 0, window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path }) if path == child_path
        ));
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.remote.path.as_ref(), Some(&child_path));
            assert_eq!(tab.remote.entries.len(), 1);
            assert!(tab.remote.is_refreshing);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.go_to_parent_directory(window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new(TEST_REMOTE_ROOT)
        ));

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.refresh_focused_pane(window, cx);
        });
        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new(TEST_REMOTE_ROOT)
        ));
    }

    #[gpui::test]
    fn disconnect_event_clears_remote_pane_and_expires_modal(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _prompt = connect_and_prompt(&workspace, &mut cx);
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    scope,
                    TabDisconnected {
                        reason: DisconnectReason::ConnectionLost,
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.connection,
                ConnectionState::Disconnected {
                    reason: DisconnectReason::ConnectionLost,
                }
            );
            assert!(tab.remote.entries.is_empty());
            assert!(
                workspace.state.modals.active.is_empty(),
                "host key modal must expire when its session disconnects"
            );
        });
    }

    #[gpui::test]
    fn request_connect_opens_form_then_reuses_session_credentials(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        // First connect request: no credentials yet — the form opens
        // and no command is sent.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.request_connect(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.connect_form.is_some(), "form must open");
        });
        assert!(channels.command_rx.try_recv().is_err(), "no command yet");

        // Fill the form and submit.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let form = workspace.connect_form.as_mut().expect("form is open");
            form.host.set_value("server.example.com");
            form.port.set_value("2200");
            form.username.set_value("alex");
            form.password.set_value("secret");
            workspace.submit_connect_form(window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("submit must send ConnectTab");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.settings.host, "server.example.com");
                assert_eq!(connect.settings.port, 2200);
                assert_eq!(connect.settings.username, "alex");
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.connect_form.is_none(), "form closes on submit");
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(tab.title, "server.example.com");
            assert!(matches!(tab.connection, ConnectionState::Connecting { .. }));
        });

        // Simulate a disconnect, then reconnect: credentials are reused
        // without reopening the form.
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    scope,
                    TabDisconnected {
                        reason: DisconnectReason::ConnectionLost,
                    },
                )),
                window,
                cx,
            );
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.request_connect(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.connect_form.is_none(),
                "cached credentials must skip the form"
            );
        });
        let command = channels
            .command_rx
            .try_recv()
            .expect("reconnect must send ConnectTab");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.settings.host, "server.example.com");
                assert_eq!(connect.session_epoch, 2, "reconnect bumps the epoch");
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }
    }

    #[gpui::test]
    fn connect_form_validates_before_sending(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });

        // Empty host: submit is rejected with an inline error.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.submit_connect_form(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            let form = workspace.connect_form.as_ref().expect("form stays open");
            assert!(form.error.is_some(), "validation error must be shown");
        });
        assert!(channels.command_rx.try_recv().is_err());

        // Bad port is rejected too.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let form = workspace.connect_form.as_mut().expect("form is open");
            form.host.set_value("h");
            form.username.set_value("u");
            form.port.set_value("99999");
            workspace.submit_connect_form(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace
                    .connect_form
                    .as_ref()
                    .expect("open")
                    .error
                    .is_some()
            );
        });
        assert!(channels.command_rx.try_recv().is_err());
    }

    #[gpui::test]
    fn escape_closes_connect_form(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });

        cx.simulate_keystrokes("escape");
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.connect_form.is_none(),
                "escape must close the connect form"
            );
        });
    }

    /// Saving, reusing, updating, and deleting profiles through the
    /// connect form must persist to disk and round-trip (plan §5/§22).
    #[gpui::test]
    fn connect_form_save_use_update_and_delete_profile(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });
        // Fill a valid connection (including a password) and save it.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace.connect_form.as_mut().expect("form is open");
            form.host.set_value("example.com");
            form.username.set_value("alex");
            form.password.set_value("hunter2");
            form.profile_name.set_value("My Server");
            workspace.save_current_profile(cx);
        });

        let saved_id = workspace.read_with(&cx, |workspace, _| {
            let profiles = workspace.profiles.profiles();
            assert_eq!(profiles.len(), 1, "one profile saved");
            assert_eq!(profiles[0].name, "My Server");
            assert_eq!(profiles[0].host, "example.com");
            assert_eq!(profiles[0].username, "alex");
            profiles[0].id
        });

        // The save also flushed to disk: reopening the store reads it back.
        let path = workspace.read_with(&cx, |workspace, _| workspace.profiles.path().clone());
        let reloaded =
            macsftp_storage::ProfileStore::open(path).expect("reload saved profiles.json");
        assert_eq!(reloaded.profiles().len(), 1, "profile persisted to disk");

        // The secret was written to the Keychain, not just the profile
        // file (plan §11). The profile on disk holds only the SecretRef.
        let secret_ref = macsftp_core::SecretRef::keychain_ref(saved_id, "password");
        let stored = workspace.read_with(&cx, |workspace, _| {
            workspace.keychain.load(&secret_ref).expect("load secret")
        });
        assert_eq!(
            stored,
            Some("hunter2".to_string()),
            "secret stored in Keychain"
        );

        // "Use" prefills the form (including the restored secret) and
        // remembers the source profile id.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.use_profile(saved_id, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form
                .as_ref()
                .expect("form open after use");
            assert_eq!(form.host.value(), "example.com");
            assert_eq!(form.source_profile_id, Some(saved_id));
            assert_eq!(form.auth_method, AuthMethodKind::Password);
            assert_eq!(
                form.password.value(),
                "hunter2",
                "secret restored from Keychain into the form"
            );
        });

        // Re-saving the used form updates the same entry, not a duplicate.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.save_current_profile(cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(
                workspace.profiles.profiles().len(),
                1,
                "re-save updates, does not duplicate"
            );
        });

        // Deleting removes it from store and disk.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.delete_profile(saved_id, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.profiles.profiles().is_empty(),
                "profile deleted from store"
            );
        });
        let path = workspace.read_with(&cx, |workspace, _| workspace.profiles.path().clone());
        let reloaded = macsftp_storage::ProfileStore::open(path).expect("reload after delete");
        assert!(reloaded.profiles().is_empty(), "profile deleted from disk");
    }

    #[gpui::test]
    fn host_key_mismatch_fails_tab_and_blocks(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.connect_with(test_settings(), None, cx);
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::HostKeyMismatch(macsftp_core::HostKeyMismatch {
                    tab_id: TabId(1),
                    host: "mock.example.com".to_string(),
                    port: 22,
                    expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
                    actual_fingerprint_sha256: "SHA256:actual".to_string(),
                }),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            match &tab.connection {
                ConnectionState::Failed { error } => {
                    assert_eq!(error.code, macsftp_core::ErrorCode::HostKeyMismatch);
                    assert!(
                        !error.retryable,
                        "mismatch must not be presented as retryable"
                    );
                }
                other => panic!("expected Failed connection state, got {other:?}"),
            }
            assert!(
                workspace.state.modals.active.is_empty(),
                "mismatch must not open a trust modal"
            );
        });
    }

    #[gpui::test]
    fn close_tab_notifies_runtime(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        cx.dispatch_action(CloseTab);

        let command = channels
            .command_rx
            .try_recv()
            .expect("close command must be enqueued");
        assert!(matches!(command, AppCommand::CloseTab { tab_id: TabId(1) }));
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.state.tabs.tabs.is_empty());
        });
    }

    #[gpui::test]
    fn flush_persists_running_transfer_as_unfinished_history(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        // Simulate an in-flight upload plan in the live transfer store.
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            let plan_id = macsftp_core::TransferPlanId(7);
            let root_job_id = macsftp_core::TransferId(7);
            let now = macsftp_core::Timestamp::unix_epoch();
            workspace
                .state
                .transfers
                .plans
                .push(macsftp_core::TransferPlan {
                    id: plan_id,
                    root_job_id,
                    source_root: TransferEndpoint::Local(LocalPath::new("/tmp/report.pdf")),
                    destination_root: TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf")),
                    state: macsftp_core::TransferPlanState::Running,
                    planned_count: 1,
                    total_bytes: None,
                    child_jobs: vec![root_job_id],
                    conflict_policy: ConflictPolicy::Ask,
                });
            workspace
                .state
                .transfers
                .jobs
                .push(macsftp_core::TransferJob {
                    id: root_job_id,
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/report.pdf")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf")),
                    state: macsftp_core::TransferState::Running {
                        bytes_done: 0,
                        bytes_total: None,
                        started_at: now,
                    },
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: now,
                });
        });

        // The quit hook path: flush the live transfers to disk.
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace.flush_transfer_history();
        });

        workspace.read_with(&cx, |workspace, _| {
            let records = workspace.transfer_history.records();
            assert_eq!(records.len(), 1, "one in-flight plan persisted");
            assert!(matches!(
                records[0].status,
                TransferHistoryStatus::Unfinished
            ));
            assert_eq!(records[0].direction, TransferDirection::Upload);
            assert_eq!(records[0].sources.len(), 1);
            assert_eq!(
                records[0].destination,
                TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf"))
            );
        });

        // And it survives a reload from disk.
        let reopened = macsftp_storage::TransferHistoryStore::open_or_empty(
            workspace.read_with(&cx, |workspace, _| {
                workspace.transfer_history.path().clone()
            }),
        );
        assert_eq!(reopened.records().len(), 1);
        assert!(matches!(
            reopened.records()[0].status,
            TransferHistoryStatus::Unfinished
        ));
        // A second flush must not duplicate (guarded by the flushed flag).
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace.flush_transfer_history();
        });
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.transfer_history.records().len(), 1);
        });
    }

    #[gpui::test]
    fn retry_history_transfer_rebuilds_command_for_connected_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        // Establish a connected active tab by mutating connection state
        // directly (the host-key handshake is exercised by other tests).
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            let active_tab_id = workspace
                .state
                .tabs
                .active_tab_id
                .expect("active tab exists");
            let tab = workspace
                .state
                .tabs
                .find_tab_mut(active_tab_id)
                .expect("active tab exists");
            tab.profile_id = Some(ProfileId(5));
            tab.connection = ConnectionState::Connected {
                session_id: SessionId(2),
                session_epoch: 3,
                connected_at: Timestamp::unix_epoch(),
            };
        });

        // Seed a retryable (unfinished) history record.
        let record_id = macsftp_core::TransferHistoryId(11);
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace
                .transfer_history
                .append(vec![TransferHistoryRecord {
                    id: record_id,
                    profile_id: None,
                    direction: TransferDirection::Download,
                    sources: vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))],
                    destination: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    status: TransferHistoryStatus::Unfinished,
                    started_at: Timestamp::unix_epoch(),
                    last_updated: Timestamp::unix_epoch(),
                }]);
        });

        let removed = workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.retry_history_transfer(record_id, cx)
        });
        assert!(removed, "retry should enqueue a transfer");

        let command = channels
            .command_rx
            .try_recv()
            .expect("a StartTransfer command must be enqueued");
        match command {
            AppCommand::StartTransfer(cmd) => {
                assert_eq!(cmd.direction, TransferDirection::Download);
                assert_eq!(
                    cmd.sources,
                    vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))]
                );
                assert_eq!(
                    cmd.destination,
                    TransferEndpoint::Local(LocalPath::new("/tmp/a.txt"))
                );
                // Rebuilt against the live connected tab.
                assert_eq!(cmd.tab_id, TabId(1));
                assert_eq!(cmd.session_epoch, 3);
                assert_eq!(cmd.profile_id, ProfileId(5));
            }
            other => panic!("expected StartTransfer, got {other:?}"),
        }

        // The record is removed from history once retried.
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.transfer_history.find(record_id).is_none());
        });
    }

    #[gpui::test]
    fn retry_history_transfer_requires_connected_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        let record_id = macsftp_core::TransferHistoryId(12);
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            workspace
                .transfer_history
                .append(vec![TransferHistoryRecord {
                    id: record_id,
                    profile_id: None,
                    direction: TransferDirection::Download,
                    sources: vec![TransferEndpoint::Remote(RemotePath::new("/srv/a.txt"))],
                    destination: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    status: TransferHistoryStatus::Unfinished,
                    started_at: Timestamp::unix_epoch(),
                    last_updated: Timestamp::unix_epoch(),
                }]);
        });

        // No connected tab → retry is refused and nothing is enqueued.
        let enqueued = workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.retry_history_transfer(record_id, cx)
        });
        assert!(!enqueued);
        assert!(channels.command_rx.try_recv().is_err());
        // Record remains so the user can retry later.
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.transfer_history.find(record_id).is_some());
        });
    }

    #[gpui::test]
    fn completed_child_jobs_finalize_root_plan(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let now = Timestamp(std::time::SystemTime::now());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let plan_id = TransferPlanId(1);
            let root_job_id = TransferId(1);
            let _child_job_id = TransferId(2);

            let plan = macsftp_core::TransferPlan {
                id: plan_id,
                root_job_id,
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/src")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/tmp/dest")),
                state: TransferPlanState::Planning,
                planned_count: 0,
                total_bytes: None,
                child_jobs: Vec::new(),
                conflict_policy: ConflictPolicy::default(),
            };
            let root_job = TransferJob {
                id: root_job_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/src")),
                destination: TransferEndpoint::Remote(RemotePath::new("/tmp/dest")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: now,
            };
            workspace.handle_app_event(
                AppEvent::TransferPlanStarted(TransferPlanSnapshot { plan, root_job }),
                window,
                cx,
            );

            workspace.handle_app_event(AppEvent::TransferPlanCompleted { plan_id }, window, cx);

            // Without children, planning completion should finalize the root
            // job immediately so it does not linger in the Queued section.
            let root_job = workspace
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = workspace
                .state
                .transfers
                .plans
                .iter()
                .find(|p| p.id == plan_id)
                .expect("plan exists");
            assert_eq!(plan.state, TransferPlanState::Completed);
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let plan_id = TransferPlanId(2);
            let root_job_id = TransferId(10);
            let child_job_id = TransferId(11);

            let plan = macsftp_core::TransferPlan {
                id: plan_id,
                root_job_id,
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/src2")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/tmp/dest2")),
                state: TransferPlanState::Planning,
                planned_count: 0,
                total_bytes: None,
                child_jobs: vec![child_job_id],
                conflict_policy: ConflictPolicy::default(),
            };
            let root_job = TransferJob {
                id: root_job_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/src2")),
                destination: TransferEndpoint::Remote(RemotePath::new("/tmp/dest2")),
                state: TransferState::Planning,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: now,
            };
            workspace.handle_app_event(
                AppEvent::TransferPlanStarted(TransferPlanSnapshot { plan, root_job }),
                window,
                cx,
            );
            workspace.handle_app_event(AppEvent::TransferPlanCompleted { plan_id }, window, cx);

            // Plan completion with children keeps root in Queued.
            let root_job = workspace
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Queued);

            let child_job = TransferJob {
                id: child_job_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/src2/file.txt")),
                destination: TransferEndpoint::Remote(RemotePath::new("/tmp/dest2/file.txt")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: now,
            };
            workspace.handle_app_event(
                AppEvent::TransferPlanProgress(TransferPlanProgress {
                    plan_id,
                    child_job,
                    planned_count: 1,
                    total_bytes: Some(100),
                }),
                window,
                cx,
            );
            workspace.handle_app_event(
                AppEvent::TransferCompleted {
                    transfer_id: child_job_id,
                },
                window,
                cx,
            );

            // After the last child completes, the root plan/job must finalize.
            let root_job = workspace
                .state
                .transfers
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = workspace
                .state
                .transfers
                .plans
                .iter()
                .find(|p| p.id == plan_id)
                .expect("plan exists");
            assert_eq!(plan.state, TransferPlanState::Completed);
        });
    }
}
