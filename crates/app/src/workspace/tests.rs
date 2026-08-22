#![allow(unused_imports)]
#![allow(clippy::module_inception)]
// Tests use unwrap/expect for fixtures; production keeps unwrap_used deny.
#![allow(clippy::unwrap_used)]
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
    ConflictRequestId, ConnectCommand, ConnectionPoolIdentity, ConnectionProfile,
    ConnectionSettings, ConnectionState, DisconnectReason, EntryPath, ErrorCode, FileKind,
    HostKeyDecisionCommand, HostKeyPrompt, LocalPath, ModalRequest, ModalRequestId, ProfileId,
    RemotePath, SecretRef, TabId, TabState, Timestamp, TransferConflictPrompt, TransferDirection,
    TransferEndpoint, TransferJob, TransferState, TrustRequestId, UserFacingError, sort_entries,
};
use macsftp_platform::{AppPaths, read_local_directory};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::{AppearancePreference, ConfigStore, ProfileStore, ResidualTempStore};
use macsftp_ui::{
    ActiveTheme, DragPreview, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel,
    Theme, TransferRow, connection_status, copy_name, empty_state, file_row, file_table_header,
    format_size, format_timestamp, icon, icon_button, section_header_static, tab, text_button,
    text_field, text_tooltip, transfer_row, transfer_title,
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
        AppCommand, AppEvent, AuthCredential, AuthMethod, AuthMethodKind, ConflictDecision,
        ConflictPolicy, ConflictRequestId, ConnectionPoolIdentity, ConnectionSettings,
        ConnectionState, DisconnectReason, EditPhase, EntryPath, ErrorCode, FileKind,
        FileSortField, HostKeyPrompt, LocalPath, MetadataPolicy, ProfileId, RemoteDirSnapshot,
        RemoteEntry, RemoteEventScope, RemoteOperationFailure, RemotePath, RemoteScoped,
        RestoredTabTarget, RuntimeBridgeConfig, SessionId, SortDirection, TabConnected,
        TabDisconnected, TabId, Timestamp, TransferConflictPrompt, TransferDirection,
        TransferEndpoint, TransferId, TransferJob, TransferPlanId, TransferPlanProgress,
        TransferPlanSnapshot, TransferPlanState, TransferState, TrustRequestId, UserFacingError,
        WindowSessionId,
    };
    use macsftp_core::{EditSession, EditSessionId, RemoteSnapshot};
    use macsftp_sftp::{BridgeChannels, EventReceiver, RuntimeClient};
    use macsftp_storage::{
        AppearancePreference, SessionFile, SessionStore, SessionTabSnapshot, SessionWindowSnapshot,
    };
    use macsftp_ui::{Appearance, Theme};

    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        AppPaths, PaneSide, STATUS_BUSY_TRY_AGAIN, STATUS_CONNECTION_SERVICE_UNAVAILABLE,
        Workspace, WorkspaceSurface,
    };
    use crate::app_actions::{
        self, ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, FilterPane, GoToPath,
        NavigateBack, NewTab, OpenProfiles, OpenSettings, PageDown, SelectAllEntries,
        SelectNextEntry, SelectNextEntryExtend, SelectPrevEntry, ShowAbout, ShowTransferDrawer,
        ToggleHiddenFiles,
    };
    use crate::resources::{ActiveResources, ActiveTransfers};
    use crate::session_coordinator::SessionCoordinator;
    use crate::workspace::ConflictChoice;
    use crate::workspace::profiles::{SettingsSection, profile_matches_filter};
    use macsftp_core::HistoryOp;
    use macsftp_ui::InputState;

    const TEST_REMOTE_ROOT: &str = "/home/tester";

    /// Per-process unique sequence so parallel test invocations never share a
    /// temp directory (AGENTS.md §9; audit TEST-001).
    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "macsftp-app-test-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    thread_local! {
        static REOPENED_EDIT_PATHS: RefCell<Vec<LocalPath>> = const { RefCell::new(Vec::new()) };
    }

    fn record_edit_reopen(temp: &LocalPath, _editor: Option<&str>) -> std::io::Result<()> {
        REOPENED_EDIT_PATHS.with(|paths| paths.borrow_mut().push(temp.clone()));
        Ok(())
    }

    fn init_workspace(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, VisualTestContext, BridgeChannels) {
        init_workspace_with_paths(cx, temp_app_paths(), false)
    }

    fn fill_command_channel(channels: &BridgeChannels) {
        while channels
            .command_tx
            .try_send(AppCommand::CloseTab { tab_id: TabId(999) })
            .is_ok()
        {}
        assert!(
            channels.command_tx.is_full(),
            "command channel must be full"
        );
    }

    fn set_local_path_and_wait(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        path: LocalPath,
    ) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.set_local_path(path, window, cx);
        });
        cx.run_until_parked();
    }

    fn init_workspace_with_paths(
        cx: &mut TestAppContext,
        app_paths: AppPaths,
        restore_session: bool,
    ) -> (Entity<Workspace>, VisualTestContext, BridgeChannels) {
        let config = macsftp_storage::ConfigStore::with_defaults(app_paths.config_file.clone());
        let mut session_coordinator =
            SessionCoordinator::new(SessionStore::open_or_empty(app_paths.session_file.clone()));
        let restore_snapshot = restore_session
            .then(|| session_coordinator.take_restore_session())
            .and_then(|session| session.windows.into_iter().next());
        let window_session_id = restore_snapshot
            .as_ref()
            .map(|snapshot| snapshot.id)
            .unwrap_or_else(|| session_coordinator.allocate_window_id());
        session_coordinator.register_window(window_session_id);
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            app_actions::init(cx);
            // Shared globals must exist before `Workspace::new` (which reads
            // them). A fresh `AppResources` per test → fresh tab-id counter
            // starting at 1, keeping each test isolated.
            cx.set_global(crate::resources::AppResources::load_for_test(
                app_paths, config,
            ));
            cx.set_global(session_coordinator);
            cx.set_global(crate::resources::SharedTransfers::default());
        });
        let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
        let client = RuntimeClient::new(channels.command_tx.clone());
        let window = cx.add_window(|window, cx| {
            Workspace::new(client, window_session_id, restore_snapshot, window, cx)
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

    #[gpui::test]
    fn local_network_recovery_opens_the_macos_privacy_pane(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.open_local_network_settings(cx);
        });

        assert_eq!(
            cx.opened_url().as_deref(),
            Some(macsftp_platform::MACOS_LOCAL_NETWORK_SETTINGS_URL)
        );
    }

    /// Drive the mock connect flow up to the host key prompt: send the
    /// connect command and inject the actor's `HostKeyUnknown` event.
    fn connect_and_prompt(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> HostKeyPrompt {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        let dir = unique_temp_dir("sel");
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("alpha.txt"), b"a").expect("write alpha.txt");
        std::fs::create_dir(dir.join("beta")).expect("create beta dir");
        std::fs::write(dir.join("gamma.txt"), b"g").expect("write gamma.txt");
        let local_root = LocalPath::new(dir.to_string_lossy().into_owned());

        set_local_path_and_wait(&workspace, &mut cx, local_root.clone());

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

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
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

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
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

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
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
            assert!(workspace.transfer_drawer.open, "drawer starts open");
        });

        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(!workspace.transfer_drawer.open);
        });

        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.transfer_drawer.open);
        });
    }

    #[gpui::test]
    fn transfer_drawer_default_height(cx: &mut TestAppContext) {
        let (workspace, _cx, _channels) = init_workspace(cx);
        workspace.read_with(&_cx, |workspace, _| {
            assert_eq!(
                workspace.transfer_drawer.height,
                crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
            );
        });
    }

    #[gpui::test]
    fn transfer_drawer_height_survives_toggle(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update(&mut cx, |workspace, _cx| {
            workspace.set_drawer_height(gpui::px(180.0), gpui::px(900.0));
        });
        cx.dispatch_action(ShowTransferDrawer);
        cx.dispatch_action(ShowTransferDrawer);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.transfer_drawer.open);
            assert_eq!(workspace.transfer_drawer.height, gpui::px(180.0));
        });
    }

    #[gpui::test]
    fn transfer_drawer_reset_height(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update(&mut cx, |workspace, _cx| {
            workspace.set_drawer_height(gpui::px(360.0), gpui::px(900.0));
            workspace.reset_drawer_height(gpui::px(900.0));
        });
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(
                workspace.transfer_drawer.height,
                crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
            );
        });
    }

    #[gpui::test]
    fn transfer_drawer_min_height_does_not_close(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update(&mut cx, |workspace, _cx| {
            // Simulate dragging the handle all the way down.
            workspace.set_drawer_height(gpui::px(1.0), gpui::px(900.0));
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.transfer_drawer.open,
                "clamping to min height must not auto-close the drawer"
            );
            assert_eq!(
                workspace.transfer_drawer.height,
                crate::workspace::drawer_height::MIN_DRAWER_HEIGHT
            );
            assert!(workspace.transfer_drawer.resize.is_none());
        });
    }

    #[gpui::test]
    fn transfer_drawer_resize_delta_grows_when_dragging_up(cx: &mut TestAppContext) {
        // Logic mirror of on_drag_move: new_height = start_height + (start_y - current_y).
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update(&mut cx, |workspace, _cx| {
            let start = crate::workspace::drawer_height::TransferDrawerResize {
                start_height: gpui::px(240.0),
                start_y: gpui::px(500.0),
            };
            workspace.transfer_drawer.resize = Some(start.clone());
            let current_y = gpui::px(400.0); // drag up → taller
            let new_height = start.start_height + (start.start_y - current_y);
            workspace.set_drawer_height(new_height, gpui::px(900.0));
            workspace.transfer_drawer.resize = None;
        });
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.transfer_drawer.height, gpui::px(340.0));
            assert!(workspace.transfer_drawer.resize.is_none());
            assert!(workspace.transfer_drawer.open);
        });
    }

    #[gpui::test]
    fn settings_action_switches_surface_and_persists_appearance(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(OpenSettings);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.surface, WorkspaceSurface::Settings);
            assert_eq!(
                workspace.settings.section,
                SettingsSection::General,
                "OpenSettings resets to General"
            );
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
    fn open_profiles_action_opens_settings_profiles_section(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(OpenProfiles);
        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.surface, WorkspaceSurface::Settings);
            assert_eq!(
                workspace.settings.section,
                SettingsSection::Profiles,
                "OpenProfiles opens Settings on the Profiles section"
            );
        });
    }

    #[gpui::test]
    fn settings_profiles_section_lists_saved_profiles(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |ws, _window, cx| {
            let profile = macsftp_core::ConnectionProfile::new(
                ProfileId(1),
                "Work Server",
                "example.com",
                "alex",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(1), "password"),
                },
            );
            match cx.resources_mut().profiles.save_profile(profile) {
                Ok(_) => {}
                Err(error) => panic!("save profile for settings list test: {error}"),
            }
            ws.surface = WorkspaceSurface::Settings;
            ws.set_settings_section(SettingsSection::Profiles, cx);
            assert_eq!(ws.settings.section, SettingsSection::Profiles);
            let n = cx.resources().profiles.profiles().len();
            assert!(n >= 1, "expected at least one saved profile");
            assert!(
                ws.settings.selected_profile_id.is_some(),
                "selected defaults to first when entering Profiles with a non-empty list"
            );
            assert_eq!(
                ws.settings.selected_profile_id,
                Some(ProfileId(1)),
                "first saved profile is selected"
            );
            assert!(
                ws.settings.profile_editor.is_some(),
                "entering Profiles with a selection opens the editor"
            );
        });
    }

    #[gpui::test]
    fn settings_new_profile_save_persists(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |ws, _window, cx| {
            ws.surface = WorkspaceSurface::Settings;
            ws.set_settings_section(SettingsSection::Profiles, cx);
            ws.start_new_profile(cx);

            let editor = ws
                .settings
                .profile_editor
                .as_mut()
                .expect("start_new_profile opens editor");
            assert!(editor.is_new);
            editor.name = InputState::with_value("Prod");
            editor.host = InputState::with_value("sftp.example.com");
            editor.port = InputState::with_value("22");
            editor.username = InputState::with_value("deploy");
            editor.password.set_value("s3cret");
            editor.default_remote_path = InputState::with_value("/var/www");

            ws.save_profile_editor(cx);

            assert_eq!(
                cx.resources().profiles.profiles().len(),
                1,
                "save must persist one profile"
            );
            let profile = &cx.resources().profiles.profiles()[0];
            assert_eq!(profile.name, "Prod");
            assert_eq!(profile.host, "sftp.example.com");
            assert_eq!(profile.username, "deploy");
            assert_eq!(
                profile.default_remote_path.as_ref().map(|p| p.as_str()),
                Some("/var/www")
            );

            let loaded = cx
                .resources()
                .profiles
                .load_connection_settings(profile.id)
                .expect("saved profile credential must load");
            let AuthCredential::Password { password } = &loaded.auth else {
                panic!("expected password auth");
            };
            assert_eq!(password, "s3cret", "password must be stored in Keychain");

            let editor = ws
                .settings
                .profile_editor
                .as_ref()
                .expect("editor remains after save");
            assert!(!editor.is_new, "saved editor is no longer new");
            assert!(editor.secret_present_hint);
            assert!(editor.error.is_none());
            assert_eq!(ws.settings.selected_profile_id, Some(profile.id));
        });
    }

    #[gpui::test]
    fn settings_edit_host_keeps_keychain_secret_when_password_blank(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |ws, _window, cx| {
            let profile_id = ProfileId(1);
            let settings = ConnectionSettings {
                host: "old.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "original-pass".into(),
                },
            };
            match cx.resources_mut().profiles.save_connection_settings(
                profile_id,
                "Work".into(),
                &settings,
            ) {
                Ok(_) => {}
                Err(error) => panic!("seed profile and credential: {error}"),
            }

            ws.surface = WorkspaceSurface::Settings;
            ws.load_profile_editor(profile_id, cx);

            let editor = ws
                .settings
                .profile_editor
                .as_mut()
                .expect("load_profile_editor opens editor");
            assert!(!editor.is_new);
            assert!(
                editor.password.value().is_empty(),
                "password must not be prefilled"
            );
            editor.host = InputState::with_value("new.example.com");
            // password left empty on purpose

            ws.save_profile_editor(cx);

            let profile = cx
                .resources()
                .profiles
                .find_profile(profile_id)
                .expect("profile still present");
            assert_eq!(profile.host, "new.example.com");
            let loaded = cx
                .resources()
                .profiles
                .load_connection_settings(profile_id)
                .expect("saved profile credential must load");
            let AuthCredential::Password { password } = &loaded.auth else {
                panic!("expected password auth retained");
            };
            assert_eq!(
                password, "original-pass",
                "blank password on edit must keep existing Keychain secret"
            );
        });
    }

    #[gpui::test]
    fn settings_new_password_profile_requires_password(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |ws, _window, cx| {
            ws.surface = WorkspaceSurface::Settings;
            ws.start_new_profile(cx);

            let editor = ws
                .settings
                .profile_editor
                .as_mut()
                .expect("start_new_profile opens editor");
            editor.host = InputState::with_value("example.com");
            editor.username = InputState::with_value("alex");
            // password left empty

            ws.save_profile_editor(cx);

            assert!(
                cx.resources().profiles.profiles().is_empty(),
                "validation failure must not write profiles.json"
            );
            let editor = ws.settings.profile_editor.as_ref().expect("editor remains");
            assert!(editor.is_new);
            assert_eq!(
                editor.error.as_ref().map(|s| s.as_ref()),
                Some("Password is required."),
                "empty password on new profile is a validation error"
            );
        });
    }

    #[gpui::test]
    fn delete_profile_requires_confirmation(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        let profile_id = ProfileId(1);
        workspace.update_in(&mut cx, |ws, window, cx| {
            let settings = ConnectionSettings {
                host: "example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "hunter2".into(),
                },
            };
            match cx.resources_mut().profiles.save_connection_settings(
                profile_id,
                "Work Server".into(),
                &settings,
            ) {
                Ok(_) => {}
                Err(error) => panic!("seed profile and credential: {error}"),
            }

            ws.surface = WorkspaceSurface::Settings;
            ws.set_settings_section(SettingsSection::Profiles, cx);
            assert_eq!(ws.settings.selected_profile_id, Some(profile_id));

            // Request only arms the confirm state — store and Keychain stay intact.
            ws.request_delete_profile(profile_id, window, cx);
            assert_eq!(ws.settings.profile_delete_confirm, Some(profile_id));
            assert!(
                cx.resources().profiles.find_profile(profile_id).is_some(),
                "request_delete must not remove the profile yet"
            );
            let loaded = cx
                .resources()
                .profiles
                .load_connection_settings(profile_id)
                .expect("profile credential must load");
            let AuthCredential::Password { password } = &loaded.auth else {
                panic!("expected password auth");
            };
            assert_eq!(
                password, "hunter2",
                "request_delete must not remove Keychain secrets yet"
            );

            // Cancel leaves everything as-is.
            ws.cancel_delete_profile(window, cx);
            assert!(ws.settings.profile_delete_confirm.is_none());
            assert!(
                cx.resources().profiles.find_profile(profile_id).is_some(),
                "cancel must keep the profile"
            );

            // Confirm deletes profile + Keychain entry and clears settings selection.
            ws.request_delete_profile(profile_id, window, cx);
            ws.confirm_delete_profile(window, cx);
            assert!(ws.settings.profile_delete_confirm.is_none());
            assert!(
                cx.resources().profiles.profiles().is_empty(),
                "confirm must delete the profile from the store"
            );
            assert!(
                ws.settings.selected_profile_id.is_none(),
                "deleted selection clears selected_profile_id when no profiles remain"
            );
            assert!(
                ws.settings.profile_editor.is_none(),
                "editor closes when the selected profile is deleted and none remain"
            );
        });
    }

    #[test]
    fn profile_matches_filter_name_host_user() {
        let profile = macsftp_core::ConnectionProfile::new(
            ProfileId(1),
            "Work Server",
            "example.com",
            "alex",
            AuthMethod::Password {
                secret_ref: macsftp_core::SecretRef::new("test-ref"),
            },
        );
        assert!(profile_matches_filter(&profile, ""));
        assert!(profile_matches_filter(&profile, "   "));
        assert!(profile_matches_filter(&profile, "work"));
        assert!(profile_matches_filter(&profile, "WORK"));
        assert!(profile_matches_filter(&profile, "example"));
        assert!(profile_matches_filter(&profile, "alex"));
        assert!(!profile_matches_filter(&profile, "nomatch"));
    }

    #[gpui::test]
    fn settings_profile_filter_narrows_list(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |ws, _window, cx| {
            let work = macsftp_core::ConnectionProfile::new(
                ProfileId(1),
                "Work Server",
                "work.example.com",
                "alex",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(1), "password"),
                },
            );
            let home = macsftp_core::ConnectionProfile::new(
                ProfileId(2),
                "Home NAS",
                "nas.local",
                "admin",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(2), "password"),
                },
            );
            match cx.resources_mut().profiles.save_profile(work) {
                Ok(_) => {}
                Err(error) => panic!("seed work profile: {error}"),
            }
            match cx.resources_mut().profiles.save_profile(home) {
                Ok(_) => {}
                Err(error) => panic!("seed home profile: {error}"),
            }

            let all = cx.resources().profiles.profiles().to_vec();
            assert_eq!(all.len(), 2);
            assert_eq!(
                ws.filtered_profiles(&all).len(),
                2,
                "empty filter matches every profile"
            );

            ws.settings.profile_filter.set_value("nas.local");
            let filtered = ws.filtered_profiles(&all);
            assert_eq!(filtered.len(), 1, "filter must narrow to one host");
            assert_eq!(filtered[0].id, ProfileId(2));
            assert_eq!(filtered[0].host, "nas.local");

            ws.settings.profile_filter.set_value("nomatch");
            assert!(
                ws.filtered_profiles(&all).is_empty(),
                "non-matching filter yields no profiles"
            );
        });
    }

    #[gpui::test]
    fn about_action_opens_and_escape_closes_overlay(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        cx.dispatch_action(ShowAbout);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.modal_inputs.about_open)
        });

        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| {
            assert!(!workspace.modal_inputs.about_open)
        });
    }

    #[gpui::test]
    fn about_escape_restores_pane_focus(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            // Simulate focus having left the pane (e.g. after interacting with About).
            window.focus(&ws.modal_focus);
            ws.modal_inputs.about_open = true;
            ws.cancel_active_modal(window, cx);
            assert!(!ws.modal_inputs.about_open, "Esc must close About");
            let pane = ws.pane_focus(ws.focused_side).clone();
            assert!(
                pane.is_focused(window),
                "Esc from About must restore pane focus"
            );
        });
    }

    #[gpui::test]
    fn about_close_button_restores_pane_focus(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            window.focus(&ws.modal_focus);
            ws.modal_inputs.about_open = true;
            ws.close_about(window, cx);
            assert!(!ws.modal_inputs.about_open, "Close must dismiss About");
            let pane = ws.pane_focus(ws.focused_side).clone();
            assert!(
                pane.is_focused(window),
                "About Close button must restore pane focus"
            );
        });
    }

    #[gpui::test]
    fn go_to_path_escape_restores_pane_focus(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.open_go_to_path(window, cx);
            assert!(ws.go_to_path.open);
            assert!(
                ws.modal_focus.is_focused(window),
                "open_go_to_path focuses modal_focus"
            );
            ws.cancel_active_modal(window, cx);
            assert!(!ws.go_to_path.open);
            let pane = ws.pane_focus(ws.focused_side).clone();
            assert!(
                pane.is_focused(window),
                "Esc from Go to Path restores pane focus"
            );
        });
    }

    // ── Runtime bridge wiring (M2a/M2c app side) ───────────────────

    #[gpui::test]
    fn connect_sends_command_with_ui_allocated_session(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
                assert_eq!(
                    connect.pool_identity,
                    ConnectionPoolIdentity::Ephemeral(SessionId(1))
                );
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
    fn full_command_channel_keeps_host_key_prompt_actionable(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = connect_and_prompt(&workspace, &mut cx);
        fill_command_channel(&channels);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.accept_host_key(prompt.request_id, window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.active_host_key_prompt().is_some());
            assert!(matches!(
                workspace.active_tab().expect("tab").connection,
                ConnectionState::AwaitingHostKey { request_id, .. }
                    if request_id == prompt.request_id
            ));
            assert_eq!(
                workspace
                    .status_message
                    .as_ref()
                    .map(|message| message.as_ref()),
                Some(STATUS_BUSY_TRY_AGAIN)
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
            workspace.connect_with(test_settings(), None, window, cx);
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
    fn full_command_channel_does_not_fake_cancel_connect(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        fill_command_channel(&channels);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.cancel_connect(window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            assert!(matches!(
                workspace.active_tab().expect("tab").connection,
                ConnectionState::Connecting { .. }
            ));
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
            assert_eq!(
                workspace.modal_inputs.conflict_rename.value(),
                "existing (copy).txt"
            );
            let pending = cx
                .transfers()
                .pending_conflicts
                .last()
                .expect("conflict should be recorded for the drawer");
            assert_eq!(pending.source_size, Some(11));
            assert_eq!(
                pending.source_modified_at,
                Some(macsftp_core::Timestamp::from_secs_since_epoch(42))
            );

            workspace.modal_inputs.conflict_rename.set_value("..");
            workspace.submit_transfer_rename(false, window, cx);
            assert!(workspace.modal_inputs.conflict_rename_error.is_some());
            assert!(workspace.active_transfer_conflict_prompt().is_some());

            workspace
                .modal_inputs
                .conflict_rename
                .set_value("renamed.txt");
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
            assert!(workspace.modal_inputs.conflict_rename_error.is_none());
        });
    }

    #[gpui::test]
    fn full_command_channel_keeps_transfer_conflict_modal(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let prompt = TransferConflictPrompt {
            request_id: ConflictRequestId(7),
            plan_id: TransferPlanId(3),
            transfer_id: TransferId(9),
            source: TransferEndpoint::Local(LocalPath::new("/tmp/source.txt")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/existing.txt")),
            source_size: Some(11),
            source_modified_at: None,
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(AppEvent::TransferConflict(prompt.clone()), window, cx);
        });
        fill_command_channel(&channels);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.resolve_transfer_conflict(
                &prompt,
                ConflictDecision::Overwrite {
                    apply_to_all: false,
                },
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.has_transfer_conflict_modal(prompt.request_id));
        });
        assert!(
            cx.cx.read(|cx| cx
                .transfers()
                .pending_conflicts
                .iter()
                .any(|pending| { pending.id == prompt.request_id })),
            "global conflict must remain retryable"
        );
    }

    #[gpui::test]
    fn stale_host_key_prompt_is_dropped(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
    fn session_snapshot_never_contains_cached_credentials(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let tab = workspace
                .state
                .tabs
                .find_tab_mut(TabId(1))
                .expect("initial tab must exist");
            tab.restored_target = Some(Box::new(RestoredTabTarget {
                host: "mock.example.com".into(),
                port: 22,
                username: "tester".into(),
                profile_id: None,
                remote_path: Some(RemotePath::new("/home/restored")),
            }));
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();

        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        remote_root: RemotePath::new("/home/root"),
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |workspace, _| {
            let tab = workspace.state.tabs.active_tab().expect("tab must exist");
            assert_eq!(
                tab.remote.path,
                Some(RemotePath::new("/home/restored")),
                "TabConnected must navigate to the restored path, not remote_root"
            );
            assert!(
                workspace
                    .state
                    .tabs
                    .find_tab(TabId(1))
                    .and_then(|tab| tab.restored_target.as_ref())
                    .and_then(|target| target.remote_path.as_ref())
                    .is_none(),
                "restored path preference must be cleared after first connect"
            );
        });

        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path == RemotePath::new("/home/restored")
        ));
    }

    #[gpui::test]
    fn classified_disconnect_reasons_reach_the_connection_state(cx: &mut TestAppContext) {
        // Mid-session drops now arrive with a classified UserFacingError so
        // the remote pane can say WHY the connection dropped instead of a
        // generic "Disconnected". The stale-event guard must keep treating
        // them like any other scoped TabDisconnected.
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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

        let network_timeout = macsftp_core::UserFacingError::new(
            macsftp_core::ErrorCode::NetworkTimeout,
            "Connection lost",
            "No response from the server for about a minute.",
        )
        .with_retryable(true);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    scope.clone(),
                    TabDisconnected {
                        reason: DisconnectReason::Error(network_timeout),
                    },
                )),
                window,
                cx,
            );
            let tab = workspace.active_tab().expect("tab must exist");
            let macsftp_core::ConnectionState::Disconnected {
                reason: DisconnectReason::Error(error),
            } = &tab.connection
            else {
                panic!("expected Disconnected(Error), got {:?}", tab.connection);
            };
            assert_eq!(error.code, macsftp_core::ErrorCode::NetworkTimeout);
            assert!(tab.remote.entries.is_empty());
        });

        // A stale-epoch repeat of the same event must not resurrect state.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let stale_scope = RemoteEventScope::new(TabId(1), SessionId(1), 0);
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    stale_scope,
                    TabDisconnected {
                        reason: DisconnectReason::UserRequested,
                    },
                )),
                window,
                cx,
            );
            let tab = workspace.active_tab().expect("tab must exist");
            assert!(matches!(
                &tab.connection,
                macsftp_core::ConnectionState::Disconnected {
                    reason: DisconnectReason::Error(error),
                } if error.code == macsftp_core::ErrorCode::NetworkTimeout
            ));
        });
    }

    #[gpui::test]
    fn disconnect_preserves_last_remote_path_for_session_restore(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    RemoteEventScope::new(TabId(1), SessionId(1), 1),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        while channels.command_rx.try_recv().is_ok() {}

        // User navigated deeper while connected.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let tab = workspace.active_tab_mut().expect("tab must exist");
            tab.remote.path = Some(RemotePath::new("/home/tester/projects"));
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    RemoteEventScope::new(TabId(1), SessionId(1), 1),
                    TabDisconnected {
                        reason: DisconnectReason::UserRequested,
                    },
                )),
                window,
                cx,
            );
            let tab = workspace.active_tab().expect("tab must exist");
            assert!(
                tab.remote.path.is_none(),
                "live remote path is cleared on disconnect"
            );
            let snapshot = workspace.build_session_snapshot();
            assert_eq!(
                snapshot.tabs[0].remote_path.as_deref(),
                Some("/home/tester/projects"),
                "session snapshot must keep the last remote path after disconnect"
            );
        });
    }

    #[gpui::test]
    fn tab_connected_upserts_recents(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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

        workspace.read_with(&cx, |_workspace, cx| {
            let entries = cx.resources().recents.entries();
            assert_eq!(
                entries.len(),
                1,
                "successful connect must upsert one recent"
            );
            assert_eq!(entries[0].host, "mock.example.com");
            assert_eq!(entries[0].port, 22);
            assert_eq!(entries[0].username, "tester");
            assert_eq!(
                entries[0].last_remote_path.as_deref(),
                Some(TEST_REMOTE_ROOT)
            );
            assert!(entries[0].profile_id.is_none());
            assert!(entries[0].display_name.is_none());
            assert!(entries[0].last_connected_at > 0);
        });
    }

    #[gpui::test]
    fn tab_connected_dedupes_recents(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        // First successful connect.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        while channels.command_rx.try_recv().is_ok() {}

        // Navigate, disconnect, then connect again with the same host/user/port.
        // Disconnect stashes the last live remote path for reconnect/recents.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let tab = workspace.active_tab_mut().expect("tab must exist");
            tab.remote.path = Some(RemotePath::new("/tmp"));
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    RemoteEventScope::new(TabId(1), SessionId(1), 1),
                    TabDisconnected {
                        reason: DisconnectReason::UserRequested,
                    },
                )),
                window,
                cx,
            );
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();
        // begin_connect bumps session_epoch to 2.
        let scope = RemoteEventScope::new(TabId(1), SessionId(2), 2);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    scope,
                    TabConnected {
                        // Actor root differs; reconnect must prefer the stashed path.
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        workspace.read_with(&cx, |_workspace, cx| {
            let entries = cx.resources().recents.entries();
            assert_eq!(
                entries.len(),
                1,
                "same host/port/user must dedupe to one recent entry"
            );
            assert_eq!(entries[0].host, "mock.example.com");
            assert_eq!(entries[0].username, "tester");
            assert_eq!(entries[0].last_remote_path.as_deref(), Some("/tmp"));
        });
    }

    #[test]
    fn format_recent_label_uses_display_name_when_present() {
        use super::format_recent_label;
        use macsftp_storage::RecentEntry;

        let with_name = RecentEntry {
            id: 1,
            host: "example.com".into(),
            port: 22,
            username: "alex".into(),
            profile_id: Some(3),
            display_name: Some("Prod".into()),
            last_remote_path: None,
            last_connected_at: 1,
        };
        assert_eq!(
            format_recent_label(&with_name),
            "Prod · alex@example.com:22"
        );

        let without_name = RecentEntry {
            display_name: None,
            ..with_name.clone()
        };
        assert_eq!(format_recent_label(&without_name), "alex@example.com:22");

        let custom_port = RecentEntry {
            port: 2222,
            display_name: None,
            ..with_name
        };
        assert_eq!(format_recent_label(&custom_port), "alex@example.com:2222");
    }

    #[gpui::test]
    fn open_recent_without_profile_prefills_form(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        let recent_id = workspace.update_in(&mut cx, |_workspace, _window, cx| {
            cx.resources_mut()
                .recents
                .upsert(macsftp_storage::RecentEntryInput {
                    host: "recent.example.com".into(),
                    port: 2222,
                    username: "alex".into(),
                    profile_id: None,
                    display_name: None,
                    last_remote_path: Some("/var/www".into()),
                    last_connected_at: 1,
                })
                .expect("seed recent without profile");
            cx.resources().recents.entries()[0].id
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_recent_connection(recent_id, window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("form must open when recent has no profile");
            assert_eq!(form.host.value(), "recent.example.com");
            assert_eq!(form.port.value(), "2222");
            assert_eq!(form.username.value(), "alex");
            assert!(form.source_profile_id.is_none());
            let tab = workspace.state.tabs.active_tab().expect("tab exists");
            assert!(
                !matches!(tab.connection, ConnectionState::Connecting { .. }),
                "must not auto-connect without a profile/secret"
            );
            assert_eq!(
                tab.remote.path.as_ref().map(|path| path.as_str()),
                Some("/var/www"),
                "last_remote_path must seed tab.remote.path for post-connect nav"
            );
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no ConnectTab without credentials"
        );
    }

    #[gpui::test]
    fn open_recent_with_profile_and_keychain_connects(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.host.set_value("profile.example.com");
            form.port.set_value("22");
            form.username.set_value("deploy");
            form.password.set_value("s3cret");
            form.profile_name.set_value("Deploy box");
            workspace.save_current_profile(cx);
        });

        let (saved_id, recent_id) = workspace.update_in(&mut cx, |_workspace, _window, cx| {
            let profiles = cx.resources().profiles.profiles();
            assert_eq!(profiles.len(), 1, "profile saved");
            let saved_id = profiles[0].id;
            cx.resources_mut()
                .recents
                .upsert(macsftp_storage::RecentEntryInput {
                    host: "profile.example.com".into(),
                    port: 22,
                    username: "deploy".into(),
                    profile_id: Some(saved_id.0),
                    display_name: Some("Deploy box".into()),
                    last_remote_path: Some("/home/deploy".into()),
                    last_connected_at: 2,
                })
                .expect("seed recent with profile");
            let recent_id = cx.resources().recents.entries()[0].id;
            (saved_id, recent_id)
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            // Close any leftover form so open_recent owns the path.
            workspace.connect_form_ui.form = None;
            workspace.open_recent_connection(recent_id, window, cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("open recent with profile+keychain must send ConnectTab");
        match command {
            AppCommand::ConnectTab(connect) => {
                assert_eq!(connect.settings.host, "profile.example.com");
                assert_eq!(connect.settings.port, 22);
                assert_eq!(connect.settings.username, "deploy");
                assert_eq!(connect.profile_id, saved_id);
            }
            other => panic!("expected ConnectTab, got {other:?}"),
        }
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.connect_form_ui.form.is_none(),
                "form closes when auto-connect succeeds"
            );
            let tab = workspace.state.tabs.active_tab().expect("tab exists");
            assert_eq!(tab.title, "profile.example.com");
            assert_eq!(tab.profile_id, Some(saved_id));
            assert!(
                matches!(tab.connection, ConnectionState::Connecting { .. }),
                "tab must enter Connecting after open recent"
            );
            assert_eq!(
                tab.remote.path.as_ref().map(|path| path.as_str()),
                Some("/home/deploy"),
                "last_remote_path must seed tab.remote.path on auto-connect"
            );
        });
    }

    #[gpui::test]
    fn open_recent_with_missing_keychain_password_keeps_form_open(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.host.set_value("missing-secret.example.com");
            form.port.set_value("22");
            form.username.set_value("deploy");
            form.password.set_value("s3cret");
            form.profile_name.set_value("Missing secret box");
            workspace.save_current_profile(cx);
        });

        let recent_id = workspace.update_in(&mut cx, |_workspace, _window, cx| {
            let profiles = cx.resources().profiles.profiles();
            assert_eq!(profiles.len(), 1, "profile saved");
            let saved = &profiles[0];
            let saved_id = saved.id;
            cx.resources()
                .profiles
                .delete_profile_credentials(saved_id)
                .expect("delete keychain secret for regression");
            assert!(
                cx.resources()
                    .profiles
                    .load_connection_settings(saved_id)
                    .is_err(),
                "secret must be gone before open recent"
            );
            cx.resources_mut()
                .recents
                .upsert(macsftp_storage::RecentEntryInput {
                    host: "missing-secret.example.com".into(),
                    port: 22,
                    username: "deploy".into(),
                    profile_id: Some(saved_id.0),
                    display_name: Some("Missing secret box".into()),
                    last_remote_path: Some("/srv".into()),
                    last_connected_at: 3,
                })
                .expect("seed recent with profile");
            cx.resources().recents.entries()[0].id
        });

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_form_ui.form = None;
            workspace.open_recent_connection(recent_id, window, cx);
        });

        assert!(
            channels.command_rx.try_recv().is_err(),
            "must not ConnectTab when Keychain password is missing"
        );
        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("form stays open so user can re-enter password");
            assert_eq!(form.host.value(), "missing-secret.example.com");
            assert_eq!(form.username.value(), "deploy");
            assert!(
                form.password.value().is_empty(),
                "password field empty when Keychain secret missing"
            );
            let tab = workspace.state.tabs.active_tab().expect("tab exists");
            assert!(
                !matches!(tab.connection, ConnectionState::Connecting { .. }),
                "must not enter Connecting with empty password"
            );
        });
    }

    #[gpui::test]
    fn drag_local_file_onto_remote_pane_begins_upload(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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

    /// Drive a workspace to Connected so `begin_edit` sees a live transfer
    /// session, then drain the command channel (TabConnected enqueues a
    /// ReadRemoteDir) so the next receive is the command under test.
    fn connect_and_drain(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        channels: &BridgeChannels,
    ) {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(cx, |workspace, window, cx| {
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
        while channels.command_rx.try_recv().is_ok() {}
    }

    #[gpui::test]
    fn edit_small_file_starts_download_to_edits_dir(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        let entry = RemoteEntry {
            name: "notes.txt".to_string(),
            path: RemotePath::new("/home/tester/notes.txt"),
            kind: FileKind::File,
            size: Some(2_048),
            permissions: Some(0o644),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            link_target: None,
        };

        let (edits_dir, edit_run_id) = workspace.read_with(&cx, |_workspace, cx| {
            (
                cx.resources().app_paths.edits_dir.as_str().to_string(),
                cx.resources().edit_run_id.clone(),
            )
        });

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(entry.path.clone(), entry.size, entry.modified_at, cx);
        });

        // A single Downloading session is registered, temp path landing under
        // <edits_dir>/<run-id>/<session-id>/<filename>.
        let expected_temp = workspace.read_with(&cx, |workspace, cx| {
            assert!(
                workspace.modal_inputs.large_edit_confirm.is_none(),
                "a small file must not raise the large-file confirmation"
            );
            let store = &cx.resources().edit_sessions;
            let session = store
                .find_active(ProfileId(0), &entry.path)
                .expect("small-file edit must register an active session");
            assert_eq!(session.phase, EditPhase::Downloading);
            assert!(session.active_transfer.is_none());
            assert_eq!(session.remote_path, entry.path);
            let expected = format!("{}/{}/{}/notes.txt", edits_dir, edit_run_id, session.id.0);
            assert_eq!(
                session.local_temp_path.as_str(),
                expected,
                "temp path must be <edits_dir>/<run-id>/<id>/<filename>"
            );
            expected
        });

        // The download command targets that temp path.
        let command = channels
            .command_rx
            .try_recv()
            .expect("begin_edit must enqueue a StartTransfer download");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer for edit download, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Remote(entry.path.clone())]
        );
        assert_eq!(
            command.destination,
            TransferEndpoint::Local(LocalPath::new(expected_temp))
        );
    }

    #[gpui::test]
    fn duplicate_edit_reopens_existing_temp_without_downloading(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);
        REOPENED_EDIT_PATHS.with(|paths| paths.borrow_mut().clear());
        crate::event_coordinator::set_edit_opener(record_edit_reopen);

        let remote_path = RemotePath::new("/home/tester/notes.txt");
        let (session_id, temp_path) = workspace.update_in(&mut cx, |_workspace, _window, cx| {
            let session_id = cx.resources_mut().edit_sessions.next_id();
            let temp_path = LocalPath::new(format!(
                "{}/{}/{}/notes.txt",
                cx.resources().app_paths.edits_dir.as_str(),
                cx.resources().edit_run_id,
                session_id.0
            ));
            let parent = std::path::Path::new(temp_path.as_str())
                .parent()
                .expect("edit temp has a parent directory");
            std::fs::create_dir_all(parent).expect("create edit temp directory");
            std::fs::write(temp_path.as_str(), b"existing edit").expect("write existing edit temp");
            cx.resources_mut().edit_sessions.register(EditSession {
                id: session_id,
                remote_path: remote_path.clone(),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(0),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::Editing,
                remote_snapshot: RemoteSnapshot {
                    size: Some(13),
                    modified_at: Some(Timestamp::from_secs_since_epoch(100)),
                },
                local_mtime: None,
                active_transfer: None,
                pending_check_id: None,
                checking_local_mtime: None,
                missing_ticks: 0,
            });
            (session_id, temp_path)
        });

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(
                remote_path.clone(),
                Some(13),
                Some(Timestamp::from_secs_since_epoch(100)),
                cx,
            );
        });

        REOPENED_EDIT_PATHS.with(|paths| {
            assert_eq!(
                paths.borrow().as_slice(),
                std::slice::from_ref(&temp_path),
                "a second edit must reopen the existing temp file"
            );
        });
        workspace.read_with(&cx, |workspace, cx| {
            let session = cx
                .resources()
                .edit_sessions
                .find_active(ProfileId(0), &remote_path)
                .expect("the existing edit session remains active");
            assert_eq!(session.id, session_id);
            assert_eq!(session.phase, EditPhase::Editing);
            assert_eq!(cx.resources().edit_sessions.editing_sessions().count(), 1);
            assert_eq!(
                workspace.status_message_for_test().as_deref(),
                Some("Reopened file for editing")
            );
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "reopening an existing edit must not dispatch another download"
        );
    }

    #[gpui::test]
    fn edit_reaps_orphaned_existing_session_and_retries(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);
        let remote_path = RemotePath::new("/home/tester/notes.txt");
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let stale_dir = std::env::temp_dir().join(format!(
            "macsftp-stale-edit-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&stale_dir).expect("create stale edit directory");
        let stale_file = stale_dir.join("notes.txt");
        std::fs::write(&stale_file, b"stale").expect("write stale edit file");
        let stale_temp = LocalPath::new(stale_file.to_string_lossy().to_string());
        let stale_id = workspace.update_in(&mut cx, |_workspace, _window, cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            cx.resources_mut().edit_sessions.register(EditSession {
                id,
                remote_path: remote_path.clone(),
                tab_id: TabId(999),
                session_epoch: 1,
                profile_id: ProfileId(0),
                local_temp_path: stale_temp,
                phase: EditPhase::Editing,
                remote_snapshot: RemoteSnapshot {
                    size: Some(5),
                    modified_at: None,
                },
                local_mtime: None,
                active_transfer: None,
                pending_check_id: None,
                checking_local_mtime: None,
                missing_ticks: 0,
            });
            id
        });

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(
                remote_path.clone(),
                Some(2_048),
                Some(Timestamp::from_secs_since_epoch(100)),
                cx,
            );
        });

        workspace.read_with(&cx, |_workspace, cx| {
            assert!(cx.resources().edit_sessions.get(stale_id).is_none());
            let fresh = cx
                .resources()
                .edit_sessions
                .find_active(ProfileId(0), &remote_path)
                .expect("orphaned session must be replaced by a fresh download");
            assert_ne!(fresh.id, stale_id);
            assert_eq!(fresh.phase, EditPhase::Downloading);
        });
        assert!(
            matches!(
                channels.command_rx.try_recv(),
                Ok(AppCommand::StartTransfer(_))
            ),
            "re-editing after a stale session must dispatch a fresh download"
        );
        assert!(
            !stale_dir.exists(),
            "reaping the stale session must remove its temp directory"
        );
    }

    #[gpui::test]
    fn edit_large_file_requires_confirmation_first(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        let entry = RemoteEntry {
            name: "huge.bin".to_string(),
            path: RemotePath::new("/home/tester/huge.bin"),
            kind: FileKind::File,
            size: Some(200 * 1024 * 1024), // > 100 MiB threshold
            permissions: Some(0o644),
            modified_at: Some(Timestamp::from_secs_since_epoch(200)),
            link_target: None,
        };

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(entry.path.clone(), entry.size, entry.modified_at, cx);
        });

        // A large file arms the confirmation and does not download or register
        // a session yet.
        workspace.read_with(&cx, |workspace, cx| {
            let pending = workspace
                .modal_inputs
                .large_edit_confirm
                .as_ref()
                .expect("large file must arm the confirmation");
            assert_eq!(pending.remote_path, entry.path);
            assert_eq!(pending.size, entry.size);
            assert!(
                cx.resources()
                    .edit_sessions
                    .find_active(ProfileId(0), &entry.path)
                    .is_none(),
                "no session may be registered before the user confirms"
            );
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "no StartTransfer may be sent before the user confirms"
        );
    }

    #[gpui::test]
    fn closing_tab_clears_its_large_edit_confirmation(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(
                RemotePath::new("/home/tester/huge.bin"),
                Some(200 * 1024 * 1024),
                Some(Timestamp::from_secs_since_epoch(200)),
                cx,
            );
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.close_tab_by_id(TabId(1), window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.modal_inputs.large_edit_confirm.is_none(),
                "closing the owning tab must dismiss its large-file confirmation"
            );
        });
    }

    #[gpui::test]
    fn closing_another_tab_preserves_large_edit_confirmation(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(
                RemotePath::new("/home/tester/huge.bin"),
                Some(200 * 1024 * 1024),
                Some(Timestamp::from_secs_since_epoch(200)),
                cx,
            );
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_new_tab(window, cx);
            workspace.close_tab_by_id(TabId(2), window, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            let pending = workspace
                .modal_inputs
                .large_edit_confirm
                .as_ref()
                .expect("closing another tab must preserve the confirmation");
            assert_eq!(pending.tab_id, TabId(1));
        });
    }

    #[gpui::test]
    fn confirm_large_edit_starts_download(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        let entry = RemoteEntry {
            name: "huge.bin".to_string(),
            path: RemotePath::new("/home/tester/huge.bin"),
            kind: FileKind::File,
            size: Some(200 * 1024 * 1024), // > 100 MiB threshold
            permissions: Some(0o644),
            modified_at: Some(Timestamp::from_secs_since_epoch(200)),
            link_target: None,
        };

        // Arm the large-file confirmation.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(entry.path.clone(), entry.size, entry.modified_at, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.modal_inputs.large_edit_confirm.is_some(),
                "large file must arm the confirmation"
            );
        });

        // Confirming the large-file edit clears the pending state and starts the
        // download.
        workspace.update(&mut cx, |workspace, cx| {
            workspace.confirm_large_edit(cx);
        });

        let expected_temp = workspace.read_with(&cx, |workspace, cx| {
            assert!(
                workspace.modal_inputs.large_edit_confirm.is_none(),
                "confirming must clear the pending large-file edit"
            );
            let store = &cx.resources().edit_sessions;
            let session = store
                .find_active(ProfileId(0), &entry.path)
                .expect("confirm must register an active download session");
            assert_eq!(session.phase, EditPhase::Downloading);
            session.local_temp_path.clone()
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("confirm_large_edit must enqueue a StartTransfer download");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer for edit download, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Remote(entry.path.clone())]
        );
        assert_eq!(command.destination, TransferEndpoint::Local(expected_temp));
    }

    #[gpui::test]
    fn confirm_large_edit_uses_refreshed_epoch_after_reconnect(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels); // Connected at epoch 1

        let entry = RemoteEntry {
            name: "huge.bin".to_string(),
            path: RemotePath::new("/home/tester/huge.bin"),
            kind: FileKind::File,
            size: Some(200 * 1024 * 1024),
            permissions: Some(0o644),
            modified_at: Some(Timestamp::from_secs_since_epoch(200)),
            link_target: None,
        };

        // Arm the large-file confirmation while at epoch 1.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.begin_edit(entry.path.clone(), entry.size, entry.modified_at, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.modal_inputs.large_edit_confirm.is_some());
        });

        // While the modal is up, the tab reconnects → epoch bumps to 2. Complete
        // the handshake so the tab is Connected again (a reconnect in flight
        // would correctly defer the edit; here we test the refreshed-epoch
        // dispatch once reconnected).
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();
        let reconnect_scope = RemoteEventScope::new(TabId(1), SessionId(2), 2);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    reconnect_scope,
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });
        while channels.command_rx.try_recv().is_ok() {}
        let live_epoch = workspace.read_with(&cx, |workspace, _| {
            workspace
                .state
                .tabs
                .find_tab(TabId(1))
                .expect("tab exists")
                .session_epoch
        });
        assert_eq!(live_epoch, 2, "reconnect must bump the tab epoch");

        // Confirming must dispatch the download with the REFRESHED epoch, not the
        // stale epoch 1 captured when the modal opened (which the runtime would
        // silently drop, stranding the session in Downloading).
        workspace.update(&mut cx, |workspace, cx| {
            workspace.confirm_large_edit(cx);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("confirm_large_edit must enqueue a StartTransfer download");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer for edit download, got {command:?}");
        };
        assert_eq!(
            command.session_epoch, live_epoch,
            "download must carry the refreshed epoch, not the stale captured one"
        );
    }

    #[gpui::test]
    fn request_edit_on_remote_file_starts_edit(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        let file_path = RemotePath::new("/home/tester/notes.txt");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let tab = workspace.active_tab_mut().expect("active tab");
            tab.remote.entries = vec![RemoteEntry {
                name: "notes.txt".to_string(),
                path: file_path.clone(),
                kind: FileKind::File,
                size: Some(2_048),
                permissions: Some(0o644),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
                link_target: None,
            }];
            tab.selection.selected_paths = vec![EntryPath::Remote(file_path.clone())];
            workspace.request_edit_selection(cx);
        });

        // A small remote file goes straight to a Downloading edit session (no
        // large-file confirmation).
        workspace.read_with(&cx, |workspace, cx| {
            assert!(
                workspace.modal_inputs.large_edit_confirm.is_none(),
                "a small file must not raise the large-file confirmation"
            );
            let session = cx
                .resources()
                .edit_sessions
                .find_active(ProfileId(0), &file_path)
                .expect("request_edit_selection on a remote file registers a session");
            assert_eq!(session.phase, EditPhase::Downloading);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("request_edit_selection must enqueue a StartTransfer download");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer for edit download, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(command.sources, vec![TransferEndpoint::Remote(file_path)]);
    }

    #[gpui::test]
    fn request_edit_on_remote_directory_is_noop(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        connect_and_drain(&workspace, &mut cx, &channels);

        let dir_path = RemotePath::new("/home/tester/project");
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let tab = workspace.active_tab_mut().expect("active tab");
            tab.remote.entries = vec![RemoteEntry {
                name: "project".to_string(),
                path: dir_path.clone(),
                kind: FileKind::Directory,
                size: None,
                permissions: Some(0o755),
                modified_at: None,
                link_target: None,
            }];
            tab.selection.selected_paths = vec![EntryPath::Remote(dir_path.clone())];
            workspace.request_edit_selection(cx);
        });

        // A directory is not editable: no session, no command.
        workspace.read_with(&cx, |workspace, cx| {
            assert!(
                workspace.modal_inputs.large_edit_confirm.is_none(),
                "a directory must not arm any edit confirmation"
            );
            assert!(
                cx.resources()
                    .edit_sessions
                    .find_active(ProfileId(0), &dir_path)
                    .is_none(),
                "selecting a directory must not register an edit session"
            );
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "editing a directory must not enqueue any command"
        );
    }

    #[gpui::test]
    fn settings_external_editor_typing_persists_and_round_trips(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        // Open Settings → General and focus the external-editor field, then type
        // a value through the real key handler the GUI routes to.
        cx.dispatch_action(OpenSettings);
        let config_path = workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focus_external_editor(window, cx);
            for ch in ["v", "i", "m"] {
                let event = gpui::KeyDownEvent {
                    keystroke: gpui::Keystroke {
                        modifiers: gpui::Modifiers::default(),
                        key: ch.to_string(),
                        key_char: Some(ch.to_string()),
                    },
                    is_held: false,
                };
                workspace.handle_external_editor_key(&event, window, cx);
            }
            cx.resources().app_paths.config_file.clone()
        });

        // Committed to the in-memory config on every keystroke.
        workspace.read_with(&cx, |_workspace, cx| {
            assert_eq!(
                cx.resources().config.config().external_editor.as_deref(),
                Some("vim"),
                "typing must commit the external editor to config"
            );
        });

        // And persisted to config.json (round-trips through a fresh store).
        let reloaded = macsftp_storage::ConfigStore::open(config_path.clone())
            .expect("config.json must be readable after commit");
        assert_eq!(
            reloaded.config().external_editor.as_deref(),
            Some("vim"),
            "external editor must persist to disk"
        );

        // Clearing the field (empty → None) removes the override and round-trips.
        workspace.update(&mut cx, |workspace, cx| {
            workspace.settings.external_editor_input.set_value("");
            workspace.commit_external_editor(cx);
        });
        workspace.read_with(&cx, |_workspace, cx| {
            assert_eq!(
                cx.resources().config.config().external_editor,
                None,
                "an empty value clears the override"
            );
        });
        let reloaded = macsftp_storage::ConfigStore::open(config_path)
            .expect("config.json must be readable after clearing");
        assert_eq!(
            reloaded.config().external_editor,
            None,
            "cleared override must persist to disk"
        );
    }

    /// Register a `RemoteConflict` edit session directly in the store and return
    /// its id, so the conflict-resolution tests can drive
    /// `resolve_edit_conflict` without replaying a full download+watch cycle.
    fn register_conflict_session(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        remote_path: RemotePath,
        temp_path: LocalPath,
    ) -> EditSessionId {
        workspace.update(cx, |_workspace, cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            cx.resources_mut().edit_sessions.register(EditSession {
                id,
                remote_path,
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(0),
                local_temp_path: temp_path,
                phase: EditPhase::RemoteConflict,
                remote_snapshot: RemoteSnapshot {
                    size: Some(10),
                    modified_at: Some(Timestamp::from_secs_since_epoch(100)),
                },
                local_mtime: None,
                active_transfer: None,
                pending_check_id: None,
                checking_local_mtime: None,
                missing_ticks: 0,
            });
            id
        })
    }

    #[gpui::test]
    fn resolve_conflict_overwrite_uploads(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        let remote_path = RemotePath::new("/home/tester/edit.txt");
        let temp_path = LocalPath::new("/tmp/edits/1/edit.txt");
        let id =
            register_conflict_session(&workspace, &mut cx, remote_path.clone(), temp_path.clone());
        while channels.command_rx.try_recv().is_ok() {}

        workspace.update(&mut cx, |workspace, cx| {
            workspace.resolve_edit_conflict(id, ConflictChoice::Overwrite, cx);
        });

        workspace.read_with(&cx, |_workspace, cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(id)
                .expect("session survives an overwrite");
            assert_eq!(session.phase, EditPhase::UploadingBack);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("overwrite must enqueue a StartTransfer upload");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer for upload, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Upload);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Local(temp_path.clone())]
        );
        assert_eq!(
            command.destination,
            TransferEndpoint::Remote(remote_path.clone())
        );
    }

    #[gpui::test]
    fn resolve_conflict_double_overwrite_dispatches_once(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        let remote_path = RemotePath::new("/home/tester/edit.txt");
        let temp_path = LocalPath::new("/tmp/edits/1/edit.txt");
        let id = register_conflict_session(&workspace, &mut cx, remote_path, temp_path);
        while channels.command_rx.try_recv().is_ok() {}

        // Two clicks before the modal is torn down. The phase guard must make the
        // second a no-op (the first moved the session out of RemoteConflict), so
        // only ONE upload command is dispatched.
        workspace.update(&mut cx, |workspace, cx| {
            workspace.resolve_edit_conflict(id, ConflictChoice::Overwrite, cx);
            workspace.resolve_edit_conflict(id, ConflictChoice::Overwrite, cx);
        });

        assert!(
            channels.command_rx.try_recv().is_ok(),
            "the first overwrite must dispatch an upload"
        );
        assert!(
            channels.command_rx.try_recv().is_err(),
            "a double-click overwrite must not dispatch a second upload"
        );
    }

    #[gpui::test]
    fn resolve_conflict_later_returns_to_editing(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        let id = register_conflict_session(
            &workspace,
            &mut cx,
            RemotePath::new("/home/tester/edit.txt"),
            LocalPath::new("/tmp/edits/1/edit.txt"),
        );
        while channels.command_rx.try_recv().is_ok() {}

        workspace.update(&mut cx, |workspace, cx| {
            workspace.resolve_edit_conflict(id, ConflictChoice::Later, cx);
        });

        workspace.read_with(&cx, |_workspace, cx| {
            let session = cx
                .resources()
                .edit_sessions
                .get(id)
                .expect("session survives a Later choice");
            assert_eq!(session.phase, EditPhase::Editing);
        });
        assert!(
            channels.command_rx.try_recv().is_err(),
            "Later must not enqueue any command"
        );
    }

    #[gpui::test]
    fn resolve_conflict_discard_local_redownloads(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        let remote_path = RemotePath::new("/home/tester/edit.txt");
        let id = register_conflict_session(
            &workspace,
            &mut cx,
            remote_path.clone(),
            LocalPath::new("/tmp/edits/1/edit.txt"),
        );
        while channels.command_rx.try_recv().is_ok() {}

        workspace.update(&mut cx, |workspace, cx| {
            workspace.resolve_edit_conflict(id, ConflictChoice::DiscardLocal, cx);
        });

        // The conflicted session is gone; a fresh Downloading session replaces it.
        workspace.read_with(&cx, |_workspace, cx| {
            let store = &cx.resources().edit_sessions;
            assert!(
                store.get(id).is_none(),
                "discarding must remove the conflicted session"
            );
            let fresh = store
                .find_active(ProfileId(0), &remote_path)
                .expect("discard must re-register a fresh session");
            assert_eq!(fresh.phase, EditPhase::Downloading);
        });

        let command = channels
            .command_rx
            .try_recv()
            .expect("discard must enqueue a StartTransfer download");
        let AppCommand::StartTransfer(command) = command else {
            panic!("expected StartTransfer for re-download, got {command:?}");
        };
        assert_eq!(command.direction, TransferDirection::Download);
        assert_eq!(
            command.sources,
            vec![TransferEndpoint::Remote(remote_path.clone())]
        );
    }

    #[gpui::test]
    fn older_directory_result_does_not_replace_a_newer_requested_path(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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

    /// Connect the workspace and land its tab on `TEST_REMOTE_ROOT` so a
    /// subsequent `RemoteDirLoaded` for that path is accepted. Returns the
    /// remote scope for building further events.
    fn connect_and_land_on_root(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        channels: &BridgeChannels,
    ) -> RemoteEventScope {
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        workspace.update_in(cx, |workspace, window, cx| {
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
        scope
    }

    /// Register an `Editing` edit session on `TabId(1)` for
    /// `/home/tester/doc.txt` with `snapshot` as its remote baseline.
    fn seed_editing_session_on_tab1(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
        snapshot: RemoteSnapshot,
    ) -> EditSessionId {
        workspace.update_in(cx, |_workspace, _window, cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            cx.resources_mut().edit_sessions.register(EditSession {
                id,
                remote_path: RemotePath::new("/home/tester/doc.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                local_temp_path: LocalPath::new("/tmp/edits/doc.txt"),
                phase: EditPhase::Editing,
                remote_snapshot: snapshot,
                local_mtime: None,
                active_transfer: None,
                pending_check_id: None,
                checking_local_mtime: None,
                missing_ticks: 0,
            });
            id
        })
    }

    fn remote_dir_loaded_with_doc(
        scope: RemoteEventScope,
        size: Option<u64>,
        modified_at: Option<Timestamp>,
    ) -> AppEvent {
        AppEvent::RemoteDirLoaded(RemoteScoped::new(
            scope,
            RemoteDirSnapshot {
                path: RemotePath::new(TEST_REMOTE_ROOT),
                entries: vec![RemoteEntry {
                    name: "doc.txt".to_string(),
                    path: RemotePath::new("/home/tester/doc.txt"),
                    kind: FileKind::File,
                    size,
                    permissions: Some(0o644),
                    modified_at,
                    link_target: None,
                }],
            },
        ))
    }

    fn session_snapshot(
        workspace: &Entity<Workspace>,
        cx: &VisualTestContext,
        id: EditSessionId,
    ) -> RemoteSnapshot {
        workspace.read_with(cx, |_workspace, cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("edit session survives refresh")
                .remote_snapshot
        })
    }

    #[gpui::test]
    fn refresh_adopts_listing_when_only_sub_second_mtime_differs(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let scope = connect_and_land_on_root(&workspace, &mut cx, &channels);
        // Session baseline carries a sub-second mtime (as an upload-back rebase
        // could historically leave); the same whole second, same size.
        let sub_second = Timestamp::from_system_time(
            std::time::UNIX_EPOCH
                + std::time::Duration::from_nanos(1_000 * 1_000_000_000 + 837_000_000),
        );
        let baseline = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(sub_second),
        };
        let id = seed_editing_session_on_tab1(&workspace, &mut cx, baseline);

        // The server lists the file at the WHOLE-second mtime, same size — the
        // difference is pure sub-second noise from our own write.
        let listed = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(1_000)),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                remote_dir_loaded_with_doc(scope, listed.size, listed.modified_at),
                window,
                cx,
            );
        });

        let after = session_snapshot(&workspace, &cx, id);
        assert_eq!(
            after, listed,
            "the session baseline must adopt the listing's exact values, \
             normalizing away the sub-second drift"
        );
        // The watcher flags a conflict when `listing != baseline`. After
        // adoption the session no longer diverges from the same listing, so the
        // next save would upload cleanly instead of popping RemoteConflict.
        let diverged = workspace.read_with(&cx, |_workspace, cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("edit session survives refresh")
                .remote_diverged(listed)
        });
        assert!(
            !diverged,
            "adopted baseline must not diverge from the listing (no false conflict)"
        );
    }

    #[gpui::test]
    fn refresh_preserves_baseline_when_remote_genuinely_changed(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let scope = connect_and_land_on_root(&workspace, &mut cx, &channels);
        let baseline = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(1_000)),
        };
        let id = seed_editing_session_on_tab1(&workspace, &mut cx, baseline);

        // The server lists the file at a DIFFERENT whole second — someone else
        // changed it. This must NOT be silently adopted.
        let changed = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(2_000)),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                remote_dir_loaded_with_doc(scope, changed.size, changed.modified_at),
                window,
                cx,
            );
        });

        let after = session_snapshot(&workspace, &cx, id);
        assert_eq!(
            after, baseline,
            "a genuine remote change must not be adopted as the new baseline"
        );
        // The watcher's divergence check (baseline != current listing) still
        // fires, so the RemoteConflict path remains reachable.
        assert_ne!(
            after, changed,
            "the preserved baseline still diverges from the changed listing"
        );
        let diverged = workspace.read_with(&cx, |_workspace, cx| {
            cx.resources()
                .edit_sessions
                .get(id)
                .expect("edit session survives refresh")
                .remote_diverged(changed)
        });
        assert!(
            diverged,
            "a genuine remote change must remain detectable as divergence"
        );
    }

    #[gpui::test]
    fn refresh_preserves_baseline_when_size_changed_at_same_second(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        let scope = connect_and_land_on_root(&workspace, &mut cx, &channels);
        let baseline = RemoteSnapshot {
            size: Some(20),
            modified_at: Some(Timestamp::from_secs_since_epoch(1_000)),
        };
        let id = seed_editing_session_on_tab1(&workspace, &mut cx, baseline);

        // Same whole second, DIFFERENT size → a real change, not mtime noise.
        let bigger = RemoteSnapshot {
            size: Some(21),
            modified_at: Some(Timestamp::from_secs_since_epoch(1_000)),
        };
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                remote_dir_loaded_with_doc(scope, bigger.size, bigger.modified_at),
                window,
                cx,
            );
        });

        let after = session_snapshot(&workspace, &cx, id);
        assert_eq!(
            after, baseline,
            "a size change at the same second must not be adopted"
        );
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

            workspace.navigate_pane_local(
                LocalPath::new(String::new()),
                HistoryOp::Back,
                window,
                cx,
            );
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
                workspace.active_tab().and_then(|tab| tab
                    .local
                    .path
                    .as_ref()
                    .map(LocalPath::as_str)),
                Some(parent.as_str()),
                "NavigateBack action restores previous local path"
            );
        });

        // Refresh must not push history.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Local;
            let back_len_before = workspace.active_tab().expect("tab").nav.local.back.len();
            workspace.refresh_focused_pane(window, cx);
            let back_len_after = workspace.active_tab().expect("tab").nav.local.back.len();
            assert_eq!(
                back_len_before, back_len_after,
                "refresh must not push history"
            );
        });

        // Closing the tab drops its nav state with it (no side table left).
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let tab_id = workspace.active_tab().expect("tab").id;
            assert!(workspace.state.tabs.find_tab(tab_id).is_some());
            workspace.close_tab_by_id(tab_id, window, cx);
            assert!(
                workspace.state.tabs.find_tab(tab_id).is_none(),
                "close_tab must remove the tab owning the nav state"
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
            assert!(workspace.go_to_path.open);
            workspace.go_to_path.input.set_value(child.as_str());
            workspace.submit_go_to_path(window, cx);
            assert!(!workspace.go_to_path.open);
            assert_eq!(
                workspace.active_tab().and_then(|tab| tab
                    .local
                    .path
                    .as_ref()
                    .map(LocalPath::as_str)),
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
                .go_to_path
                .input
                .set_value(format!("{}/does-not-exist", parent.as_str()));
            workspace.submit_go_to_path(window, cx);
            assert!(
                workspace.go_to_path.open,
                "modal stays open on missing path"
            );
            assert_eq!(
                workspace.go_to_path.error.as_ref().map(|s| s.as_ref()),
                Some("Path not found")
            );
            assert_eq!(
                workspace.active_tab().and_then(|tab| tab
                    .local
                    .path
                    .as_ref()
                    .map(LocalPath::as_str)),
                Some(child.as_str()),
                "failed go to path must not change current path"
            );
        });

        // Esc / CancelActiveModal closes the modal.
        cx.dispatch_action(CancelActiveModal);
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                !workspace.go_to_path.open,
                "CancelActiveModal closes go to path"
            );
        });

        // Action opens the modal again.
        cx.dispatch_action(GoToPath);
        workspace.read_with(&cx, |workspace, _| {
            assert!(workspace.go_to_path.open, "GoToPath action opens modal");
        });

        // Empty path is rejected.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.go_to_path.input.clear();
            workspace.submit_go_to_path(window, cx);
            assert!(workspace.go_to_path.open);
            assert_eq!(
                workspace.go_to_path.error.as_ref().map(|s| s.as_ref()),
                Some("Enter a path")
            );
        });

        // A file path is rejected (must be a directory).
        let file_path = fixture.join("note.txt");
        std::fs::write(&file_path, b"x").expect("write file");
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_go_to_path(window, cx);
            workspace
                .go_to_path
                .input
                .set_value(file_path.to_string_lossy().as_ref());
            workspace.submit_go_to_path(window, cx);
            assert!(
                workspace.go_to_path.open,
                "modal stays open when target is a file"
            );
            assert_eq!(
                workspace.go_to_path.error.as_ref().map(|s| s.as_ref()),
                Some("Not a directory")
            );
            assert_eq!(
                workspace.active_tab().and_then(|tab| tab
                    .local
                    .path
                    .as_ref()
                    .map(LocalPath::as_str)),
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

        set_local_path_and_wait(&workspace, &mut cx, parent);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Local;
            workspace.local.filter.query = "alpha".into();
            workspace.local.filter.explicit_focus = true;
            assert_eq!(workspace.entry_count(PaneSide::Local, cx), 1);
            workspace.navigate_pane_local(child, HistoryOp::Push, window, cx);
            assert!(
                workspace.local.filter.query.is_empty(),
                "navigate must clear filter query"
            );
            assert!(
                !workspace.local.filter.explicit_focus,
                "navigate must clear filter focus"
            );
        });

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn go_to_path_remote_requests_directory_with_push(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
                .go_to_path
                .input
                .set_value(format!("{TEST_REMOTE_ROOT}/docs"));
            workspace.submit_go_to_path(window, cx);
            assert!(!workspace.go_to_path.open);
            assert!(
                workspace.pane_can_navigate_back(PaneSide::Remote),
                "remote go to path must push history"
            );
        });

        assert!(matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path })
                if path.as_str() == format!("{TEST_REMOTE_ROOT}/docs")
        ));
    }

    #[gpui::test]
    fn remote_navigation_and_refresh_keep_the_previous_listing_visible(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
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
            assert!(workspace.connect_form_ui.form.is_some(), "form must open");
        });
        assert!(channels.command_rx.try_recv().is_err(), "no command yet");

        // Fill the form and submit.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
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
            assert!(
                workspace.connect_form_ui.form.is_none(),
                "form closes on submit"
            );
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
                workspace.connect_form_ui.form.is_none(),
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
    fn open_connect_form_resets_picker_and_save_as_flags(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.open_connect_form(window, cx);
            let form = ws.connect_form_ui.form.as_mut().expect("form opens");
            form.profile_picker_open = true;
            form.save_as_expanded = true;
            form.profile_picker_filter.set_value("partial");
            ws.close_connect_form(window, cx);
            ws.open_connect_form(window, cx);
            let form = ws.connect_form_ui.form.as_ref().expect("form reopens");
            assert!(
                !form.profile_picker_open,
                "reopening connect form must reset profile_picker_open"
            );
            assert!(
                !form.save_as_expanded,
                "reopening connect form must reset save_as_expanded"
            );
            assert!(
                form.profile_picker_filter.value().is_empty(),
                "reopening connect form must clear profile_picker_filter"
            );
        });
    }

    #[gpui::test]
    fn connect_save_as_collapsed_by_default(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.open_connect_form(window, cx);
            let form = ws.connect_form_ui.form.as_ref().expect("form opens");
            assert!(
                !form.save_as_expanded,
                "Save as profile must start collapsed"
            );
        });
    }

    #[gpui::test]
    fn connect_save_as_expand_and_save_still_works(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            assert!(!form.save_as_expanded);
            form.save_as_expanded = true;
            form.host.set_value("save-as.example.com");
            form.port.set_value("22");
            form.username.set_value("deploy");
            form.password.set_value("s3cret");
            form.profile_name.set_value("Save As Box");
            workspace.save_current_profile(cx);
        });

        workspace.read_with(&cx, |workspace, cx| {
            let profiles = cx.resources().profiles.profiles();
            assert_eq!(profiles.len(), 1, "profile saved to store");
            assert_eq!(profiles[0].name, "Save As Box");
            assert_eq!(profiles[0].host, "save-as.example.com");
            assert_eq!(profiles[0].username, "deploy");
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("form stays open after save");
            assert!(!form.save_as_expanded, "successful save collapses Save as");
            assert_eq!(
                form.source_profile_id,
                Some(profiles[0].id),
                "form links to the saved profile"
            );
        });
    }

    #[gpui::test]
    fn connect_picker_select_profile_prefills_form(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.host.set_value("example.com");
            form.username.set_value("alex");
            form.password.set_value("hunter2");
            form.profile_name.set_value("Work");
            workspace.save_current_profile(cx);
        });

        let saved_id = workspace.read_with(&cx, |_workspace, cx| {
            let profiles = cx.resources().profiles.profiles();
            assert_eq!(profiles.len(), 1, "one profile saved");
            assert_eq!(profiles[0].name, "Work");
            profiles[0].id
        });

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.host.set_value("other.example.com");
            form.username.set_value("other");
            form.password.clear();
            form.profile_picker_open = true;
            form.profile_picker_filter.set_value("work");
            workspace.select_connect_profile(saved_id, cx);
        });

        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("form open after select");
            assert_eq!(form.source_profile_id, Some(saved_id));
            assert_eq!(form.host.value(), "example.com");
            assert_eq!(form.username.value(), "alex");
            assert_eq!(
                form.password.value(),
                "hunter2",
                "secret restored from Keychain"
            );
            assert!(
                !form.profile_picker_open,
                "selecting a profile must close the picker"
            );
            assert!(
                form.profile_picker_filter.value().is_empty(),
                "selecting a profile must clear the picker filter"
            );
        });
    }

    #[gpui::test]
    fn connect_picker_filter_narrows_profiles(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let work = macsftp_core::ConnectionProfile::new(
                ProfileId(1),
                "Work Server",
                "work.example.com",
                "alex",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(1), "password"),
                },
            );
            let home = macsftp_core::ConnectionProfile::new(
                ProfileId(2),
                "Home NAS",
                "nas.local",
                "admin",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(2), "password"),
                },
            );
            match cx.resources_mut().profiles.save_profile(work) {
                Ok(_) => {}
                Err(error) => panic!("seed work profile: {error}"),
            }
            match cx.resources_mut().profiles.save_profile(home) {
                Ok(_) => {}
                Err(error) => panic!("seed home profile: {error}"),
            }

            workspace.open_connect_form(window, cx);
            {
                let form = workspace.connect_form_ui.form.as_mut().expect("form opens");
                form.profile_picker_open = true;
            }

            assert_eq!(
                workspace.filtered_connect_profiles(cx).len(),
                2,
                "empty filter matches every profile"
            );

            {
                let form = workspace.connect_form_ui.form.as_mut().expect("form opens");
                form.profile_picker_filter.set_value("work");
            }
            let filtered = workspace.filtered_connect_profiles(cx);
            assert_eq!(filtered.len(), 1, "filter must narrow to one profile");
            assert_eq!(filtered[0].id, ProfileId(1));
            assert_eq!(filtered[0].name, "Work Server");

            {
                let form = workspace.connect_form_ui.form.as_mut().expect("form opens");
                form.profile_picker_filter.set_value("nomatch");
            }
            assert!(
                workspace.filtered_connect_profiles(cx).is_empty(),
                "non-matching filter yields no profiles"
            );
        });
    }

    #[gpui::test]
    fn escape_closes_profile_picker_before_connect_form(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
            let form = workspace.connect_form_ui.form.as_mut().expect("form opens");
            form.profile_picker_open = true;
            form.profile_picker_filter.set_value("work");

            workspace.cancel_active_modal(window, cx);
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("first Esc must keep Connect open");
            assert!(
                !form.profile_picker_open,
                "first Esc must close the profile picker only"
            );
            assert!(
                form.profile_picker_filter.value().is_empty(),
                "closing the picker must clear its filter"
            );

            workspace.cancel_active_modal(window, cx);
            assert!(
                workspace.connect_form_ui.form.is_none(),
                "second Esc must close the Connect form"
            );
        });
    }

    #[gpui::test]
    fn connect_picker_enter_selects_first_filtered_profile(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            let work = macsftp_core::ConnectionProfile::new(
                ProfileId(1),
                "Work",
                "work.example.com",
                "alex",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(1), "password"),
                },
            );
            let home = macsftp_core::ConnectionProfile::new(
                ProfileId(2),
                "Home",
                "home.example.com",
                "root",
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(2), "password"),
                },
            );
            match cx.resources_mut().profiles.save_profile(work) {
                Ok(_) => {}
                Err(error) => panic!("seed work: {error}"),
            }
            match cx.resources_mut().profiles.save_profile(home) {
                Ok(_) => {}
                Err(error) => panic!("seed home: {error}"),
            }
            let settings = ConnectionSettings {
                host: "work.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "secret".into(),
                },
            };
            match cx.resources_mut().profiles.save_connection_settings(
                ProfileId(1),
                "Work".into(),
                &settings,
            ) {
                Ok(_) => {}
                Err(error) => panic!("seed work credential: {error}"),
            }

            workspace.open_connect_form(window, cx);
            {
                let form = workspace.connect_form_ui.form.as_mut().expect("form");
                form.profile_picker_open = true;
                form.profile_picker_filter.set_value("work");
            }
            // Simulate Enter while picker is open (handle_connect_form_key path).
            let first_id = workspace
                .filtered_connect_profiles(cx)
                .first()
                .map(|p| p.id)
                .expect("filter matches Work");
            assert_eq!(first_id, ProfileId(1));
            workspace.select_connect_profile(first_id, cx);

            let form = workspace.connect_form_ui.form.as_ref().expect("form");
            assert_eq!(form.source_profile_id, Some(ProfileId(1)));
            assert_eq!(form.host.value(), "work.example.com");
            assert!(!form.profile_picker_open);
        });

        // Selecting a profile must not enqueue ConnectTab.
        assert!(
            channels.command_rx.try_recv().is_err(),
            "profile select must not auto-connect"
        );
    }

    #[gpui::test]
    fn leave_settings_discards_unsaved_new_profile_draft(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.surface = WorkspaceSurface::Settings;
            workspace.set_settings_section(SettingsSection::Profiles, cx);
            workspace.start_new_profile(cx);
            {
                let editor = workspace
                    .settings
                    .profile_editor
                    .as_mut()
                    .expect("new profile draft");
                editor.host.set_value("draft.example.com");
                editor.username.set_value("draft-user");
            }
            assert!(
                workspace
                    .settings
                    .profile_editor
                    .as_ref()
                    .is_some_and(|e| e.is_new),
                "draft should be open"
            );

            workspace.leave_settings(window, cx);

            assert_eq!(workspace.surface, WorkspaceSurface::Files);
            assert!(
                workspace.settings.profile_editor.is_none(),
                "leave Settings must discard unsaved New draft"
            );
            assert!(
                cx.resources().profiles.profiles().is_empty(),
                "discarded draft must not write profiles.json"
            );
        });
    }

    #[gpui::test]
    fn connect_manual_entry_clears_profile_link_and_secrets(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_connect_form(window, cx);
        });
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.host.set_value("example.com");
            form.username.set_value("alex");
            form.password.set_value("hunter2");
            form.profile_name.set_value("Work");
            workspace.save_current_profile(cx);
        });

        let saved_id = workspace.read_with(&cx, |_workspace, cx| {
            cx.resources().profiles.profiles()[0].id
        });

        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.select_connect_profile(saved_id, cx);
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.profile_picker_open = true;
            form.profile_picker_filter.set_value("work");
            form.switch_to_manual_entry();
        });

        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("form stays open after manual entry");
            assert!(
                form.source_profile_id.is_none(),
                "manual entry must clear source_profile_id"
            );
            assert!(
                form.password.value().is_empty(),
                "manual entry must clear password"
            );
            assert!(
                form.profile_picker_filter.value().is_empty(),
                "manual entry must clear picker filter"
            );
            assert_eq!(
                form.host.value(),
                "example.com",
                "manual entry keeps host metadata"
            );
            assert_eq!(form.username.value(), "alex");
            assert!(
                !form.profile_picker_open,
                "manual entry must close the picker"
            );
        });
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
            let form = workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("form stays open");
            assert!(form.error.is_some(), "validation error must be shown");
        });
        assert!(channels.command_rx.try_recv().is_err());

        // Bad port is rejected too.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
            form.host.set_value("h");
            form.username.set_value("u");
            form.port.set_value("99999");
            workspace.submit_connect_form(window, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace
                    .connect_form_ui
                    .form
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
                workspace.connect_form_ui.form.is_none(),
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
            let form = workspace
                .connect_form_ui
                .form
                .as_mut()
                .expect("form is open");
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
        let stored = workspace.read_with(&cx, |_workspace, cx| {
            cx.resources()
                .profiles
                .load_connection_settings(saved_id)
                .expect("load secret")
        });
        let AuthCredential::Password { password } = &stored.auth else {
            panic!("expected password auth");
        };
        assert_eq!(password, "hunter2", "secret stored in Keychain");

        // "Use" prefills the form (including the restored secret) and
        // remembers the source profile id.
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.use_profile(saved_id, cx);
        });
        workspace.read_with(&cx, |workspace, _| {
            let form = workspace
                .connect_form_ui
                .form
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
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::HostKeyMismatch(macsftp_core::HostKeyMismatch {
                    scope: macsftp_core::RemoteEventScope::new(
                        macsftp_core::TabId(1),
                        macsftp_core::SessionId(1),
                        1,
                    ),
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
    fn stale_host_key_mismatch_does_not_fail_replacement_session(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();

        // Reconnect: bump the tab to epoch 2 / session 2.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabDisconnected(RemoteScoped::new(
                    RemoteEventScope::new(TabId(1), SessionId(1), 1),
                    TabDisconnected {
                        reason: DisconnectReason::UserRequested,
                    },
                )),
                window,
                cx,
            );
            workspace.connect_with(test_settings(), None, window, cx);
        });
        let _ = channels.command_rx.try_recv();
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::TabConnected(RemoteScoped::new(
                    RemoteEventScope::new(TabId(1), SessionId(2), 2),
                    TabConnected {
                        remote_root: RemotePath::new(TEST_REMOTE_ROOT),
                    },
                )),
                window,
                cx,
            );
        });

        // A stale mismatch from the OLD session (epoch 1) must be dropped by
        // the central stale-event guard and must NOT fail the replacement.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.handle_app_event(
                AppEvent::HostKeyMismatch(macsftp_core::HostKeyMismatch {
                    scope: RemoteEventScope::new(TabId(1), SessionId(1), 1),
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
            assert!(
                !matches!(tab.connection, ConnectionState::Failed { .. }),
                "stale mismatch must not fail the replacement session"
            );
            assert!(
                workspace.state.modals.active.is_empty(),
                "stale mismatch must not open a trust modal"
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
    fn full_command_channel_eventually_releases_closed_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        fill_command_channel(&channels);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.close_tab_by_id(TabId(1), window, cx);
        });
        channels
            .command_rx
            .try_recv()
            .expect("free one command slot for lifecycle retry");
        cx.run_until_parked();

        assert!(
            channels
                .command_rx
                .try_iter()
                .any(|command| matches!(command, AppCommand::CloseTab { tab_id: TabId(1) })),
            "the detached lifecycle retry must enqueue the real close command"
        );
    }

    #[gpui::test]
    fn window_release_notifies_runtime_for_all_remaining_tabs(cx: &mut TestAppContext) {
        let (workspace, mut cx, channels) = init_workspace(cx);
        cx.cx.update(|cx| {
            cx.on_window_closed(crate::session_coordinator::checkpoint_after_window_closed)
                .detach();
        });
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_new_tab(window, cx);
        });

        workspace.update_in(&mut cx, |_workspace, window, _cx| window.remove_window());
        cx.run_until_parked();

        let command = channels
            .command_rx
            .try_recv()
            .expect("window release command must be enqueued");
        assert_eq!(
            command,
            AppCommand::CloseTabs {
                tab_ids: vec![TabId(1), TabId(2)],
            }
        );
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
            let root_job = cx
                .transfers()
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = cx
                .transfers()
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
            let root_job = cx
                .transfers()
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
                    child_jobs: vec![child_job],
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
            let root_job = cx
                .transfers()
                .jobs
                .iter()
                .find(|job| job.id == root_job_id)
                .expect("root job exists");
            assert_eq!(root_job.state, TransferState::Completed);
            let plan = cx
                .transfers()
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
        (
            dir.clone(),
            LocalPath::new(dir.to_string_lossy().into_owned()),
        )
    }

    #[gpui::test]
    fn hidden_files_filtered_by_default(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("hidden-files");
        std::fs::write(fixture.join(".secret"), b"secret").expect("write hidden");
        std::fs::write(fixture.join("visible.txt"), b"ok").expect("write visible");

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
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

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            assert_eq!(
                workspace.entry_count(PaneSide::Local, cx),
                2,
                "both files visible before filter"
            );

            workspace.local.filter.query = "a".into();
            workspace
                .local
                .filter
                .input
                .set_value(workspace.local.filter.query.clone());
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
            assert!(!workspace.local.filter.is_active());
        });

        // cmd-f / FilterPane opens explicit focus without requiring a query.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_filter_pane(window, cx);
            assert!(workspace.local.filter.explicit_focus);
            assert!(workspace.local.filter.is_active());
            workspace.local.filter.query = "b".into();
            assert_eq!(workspace.entry_count(PaneSide::Local, cx), 1);
        });
        cx.dispatch_action(FilterPane);
        workspace.read_with(&cx, |workspace, _cx| {
            assert!(
                workspace.local.filter.explicit_focus,
                "FilterPane action sets explicit_focus"
            );
        });

        // Tab switch clears filters.
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.open_new_tab(window, cx);
            assert!(!workspace.local.filter.is_active());
            assert!(!workspace.remote.filter.is_active());
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
            let dir =
                std::env::temp_dir().join(format!("macsftp-sort-{}-{}", std::process::id(), seq));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("big.bin"), vec![0u8; 100]).unwrap();
            std::fs::write(dir.join("small.bin"), vec![0u8; 1]).unwrap();
            (
                dir.clone(),
                LocalPath::new(dir.to_string_lossy().into_owned()),
            )
        };
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            if let Some(tab) = workspace.active_tab_mut() {
                tab.sort.field = FileSortField::Size;
                tab.sort.direction = SortDirection::Ascending;
            }
        });
        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.read_with(&cx, |workspace, _| {
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

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, window, cx| {
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
                .modal_inputs
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

        set_local_path_and_wait(&workspace, &mut cx, base.clone());
        workspace.update_in(&mut cx, |workspace, window, cx| {
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
            if let Some(state) = &mut workspace.modal_inputs.delete_confirm {
                state.dont_ask_again = true;
            }
            workspace.confirm_delete(window, cx);
            assert!(
                !cx.resources().config.config().confirm_delete,
                "confirm_delete should persist false"
            );
        });
        cx.run_until_parked();
        assert!(!fixture.join("gone.txt").exists());

        // Create another file and delete without modal.
        std::fs::write(fixture.join("second.txt"), b"y").expect("second");
        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, window, cx| {
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
            assert!(workspace.modal_inputs.delete_confirm.is_none());
        });
        cx.run_until_parked();
        assert!(!fixture.join("second.txt").exists());
        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[gpui::test]
    fn local_new_folder_and_rename_via_inline_edit(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        let (fixture, base) = temp_local_fixture("inline-edit");

        set_local_path_and_wait(&workspace, &mut cx, base.clone());
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.focused_side = PaneSide::Local;
            workspace.begin_new_folder(window, cx);
            if let Some(edit) = &mut workspace.modal_inputs.inline_edit {
                edit.input.set_value("Created Folder");
            }
            workspace.submit_inline_edit(window, cx);
        });
        cx.run_until_parked();
        assert!(fixture.join("Created Folder").is_dir());

        // Reload so listing includes the new folder, then rename it.
        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, window, cx| {
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
            if let Some(edit) = &mut workspace.modal_inputs.inline_edit {
                edit.input.set_value("Renamed Folder");
            }
            workspace.submit_inline_edit(window, cx);
        });
        cx.run_until_parked();
        assert!(!fixture.join("Created Folder").exists());
        assert!(fixture.join("Renamed Folder").is_dir());
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

        set_local_path_and_wait(&workspace, &mut cx, base);
        workspace.update_in(&mut cx, |workspace, _window, _cx| {
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
                tab.selection
                    .selected_paths
                    .push(EntryPath::Remote(RemotePath::new(format!(
                        "{TEST_REMOTE_ROOT}/remote-only.txt"
                    ))));
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

        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.activate_tab(TabId(2), window, cx);
        });
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.activate_tab(TabId(3), window, cx);
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
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.activate_tab(TabId(1), window, cx);
        });
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.activate_tab(TabId(3), window, cx);
        });
        workspace.read_with(&cx, |ws, _| {
            assert_eq!(ws.tab_mru, vec![TabId(3), TabId(1), TabId(2)]);
            assert_eq!(ws.state.tabs.active_tab_id, Some(TabId(3)));
        });

        // From tab 1 in creation order: ActivateNextTab → tab 2 (not MRU next).
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.activate_tab(TabId(1), window, cx);
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
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.activate_tab(TabId(2), window, cx);
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
    fn close_tab_tears_down_its_edit_sessions_and_temp_dir(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab); // TabId(2)

        // A real temp dir on disk so the cleanup has something to remove,
        // mirroring production's `<edits>/<run>/<id>/<file>` layout.
        let session_dir =
            std::env::temp_dir().join(format!("macsftp-close-tab-{}", std::process::id()));
        std::fs::create_dir_all(&session_dir).expect("create session temp dir");
        let temp_file = session_dir.join("doc.txt");
        std::fs::write(&temp_file, b"edited").expect("write temp file");
        let temp_path = LocalPath::new(temp_file.to_string_lossy().to_string());

        let id = workspace.update_in(&mut cx, |_ws, _window, cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            cx.resources_mut().edit_sessions.register(EditSession {
                id,
                remote_path: RemotePath::new("/srv/doc.txt"),
                tab_id: TabId(2),
                session_epoch: 1,
                profile_id: ProfileId(1),
                local_temp_path: temp_path.clone(),
                phase: EditPhase::Editing,
                remote_snapshot: RemoteSnapshot {
                    size: Some(6),
                    modified_at: None,
                },
                local_mtime: None,
                active_transfer: None,
                pending_check_id: None,
                checking_local_mtime: None,
                missing_ticks: 0,
            });
            id
        });

        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.close_tab_by_id(TabId(2), window, cx);
        });

        workspace.read_with(&cx, |_ws, cx| {
            assert!(
                cx.resources().edit_sessions.get(id).is_none(),
                "closing a tab must remove its edit sessions so re-edit is not blocked"
            );
        });
        assert!(
            !session_dir.exists(),
            "closing a tab must delete its edit session's temp directory"
        );
        std::fs::remove_dir_all(&session_dir).ok();
    }

    #[gpui::test]
    fn window_closed_callback_reaps_orphaned_edit_session(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        let app = cx.cx.clone();
        cx.update(|_window, cx| {
            cx.on_window_closed(crate::workspace::cleanup_orphaned_edit_sessions)
                .detach();
        });
        let session_dir =
            std::env::temp_dir().join(format!("macsftp-close-window-edit-{}", std::process::id()));
        std::fs::create_dir_all(&session_dir).expect("create session temp dir");
        let temp_file = session_dir.join("doc.txt");
        std::fs::write(&temp_file, b"edited").expect("write temp file");
        let temp_path = LocalPath::new(temp_file.to_string_lossy().to_string());
        let id = workspace.update_in(&mut cx, |_workspace, _window, cx| {
            let id = cx.resources_mut().edit_sessions.next_id();
            cx.resources_mut().edit_sessions.register(EditSession {
                id,
                remote_path: RemotePath::new("/srv/doc.txt"),
                tab_id: TabId(1),
                session_epoch: 1,
                profile_id: ProfileId(1),
                local_temp_path: temp_path,
                phase: EditPhase::Editing,
                remote_snapshot: RemoteSnapshot {
                    size: Some(6),
                    modified_at: None,
                },
                local_mtime: None,
                active_transfer: None,
                pending_check_id: None,
                checking_local_mtime: None,
                missing_ticks: 0,
            });
            id
        });

        workspace.update_in(&mut cx, |_workspace, window, _cx| window.remove_window());

        assert!(
            app.read(|cx| cx.resources().edit_sessions.get(id).is_none()),
            "the real window-close callback must remove orphaned edit sessions"
        );
        assert!(!session_dir.exists());
    }

    #[gpui::test]
    fn close_active_tab_touches_mru_with_new_active(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab); // mru: 2, 1
        cx.dispatch_action(NewTab); // mru: 3, 2, 1; active 3
        workspace.update_in(&mut cx, |ws, window, cx| {
            assert_eq!(ws.state.tabs.active_tab_id, Some(TabId(3)));
            assert_eq!(ws.tab_mru[0], TabId(3));
            ws.close_tab_by_id(TabId(3), window, cx);
            let active = ws.state.tabs.active_tab_id.expect("a tab remains");
            assert_ne!(active, TabId(3));
            assert_eq!(
                ws.tab_mru.first().copied(),
                Some(active),
                "after closing the active tab, MRU front must match the new active tab"
            );
        });
    }

    #[gpui::test]
    fn tab_switcher_next_and_enter_activates_mru(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab); // mru: 2, 1
        cx.dispatch_action(NewTab); // mru: 3, 2, 1 active 3
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.tab_switcher_next(window, cx);
            assert!(ws.tab_switcher.open);
            assert_eq!(ws.tab_switcher.index, 1, "first open starts at MRU[1]");
            assert_eq!(ws.tab_mru[1], TabId(2));
            ws.confirm_tab_switcher(window, cx);
            assert!(!ws.tab_switcher.open);
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
            assert_eq!(ws.tab_switcher.index, 1);
            ws.cancel_active_modal(window, cx);
            assert!(!ws.tab_switcher.open);
            assert_eq!(
                ws.state.tabs.active_tab_id,
                Some(TabId(3)),
                "Esc must not change the active tab"
            );
        });
    }

    #[gpui::test]
    fn overflowing_tab_switcher_renders_custom_scrollbar(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        for _ in 0..20 {
            cx.dispatch_action(NewTab);
        }
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.tab_switcher_next(window, cx);
        });
        cx.run_until_parked();
        cx.run_until_parked();

        let track = cx
            .debug_bounds("tab-switcher-scrollbar")
            .expect("an overflowing tab switcher must render the custom scrollbar track");
        let thumb = cx
            .debug_bounds("tab-switcher-scrollbar-thumb")
            .expect("an overflowing tab switcher must render the custom scrollbar thumb");
        assert!(track.contains(&thumb.center()));
    }

    #[gpui::test]
    fn cancel_modal_closes_palette_before_switcher(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        cx.dispatch_action(NewTab);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.tab_switcher_next(window, cx);
            assert!(ws.tab_switcher.open);
            ws.open_command_palette(window, cx);
            assert!(ws.palette.open);
            ws.cancel_active_modal(window, cx);
            assert!(!ws.palette.open, "palette closes first");
            assert!(ws.tab_switcher.open, "switcher remains until next Esc");
            ws.cancel_active_modal(window, cx);
            assert!(!ws.tab_switcher.open);
        });
    }

    #[gpui::test]
    fn command_palette_filters_and_runs_new_tab(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            assert_eq!(ws.state.tabs.tabs.len(), 1);
            ws.open_command_palette(window, cx);
            assert!(ws.palette.open);
            ws.palette.input.set_value("new tab");
            // rebuild selection to first hit
            ws.palette.selected = 0;
            ws.execute_palette_selected(window, cx);
            assert!(!ws.palette.open);
            assert_eq!(ws.state.tabs.tabs.len(), 2);
        });
    }

    #[gpui::test]
    fn command_palette_manage_profiles_opens_profiles_section(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.open_command_palette(window, cx);
            ws.palette.input.set_value("manage profiles");
            ws.palette.selected = 0;
            ws.execute_palette_selected(window, cx);
            assert!(!ws.palette.open);
            assert_eq!(ws.surface, WorkspaceSurface::Settings);
            assert_eq!(ws.settings.section, SettingsSection::Profiles);
        });
    }

    #[gpui::test]
    fn command_palette_esc_closes_before_other_modals(cx: &mut TestAppContext) {
        let (workspace, mut cx, _) = init_workspace(cx);
        workspace.update_in(&mut cx, |ws, window, cx| {
            ws.open_go_to_path(window, cx);
            assert!(ws.go_to_path.open);
            ws.open_command_palette(window, cx);
            assert!(ws.palette.open);
            ws.cancel_active_modal(window, cx);
            assert!(!ws.palette.open, "Esc closes palette first");
            assert!(ws.go_to_path.open, "underlying go-to-path stays open");
            ws.cancel_active_modal(window, cx);
            assert!(!ws.go_to_path.open);
        });
    }

    #[gpui::test]
    fn restore_session_rebuilds_tabs_without_connect(cx: &mut TestAppContext) {
        let app_paths = temp_app_paths();
        let mut store = SessionStore::open_or_empty(app_paths.session_file.clone());
        store.replace(SessionFile {
            version: SessionFile::CURRENT_VERSION,
            active_window_index: 0,
            windows: vec![SessionWindowSnapshot {
                id: WindowSessionId(9),
                active_tab_index: 1,
                tabs: vec![
                    SessionTabSnapshot {
                        title: "alpha.example".into(),
                        profile_id: Some(3),
                        host: "alpha.example".into(),
                        port: 22,
                        username: "alice".into(),
                        local_path: None,
                        remote_path: Some("/home/alice/proj".into()),
                    },
                    SessionTabSnapshot {
                        title: "beta.example".into(),
                        profile_id: None,
                        host: "beta.example".into(),
                        port: 2222,
                        username: "bob".into(),
                        local_path: None,
                        remote_path: Some("/var/www".into()),
                    },
                ],
            }],
        });
        store
            .save()
            .expect("session.json must save for restore test");

        let (workspace, cx, channels) = init_workspace_with_paths(cx, app_paths, true);

        workspace.read_with(&cx, |workspace, _| {
            assert_eq!(workspace.state.tabs.tabs.len(), 2);
            assert_eq!(workspace.state.tabs.tabs[0].title, "alpha.example");
            assert_eq!(workspace.state.tabs.tabs[1].title, "beta.example");
            assert_eq!(workspace.state.tabs.tabs[0].profile_id, Some(ProfileId(3)));
            assert_eq!(
                workspace.state.tabs.tabs[0].remote.path,
                Some(RemotePath::new("/home/alice/proj"))
            );
            assert_eq!(
                workspace.state.tabs.tabs[1].remote.path,
                Some(RemotePath::new("/var/www"))
            );
            for tab in &workspace.state.tabs.tabs {
                assert_eq!(
                    tab.connection,
                    ConnectionState::Empty,
                    "restored tabs must not auto-connect"
                );
            }
            assert_eq!(
                workspace.state.tabs.active_tab_id,
                Some(TabId(2)),
                "active_tab_index 1 selects the second restored tab"
            );
        });

        assert!(
            channels.command_rx.try_recv().is_err(),
            "restore must not enqueue ConnectTab or any other command"
        );
    }

    #[gpui::test]
    fn build_session_snapshot_round_trips_active_tab_and_paths(cx: &mut TestAppContext) {
        let (workspace, mut cx, _channels) = init_workspace(cx);

        workspace.update_in(&mut cx, |workspace, window, cx| {
            // Start with the default tab; open a second and configure both.
            {
                let tab = workspace.active_tab_mut().expect("default tab");
                tab.title = "first-host".into();
                tab.profile_id = Some(ProfileId(9));
                tab.local.path = Some(LocalPath::new("/tmp/local-a"));
                tab.remote.path = Some(RemotePath::new("/home/a"));
            }
            workspace.open_new_tab(window, cx);
            {
                let tab = workspace.active_tab_mut().expect("second tab");
                tab.title = "second-host".into();
                tab.local.path = Some(LocalPath::new("/tmp/local-b"));
                tab.remote.path = Some(RemotePath::new("/home/b"));
            }
            // Activate the first tab so active index is 0.
            let first_id = workspace.state.tabs.tabs[0].id;
            workspace.activate_tab(first_id, window, cx);

            let snapshot = workspace.build_session_snapshot();
            assert_eq!(snapshot.active_tab_index, 0);
            assert_eq!(snapshot.tabs.len(), 2);
            assert_eq!(snapshot.tabs[0].title, "first-host");
            assert_eq!(snapshot.tabs[0].profile_id, Some(9));
            assert_eq!(snapshot.tabs[0].local_path.as_deref(), Some("/tmp/local-a"));
            assert_eq!(snapshot.tabs[0].remote_path.as_deref(), Some("/home/a"));
            assert_eq!(snapshot.tabs[1].title, "second-host");
            assert_eq!(snapshot.tabs[1].local_path.as_deref(), Some("/tmp/local-b"));
            assert_eq!(snapshot.tabs[1].remote_path.as_deref(), Some("/home/b"));
        });
    }

    #[gpui::test]
    fn session_snapshot_never_persists_credentials(cx: &mut TestAppContext) {
        let app_paths = temp_app_paths();
        let session_path = app_paths.session_file.clone();
        let (workspace, mut cx, _channels) = init_workspace_with_paths(cx, app_paths, false);

        workspace.update_in(&mut cx, |workspace, _window, _cx| {
            let tab_id = workspace.active_tab().expect("default tab").id;
            let tab = workspace
                .state
                .tabs
                .find_tab_mut(tab_id)
                .expect("default tab must exist");
            tab.connection_settings = Some(Box::new(ConnectionSettings {
                host: "saved-host".into(),
                port: 2222,
                username: "deploy".into(),
                auth: AuthCredential::Password {
                    password: "not-persisted".into(),
                },
            }));
            SessionFile {
                version: SessionFile::CURRENT_VERSION,
                active_window_index: 0,
                windows: vec![workspace.build_session_snapshot()],
            }
            .save(&session_path)
            .expect("session snapshot should save");
        });

        let raw = std::fs::read_to_string(session_path.as_str()).expect("read session.json");
        assert!(
            !raw.contains("not-persisted"),
            "password must never appear in session.json: {raw}"
        );
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
            cx.observe_transfer_rate(transfer_id, 0, t0);
            cx.observe_transfer_rate(transfer_id, 1_000_000, t0 + Duration::from_secs(1));
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
            assert!(
                detail.contains("ETA"),
                "detail should include ETA: {detail}"
            );
        });
    }

    #[test]
    fn user_status_strings_avoid_internal_jargon() {
        let banlist = [
            "runtime",
            "actor",
            "channel",
            "session epoch",
            "appcommand",
            "crate",
        ];
        for message in [STATUS_BUSY_TRY_AGAIN, STATUS_CONNECTION_SERVICE_UNAVAILABLE] {
            let lower = message.to_lowercase();
            for word in banlist {
                assert!(
                    !lower.contains(word),
                    "status string {message:?} must not contain internal jargon {word:?}"
                );
            }
        }
        assert_eq!(STATUS_BUSY_TRY_AGAIN, "Busy — try again in a moment.");
        assert_eq!(
            STATUS_CONNECTION_SERVICE_UNAVAILABLE,
            "Connection service is unavailable."
        );
    }
}
