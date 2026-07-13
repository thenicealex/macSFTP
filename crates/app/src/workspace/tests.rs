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
    ActiveTheme, DragPreview, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel,
    Theme, TransferRow, connection_status, copy_name, empty_state, file_row, file_table_header,
    format_size, format_timestamp, icon, icon_button, section_header_static, tab, text_button,
    text_field, text_tooltip, transfer_history_detail, transfer_history_title, transfer_row,
    transfer_title,
};

use tracing::{debug, warn};

use crate::app_actions::*;
use crate::workspace::connect_form::*;
use crate::workspace::helpers::*;
use crate::workspace::*;

#[cfg(test)]
mod tests {
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use macsftp_core::{
        AppCommand, AppEvent, AuthCredential, AuthMethodKind, ConflictDecision, ConflictPolicy,
        ConflictRequestId, ConnectionSettings, ConnectionState, DisconnectReason, EntryPath,
        ErrorCode, FileKind, FileSortField, HostKeyPrompt, LocalPath, MetadataPolicy, ProfileId,
        RemoteDirSnapshot, RemoteEntry, RemoteEventScope, RemoteOperationFailure, RemotePath,
        RemoteScoped, RuntimeBridgeConfig, SessionId, SortDirection, TabConnected, TabDisconnected,
        TabId, Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint,
        TransferHistoryRecord, TransferHistoryStatus, TransferId, TransferJob, TransferPlanId,
        TransferPlanProgress, TransferPlanSnapshot, TransferPlanState, TransferState,
        TrustRequestId, UserFacingError,
    };
    use macsftp_sftp::{BridgeChannels, EventReceiver, RuntimeClient};
    use macsftp_storage::AppearancePreference;
    use macsftp_ui::{Appearance, Theme};

    use super::{AppPaths, PaneSide, Workspace, WorkspaceSurface};
    use crate::app_actions::{
        self, ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, FilterPane, GoToPath,
        NavigateBack, NewTab, OpenSettings, PageDown, SelectAllEntries, SelectNextEntry,
        SelectNextEntryExtend, SelectPrevEntry, ShowAbout, ShowTransferDrawer, ToggleHiddenFiles,
    };
    use crate::resources::{ActiveResources, ActiveTransfers};
    use crate::workspace::nav::HistoryOp;

    const TEST_REMOTE_ROOT: &str = "/home/tester";

    fn init_workspace(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, VisualTestContext, BridgeChannels) {
        let app_paths = temp_app_paths();
        let config = macsftp_storage::ConfigStore::with_defaults(app_paths.config_file.clone());
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
            // Shared globals must exist before `Workspace::new` (which reads
            // them). A fresh `AppResources` per test → fresh tab-id counter
            // starting at 1, keeping each test isolated.
            cx.set_global(crate::resources::AppResources::load(
                app_paths,
                config,
                macsftp_storage::KeychainStore::new_memory(),
            ));
            cx.set_global(crate::resources::SharedTransfers::default());
        });
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let (_event_tx, receiver) = macsftp_sftp::test_event_channel(
            RuntimeBridgeConfig::default().event_channel_capacity,
        );
        let window = cx.add_window(|window, cx| Workspace::new(client, receiver, window, cx));
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
    fn page_down_moves_by_ten_on_visible_list(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("page-down");
        for i in 0..15 {
            std::fs::write(fixture.join(format!("f{i:02}.txt")), b"x").expect("write file");
        }

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                15,
                "fixture has 15 plain files"
            );
            workspace.select_index(PaneSide::Local, 0, cx);
        });

        cx.dispatch_action(PageDown);
        workspace.read_with(&cx, |workspace, cx| {
            assert_eq!(
                workspace.selected_index(PaneSide::Local, cx),
                Some(10),
                "page down steps PAGE_SIZE=10 on the visible list"
            );
            let tab = workspace.active_tab().expect("active tab");
            assert_eq!(
                tab.selection.selected_paths.len(),
                1,
                "page down single-selects"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn shift_down_extends_selection_range(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("shift-range");
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            std::fs::write(fixture.join(name), b"x").expect("write file");
        }

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            assert_eq!(workspace.entry_count(PaneSide::Local, cx), 5);
            // Anchor at visible index 1, then extend to 3.
            workspace.select_index(PaneSide::Local, 1, cx);
            workspace.extend_selection_to(PaneSide::Local, 3, cx);
            let tab = workspace.active_tab().expect("active tab");
            assert_eq!(
                tab.selection.selected_paths.len(),
                3,
                "closed range [1,3] yields three paths"
            );
        });

        // Shift-down from the active edge continues the range.
        cx.dispatch_action(SelectNextEntryExtend);
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.active_tab().expect("active tab");
            assert_eq!(
                tab.selection.selected_paths.len(),
                4,
                "shift-down grows selection by one"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn select_all_selects_all_visible(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("select-all");
        std::fs::write(fixture.join(".hidden"), b"h").expect("write hidden");
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(fixture.join(name), b"x").expect("write visible");
        }

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                3,
                "dotfile hidden by default"
            );
        });

        cx.dispatch_action(SelectAllEntries);
        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.active_tab().expect("active tab");
            assert_eq!(
                tab.selection.selected_paths.len(),
                3,
                "cmd-a selects only visible entries"
            );
            assert!(
                tab.selection
                    .selected_paths
                    .iter()
                    .all(|path| matches!(path, EntryPath::Local(local) if !local.as_str().contains("/.hidden"))),
                "hidden path must not be selected"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
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
        workspace.read_with(&cx, |_workspace, cx| {
            assert_eq!(
                cx.resources().config.config().appearance,
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
    fn cancel_connect_sends_disconnect_and_clears_connecting(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, cx);
            assert!(matches!(
                workspace.active_tab().expect("tab").connection,
                ConnectionState::Connecting { .. }
            ));
            workspace.cancel_connect(window, cx);
            assert_eq!(
                workspace.active_tab().expect("tab").connection,
                ConnectionState::Disconnected {
                    reason: DisconnectReason::UserRequested,
                }
            );
            let tab = workspace.active_tab().expect("tab");
            assert!(tab.remote.entries.is_empty());
            assert!(tab.remote.path.is_none());
            assert!(!tab.remote.is_refreshing);
            assert!(tab.remote.error.is_none());
        });

        let mut saw_disconnect = false;
        while let Ok(command) = channels.command_rx.try_recv() {
            if matches!(command, AppCommand::DisconnectTab { .. }) {
                saw_disconnect = true;
            }
        }
        assert!(saw_disconnect, "DisconnectTab must be sent");
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
            let pending = cx.transfers()
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
    fn navigate_back_restores_previous_local_path(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, parent) = temp_local_fixture("nav-back");
        let child_dir = fixture.join("child");
        std::fs::create_dir(&child_dir).expect("child dir");
        std::fs::write(child_dir.join("note.txt"), b"hi").expect("child file");
        let child = LocalPath::new(child_dir.to_string_lossy().into_owned());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            // Seed current path without history (same as open_new_tab / refresh).
            workspace.set_local_path(parent.clone(), window, cx);
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.clone())
                    .as_ref()
                    .map(LocalPath::as_str),
                Some(parent.as_str())
            );

            workspace.navigate_pane_local(child.clone(), HistoryOp::Push, window, cx);
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.clone())
                    .as_ref()
                    .map(LocalPath::as_str),
                Some(child.as_str())
            );
            assert!(
                workspace.pane_can_navigate_back(PaneSide::Local),
                "push must enable back"
            );
            assert!(
                !workspace.pane_can_navigate_forward(PaneSide::Local),
                "push clears forward"
            );

            workspace.navigate_pane_local(LocalPath::new(String::new()), HistoryOp::Back, window, cx);
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.clone())
                    .as_ref()
                    .map(LocalPath::as_str),
                Some(parent.as_str()),
                "back restores parent"
            );
            assert!(
                workspace.pane_can_navigate_forward(PaneSide::Local),
                "back enables forward"
            );

            workspace.navigate_pane_local(
                LocalPath::new(String::new()),
                HistoryOp::Forward,
                window,
                cx,
            );
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.clone())
                    .as_ref()
                    .map(LocalPath::as_str),
                Some(child.as_str()),
                "forward restores child"
            );
        });

        // Keyboard action path (cmd-[).
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Local;
            workspace.navigate_pane_local(child.clone(), HistoryOp::Push, window, cx);
        });
        cx.dispatch_action(NavigateBack);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.as_ref().map(LocalPath::as_str)),
                Some(parent.as_str()),
                "NavigateBack action restores previous local path"
            );
        });

        // Refresh must not push history.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Local;
            let back_len_before = workspace
                .tab_nav
                .get(
                    &workspace
                        .active_tab()
                        .expect("tab")
                        .id,
                )
                .map(|nav| nav.local.back.len())
                .unwrap_or(0);
            workspace.refresh_focused_pane(window, cx);
            let back_len_after = workspace
                .tab_nav
                .get(
                    &workspace
                        .active_tab()
                        .expect("tab")
                        .id,
                )
                .map(|nav| nav.local.back.len())
                .unwrap_or(0);
            assert_eq!(
                back_len_before, back_len_after,
                "refresh must not push history"
            );
        });

        // Closing the tab drops its nav state.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let tab_id = workspace.active_tab().expect("tab").id;
            assert!(workspace.tab_nav.contains_key(&tab_id));
            workspace.close_tab_by_id(tab_id, window, cx);
            assert!(
                !workspace.tab_nav.contains_key(&tab_id),
                "close_tab must remove tab_nav entry"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn go_to_path_navigates_local_with_push_history(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, parent) = temp_local_fixture("go-to-path");
        let child_dir = fixture.join("child");
        std::fs::create_dir(&child_dir).expect("child dir");
        let child = LocalPath::new(child_dir.to_string_lossy().into_owned());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(parent.clone(), window, cx);
            workspace.focused_side = PaneSide::Local;
            workspace.open_go_to_path(window, cx);
            assert!(workspace.go_to_path_open);
            workspace.go_to_path_input.set_value(child.as_str());
            workspace.submit_go_to_path(window, cx);
            assert!(!workspace.go_to_path_open);
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.as_ref().map(LocalPath::as_str)),
                Some(child.as_str()),
                "submit_go_to_path must navigate to entered local path"
            );
            assert!(
                workspace.pane_can_navigate_back(PaneSide::Local),
                "go to path must push history"
            );
        });

        // Missing path stays on current directory and reports an error.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_go_to_path(window, cx);
            workspace
                .go_to_path_input
                .set_value(format!("{}/does-not-exist", parent.as_str()));
            workspace.submit_go_to_path(window, cx);
            assert!(
                workspace.go_to_path_open,
                "modal stays open on missing path"
            );
            assert_eq!(
                workspace.go_to_path_error.as_ref().map(|s| s.as_ref()),
                Some("Path not found")
            );
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.as_ref().map(LocalPath::as_str)),
                Some(child.as_str()),
                "failed go to path must not change current path"
            );
        });

        // Esc / CancelActiveModal closes the modal.
        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                !workspace.go_to_path_open,
                "CancelActiveModal closes go to path"
            );
        });

        // Action opens the modal again.
        cx.dispatch_action(GoToPath);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.go_to_path_open, "GoToPath action opens modal");
        });

        // Empty path is rejected.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.go_to_path_input.clear();
            workspace.submit_go_to_path(window, cx);
            assert!(workspace.go_to_path_open);
            assert_eq!(
                workspace.go_to_path_error.as_ref().map(|s| s.as_ref()),
                Some("Enter a path")
            );
        });

        // A file path is rejected (must be a directory).
        let file_path = fixture.join("note.txt");
        std::fs::write(&file_path, b"x").expect("write file");
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_go_to_path(window, cx);
            workspace
                .go_to_path_input
                .set_value(file_path.to_string_lossy().as_ref());
            workspace.submit_go_to_path(window, cx);
            assert!(
                workspace.go_to_path_open,
                "modal stays open when target is a file"
            );
            assert_eq!(
                workspace.go_to_path_error.as_ref().map(|s| s.as_ref()),
                Some("Not a directory")
            );
            assert_eq!(
                workspace
                    .active_tab()
                    .and_then(|tab| tab.local.path.as_ref().map(LocalPath::as_str)),
                Some(child.as_str()),
                "file path must not navigate"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn navigate_clears_type_to_filter_query(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, parent) = temp_local_fixture("nav-clears-filter");
        let child_dir = fixture.join("sub");
        std::fs::create_dir(&child_dir).expect("child");
        std::fs::write(fixture.join("alpha.txt"), b"a").expect("alpha");
        std::fs::write(fixture.join("beta.txt"), b"b").expect("beta");
        let child = LocalPath::new(child_dir.to_string_lossy().into_owned());

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(parent, window, cx);
            workspace.focused_side = PaneSide::Local;
            workspace.local_filter.query = "alpha".into();
            workspace.local_filter.explicit_focus = true;
            assert_eq!(workspace.entry_count(PaneSide::Local, cx), 1);
            workspace.navigate_pane_local(child, HistoryOp::Push, window, cx);
            assert!(
                workspace.local_filter.query.is_empty(),
                "navigate must clear filter query"
            );
            assert!(
                !workspace.local_filter.explicit_focus,
                "navigate must clear filter focus"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn go_to_path_remote_requests_directory_with_push(cx: &mut TestAppContext) {
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
        // TabConnected requests the remote root listing.
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::RemoteDirLoaded(RemoteScoped::new(
                    scope,
                    RemoteDirSnapshot {
                        path: RemotePath::new(TEST_REMOTE_ROOT),
                        entries: vec![],
                    },
                )),
                window,
                cx,
            );

            workspace.focused_side = PaneSide::Remote;
            workspace.open_go_to_path(window, cx);
            workspace
                .go_to_path_input
                .set_value(format!("{TEST_REMOTE_ROOT}/docs"));
            workspace.submit_go_to_path(window, cx);
            assert!(!workspace.go_to_path_open);
            assert!(
                workspace.pane_can_navigate_back(PaneSide::Remote),
                "remote go to path must push history"
            );
        });

        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path.as_str() == &format!("{TEST_REMOTE_ROOT}/docs")
        ));
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

        let saved_id = workspace.read_with(&cx, |_workspace, cx| {
            let profiles = cx.resources().profiles.profiles();
            assert_eq!(profiles.len(), 1, "one profile saved");
            assert_eq!(profiles[0].name, "My Server");
            assert_eq!(profiles[0].host, "example.com");
            assert_eq!(profiles[0].username, "alex");
            profiles[0].id
        });

        // The save also flushed to disk: reopening the store reads it back.
        let path =
            workspace.read_with(&cx, |_workspace, cx| cx.resources().profiles.path().clone());
        let reloaded =
            macsftp_storage::ProfileStore::open(path).expect("reload saved profiles.json");
        assert_eq!(reloaded.profiles().len(), 1, "profile persisted to disk");

        // The secret was written to the Keychain, not just the profile
        // file (plan §11). The profile on disk holds only the SecretRef.
        let secret_ref = macsftp_core::SecretRef::keychain_ref(saved_id, "password");
        let stored = workspace.read_with(&cx, |_workspace, cx| {
            cx.resources().keychain.load(&secret_ref).expect("load secret")
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
        workspace.read_with(&cx, |_workspace, cx| {
            assert_eq!(
                cx.resources().profiles.profiles().len(),
                1,
                "re-save updates, does not duplicate"
            );
        });

        // Deleting removes it from store and disk.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.delete_profile(saved_id, cx);
        });
        workspace.read_with(&cx, |_workspace, cx| {
            assert!(
                cx.resources().profiles.profiles().is_empty(),
                "profile deleted from store"
            );
        });
        let path =
            workspace.read_with(&cx, |_workspace, cx| cx.resources().profiles.path().clone());
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
        workspace.update_in(&mut cx, |_workspace, _window, cx| {
            let plan_id = macsftp_core::TransferPlanId(7);
            let root_job_id = macsftp_core::TransferId(7);
            let now = macsftp_core::Timestamp::unix_epoch();
            cx.transfers_mut().plans.push(macsftp_core::TransferPlan {
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
            cx.transfers_mut().jobs.push(macsftp_core::TransferJob {
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
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.flush_transfer_history(cx);
        });

        workspace.read_with(&cx, |_workspace, cx| {
            let records = cx.resources().transfer_history.records();
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
            workspace.read_with(&cx, |_workspace, cx| {
                cx.resources().transfer_history.path().clone()
            }),
        );
        assert_eq!(reopened.records().len(), 1);
        assert!(matches!(
            reopened.records()[0].status,
            TransferHistoryStatus::Unfinished
        ));
        // A second flush must not duplicate (guarded by the flushed flag).
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.flush_transfer_history(cx);
        });
        workspace.read_with(&cx, |_workspace, cx| {
            assert_eq!(cx.resources().transfer_history.records().len(), 1);
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
        workspace.update_in(&mut cx, |_workspace, _window, cx| {
            cx.resources_mut()
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
        workspace.read_with(&cx, |_workspace, cx| {
            assert!(cx.resources().transfer_history.find(record_id).is_none());
        });
    }

    #[gpui::test]
    fn retry_history_transfer_requires_connected_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let _ = channels.command_rx.try_recv();

        let record_id = macsftp_core::TransferHistoryId(12);
        workspace.update_in(&mut cx, |_workspace, _window, cx| {
            cx.resources_mut()
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
        workspace.read_with(&cx, |_workspace, cx| {
            assert!(cx.resources().transfer_history.find(record_id).is_some());
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
            let root_job = cx.transfers()
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = cx.transfers()
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
            let root_job = cx.transfers()
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
            let root_job = cx.transfers()
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = cx.transfers()
                .plans
                .iter()
                .find(|p| p.id == plan_id)
                .expect("plan exists");
            assert_eq!(plan.state, TransferPlanState::Completed);
        });
    }

    fn temp_local_fixture(label: &str) -> (std::path::PathBuf, LocalPath) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "macsftp-fs-ops-{label}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        (dir.clone(), LocalPath::new(dir.to_string_lossy().into_owned()))
    }

    #[gpui::test]
    fn hidden_files_filtered_by_default(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("hidden-files");
        std::fs::write(fixture.join(".secret"), b"secret").expect("write hidden");
        std::fs::write(fixture.join("visible.txt"), b"ok").expect("write visible");

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            let tab = workspace.active_tab().expect("tab");
            assert_eq!(
                tab.local.entries.len(),
                2,
                "stored listing keeps every entry"
            );
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                1,
                "dotfiles hidden by default"
            );
            let path = workspace
                .entry_path_at(PaneSide::Local, 0, cx)
                .expect("one visible path");
            match path {
                EntryPath::Local(local) => {
                    assert!(
                        local.as_str().ends_with("visible.txt"),
                        "visible row should be visible.txt, got {}",
                        local.as_str()
                    );
                }
                EntryPath::Remote(_) => panic!("local path expected"),
            }

            workspace.toggle_hidden_files(cx);
            assert!(
                cx.resources().config.config().show_hidden_files,
                "toggle persists true"
            );
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                2,
                "both entries after show_hidden"
            );
        });

        cx.dispatch_action(ToggleHiddenFiles);
        workspace.read_with(&cx, |workspace, cx| {
            assert!(
                !cx.resources().config.config().show_hidden_files,
                "action toggles back to false"
            );
            assert_eq!(workspace.entry_count(PaneSide::Local, cx), 1);
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn type_to_filter_reduces_visible_local_entries(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("type-to-filter");
        std::fs::write(fixture.join("a.txt"), b"a").expect("write a");
        std::fs::write(fixture.join("b.txt"), b"b").expect("write b");

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                2,
                "both files visible before filter"
            );

            workspace.local_filter.query = "a".into();
            workspace
                .local_filter
                .input
                .set_value(workspace.local_filter.query.clone());
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                1,
                "filter query reduces to matching names"
            );
            let path = workspace
                .entry_path_at(PaneSide::Local, 0, cx)
                .expect("one match");
            match path {
                EntryPath::Local(local) => {
                    assert!(
                        local.as_str().ends_with("a.txt"),
                        "matched a.txt, got {}",
                        local.as_str()
                    );
                }
                EntryPath::Remote(_) => panic!("local path expected"),
            }
            assert_eq!(
                workspace.count_after_hidden(PaneSide::Local, cx),
                2,
                "total after hidden ignores name filter"
            );

            workspace.clear_filter(PaneSide::Local);
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                2,
                "clear restores full list"
            );
            assert!(!workspace.local_filter.is_active());
        });

        // cmd-f / FilterPane opens explicit focus without requiring a query.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_filter_pane(window, cx);
            assert!(workspace.local_filter.explicit_focus);
            assert!(workspace.local_filter.is_active());
            workspace.local_filter.query = "b".into();
            assert_eq!(workspace.entry_count(PaneSide::Local, cx), 1);
        });
        cx.dispatch_action(FilterPane);
        workspace.read_with(&cx, |workspace, _cx| {
            assert!(
                workspace.local_filter.explicit_focus,
                "FilterPane action sets explicit_focus"
            );
        });

        // Tab switch clears filters.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_new_tab(window, cx);
            assert!(!workspace.local_filter.is_active());
            assert!(!workspace.remote_filter.is_active());
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn local_directory_respects_tab_sort_by_size(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!(
                "macsftp-sort-{}-{}",
                std::process::id(),
                seq
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("big.bin"), vec![0u8; 100]).unwrap();
            std::fs::write(dir.join("small.bin"), vec![0u8; 1]).unwrap();
            (dir.clone(), LocalPath::new(dir.to_string_lossy().into_owned()))
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            if let Some(tab) = workspace.active_tab_mut() {
                tab.sort.field = FileSortField::Size;
                tab.sort.direction = SortDirection::Ascending;
            }
            workspace.set_local_path(base, window, cx);
            let names: Vec<_> = workspace
                .active_tab()
                .unwrap()
                .local
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect();
            // directories_first: only files here — small before big when ascending size
            let small_i = names.iter().position(|n| *n == "small.bin").unwrap();
            let big_i = names.iter().position(|n| *n == "big.bin").unwrap();
            assert!(small_i < big_i, "expected size ascending, got {names:?}");
        });
        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn apply_sort_field_toggles_direction_on_same_column(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update(&mut cx, |workspace, cx| {
            workspace.apply_sort_field(FileSortField::Name, cx);
            // default was already Name Ascending → becomes Descending
            assert_eq!(
                workspace.active_tab().unwrap().sort.direction,
                SortDirection::Descending
            );
            workspace.apply_sort_field(FileSortField::Size, cx);
            let sort = &workspace.active_tab().unwrap().sort;
            assert_eq!(sort.field, FileSortField::Size);
            assert_eq!(sort.direction, SortDirection::Ascending);
        });
    }

    #[gpui::test]
    fn delete_confirm_modal_lists_selection_and_dir_warning(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("delete-confirm");
        std::fs::write(fixture.join("a.txt"), b"a").expect("file");
        std::fs::create_dir(fixture.join("subdir")).expect("dir");

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            let paths: Vec<EntryPath> = workspace
                .active_tab()
                .expect("tab")
                .local
                .entries
                .iter()
                .map(|e| EntryPath::Local(e.path.clone()))
                .collect();
            if let Some(tab) = workspace.active_tab_mut() {
                tab.selection.selected_paths = paths;
            }
            workspace.focused_side = PaneSide::Local;
            workspace.request_delete_selection(window, cx);
            let confirm = workspace
                .delete_confirm
                .as_ref()
                .expect("confirm modal open");
            assert_eq!(confirm.entries.len(), 2);
            assert!(confirm.entries.iter().any(|e| e.is_dir));
        });
        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn dont_ask_again_persists_and_skips_next_confirm(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("dont-ask");
        std::fs::write(fixture.join("gone.txt"), b"x").expect("file");

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base.clone(), window, cx);
            let path = workspace
                .active_tab()
                .expect("tab")
                .local
                .entries
                .iter()
                .find(|e| e.name == "gone.txt")
                .map(|e| e.path.clone())
                .expect("gone.txt");
            if let Some(tab) = workspace.active_tab_mut() {
                tab.selection.selected_paths = vec![EntryPath::Local(path)];
            }
            workspace.focused_side = PaneSide::Local;
            workspace.request_delete_selection(window, cx);
            if let Some(state) = &mut workspace.delete_confirm {
                state.dont_ask_again = true;
            }
            workspace.confirm_delete(window, cx);
            assert!(!std::path::Path::new(fixture.join("gone.txt").as_path()).exists());
            assert!(
                !cx.resources().config.config().confirm_delete,
                "confirm_delete should persist false"
            );

            // Create another file and delete without modal.
            std::fs::write(fixture.join("second.txt"), b"y").expect("second");
            workspace.set_local_path(base, window, cx);
            let path = workspace
                .active_tab()
                .expect("tab")
                .local
                .entries
                .iter()
                .find(|e| e.name == "second.txt")
                .map(|e| e.path.clone())
                .expect("second.txt");
            if let Some(tab) = workspace.active_tab_mut() {
                tab.selection.selected_paths = vec![EntryPath::Local(path)];
            }
            workspace.request_delete_selection(window, cx);
            assert!(workspace.delete_confirm.is_none());
            assert!(!std::path::Path::new(fixture.join("second.txt").as_path()).exists());
        });
        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn local_new_folder_and_rename_via_inline_edit(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("inline-edit");

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base.clone(), window, cx);
            workspace.focused_side = PaneSide::Local;
            workspace.begin_new_folder(window, cx);
            if let Some(edit) = &mut workspace.inline_edit {
                edit.input.set_value("Created Folder");
            }
            workspace.submit_inline_edit(window, cx);
            assert!(fixture.join("Created Folder").is_dir());

            // Reload so listing includes the new folder, then rename it.
            workspace.set_local_path(base, window, cx);
            let path = workspace
                .active_tab()
                .expect("tab")
                .local
                .entries
                .iter()
                .find(|e| e.name == "Created Folder")
                .map(|e| e.path.clone())
                .expect("folder entry");
            if let Some(tab) = workspace.active_tab_mut() {
                tab.selection.selected_paths = vec![EntryPath::Local(path)];
            }
            workspace.begin_rename_selection(window, cx);
            if let Some(edit) = &mut workspace.inline_edit {
                edit.input.set_value("Renamed Folder");
            }
            workspace.submit_inline_edit(window, cx);
            assert!(!fixture.join("Created Folder").exists());
            assert!(fixture.join("Renamed Folder").is_dir());
        });
        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn fs_operation_failed_sets_pane_error(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let tab_id = workspace.active_tab().expect("tab").id;
            let epoch = workspace.active_tab().expect("tab").session_epoch;
            workspace.handle_app_event(
                AppEvent::FsOperationFailed {
                    scope: macsftp_core::FsScope::Remote {
                        tab_id,
                        session_epoch: epoch,
                    },
                    failure: UserFacingError::new(
                        ErrorCode::PermissionDenied,
                        "Could not delete",
                        "Permission denied",
                    ),
                },
                window,
                cx,
            );
            assert!(
                workspace
                    .status_message
                    .as_ref()
                    .is_some_and(|m| m.contains("Could not delete"))
            );
        });
    }

    #[gpui::test]
    fn status_bar_selection_count_tracks_focused_pane(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("status-selection");
        std::fs::write(fixture.join("a.txt"), b"a").expect("file a");
        std::fs::write(fixture.join("b.txt"), b"b").expect("file b");
        std::fs::write(fixture.join("c.txt"), b"c").expect("file c");

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_local_path(base, window, cx);
            let local_paths: Vec<EntryPath> = workspace
                .active_tab()
                .expect("tab")
                .local
                .entries
                .iter()
                .take(2)
                .map(|entry| EntryPath::Local(entry.path.clone()))
                .collect();
            assert_eq!(local_paths.len(), 2);

            if let Some(tab) = workspace.active_tab_mut() {
                tab.selection.selected_paths = local_paths.clone();
                tab.selection.selected_paths.push(EntryPath::Remote(
                    RemotePath::new(format!("{TEST_REMOTE_ROOT}/remote-only.txt")),
                ));
            }

            workspace.focused_side = PaneSide::Local;
            assert_eq!(
                workspace.focused_selection_count(),
                2,
                "local focus should count only local selections"
            );

            workspace.focused_side = PaneSide::Remote;
            assert_eq!(
                workspace.focused_selection_count(),
                1,
                "remote focus should count only remote selections"
            );

            if let Some(tab) = workspace.active_tab_mut() {
                tab.selection.selected_paths.clear();
            }
            assert_eq!(workspace.focused_selection_count(), 0);
        });
        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn activate_tab_updates_mru_front(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        // open_new_tab seeds MRU with tab 1.
        cx.dispatch_action(NewTab); // tabs 1,2 active 2
        cx.dispatch_action(NewTab); // tabs 1,2,3 active 3
        workspace.read_with(&cx, |ws, _| {
            assert_eq!(ws.tab_mru.first(), Some(&TabId(3)));
            assert_eq!(ws.tab_mru, vec![TabId(3), TabId(2), TabId(1)]);
        });

        workspace.update(&mut cx, |ws, cx| {
            ws.activate_tab(TabId(2), cx);
        });
        workspace.update(&mut cx, |ws, cx| {
            ws.activate_tab(TabId(3), cx);
        });
        workspace.read_with(&cx, |ws, _| {
            assert_eq!(ws.tab_mru[0], TabId(3), "most recent activation is front");
            let pos2 = ws
                .tab_mru
                .iter()
                .position(|id| *id == TabId(2))
                .expect("tab 2 stays in mru");
            let pos1 = ws
                .tab_mru
                .iter()
                .position(|id| *id == TabId(1))
                .expect("tab 1 stays in mru");
            assert!(pos2 < pos1, "tab 2 was activated more recently than tab 1");
        });
    }

    #[gpui::test]
    fn cmd_shift_tab_still_creation_order(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab); // 1, 2 active 2
        cx.dispatch_action(NewTab); // 1, 2, 3 active 3
        // Scramble MRU so it differs from creation order: activate 1, then 3.
        workspace.update(&mut cx, |ws, cx| {
            ws.activate_tab(TabId(1), cx);
        });
        workspace.update(&mut cx, |ws, cx| {
            ws.activate_tab(TabId(3), cx);
        });
        workspace.read_with(&cx, |ws, _| {
            assert_eq!(ws.tab_mru, vec![TabId(3), TabId(1), TabId(2)]);
            assert_eq!(ws.state.tabs.active_tab_id, Some(TabId(3)));
        });

        // From tab 1 in creation order: ActivateNextTab → tab 2 (not MRU next).
        workspace.update(&mut cx, |ws, cx| {
            ws.activate_tab(TabId(1), cx);
        });
        cx.dispatch_action(ActivateNextTab);
        workspace.read_with(&cx, |ws, _| {
            assert_eq!(
                ws.state.tabs.active_tab_id,
                Some(TabId(2)),
                "cmd-shift-] must walk creation order, not MRU"
            );
        });
    }

    #[gpui::test]
    fn close_tab_removes_from_mru(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab);
        cx.dispatch_action(NewTab);
        workspace.update(&mut cx, |ws, cx| {
            ws.activate_tab(TabId(2), cx);
        });
        workspace.read_with(&cx, |ws, _| {
            assert!(ws.tab_mru.contains(&TabId(3)));
        });
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.close_tab_by_id(TabId(3), window, cx);
        });
        workspace.read_with(&cx, |ws, _| {
            assert!(!ws.tab_mru.contains(&TabId(3)));
            assert_eq!(ws.tab_mru.len(), 2);
        });
    }

    #[gpui::test]
    fn tab_switcher_next_and_enter_activates_mru(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab); // mru: 2, 1
        cx.dispatch_action(NewTab); // mru: 3, 2, 1 active 3
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.tab_switcher_next(window, cx);
            assert!(ws.tab_switcher_open);
            assert_eq!(ws.tab_switcher_index, 1, "first open starts at MRU[1]");
            assert_eq!(ws.tab_mru[1], TabId(2));
            ws.confirm_tab_switcher(window, cx);
            assert!(!ws.tab_switcher_open);
            assert_eq!(ws.state.tabs.active_tab_id, Some(TabId(2)));
            assert_eq!(ws.tab_mru[0], TabId(2));
        });
    }

    #[gpui::test]
    fn tab_switcher_esc_cancels_without_switch(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab);
        cx.dispatch_action(NewTab); // active 3
        workspace.update_in(&mut cx, |ws, window, cx| {
            assert_eq!(ws.state.tabs.active_tab_id, Some(TabId(3)));
            ws.tab_switcher_next(window, cx);
            assert_eq!(ws.tab_switcher_index, 1);
            ws.cancel_active_modal(window, cx);
            assert!(!ws.tab_switcher_open);
            assert_eq!(
                ws.state.tabs.active_tab_id,
                Some(TabId(3)),
                "Esc must not change the active tab"
            );
        });
    }

    #[gpui::test]
    fn cancel_modal_closes_palette_before_switcher(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.tab_switcher_next(window, cx);
            assert!(ws.tab_switcher_open);
            ws.open_command_palette(window, cx);
            assert!(ws.palette_open);
            ws.cancel_active_modal(window, cx);
            assert!(!ws.palette_open, "palette closes first");
            assert!(ws.tab_switcher_open, "switcher remains until next Esc");
            ws.cancel_active_modal(window, cx);
            assert!(!ws.tab_switcher_open);
        });
    }

    #[gpui::test]
    fn command_palette_filters_and_runs_new_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            assert_eq!(ws.state.tabs.tabs.len(), 1);
            ws.open_command_palette(window, cx);
            assert!(ws.palette_open);
            ws.palette_input.set_value("new tab");
            // rebuild selection to first hit
            ws.palette_selected = 0;
            ws.execute_palette_selected(window, cx);
            assert!(!ws.palette_open);
            assert_eq!(ws.state.tabs.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn command_palette_esc_closes_before_other_modals(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.open_go_to_path(window, cx);
            assert!(ws.go_to_path_open);
            ws.open_command_palette(window, cx);
            assert!(ws.palette_open);
            ws.cancel_active_modal(window, cx);
            assert!(!ws.palette_open, "Esc closes palette first");
            assert!(ws.go_to_path_open, "underlying go-to-path stays open");
            ws.cancel_active_modal(window, cx);
            assert!(!ws.go_to_path_open);
        });
    }

    /// App-level wiring: after workspace init, `SharedTransfers.rates` is
    /// reachable via `ActiveTransfers` and produces speed from observes.
    #[gpui::test]
    fn rate_book_observe_produces_speed_snapshot(cx: &mut TestAppContext) {
        use std::time::{Duration, Instant};

        let (workspace, mut cx, _channels) = init_workspace(cx);
        let transfer_id = TransferId(7);
        let t0 = Instant::now();

        workspace.update(&mut cx, |_workspace, cx| {
            cx.rates_mut().observe(transfer_id, 0, t0);
            cx.rates_mut()
                .observe(transfer_id, 1_000_000, t0 + Duration::from_secs(1));
            let snap = cx.rates().snapshot(
                transfer_id,
                1_000_000,
                Some(2_000_000),
                t0 + Duration::from_secs(1),
            );
            assert!(
                snap.speed_bps
                    .is_some_and(|speed| speed > 900_000.0 && speed < 1_100_000.0),
                "expected ~1 MB/s after two observes 1s apart, got {:?}",
                snap.speed_bps
            );
            assert!(!snap.stalled);
            let detail =
                crate::rate_sampler::format_running_detail(1_000_000, Some(2_000_000), &snap);
            assert!(
                detail.contains("MB/s") || detail.contains("KB/s"),
                "detail should include speed: {detail}"
            );
            assert!(detail.contains("ETA"), "detail should include ETA: {detail}");
        });
    }
}
