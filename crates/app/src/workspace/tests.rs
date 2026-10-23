// Tests use unwrap/expect for fixtures; production keeps unwrap_used deny.
#![allow(clippy::unwrap_used)]
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, TestAppContext, VisualTestContext, Window, div, point, px, size,
};
use macsftp_core::{
    AppCommand, AppEvent, AuthCredential, AuthMethod, AuthMethodKind, ConflictDecision,
    ConflictPolicy, ConflictRequestId, ConnectionKey, ConnectionPoolIdentity, ConnectionProfile,
    ConnectionSettings, ConnectionState, DisconnectReason, EditPhase, EntryPath, ErrorCode,
    FileKind, FileSortField, HostKeyPrompt, KeyboardInteractivePrompt,
    KeyboardInteractivePromptField, KeyboardInteractiveRequestId, LocalPath, MetadataPolicy,
    ProfileId, RemoteDirSnapshot, RemoteEntry, RemoteEntryResolution, RemoteEventScope,
    RemoteOperationFailure, RemotePath, RemoteScoped, RestoredTabTarget, RuntimeBridgeConfig,
    SessionId, SortDirection, TabConnected, TabDisconnected, TabId, TabState, Timestamp,
    TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferId, TransferJob,
    TransferPlanId, TransferPlanProgress, TransferPlanSnapshot, TransferPlanState, TransferState,
    TrustRequestId, UserFacingError, WindowSessionId,
};
use macsftp_core::{EditSession, EditSessionId, RemoteSnapshot};
use macsftp_platform::AppPaths;
use macsftp_sftp::{BridgeChannels, RuntimeClient};
use macsftp_storage::{
    AppearancePreference, ConfigError, PrivateKeyPassphraseUpdate, ProfileAuthUpdate,
    ProfileSaveRequest, SessionFile, SessionStore, SessionTabSnapshot, SessionWindowSnapshot,
};
use macsftp_ui::{ActiveTheme as _, Appearance, Theme};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{
    PaneSide, STATUS_BUSY_TRY_AGAIN, STATUS_CONNECTION_SERVICE_UNAVAILABLE, Workspace,
    WorkspaceSurface,
};
use crate::app_actions::{
    self, ActivateNextTab, ActivatePrevTab, CancelActiveModal, CloseTab, CopyPath, FilterPane,
    GoToPath, NavigateBack, NewTab, OpenProfiles, OpenSettings, PageDown, SelectAllEntries,
    SelectNextEntry, SelectNextEntryExtend, SelectPrevEntry, ShowAbout, ShowTransferDrawer,
    SortByNameAscending, SortBySizeAscending, SortBySizeDescending, ToggleHiddenFiles,
};
use crate::resources::{ActiveResources, ActiveTransfers};
use crate::session_coordinator::SessionCoordinator;
use crate::workspace::ConflictChoice;
use crate::workspace::connect_form::ConnectForm;
use crate::workspace::drawer_height::TransferDrawerResize;
use crate::workspace::helpers::format_recent_label;
use crate::workspace::overlay::{OVERLAY_FADE, OverlayFade};
use crate::workspace::profiles::{PassphrasePolicy, ProfileEditorState, ProfileRouteKind};
use crate::workspace::profiles::{SettingsSection, profile_matches_filter};
use crate::workspace::render::DragPayload;
use crate::workspace::transfer_render::{PROGRESS_SMOOTHING, painted_progress};
use macsftp_core::HistoryOp;
use macsftp_ui::InputState;

const TEST_REMOTE_ROOT: &str = "/home/tester";

fn save_profile_fixture(cx: &mut App, profile: ConnectionProfile) -> ConnectionProfile {
    let auth = match &profile.auth {
        AuthMethod::Password { .. } => ProfileAuthUpdate::Password {
            password: Some("test-password".into()),
        },
        AuthMethod::PrivateKey { key_path, .. } => ProfileAuthUpdate::PrivateKey {
            key_path: key_path.clone(),
            passphrase: PrivateKeyPassphraseUpdate::NoPassphrase,
        },
        AuthMethod::KeyboardInteractive => ProfileAuthUpdate::KeyboardInteractive,
        AuthMethod::SshAgent { socket_path } => ProfileAuthUpdate::SshAgent {
            socket_path: socket_path.clone(),
        },
    };
    cx.resources_mut()
        .profiles
        .save_request(ProfileSaveRequest {
            profile_id: profile.id,
            name: profile.name,
            host: profile.host,
            port: profile.port,
            username: profile.username,
            auth,
            route: profile.route,
            default_remote_path: profile.default_remote_path,
        })
        .expect("save profile fixture")
        .profile
}

fn save_settings_fixture(
    cx: &mut App,
    profile_id: ProfileId,
    name: &str,
    settings: &ConnectionSettings,
) -> ConnectionProfile {
    let auth = match &settings.auth {
        AuthCredential::Password { password } => ProfileAuthUpdate::Password {
            password: Some(password.clone()),
        },
        AuthCredential::PrivateKey {
            key_path,
            passphrase,
        } => ProfileAuthUpdate::PrivateKey {
            key_path: LocalPath::new(key_path.clone()),
            passphrase: passphrase
                .as_ref()
                .map(|passphrase| PrivateKeyPassphraseUpdate::Remember(passphrase.clone()))
                .unwrap_or(PrivateKeyPassphraseUpdate::NoPassphrase),
        },
        AuthCredential::KeyboardInteractive => ProfileAuthUpdate::KeyboardInteractive,
        AuthCredential::SshAgent { socket_path } => ProfileAuthUpdate::SshAgent {
            socket_path: socket_path.as_ref().map(LocalPath::new),
        },
    };
    let route = match &settings.route {
        macsftp_core::ResolvedConnectionRoute::Direct => macsftp_core::ConnectionRoute::Direct,
        macsftp_core::ResolvedConnectionRoute::ProxyCommand { command } => {
            macsftp_core::ConnectionRoute::ProxyCommand {
                command: command.clone(),
            }
        }
        macsftp_core::ResolvedConnectionRoute::JumpHost { .. } => {
            panic!("test fixtures must save jump routes through ProfileSaveRequest")
        }
    };
    cx.resources_mut()
        .profiles
        .save_request(ProfileSaveRequest {
            profile_id,
            name: name.into(),
            host: settings.host.clone(),
            port: settings.port,
            username: settings.username.clone(),
            auth,
            route,
            default_remote_path: None,
        })
        .expect("save settings fixture")
        .profile
}

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
    /// Paths the workspace asked macOS to open, in call order.
    static OPENED_SYSTEM_PATHS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Paths the workspace asked Finder to reveal, in call order.
    static REVEALED_PATHS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn record_edit_reopen(temp: &LocalPath, _editor: Option<&str>) -> std::io::Result<()> {
    REOPENED_EDIT_PATHS.with(|paths| paths.borrow_mut().push(temp.clone()));
    Ok(())
}

fn record_system_open(_cx: &App, path: &std::path::Path) {
    OPENED_SYSTEM_PATHS.with(|paths| paths.borrow_mut().push(path.display().to_string()));
}

fn record_system_reveal(_cx: &App, path: &std::path::Path) {
    REVEALED_PATHS.with(|paths| paths.borrow_mut().push(path.display().to_string()));
}

/// Record every system open/reveal instead of reaching LaunchServices, which the
/// test platform implements as `unimplemented!()`.
fn install_system_path_seams() {
    crate::workspace::set_system_path_opener(record_system_open);
    crate::workspace::set_system_path_revealer(record_system_reveal);
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
        Theme::dark().install(cx);
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
        let workspace =
            cx.new(|cx| Workspace::new(client, window_session_id, restore_snapshot, window, cx));
        gpui_kit::base::Root::new(workspace, window, cx)
    });
    let workspace = window
        .root(cx)
        .expect("component root view should be available")
        .read_with(cx, |root, _| root.view().clone().downcast::<Workspace>())
        .expect("component root must retain Workspace content");
    let visual_cx = VisualTestContext::from_window(window.into(), cx);
    (workspace, visual_cx, channels)
}

/// A unique temp `AppPaths` per call so parallel tests never share a
/// `profiles.json` (current architecture §14: tests must not clobber each other's files).
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
        route: macsftp_core::ResolvedConnectionRoute::Direct,
    }
}

/// The connection identity a manual (`connect_with(test_settings(), None)`)
/// tab captures: the same key production derives in `connect_with` from
/// these settings and an ephemeral session id. Tests that need a different
/// physical connection build variants of it.
fn test_connection_key(session_id: SessionId) -> ConnectionKey {
    ConnectionKey::new(
        &test_settings(),
        ConnectionPoolIdentity::Ephemeral(session_id),
    )
}

#[gpui_kit::test]
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
fn connect_and_prompt(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> HostKeyPrompt {
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn mouse_click_supports_cmd_toggle_and_shift_range(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("mouse-multiselect");
    for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
        std::fs::write(fixture.join(name), b"x").expect("write file");
    }

    set_local_path_and_wait(&workspace, &mut cx, base);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        assert_eq!(workspace.entry_count(PaneSide::Local, cx), 5);

        let make_click = |modifiers: gpui_kit::Modifiers| {
            gpui_kit::ClickEvent::Mouse(gpui_kit::MouseClickEvent {
                down: gpui_kit::MouseDownEvent {
                    button: gpui_kit::MouseButton::Left,
                    position: Default::default(),
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                },
                up: gpui_kit::MouseUpEvent {
                    button: gpui_kit::MouseButton::Left,
                    position: Default::default(),
                    modifiers,
                    click_count: 1,
                },
            })
        };

        // Plain click on index 1 selects only index 1.
        let plain_click = make_click(Default::default());
        workspace.on_row_clicked(PaneSide::Local, 1, &plain_click, window, cx);
        assert_eq!(
            workspace
                .active_tab()
                .expect("tab")
                .selection
                .selected_paths
                .len(),
            1
        );

        // ⌘-click on index 3 adds index 3 to the selection (total 2).
        let cmd_click = make_click(gpui_kit::Modifiers {
            platform: true,
            ..Default::default()
        });
        workspace.on_row_clicked(PaneSide::Local, 3, &cmd_click, window, cx);
        assert_eq!(
            workspace
                .active_tab()
                .expect("tab")
                .selection
                .selected_paths
                .len(),
            2,
            "cmd-click adds a row to selection"
        );

        // ⌘-click on index 1 again deselects it (leaving only index 3).
        workspace.on_row_clicked(PaneSide::Local, 1, &cmd_click, window, cx);
        let selected = &workspace
            .active_tab()
            .expect("tab")
            .selection
            .selected_paths;
        assert_eq!(
            selected.len(),
            1,
            "cmd-click on selected row toggles it off"
        );
        assert_eq!(
            selected[0],
            workspace
                .entry_path_at(PaneSide::Local, 3, cx)
                .expect("path")
        );

        // Plain click on index 0 resets selection to only index 0.
        workspace.on_row_clicked(PaneSide::Local, 0, &plain_click, window, cx);
        assert_eq!(
            workspace
                .active_tab()
                .expect("tab")
                .selection
                .selected_paths
                .len(),
            1
        );

        // Shift-click on index 3 extends range from 0 to 3 (4 items).
        let shift_click = make_click(gpui_kit::Modifiers {
            shift: true,
            ..Default::default()
        });
        workspace.on_row_clicked(PaneSide::Local, 3, &shift_click, window, cx);
        assert_eq!(
            workspace
                .active_tab()
                .expect("tab")
                .selection
                .selected_paths
                .len(),
            4,
            "shift-click extends range [0, 3]"
        );
    });

    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn shift_selection_replaces_filtered_out_anchor(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        let tab = workspace
            .active_tab_mut()
            .expect("workspace has an active tab");
        tab.local.entries = ["a.txt", "b.txt", "c.txt", "d.txt"]
            .into_iter()
            .map(|name| macsftp_core::LocalEntry {
                name: name.into(),
                path: LocalPath::new(format!("/selection/{name}")),
                kind: FileKind::File,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            })
            .collect();
        workspace.select_index(PaneSide::Local, 0, cx);
        workspace.local.filter.query = "b".into();
        workspace.extend_selection_to(PaneSide::Local, 0, cx);
        workspace.local.filter.query.clear();
        workspace.move_selection_extend(PaneSide::Local, 1, cx);
        let selected = &workspace
            .active_tab()
            .expect("active tab survives selection")
            .selection
            .selected_paths;
        assert_eq!(
            selected,
            &vec![
                EntryPath::Local(LocalPath::new("/selection/b.txt")),
                EntryPath::Local(LocalPath::new("/selection/c.txt")),
            ],
            "extending after filtering must use the replacement anchor"
        );
    });
}

#[gpui_kit::test]
fn shift_selection_preserves_visible_range_when_anchor_is_filtered(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        let tab = workspace
            .active_tab_mut()
            .expect("workspace has an active tab");
        tab.local.entries = ["a.txt", "b-match.txt", "c-match.txt"]
            .into_iter()
            .map(|name| macsftp_core::LocalEntry {
                name: name.into(),
                path: LocalPath::new(format!("/selection/{name}")),
                kind: FileKind::File,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            })
            .collect();
        workspace.select_index(PaneSide::Local, 0, cx);
        workspace.extend_selection_to(PaneSide::Local, 2, cx);
        workspace.local.filter.query = "match".into();
        workspace.move_selection_extend(PaneSide::Local, 1, cx);
        assert_eq!(
            workspace
                .active_tab()
                .expect("active tab survives filtering")
                .selection
                .selected_paths,
            vec![
                EntryPath::Local(LocalPath::new("/selection/b-match.txt")),
                EntryPath::Local(LocalPath::new("/selection/c-match.txt")),
            ],
            "a hidden anchor must not discard the remaining visible selection"
        );
    });
}

#[gpui_kit::test]
fn large_directory_selection_smoke(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, _window, cx| {
            let tab = workspace.active_tab_mut().expect("workspace has an active tab");
            tab.local.entries = (0..10_000)
                .map(|index| macsftp_core::LocalEntry {
                    name: format!("entry-{index:05}.txt"),
                    path: LocalPath::new(format!("/selection/entry-{index:05}.txt")),
                    kind: FileKind::File,
                    size: None,
                    permissions: None,
                    modified_at: None,
                    link_target: None,
                })
                .collect();
            tab.remote.entries = tab.local.entries.iter().map(|entry| RemoteEntry {
                name: entry.name.clone(),
                path: RemotePath::new(entry.path.as_str()),
                kind: entry.kind,
                size: None,
                permissions: None,
                modified_at: None,
                link_target: None,
            }).collect();
            for side in [PaneSide::Local, PaneSide::Remote] {
                workspace.pane_filter_mut(side).query = "ENTRY".into();
                let started = std::time::Instant::now();
                workspace.select_all_visible(side, cx);
                assert_eq!(workspace.active_tab().expect("active tab").selection.selected_paths.len(), 10_000);
                let select_all_elapsed = started.elapsed();
                workspace.select_index(side, 0, cx);
                let started = std::time::Instant::now();
                workspace.extend_selection_to(side, 9_999, cx);
                assert_eq!(workspace.active_tab().expect("active tab").selection.selected_paths.len(), 10_000);
                let range_elapsed = started.elapsed();
                let started = std::time::Instant::now();
                workspace.move_selection_extend(side, -1, cx);
                assert_eq!(workspace.active_tab().expect("active tab").selection.selected_paths.len(), 9_999);
                eprintln!("10k {side:?}: select-all={select_all_elapsed:?}, range={range_elapsed:?}, shrink={:?}", started.elapsed());
                let started = std::time::Instant::now();
                workspace.extend_type_select(side, "entry-09999", std::time::Instant::now(), cx);
                let type_select_elapsed = started.elapsed();
                assert_eq!(
                    workspace.selected_index(side, cx),
                    Some(9_999),
                    "a typed prefix selects its entry in a 10k listing"
                );
                eprintln!("10k {side:?}: type-select={type_select_elapsed:?}");
                workspace.pane_filter_mut(side).query = "absent".into();
                workspace.select_all_visible(side, cx);
                assert!(workspace.active_tab().expect("active tab").selection.selected_paths.is_empty());
            }
        });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
fn connection_entry_preserves_geometry_and_keyboard_activation(cx: &mut TestAppContext) {
    for key in ["enter", "space"] {
        let (workspace, mut cx, _channels) = init_workspace(cx);
        cx.simulate_resize(size(px(720.0), px(480.0)));
        cx.run_until_parked();
        assert_eq!(
            cx.debug_bounds("connect-remote")
                .expect("connection entry is visible")
                .size,
            size(px(160.0), px(34.0))
        );
        let entry = cx
            .debug_bounds("connect-remote")
            .expect("connection entry is visible");
        cx.simulate_event(gpui_kit::MouseDownEvent {
            position: entry.center(),
            button: gpui_kit::MouseButton::Left,
            click_count: 1,
            ..Default::default()
        });
        cx.simulate_event(gpui_kit::MouseUpEvent {
            position: point(entry.left() - px(10.0), entry.center().y),
            button: gpui_kit::MouseButton::Left,
            click_count: 1,
            ..Default::default()
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let keystroke = gpui_kit::Keystroke::parse(key).expect("button activation key is valid");
        let mut keystroke = keystroke;
        if key == "space" {
            keystroke.key_char = Some(" ".into());
        }
        cx.simulate_event(gpui_kit::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui_kit::KeyUpEvent { keystroke });
        workspace.read_with(&cx, |workspace, _| {
            assert!(
                workspace.pane_filter(PaneSide::Remote).query.is_empty(),
                "button activation must not start a file filter"
            );
            assert!(
                workspace.connect_form_ui.form.is_some(),
                "keyboard activation opens the existing connection form"
            );
        });
    }
}

#[gpui_kit::test]
fn compact_workspace_centers_empty_content(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    workspace.update(&mut cx, |workspace, cx| {
        let tab = workspace
            .active_tab_mut()
            .expect("fixture has an active tab");
        tab.local.entries.clear();
        tab.local.error = None;
        cx.notify();
    });
    for appearance in [AppearancePreference::Light, AppearancePreference::Dark] {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            workspace.set_appearance(appearance, window, cx);
        });
        for dimensions in [size(px(720.0), px(480.0)), size(px(1200.0), px(800.0))] {
            cx.simulate_resize(dimensions);
            cx.run_until_parked();
            for (content, viewport) in [
                ("empty-state-message", "local-list-container-viewport"),
                ("remote-empty-card", "remote-list-container-viewport"),
            ] {
                let content = cx.debug_bounds(content).expect("empty content is visible");
                let viewport = cx.debug_bounds(viewport).expect("pane viewport is visible");
                assert!(
                    (content.center().y - viewport.center().y).abs() < px(2.0),
                    "empty content is vertically centered: {content:?} in {viewport:?}"
                );
                assert!(content.left() >= viewport.left() && content.right() <= viewport.right());
            }
        }
    }
}

/// The path bar's three rules must keep the clusters apart: navigation, the
/// breadcrumb trail, file actions, and transfer actions. Each pane is about
/// 360 points wide in this window, so the trail also has to survive the narrow
/// case with every segment still laid out inside its own cluster.
#[gpui_kit::test]
fn path_bar_rules_keep_the_clusters_apart(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.set_local_path(LocalPath::new("/tmp/test"), window, cx);
    });
    cx.simulate_resize(size(px(720.0), px(480.0)));
    cx.run_until_parked();

    let navigation_rule = cx
        .debug_bounds("path-bar-separator-local-navigation-breadcrumb")
        .expect("rule after the navigation cluster");
    let first_segment = cx
        .debug_bounds("breadcrumb-segment-local-0")
        .expect("first breadcrumb segment");
    let last_segment = cx
        .debug_bounds("breadcrumb-segment-local-2")
        .expect("last breadcrumb segment");
    let actions_rule = cx
        .debug_bounds("path-bar-separator-local-breadcrumb-file-actions")
        .expect("rule after the breadcrumb");
    let transfer_rule = cx
        .debug_bounds("path-bar-separator-local-file-actions-transfer")
        .expect("rule before the transfer cluster");

    // The rules are hairlines, not filler: one pixel wide and shorter than the
    // bar, so they read as grouping rather than as a border.
    for rule in [navigation_rule, actions_rule, transfer_rule] {
        assert_eq!(rule.size.width, px(1.0), "the rule is a hairline: {rule:?}");
        assert!(
            rule.size.height <= px(16.0),
            "the rule must stay shorter than the bar: {rule:?}"
        );
    }
    assert!(
        navigation_rule.left() < actions_rule.left() && actions_rule.left() < transfer_rule.left(),
        "the rules run navigation, breadcrumb, actions, transfer: \
         {navigation_rule:?} {actions_rule:?} {transfer_rule:?}"
    );
    assert!(
        navigation_rule.right() <= first_segment.left(),
        "the breadcrumb starts after the navigation rule: \
         {navigation_rule:?} {first_segment:?}"
    );
    let trail = cx
        .debug_bounds("path-breadcrumb-local")
        .expect("the breadcrumb trail is laid out");
    assert!(
        trail.left() >= navigation_rule.right() && trail.right() <= actions_rule.left(),
        "the trail is its own cluster between the rules: {trail:?}"
    );
    // Segment geometry is font dependent, so the guard stays structural: the
    // trail keeps its segments ordered, and the last one is never dropped.
    assert!(
        first_segment.left() < last_segment.left(),
        "the trail keeps its order: {first_segment:?} {last_segment:?}"
    );
    let viewport = cx
        .debug_bounds("local-list-container-viewport")
        .expect("local pane viewport");
    assert!(
        first_segment.left() >= viewport.left() && first_segment.right() <= viewport.right(),
        "the breadcrumb starts inside the pane: {first_segment:?} in {viewport:?}"
    );

    // A trail longer than its slot must not push the rules around: the trail is
    // the flexible element, the clusters and their rules keep their places.
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.set_local_path(
            LocalPath::new("/tmp/macsftp-path-bar-test-is-longer-than-its-slot"),
            window,
            cx,
        );
    });
    cx.run_until_parked();
    for (selector, before) in [
        (
            "path-bar-separator-local-navigation-breadcrumb",
            navigation_rule.left(),
        ),
        (
            "path-bar-separator-local-file-actions-transfer",
            transfer_rule.left(),
        ),
    ] {
        let after = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} must stay laid out"))
            .left();
        assert_eq!(
            after, before,
            "a long path must not move {selector}: {after:?} was {before:?}"
        );
    }
    assert!(
        cx.debug_bounds("breadcrumb-segment-local-2").is_some(),
        "the last path component stays in the trail, however long the path is"
    );
}

#[gpui_kit::test]
fn command_palette_fits_inside_short_window(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.simulate_resize(size(px(720.0), px(480.0)));
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_command_palette(window, cx);
    });
    cx.run_until_parked();

    let card = cx
        .debug_bounds("command-palette-card")
        .expect("command palette card is rendered");
    assert_eq!(card.top(), px(48.0), "palette top padding is 48px");
    assert!(
        card.bottom() <= px(480.0),
        "palette card bottom {:?} must fit within 480px viewport",
        card.bottom()
    );
}

/// A classified error leads with its title and keeps the message on a second,
/// quieter line instead of splicing both into one line with a dash.
#[gpui_kit::test]
fn error_state_splits_title_and_message_onto_two_lines(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, cx| {
        let tab = workspace
            .active_tab_mut()
            .expect("fixture has an active tab");
        tab.connection = ConnectionState::Failed {
            error: macsftp_core::UserFacingError::new(
                macsftp_core::ErrorCode::PermissionDenied,
                "Can’t open this folder",
                "You don’t have permission to view it.",
            ),
        };
        cx.notify();
    });
    cx.simulate_resize(size(px(720.0), px(480.0)));
    cx.run_until_parked();

    let title = cx.debug_bounds("empty-state-message").expect("error title");
    let detail = cx
        .debug_bounds("empty-state-detail")
        .expect("error message");
    assert!(
        title.bottom() <= detail.top(),
        "the message sits under the title: {title:?} {detail:?}"
    );
    let viewport = cx
        .debug_bounds("remote-list-container-viewport")
        .expect("remote pane viewport");
    assert!(
        title.left() >= viewport.left() && detail.right() <= viewport.right(),
        "both lines stay inside the pane: {title:?} {detail:?} in {viewport:?}"
    );
}

#[gpui_kit::test]
fn compact_workspace_scrolls_recent_connections(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |_workspace, cx| {
        for index in 0..6 {
            cx.resources_mut()
                .recents
                .upsert(macsftp_storage::RecentEntryInput {
                    host: format!("recent-{index}.example.com"),
                    port: 22,
                    username: "operator".into(),
                    profile_id: None,
                    display_name: None,
                    last_remote_path: None,
                    last_connected_at: index,
                })
                .expect("seed recent connection fixture");
        }
        cx.notify();
    });
    cx.simulate_resize(size(px(720.0), px(480.0)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("remote-connection-empty-scrollbar-thumb")
            .is_some(),
        "short windows expose a scrollbar for all recent connections"
    );
    let viewport = cx
        .debug_bounds("remote-list-container-viewport")
        .expect("remote viewport exists");
    let card = cx
        .debug_bounds("remote-empty-card")
        .expect("recent connection card is visible");
    // Overflowing content starts at the top edge (inside the card's own
    // vertical padding): centering it would push the first row above the
    // viewport, where neither scrolling nor clipping could bring it back.
    assert!(
        card.top() >= viewport.top() && card.top() <= viewport.top() + px(12.0),
        "overflowing recent connections start at the viewport top: {card:?} in {viewport:?}"
    );
    let position = viewport.center();
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
        ..Default::default()
    });
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.remote_empty_scroll.offset().y < px(0.0))
    });
}

#[gpui_kit::test]
fn compact_workspace_centers_recent_connections_that_fit(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |_workspace, cx| {
        for index in 0..3 {
            cx.resources_mut()
                .recents
                .upsert(macsftp_storage::RecentEntryInput {
                    host: format!("recent-{index}.example.com"),
                    port: 22,
                    username: "operator".into(),
                    profile_id: None,
                    display_name: None,
                    last_remote_path: None,
                    last_connected_at: index,
                })
                .expect("seed recent connection fixture");
        }
        cx.notify();
    });
    for dimensions in [size(px(720.0), px(480.0)), size(px(1200.0), px(800.0))] {
        cx.simulate_resize(dimensions);
        cx.run_until_parked();
        let viewport = cx
            .debug_bounds("remote-list-container-viewport")
            .expect("remote viewport exists");
        let card = cx
            .debug_bounds("remote-empty-card")
            .expect("recent connection card is visible");
        assert!(
            card.size.height < viewport.size.height,
            "fixture card must fit the viewport: {card:?} in {viewport:?}"
        );
        assert!(
            (card.center().y - viewport.center().y).abs() < px(2.0),
            "a card that fits is vertically centered: {card:?} in {viewport:?}"
        );
    }
}

#[gpui_kit::test]
fn transfer_drawer_buttons_preserve_resized_height(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, cx| {
        workspace.set_drawer_height(px(180.0), px(900.0));
        cx.notify();
    });
    cx.run_until_parked();
    let collapse = cx
        .debug_bounds("hide-transfer-drawer")
        .expect("drawer has a collapse button");
    cx.simulate_click(collapse.center(), gpui_kit::Modifiers::default());
    workspace.read_with(&cx, |workspace, _| assert!(!workspace.transfer_drawer.open));
    cx.run_until_parked();
    let reopen = cx
        .debug_bounds("status-transfers")
        .expect("footer keeps the transfer toggle visible");
    cx.simulate_click(reopen.center(), gpui_kit::Modifiers::default());
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.transfer_drawer.open);
        assert_eq!(workspace.transfer_drawer.height, px(180.0));
    });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
fn transfer_drawer_default_height(cx: &mut TestAppContext) {
    let (workspace, _cx, _channels) = init_workspace(cx);
    workspace.read_with(&_cx, |workspace, _| {
        assert_eq!(
            workspace.transfer_drawer.height,
            crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
        );
    });
}

#[gpui_kit::test]
fn transfer_drawer_height_survives_toggle(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        workspace.set_drawer_height(gpui_kit::px(180.0), gpui_kit::px(900.0));
    });
    cx.dispatch_action(ShowTransferDrawer);
    cx.dispatch_action(ShowTransferDrawer);
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.transfer_drawer.open);
        assert_eq!(workspace.transfer_drawer.height, gpui_kit::px(180.0));
    });
}

// ---------------------------------------------------------------------------
// Motion (WS1): the drawer spring, the drag override, the overlay fade, and
// the eased transfer progress bar.
// ---------------------------------------------------------------------------

/// One visible frame: advance the simulated clock, then draw. Motion state is
/// sampled while rendering, so a test that advances without drawing sees
/// nothing move; `Duration::ZERO` draws at the current time.
fn advance_frame(cx: &mut VisualTestContext, by: std::time::Duration) {
    cx.executor().advance_clock(by);
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// The height the drawer paints right now; `None` once a closing spring has
/// settled and taken it out of the tree.
fn mounted_drawer_height(cx: &mut VisualTestContext) -> Option<Pixels> {
    cx.debug_bounds("transfer-drawer")
        .map(|bounds| bounds.size.height)
}

/// Layout is not bit-exact, so heights are compared with a hair of tolerance —
/// far tighter than any jump a user could see.
fn assert_height(actual: Option<Pixels>, expected: Pixels, what: &str) {
    let actual = actual.unwrap_or_else(|| panic!("{what}: the drawer is not mounted"));
    assert!(
        (actual.as_f32() - expected.as_f32()).abs() < 0.05,
        "{what}: painted {actual:?}, expected {expected:?}"
    );
}

/// The value a probe view pushed on its most recent render.
fn last_sample(samples: &Rc<RefCell<Vec<f32>>>) -> f32 {
    *samples
        .borrow()
        .last()
        .expect("the probe pushes one sample per render")
}

#[gpui_kit::test]
fn transfer_drawer_springs_shut_and_unmounts_only_at_zero(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    let open_height = mounted_drawer_height(&mut cx).expect("the drawer starts mounted");
    assert!(open_height > px(0.0));

    cx.dispatch_action(ShowTransferDrawer);
    advance_frame(&mut cx, std::time::Duration::from_millis(60));
    let first = mounted_drawer_height(&mut cx).expect("still mounted on the way out");
    assert!(
        first < open_height && first > px(0.0),
        "closing travels from {open_height:?} toward zero instead of jumping: {first:?}"
    );

    advance_frame(&mut cx, std::time::Duration::from_millis(60));
    let second = mounted_drawer_height(&mut cx).expect("still mounted on the way out");
    assert!(
        second < first,
        "the spring keeps closing: {second:?} < {first:?}"
    );

    for _ in 0..15 {
        advance_frame(&mut cx, std::time::Duration::from_millis(100));
    }
    assert_eq!(
        mounted_drawer_height(&mut cx),
        None,
        "a settled spring leaves the tree at exactly zero"
    );
    workspace.read_with(&cx, |workspace, _| {
        assert!(!workspace.transfer_drawer.open);
        assert!(
            workspace.transfer_drawer.height > px(0.0),
            "the stored height survives the animation"
        );
    });
}

#[gpui_kit::test]
fn transfer_drawer_opens_from_zero_and_settles_at_the_stored_height(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    let stored = workspace.read_with(&cx, |workspace, _| workspace.transfer_drawer.height);
    assert!(stored > px(0.0));

    cx.dispatch_action(ShowTransferDrawer);
    for _ in 0..15 {
        advance_frame(&mut cx, std::time::Duration::from_millis(100));
    }
    assert_eq!(mounted_drawer_height(&mut cx), None, "closed first");

    cx.dispatch_action(ShowTransferDrawer);
    advance_frame(&mut cx, std::time::Duration::from_millis(60));
    let growing = mounted_drawer_height(&mut cx).expect("an opening drawer is mounted at once");
    assert!(
        growing > px(0.0) && growing < stored,
        "opening grows from zero rather than popping back: {growing:?} of {stored:?}"
    );

    for _ in 0..15 {
        advance_frame(&mut cx, std::time::Duration::from_millis(100));
    }
    assert_height(
        mounted_drawer_height(&mut cx),
        stored,
        "a settled spring lands on the stored height",
    );
}

#[gpui_kit::test]
fn a_resize_drag_shows_the_stored_height_without_spring_travel(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    let open_height = mounted_drawer_height(&mut cx).expect("the drawer starts open");

    let dragged_to = open_height + px(60.0);
    workspace.update(&mut cx, |workspace, cx| {
        workspace.transfer_drawer.resize = Some(TransferDrawerResize {
            start_height: open_height,
            start_y: px(0.0),
        });
        workspace.set_drawer_height(dragged_to, px(900.0));
        cx.notify();
    });
    cx.run_until_parked();
    assert_height(
        mounted_drawer_height(&mut cx),
        dragged_to,
        "while a drag is live the pointer owns the height",
    );
}

#[gpui_kit::test]
fn reduced_motion_snaps_the_drawer_closed(cx: &mut TestAppContext) {
    let (_workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    assert!(
        mounted_drawer_height(&mut cx).is_some(),
        "the drawer starts open"
    );

    cx.update(|_, cx| cx.set_reduce_motion(true));
    cx.dispatch_action(ShowTransferDrawer);
    cx.run_until_parked();
    assert_eq!(
        mounted_drawer_height(&mut cx),
        None,
        "with reduce motion on, the drawer is gone in the same frame"
    );
}

/// Reports the opacity `OverlayFade` samples, driven by `open`.
struct OverlayFadeProbe {
    open: Rc<Cell<bool>>,
    samples: Rc<RefCell<Vec<f32>>>,
}

impl Render for OverlayFadeProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fade = OverlayFade::sample("overlay-fade-probe", self.open.get(), window, cx);
        self.samples.borrow_mut().push(fade.opacity());
        div()
    }
}

#[gpui_kit::test]
fn an_overlay_fades_in_from_transparent_and_lands_opaque(cx: &mut TestAppContext) {
    let open = Rc::new(Cell::new(false));
    let samples = Rc::new(RefCell::new(Vec::new()));
    let window = cx.open_window(size(px(200.0), px(200.0)), {
        let open = open.clone();
        let samples = samples.clone();
        move |_, _| OverlayFadeProbe { open, samples }
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    assert_eq!(
        last_sample(&samples),
        0.0,
        "a hidden surface is fully transparent"
    );

    samples.borrow_mut().clear();
    open.set(true);
    advance_frame(&mut cx, std::time::Duration::ZERO);
    assert_eq!(
        last_sample(&samples),
        0.0,
        "the opening frame starts transparent"
    );

    advance_frame(&mut cx, std::time::Duration::from_millis(60));
    let part_way = last_sample(&samples);
    assert!(
        part_way > 0.0 && part_way < 1.0,
        "the fade is in flight: {part_way}"
    );

    advance_frame(&mut cx, OVERLAY_FADE);
    assert_eq!(last_sample(&samples), 1.0, "the fade lands fully opaque");

    // Reduce motion skips every frame in between, in both directions.
    cx.update(|_, cx| cx.set_reduce_motion(true));
    open.set(false);
    advance_frame(&mut cx, std::time::Duration::ZERO);
    assert_eq!(last_sample(&samples), 0.0, "no exit is played");
    open.set(true);
    advance_frame(&mut cx, std::time::Duration::ZERO);
    assert_eq!(
        last_sample(&samples),
        1.0,
        "under reduce motion the surface is opaque at once"
    );
}

#[gpui_kit::test]
fn a_dismissed_overlay_settles_without_playing_an_exit(cx: &mut TestAppContext) {
    let open = Rc::new(Cell::new(true));
    let samples = Rc::new(RefCell::new(Vec::new()));
    let window = cx.open_window(size(px(200.0), px(200.0)), {
        let open = open.clone();
        let samples = samples.clone();
        move |_, _| OverlayFadeProbe { open, samples }
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    advance_frame(&mut cx, OVERLAY_FADE);
    assert_eq!(
        last_sample(&samples),
        1.0,
        "the surface has finished fading in"
    );

    samples.borrow_mut().clear();
    open.set(false);
    advance_frame(&mut cx, std::time::Duration::ZERO);
    assert_eq!(
        last_sample(&samples),
        0.0,
        "a dismissed surface is fully transparent on the frame it closes"
    );
}

/// Reports the fraction `painted_progress` hands to the bar, driven by
/// `reported`.
struct ProgressProbe {
    reported: Rc<Cell<f32>>,
    samples: Rc<RefCell<Vec<f32>>>,
}

impl Render for ProgressProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let painted = painted_progress(TransferId(7), self.reported.get(), window, cx);
        self.samples.borrow_mut().push(painted);
        div()
    }
}

#[gpui_kit::test]
fn painted_progress_eases_toward_the_reported_fraction(cx: &mut TestAppContext) {
    let reported = Rc::new(Cell::new(0.25));
    let samples = Rc::new(RefCell::new(Vec::new()));
    let window = cx.open_window(size(px(200.0), px(200.0)), {
        let reported = reported.clone();
        let samples = samples.clone();
        move |_, _| ProgressProbe { reported, samples }
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    assert_eq!(
        last_sample(&samples),
        0.25,
        "a row that appears mid-transfer shows the real value at once"
    );

    reported.set(0.75);
    advance_frame(&mut cx, std::time::Duration::ZERO);
    assert!(
        (last_sample(&samples) - 0.25).abs() < 0.01,
        "the bar does not jump to a newly reported value"
    );

    advance_frame(&mut cx, PROGRESS_SMOOTHING / 2);
    let part_way = last_sample(&samples);
    assert!(
        part_way > 0.25 && part_way < 0.75,
        "the bar is chasing the value: {part_way}"
    );

    advance_frame(&mut cx, PROGRESS_SMOOTHING);
    assert_eq!(last_sample(&samples), 0.75, "it arrives exactly");

    // Reduce motion paints the reported value in the same frame.
    cx.update(|_, cx| cx.set_reduce_motion(true));
    reported.set(0.5);
    advance_frame(&mut cx, std::time::Duration::ZERO);
    assert_eq!(
        last_sample(&samples),
        0.5,
        "reduce motion paints the reported fraction directly"
    );
}

#[gpui_kit::test]
fn transfer_drawer_reset_height(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        workspace.set_drawer_height(gpui_kit::px(360.0), gpui_kit::px(900.0));
        workspace.reset_drawer_height(gpui_kit::px(900.0));
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.transfer_drawer.height,
            crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
        );
    });
}

#[gpui_kit::test]
fn transfer_drawer_min_height_does_not_close(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        // Simulate dragging the handle all the way down.
        workspace.set_drawer_height(gpui_kit::px(1.0), gpui_kit::px(900.0));
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

#[gpui_kit::test]
fn transfer_drawer_resize_delta_grows_when_dragging_up(cx: &mut TestAppContext) {
    // Logic mirror of on_drag_move: new_height = start_height + (start_y - current_y).
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        let start = crate::workspace::drawer_height::TransferDrawerResize {
            start_height: gpui_kit::px(240.0),
            start_y: gpui_kit::px(500.0),
        };
        workspace.transfer_drawer.resize = Some(start.clone());
        let current_y = gpui_kit::px(400.0); // drag up → taller
        let new_height = start.start_height + (start.start_y - current_y);
        workspace.set_drawer_height(new_height, gpui_kit::px(900.0));
        workspace.transfer_drawer.resize = None;
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.transfer_drawer.height, gpui_kit::px(340.0));
        assert!(workspace.transfer_drawer.resize.is_none());
        assert!(workspace.transfer_drawer.open);
    });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
fn settings_sections_render_and_toggles_persist(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.dispatch_action(OpenSettings);
    cx.simulate_resize(size(px(760.0), px(480.0)));
    cx.run_until_parked();

    cx.update(|window, cx| {
        window.render_frame(cx);
        let toggle = window.find("settings-confirm-delete");
        assert_eq!(toggle.label(), Some("Confirm before delete"));
        assert_eq!(toggle.role(), Some(gpui_kit::Role::Button));
        assert_eq!(
            window.find("settings-show-hidden-files").label(),
            Some("Show hidden files")
        );
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("settings-general-scroll-scrollbar-thumb")
            .is_some(),
        "the General form must scroll instead of clipping in a short window"
    );

    // A taller window keeps every switch visible so the test can click each
    // one instead of calling its setter directly.
    cx.simulate_resize(size(px(760.0), px(760.0)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("settings-confirm-delete", cx);
        window.render_frame(cx);
        window.click("settings-show-hidden-files", cx);
        window.render_frame(cx);
        window.click("settings-preserve-mtime", cx);
        window.render_frame(cx);
        window.click("settings-preserve-permissions", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("settings-confirm-delete").checked(),
            Some(false),
            "the clicked switch must repaint in its new state"
        );
        assert_eq!(
            window.find("settings-show-hidden-files").checked(),
            Some(true)
        );
        assert_eq!(
            window.find("settings-preserve-mtime").checked(),
            Some(false)
        );
        assert_eq!(
            window.find("settings-preserve-permissions").checked(),
            Some(false)
        );
    });
    workspace.read_with(&cx, |_workspace, cx| {
        let config = cx.resources().config.config();
        assert!(!config.confirm_delete);
        assert!(config.show_hidden_files);
        assert!(!config.metadata_policy.preserve_mtime);
        assert!(!config.metadata_policy.preserve_permissions);
    });

    // A config error is surfaced on every section, not only General.
    for section in [
        SettingsSection::General,
        SettingsSection::Editor,
        SettingsSection::Profiles,
    ] {
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            workspace.config_error = Some("Settings couldn’t be saved.".into());
            workspace.settings.section = section;
            cx.notify();
        });
        cx.update(|window, cx| window.render_frame(cx));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("settings-config-error").is_some(),
            "the config error banner must be visible on {section:?}"
        );
    }

    // The sidebar entry is a Base Button, so it is keyboard reachable and
    // dispatches the section change like a pointer click.
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.config_error = None;
        workspace.settings.section = SettingsSection::General;
        cx.notify();
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("settings-section-editor", cx);
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.settings.section, SettingsSection::Editor);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.find("settings-external-editor-input");
    });
}

#[gpui_kit::test]
fn config_write_results_share_error_state(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        assert!(!workspace.finish_config_change(
            Err(ConfigError::Parse {
                message: "bad config".into(),
            }),
            "test setting",
            cx,
        ));
        assert_eq!(
            workspace.config_error.as_deref(),
            Some("Settings couldn’t be saved. Check file permissions."),
            "every config write failure must publish the same settings error"
        );

        assert!(workspace.finish_config_change(Ok(()), "test setting", cx));
        assert!(
            workspace.config_error.is_none(),
            "a later successful write must clear the settings error"
        );
    });
}

#[gpui_kit::test]
fn settings_switch_is_keyboard_reachable_and_activates(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.dispatch_action(OpenSettings);
    cx.simulate_resize(size(px(760.0), px(760.0)));
    cx.run_until_parked();

    let mut reached = false;
    cx.update(|window, cx| {
        for _ in 0..12 {
            window.focus_next(cx);
            window.render_frame(cx);
            if window.find("settings-confirm-delete").focused() == Some(true) {
                reached = true;
                break;
            }
        }
    });
    assert!(reached, "Tab traversal must reach the confirm switch");

    let mut space = gpui_kit::Keystroke::parse("space").expect("space key must parse");
    space.key_char = Some(" ".into());
    cx.simulate_event(gpui_kit::KeyDownEvent {
        keystroke: space.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui_kit::KeyUpEvent { keystroke: space });
    workspace.read_with(&cx, |_workspace, cx| {
        assert!(
            !cx.resources().config.config().confirm_delete,
            "Space must activate the focused switch"
        );
    });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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
        save_profile_fixture(cx, profile);
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

#[gpui_kit::test]
fn settings_profile_list_overflow_uses_custom_scrollbar(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        for id in 1..=12 {
            let host = if id == 1 {
                "a-very-long-production-hostname.internal.example.com".to_string()
            } else {
                format!("host-{id}.example.com")
            };
            let profile = macsftp_core::ConnectionProfile::new(
                ProfileId(id),
                format!("Server {id}"),
                host,
                format!("operator-{id}"),
                AuthMethod::Password {
                    secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(id), "password"),
                },
            );
            save_profile_fixture(cx, profile);
        }
        workspace.surface = WorkspaceSurface::Settings;
        workspace.set_settings_section(SettingsSection::Profiles, cx);
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.settings.selected_profile_id, Some(ProfileId(1)));
    });
    cx.simulate_resize(size(px(720.0), px(480.0)));
    cx.run_until_parked();
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("settings-profile-list-scroll-scrollbar-thumb")
            .is_some(),
        "overflowing profile lists expose the shared custom scrollbar"
    );
}

#[gpui_kit::test]
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

#[gpui_kit::test]
fn profile_editor_saves_jump_and_proxy_routes(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        save_settings_fixture(cx, ProfileId(1), "Bastion", &test_settings());
        workspace.start_new_profile(cx);
        let editor = workspace
            .settings
            .profile_editor
            .as_mut()
            .expect("new profile editor opens");
        editor.name.set_value("Internal");
        editor.host.set_value("internal.example");
        editor.username.set_value("deploy");
        editor.auth_method = AuthMethodKind::SshAgent;
        editor.route_kind = ProfileRouteKind::JumpHost;
        editor.jump_profile_id = Some(ProfileId(1));
        workspace.save_profile_editor(cx);

        let target_id = workspace
            .settings
            .selected_profile_id
            .expect("saved target is selected");
        assert!(matches!(
            cx.resources()
                .profiles
                .find_profile(target_id)
                .expect("target saved")
                .route,
            macsftp_core::ConnectionRoute::JumpHost {
                profile_id: ProfileId(1)
            }
        ));

        workspace.load_profile_editor(target_id, cx);
        let editor = workspace
            .settings
            .profile_editor
            .as_mut()
            .expect("target editor reloads");
        editor.route_kind = ProfileRouteKind::ProxyCommand;
        editor.proxy_command.set_value("ssh -W %h:%p gateway");
        workspace.save_profile_editor(cx);
        assert!(matches!(
            &cx.resources()
                .profiles
                .find_profile(target_id)
                .expect("target updated")
                .route,
            macsftp_core::ConnectionRoute::ProxyCommand { command }
                if command == "ssh -W %h:%p gateway"
        ));
    });
}

#[gpui_kit::test]
fn profile_editor_scrolls_at_minimum_window_size(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let scroll = workspace.read_with(&cx, |workspace, _| workspace.settings.editor_scroll.clone());
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        let mut editor = ProfileEditorState::blank();
        editor.host.set_value("internal.example");
        editor.username.set_value("deploy");
        editor.auth_method = AuthMethodKind::SshAgent;
        editor.route_kind = ProfileRouteKind::ProxyCommand;
        editor.proxy_command.set_value("ssh -W %h:%p bastion");
        workspace.surface = WorkspaceSurface::Settings;
        workspace.settings.section = SettingsSection::Profiles;
        workspace.settings.profile_editor = Some(editor);
        cx.notify();
    });
    cx.simulate_resize(size(px(720.0), px(480.0)));
    cx.run_until_parked();
    cx.run_until_parked();

    let thumb_before = cx
        .debug_bounds("settings-profile-editor-scroll-scrollbar-thumb")
        .expect("minimum-height profile editor must expose its custom scrollbar");
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(600.0), px(400.0)),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
        ..Default::default()
    });
    assert!(scroll.offset().y < px(0.0));
    let thumb_after = cx
        .debug_bounds("settings-profile-editor-scroll-scrollbar-thumb")
        .expect("scrollbar thumb remains rendered after wheel input");
    assert!(thumb_after.top() > thumb_before.top());
}

#[gpui_kit::test]
fn profile_editor_passphrase_policy_round_trip_to_ask_every_time(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);

    workspace.update_in(&mut cx, |ws, _window, cx| {
        let settings = ConnectionSettings {
            host: "srv.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::PrivateKey {
                key_path: "~/.ssh/id_ed25519".into(),
                passphrase: Some("old-secret".into()),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, ProfileId(1), "Keyed", &settings);

        ws.surface = WorkspaceSurface::Settings;
        ws.load_profile_editor(ProfileId(1), cx);

        let editor = ws
            .settings
            .profile_editor
            .as_mut()
            .expect("load_profile_editor opens editor");
        assert_eq!(editor.passphrase_policy, PassphrasePolicy::Remember);
        assert!(editor.secret_present_hint);
        assert!(
            editor.passphrase.value().is_empty(),
            "secrets are never prefilled"
        );

        // Switching to ask-every-time with an empty input drops the
        // remembered secret while keeping the has-passphrase flag.
        editor.passphrase_policy = PassphrasePolicy::AskEveryTime;
        ws.save_profile_editor(cx);

        let profile = cx
            .resources()
            .profiles
            .find_profile(ProfileId(1))
            .expect("profile kept after save");
        let AuthMethod::PrivateKey {
            has_passphrase,
            passphrase_ref,
            ..
        } = &profile.auth
        else {
            panic!("expected private-key auth");
        };
        assert!(*has_passphrase, "the key still requires a passphrase");
        assert!(
            passphrase_ref.is_none(),
            "remembered secret must be dropped"
        );
        let saved_secret = cx
            .resources()
            .profiles
            .has_saved_secret(ProfileId(1))
            .expect("keychain query succeeds");
        assert!(!saved_secret);
    });
}

#[gpui_kit::test]
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
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, profile_id, "Work", &settings);

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

#[gpui_kit::test]
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
            Some("Enter a password."),
            "empty password on new profile is a validation error"
        );
    });
}

#[gpui_kit::test]
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
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, profile_id, "Work Server", &settings);

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

/// Regression for the profiles-audit consistency risk: deleting a
/// profile used to leave recents entries and live tabs pointing at the
/// dead id. The stale recents linkage broke upsert dedupe (manual
/// reconnects created duplicate entries) and future session snapshots
/// kept propagating the dead reference.
#[gpui_kit::test]
fn deleting_profile_decouples_recents_and_live_tabs(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);

    let profile_id = ProfileId(4);
    workspace.update_in(&mut cx, |ws, window, cx| {
        let settings = ConnectionSettings {
            host: "rec.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "hunter2".into(),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, profile_id, "Prod", &settings);
        match cx
            .resources_mut()
            .recents
            .upsert(macsftp_storage::RecentEntryInput {
                host: "rec.example.com".into(),
                port: 22,
                username: "alex".into(),
                profile_id: Some(profile_id.0),
                display_name: Some("Prod".into()),
                last_remote_path: Some("/home/alex".into()),
                last_connected_at: 100,
            }) {
            Ok(_) => {}
            Err(error) => panic!("seed recent entry: {error}"),
        }

        {
            let tab = ws.active_tab_mut().expect("default tab");
            tab.profile_id = Some(profile_id);
            tab.restored_target = Some(Box::new(RestoredTabTarget {
                host: "rec.example.com".into(),
                port: 22,
                username: "alex".into(),
                profile_id: Some(profile_id),
                remote_path: None,
            }));
        }

        ws.request_delete_profile(profile_id, window, cx);
        ws.confirm_delete_profile(window, cx);

        // The recents entry survives as plain manual-connection history.
        let entries = cx.resources().recents.entries();
        assert_eq!(entries.len(), 1, "entry is kept, not removed");
        assert_eq!(entries[0].profile_id, None, "stale link must be dropped");
        assert_eq!(entries[0].display_name.as_deref(), Some("Prod"));

        let tab = ws.active_tab_mut().expect("tab kept after delete");
        assert_eq!(tab.profile_id, None, "live tab must lose the dead link");
        assert_eq!(
            tab.restored_target.as_ref().map(|target| target.profile_id),
            Some(None),
            "restored target must lose the dead link"
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

#[gpui_kit::test]
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
        save_profile_fixture(cx, work);
        save_profile_fixture(cx, home);

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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn about_escape_restores_pane_focus(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        // Simulate focus having left the pane (e.g. after interacting with About).
        window.focus(&ws.modal_focus, cx);
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

#[gpui_kit::test]
fn about_close_button_restores_pane_focus(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        window.focus(&ws.modal_focus, cx);
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

#[gpui_kit::test]
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

// ── Runtime bridge wiring ─────────────────────────────────────

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn keyboard_interactive_prompt_submits_redacted_responses(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.connect_with(test_settings(), None, window, cx);
    });
    let _ = channels.command_rx.try_recv();
    let request_id = KeyboardInteractiveRequestId(7);
    let prompt = KeyboardInteractivePrompt {
        request_id,
        scope: RemoteEventScope::new(TabId(1), SessionId(1), 1),
        name: "Verification".into(),
        instruction: "Enter the code".into(),
        prompts: vec![KeyboardInteractivePromptField {
            prompt: "Code:".into(),
            echo: false,
        }],
    };
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::KeyboardInteractivePrompt(prompt.clone()),
            window,
            cx,
        );
        let inputs = workspace
            .modal_inputs
            .keyboard_interactive
            .get_mut(&request_id)
            .expect("prompt creates input state");
        inputs.inputs[0].state_mut().set_value("123456");
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("keyboard-interactive-scrim").is_some(),
        "keyboard-interactive prompt must render an actionable modal"
    );
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.submit_keyboard_interactive(request_id, window, cx);
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("interactive response is dispatched");
    match command {
        AppCommand::RespondKeyboardInteractive(response) => {
            assert_eq!(response.request_id, request_id);
            assert_eq!(response.responses, vec!["123456"]);
            assert!(!format!("{response:?}").contains("123456"));
        }
        other => panic!("expected keyboard-interactive response, got {other:?}"),
    }
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.active_keyboard_interactive_prompt().is_none());
        assert!(workspace.modal_inputs.keyboard_interactive.is_empty());
    });
}

#[gpui_kit::test]
fn keyboard_interactive_prompt_supports_clipboard_paste(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.connect_with(test_settings(), None, window, cx);
    });
    let _ = channels.command_rx.try_recv();
    let request_id = KeyboardInteractiveRequestId(8);
    let prompt = KeyboardInteractivePrompt {
        request_id,
        scope: RemoteEventScope::new(TabId(1), SessionId(1), 1),
        name: "Verification".into(),
        instruction: "Enter the code".into(),
        prompts: vec![KeyboardInteractivePromptField {
            prompt: "Code:".into(),
            echo: false,
        }],
    };
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::KeyboardInteractivePrompt(prompt.clone()),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("987654".into()));

    workspace.update_in(&mut cx, |workspace, window, cx| {
        let key_event = gpui_kit::KeyDownEvent {
            keystroke: gpui_kit::Keystroke {
                modifiers: gpui_kit::Modifiers {
                    platform: true,
                    ..Default::default()
                },
                key: "v".into(),
                key_char: None,
            },
            is_held: false,
            prefer_character_input: false,
        };
        workspace.handle_keyboard_interactive_key(&key_event, window, cx);
    });

    workspace.read_with(&cx, |workspace, _| {
        let inputs = workspace
            .modal_inputs
            .keyboard_interactive
            .get(&request_id)
            .expect("modal inputs exist");
        assert_eq!(
            inputs.inputs[0].state().value(),
            "987654",
            "Cmd+V must paste clipboard contents into keyboard-interactive input"
        );
    });
}

#[gpui_kit::test]
fn stale_keyboard_interactive_prompt_is_dropped(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.connect_with(test_settings(), None, window, cx);
    });
    let _ = channels.command_rx.try_recv();
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::KeyboardInteractivePrompt(KeyboardInteractivePrompt {
                request_id: KeyboardInteractiveRequestId(9),
                scope: RemoteEventScope::new(TabId(1), SessionId(99), 0),
                name: String::new(),
                instruction: String::new(),
                prompts: Vec::new(),
            }),
            window,
            cx,
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.active_keyboard_interactive_prompt().is_none());
        assert!(workspace.modal_inputs.keyboard_interactive.is_empty());
    });
}

#[gpui_kit::test]
fn keyboard_interactive_inputs_are_isolated_per_tab(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    let prompt = |request_id, tab_id| KeyboardInteractivePrompt {
        request_id: KeyboardInteractiveRequestId(request_id),
        scope: RemoteEventScope::new(TabId(tab_id), SessionId(tab_id), 1),
        name: String::new(),
        instruction: String::new(),
        prompts: vec![KeyboardInteractivePromptField {
            prompt: "Code:".into(),
            echo: false,
        }],
    };
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.present_keyboard_interactive(prompt(1, 1), window, cx);
        workspace
            .state
            .tabs
            .open_tab(TabState::new(TabId(2), "second"));
        workspace.present_keyboard_interactive(prompt(2, 2), window, cx);
        assert_eq!(workspace.modal_inputs.keyboard_interactive.len(), 2);
        workspace.cancel_keyboard_interactive(KeyboardInteractiveRequestId(2), window, cx);
        assert!(
            workspace
                .modal_inputs
                .keyboard_interactive
                .contains_key(&KeyboardInteractiveRequestId(1))
        );
        assert!(
            !workspace
                .modal_inputs
                .keyboard_interactive
                .contains_key(&KeyboardInteractiveRequestId(2))
        );
    });
    assert!(matches!(
        channels.command_rx.try_recv(),
        Ok(AppCommand::CancelKeyboardInteractive {
            request_id: KeyboardInteractiveRequestId(2)
        })
    ));
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
        "Connection Lost",
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn open_recent_with_profile_and_keychain_connects(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);

    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        save_settings_fixture(
            cx,
            ProfileId(1),
            "Deploy box",
            &ConnectionSettings {
                host: "profile.example.com".into(),
                port: 22,
                username: "deploy".into(),
                auth: AuthCredential::Password {
                    password: "s3cret".into(),
                },
                route: macsftp_core::ResolvedConnectionRoute::Direct,
            },
        );
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

#[gpui_kit::test]
fn open_recent_with_missing_keychain_password_keeps_form_open(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);

    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        save_settings_fixture(
            cx,
            ProfileId(1),
            "Missing secret box",
            &ConnectionSettings {
                host: "missing-secret.example.com".into(),
                port: 22,
                username: "deploy".into(),
                auth: AuthCredential::Password {
                    password: "s3cret".into(),
                },
                route: macsftp_core::ResolvedConnectionRoute::Direct,
            },
        );
    });

    let recent_id = workspace.update_in(&mut cx, |_workspace, _window, cx| {
        let profiles = cx.resources().profiles.profiles();
        assert_eq!(profiles.len(), 1, "profile saved");
        let saved = &profiles[0];
        let saved_id = saved.id;
        let profiles_path = cx.resources().profiles.path().clone();
        cx.resources_mut().profiles =
            macsftp_storage::ProfileStore::open_or_empty_memory(profiles_path);
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn transfers_use_the_configured_metadata_policy(cx: &mut TestAppContext) {
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
    while channels.command_rx.try_recv().is_ok() {}

    let policy = MetadataPolicy {
        preserve_permissions: false,
        preserve_mtime: false,
        preserve_symlinks: true,
    };
    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        cx.resources_mut()
            .config
            .set_metadata_preserve_mtime(false)
            .expect("persist metadata mtime");
        cx.resources_mut()
            .config
            .set_metadata_preserve_permissions(false)
            .expect("persist metadata permissions");
    });

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_upload(vec![LocalPath::new("/tmp/foo.txt")], cx);
    });
    let upload = channels
        .command_rx
        .try_recv()
        .expect("upload must enqueue a StartTransfer");
    let AppCommand::StartTransfer(upload) = upload else {
        panic!("expected StartTransfer for upload, got {upload:?}");
    };
    assert_eq!(upload.metadata_policy, policy);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_download(vec![RemotePath::new("/home/tester/bar.txt")], cx);
    });
    let download = channels
        .command_rx
        .try_recv()
        .expect("download must enqueue a StartTransfer");
    let AppCommand::StartTransfer(download) = download else {
        panic!("expected StartTransfer for download, got {download:?}");
    };
    assert_eq!(download.metadata_policy, policy);

    // Remote-edit upload-back keeps the default policy on purpose: its
    // conflict baseline is derived from the local temp file's mtime, so
    // disabling mtime preservation here would fabricate a conflict.
    let edit = super::remote_edit::build_edit_upload_command(
        &LocalPath::new("/tmp/edit.txt"),
        &RemotePath::new("/home/tester/edit.txt"),
        1,
        ProfileId(1),
        TabId(1),
    );
    let AppCommand::StartTransfer(edit) = edit else {
        panic!("expected StartTransfer for edit upload, got {edit:?}");
    };
    assert_eq!(edit.metadata_policy, MetadataPolicy::default());
}

#[gpui_kit::test]
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
    connect_and_drain_with(workspace, cx, channels, test_settings(), None);
}

/// `connect_and_drain` for an explicit settings/profile combination, e.g.
/// connecting through a saved profile.
fn connect_and_drain_with(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    channels: &BridgeChannels,
    settings: ConnectionSettings,
    profile_id: Option<ProfileId>,
) {
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.connect_with(settings, profile_id, window, cx);
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

/// Complete a pending (re)connect handshake on TabId(1) with the given
/// session id/epoch and drain the resulting command.
fn complete_connect(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    channels: &BridgeChannels,
    session_id: SessionId,
    epoch: u64,
) {
    let scope = RemoteEventScope::new(TabId(1), session_id, epoch);
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

/// The connection key a tab captured at its last connect attempt, read
/// from inside a `read_with`/`update` closure (whose `cx` derefs to
/// `&App`).
fn tab_connection_key_in_cx(workspace: &Workspace, _cx: &App, tab_id: TabId) -> ConnectionKey {
    workspace
        .state
        .tabs
        .find_tab(tab_id)
        .and_then(|tab| tab.connection_key.as_deref().cloned())
        .expect("tab must hold a captured connection key")
}

/// The key the first manual `connect_with(test_settings(), None)` captures:
/// session ids are allocated from 1, so a fresh tab's first connection is
/// `Ephemeral(SessionId(1))`.
fn manual_connection_key() -> ConnectionKey {
    test_connection_key(SessionId(1))
}

#[gpui_kit::test]
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
            .find_active(&manual_connection_key(), &entry.path)
            .expect("small-file edit must register an active session");
        assert_eq!(session.phase, EditPhase::Downloading);
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

#[gpui_kit::test]
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
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path.clone(),
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(13),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
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
            .find_active(&manual_connection_key(), &remote_path)
            .expect("the existing edit session remains active");
        assert_eq!(session.id, session_id);
        assert_eq!(session.phase, EditPhase::Editing);
        assert!(
            !cx.resources().edit_sessions.session_tab_ids().is_empty(),
            "the existing edit session remains registered"
        );
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Your copy is open for editing.")
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "reopening an existing edit must not dispatch another download"
    );
}

#[gpui_kit::test]
fn symlink_edit_uploads_through_to_the_resolved_target(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    let link_path = RemotePath::new("/home/tester/latest.txt");
    let target_path = RemotePath::new("/home/tester/releases/2.txt");

    let (session_id, _temp_path) = workspace.update_in(&mut cx, |workspace, _window, cx| {
        let session_id = cx.resources_mut().edit_sessions.next_id();
        let temp_path = LocalPath::new(format!(
            "{}/{}/{}/2.txt",
            cx.resources().app_paths.edits_dir.as_str(),
            cx.resources().edit_run_id,
            session_id.0
        ));
        let parent = std::path::Path::new(temp_path.as_str())
            .parent()
            .expect("edit temp has a parent directory");
        std::fs::create_dir_all(parent).expect("create edit temp directory");
        std::fs::write(temp_path.as_str(), b"modified through link").expect("write edit temp");
        cx.resources_mut().edit_sessions.register(EditSession {
            id: session_id,
            remote_path: link_path.clone(),
            io_path: target_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path.clone(),
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(9),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        let tab = workspace.active_tab_mut().expect("active tab");
        tab.selection.selected_paths = vec![EntryPath::Remote(link_path.clone())];
        cx.refresh_windows();
        (session_id, temp_path)
    });

    // Selecting the link in the pane still maps to its edit session.
    workspace.read_with(&cx, |workspace, cx| {
        assert_eq!(
            workspace.selected_edit_session(cx),
            Some((session_id, EditPhase::Editing))
        );
    });

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.upload_selected_edit(cx);
    });
    let command = channels
        .command_rx
        .try_recv()
        .expect("upload must check the live remote target");
    let AppCommand::CheckRemoteEditSnapshot(command) = command else {
        panic!("expected CheckRemoteEditSnapshot, got {command:?}");
    };
    assert_eq!(
        command.path, target_path,
        "the authoritative check must stat the file the link points at"
    );
    assert_eq!(
        command.edit_session_id, session_id,
        "the check stays bound to the link's edit session"
    );
}

#[gpui_kit::test]
fn upload_modified_file_is_explicit_and_checks_remote_first(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    cx.run_until_parked();
    let remote_path = RemotePath::new("/home/tester/notes.txt");

    let session_id = workspace.update_in(&mut cx, |workspace, _window, cx| {
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
        std::fs::write(temp_path.as_str(), b"modified locally").expect("write modified edit temp");
        cx.resources_mut().edit_sessions.register(EditSession {
            id: session_id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path,
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(13),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        let tab = workspace.active_tab_mut().expect("active tab");
        tab.selection.selected_paths = vec![EntryPath::Remote(remote_path.clone())];
        cx.refresh_windows();
        session_id
    });

    workspace.read_with(&cx, |workspace, cx| {
        assert_eq!(
            workspace.selected_edit_session(cx),
            Some((session_id, EditPhase::Editing))
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "opening an editable copy must not monitor or upload local saves automatically"
    );

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.upload_selected_edit(cx);
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("explicit upload must request a live remote check");
    let AppCommand::CheckRemoteEditSnapshot(command) = command else {
        panic!("expected CheckRemoteEditSnapshot, got {command:?}");
    };
    assert_eq!(command.edit_session_id, session_id);
    assert_eq!(command.path, remote_path);
    workspace.read_with(&cx, |_workspace, cx| {
        let session = cx
            .resources()
            .edit_sessions
            .get(session_id)
            .expect("edit session remains live");
        assert_eq!(session.phase, EditPhase::CheckingRemote);
        assert_eq!(session.pending_check_id, Some(command.check_id));
        assert!(session.checking_local_mtime.is_some());
    });
}

#[gpui_kit::test]
fn edit_metadata_failure_preserves_session_and_temp_directory(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    let remote_path = RemotePath::new("/home/tester/notes.txt");
    let temp_dir = unique_temp_dir("edit-metadata-failure");
    std::fs::create_dir_all(&temp_dir).expect("create edit temp directory");
    let preserved_file = temp_dir.join("notes.txt");
    std::fs::write(&preserved_file, b"important local edits").expect("write local edit");
    let invalid_temp_path = LocalPath::new(format!(
        "{}/unreadable\0notes.txt",
        temp_dir.to_string_lossy()
    ));

    let session_id = workspace.update_in(&mut cx, |workspace, _window, cx| {
        let session_id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id: session_id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: invalid_temp_path,
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(21),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        workspace
            .active_tab_mut()
            .expect("active tab")
            .selection
            .selected_paths = vec![EntryPath::Remote(remote_path)];
        workspace.upload_selected_edit(cx);
        session_id
    });

    workspace.read_with(&cx, |workspace, cx| {
        let session = cx
            .resources()
            .edit_sessions
            .get(session_id)
            .expect("metadata failure keeps the edit session retryable");
        assert_eq!(session.phase, EditPhase::Editing);
        assert!(session.pending_check_id.is_none());
        assert!(session.checking_local_mtime.is_none());
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Your edited copy couldn’t be read. Check file permissions and try again.")
        );
    });
    assert!(
        preserved_file.exists(),
        "metadata failure must not delete the edit session directory"
    );
    assert!(
        channels.command_rx.try_recv().is_err(),
        "metadata failure must not authorize a remote check or upload"
    );

    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        cx.resources_mut().edit_sessions.remove(session_id);
    });
    std::fs::remove_dir_all(temp_dir).expect("remove edit metadata fixture");
}

#[gpui_kit::test]
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
            io_path: remote_path.clone(),
            tab_id: TabId(999),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: stale_temp,
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(5),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
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
            .find_active(&manual_connection_key(), &remote_path)
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

#[gpui_kit::test]
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
                .find_active(&manual_connection_key(), &entry.path)
                .is_none(),
            "no session may be registered before the user confirms"
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "no StartTransfer may be sent before the user confirms"
    );
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
            .find_active(&manual_connection_key(), &entry.path)
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn open_remote_file_starts_edit(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let file_path = RemotePath::new("/home/tester/notes.txt");
    workspace.update_in(&mut cx, |workspace, window, cx| {
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
        // Opening a remote file (Enter, double-click or the pane context menu)
        // resolves through the focused pane's selection.
        workspace.focus_pane(PaneSide::Remote, window, cx);
        workspace.open_selected_entry(window, cx);
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
            .find_active(&manual_connection_key(), &file_path)
            .expect("opening a remote file registers an edit session");
        assert_eq!(session.phase, EditPhase::Downloading);
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("opening a remote file must enqueue a StartTransfer download");
    let AppCommand::StartTransfer(command) = command else {
        panic!("expected StartTransfer for edit download, got {command:?}");
    };
    assert_eq!(command.direction, TransferDirection::Download);
    assert_eq!(command.sources, vec![TransferEndpoint::Remote(file_path)]);
}

#[gpui_kit::test]
fn open_remote_directory_navigates_instead_of_editing(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let dir_path = RemotePath::new("/home/tester/project");
    workspace.update_in(&mut cx, |workspace, window, cx| {
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
        workspace.focus_pane(PaneSide::Remote, window, cx);
        workspace.open_selected_entry(window, cx);
    });

    // A directory is not editable: it navigates instead of arming an edit.
    workspace.read_with(&cx, |workspace, cx| {
        assert!(
            workspace.modal_inputs.large_edit_confirm.is_none(),
            "a directory must not arm any edit confirmation"
        );
        assert!(
            cx.resources()
                .edit_sessions
                .find_active(&manual_connection_key(), &dir_path)
                .is_none(),
            "opening a directory must not register an edit session"
        );
        let tab = workspace.active_tab().expect("active tab");
        assert_eq!(
            tab.remote.path.as_ref(),
            Some(&dir_path),
            "opening a directory navigates into it"
        );
    });
    let command = channels
        .command_rx
        .try_recv()
        .expect("navigation must ask the runtime for the directory");
    assert!(
        !matches!(command, AppCommand::StartTransfer(_)),
        "a directory must not start an edit download, got {command:?}"
    );
}

#[gpui_kit::test]
fn settings_external_editor_typing_persists_and_round_trips(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);

    // Open Settings → Editor and focus the external-editor field, then type
    // a value through the real key handler the GUI routes to.
    cx.dispatch_action(OpenSettings);
    let config_path = workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.settings.section = SettingsSection::Editor;
        cx.notify();
        workspace.focus_external_editor(window, cx);
        cx.resources().app_paths.config_file.clone()
    });

    cx.simulate_keystrokes("v i m");

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
    cx.simulate_keystrokes("cmd-a backspace");
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

#[gpui_kit::test]
fn component_input_keeps_unicode_and_focus_across_redraws(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.dispatch_action(OpenSettings);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.settings.section = SettingsSection::Editor;
        cx.notify();
        workspace.focus_external_editor(window, cx);
    });
    cx.simulate_input("编辑器");
    workspace.update(&mut cx, |_workspace, cx| cx.notify());
    cx.simulate_input(" app");
    workspace.read_with(&cx, |workspace, cx| {
        assert_eq!(
            workspace.settings.external_editor_input.read(cx).value(),
            "编辑器 app"
        );
        assert_eq!(
            cx.resources().config.config().external_editor.as_deref(),
            Some("编辑器 app")
        );
    });
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("vim");
    workspace.read_with(&cx, |_workspace, cx| {
        assert_eq!(
            cx.resources().config.config().external_editor.as_deref(),
            Some("vim")
        );
    });
    cx.simulate_keystrokes("escape");
    workspace.read_with(&cx, |workspace, _cx| {
        assert_eq!(
            workspace.surface,
            WorkspaceSurface::Files,
            "Escape must leave Settings when the input has focus"
        );
    });
}

/// Register a `RemoteConflict` edit session directly in the store and return
/// its id, so the conflict-resolution tests can drive
/// `resolve_edit_conflict` without replaying a full download cycle.
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
            io_path: remote_path.clone(),
            remote_path,
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path,
            phase: EditPhase::RemoteConflict,
            remote_snapshot: RemoteSnapshot {
                size: Some(10),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        id
    })
}

#[gpui_kit::test]
fn resolve_conflict_overwrite_uploads(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);

    let remote_path = RemotePath::new("/home/tester/edit.txt");
    let temp_path = LocalPath::new("/tmp/edits/1/edit.txt");
    let id = register_conflict_session(&workspace, &mut cx, remote_path.clone(), temp_path.clone());
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
            .find_active(&manual_connection_key(), &remote_path)
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

#[gpui_kit::test]
fn edit_dedup_follows_captured_connection_key_not_profile_id(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let remote_path = RemotePath::new("/srv/config.json");
    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        // A live edit session opened through a DIFFERENT physical
        // connection (another manual connection to another server) that
        // happens to carry the same ProfileId(0) placeholder.
        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(99),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: test_connection_key(SessionId(7)),
            local_temp_path: LocalPath::new("/tmp/edits/other/config.json"),
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(10),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
    });
    while channels.command_rx.try_recv().is_ok() {}

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_edit(
            remote_path.clone(),
            Some(2_048),
            Some(Timestamp::from_secs_since_epoch(100)),
            cx,
        );
    });

    // The foreign-connection session must not be reopened or block the
    // edit; this connection downloads its own temp copy instead.
    workspace.read_with(&cx, |_workspace, cx| {
        let store = &cx.resources().edit_sessions;
        assert_eq!(store.editing_sessions_for_tab(TabId(99)).count(), 1);
        assert_eq!(
            store
                .editing_sessions_for_tab(TabId(99))
                .next()
                .map(|s| s.connection_key.clone()),
            Some(test_connection_key(SessionId(7))),
            "the foreign session must be untouched"
        );
        let fresh = store
            .find_active(&manual_connection_key(), &remote_path)
            .expect("this connection must start its own edit session")
            .id;
        assert_ne!(
            fresh,
            EditSessionId(1),
            "the edit must not reuse the foreign session"
        );
        assert_eq!(
            store.get(EditSessionId(1)).map(|s| s.phase.clone()),
            Some(EditPhase::Editing)
        );
    });
    assert!(
        matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::StartTransfer(_))
        ),
        "a different connection must dispatch a fresh download despite ProfileId(0)"
    );
}

#[gpui_kit::test]
fn saved_profile_host_change_does_not_hit_old_connections_edit(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);

    // Connect through a saved profile (auth fingerprint → Saved identity).
    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        let profile_settings = ConnectionSettings {
            host: "old.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "profile-pass".into(),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, ProfileId(1), "Prod", &profile_settings);
    });
    connect_and_drain_with(
        &workspace,
        &mut cx,
        &channels,
        ConnectionSettings {
            host: "old.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "profile-pass".into(),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        },
        Some(ProfileId(1)),
    );

    // A live edit from the old-host connection.
    let remote_path = RemotePath::new("/srv/config.json");
    let old_key = workspace.update(&mut cx, |workspace, cx| {
        let key = tab_connection_key_in_cx(workspace, cx, TabId(1));
        assert!(
            matches!(key.identity, ConnectionPoolIdentity::Saved(_)),
            "a saved-profile connection must capture a Saved identity"
        );
        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            connection_key: key.clone(),
            local_temp_path: LocalPath::new("/tmp/edits/old/config.json"),
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(10),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        key
    });
    while channels.command_rx.try_recv().is_ok() {}

    // The profile is re-pointed at another host mid-life; the tab then
    // reconnects with the same profile id.
    workspace.update_in(&mut cx, |workspace, window, cx| {
        let updated_profile_settings = ConnectionSettings {
            host: "new.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "profile-pass".into(),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, ProfileId(1), "Prod", &updated_profile_settings);
        workspace.connect_with(
            ConnectionSettings {
                host: "new.example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "profile-pass".into(),
                },
                route: macsftp_core::ResolvedConnectionRoute::Direct,
            },
            Some(ProfileId(1)),
            window,
            cx,
        );
    });
    while channels.command_rx.try_recv().is_ok() {}
    complete_connect(&workspace, &mut cx, &channels, SessionId(2), 2);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_edit(
            remote_path.clone(),
            Some(2_048),
            Some(Timestamp::from_secs_since_epoch(100)),
            cx,
        );
    });

    workspace.read_with(&cx, |workspace, cx| {
        let store = &cx.resources().edit_sessions;
        let reconnected = store
            .find_active(
                &tab_connection_key_in_cx(workspace, cx, TabId(1)),
                &remote_path,
            )
            .expect("the reconnected tab must open a fresh edit");
        assert_eq!(reconnected.phase, EditPhase::Downloading);
        assert_ne!(
            reconnected.connection_key, old_key,
            "the old connection's key must not match after the host change"
        );
    });
}

#[gpui_kit::test]
fn reconnect_same_target_reuses_edit_session_for_upload(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let remote_path = RemotePath::new("/home/tester/notes.txt");
    let temp_path = LocalPath::new("/tmp/edits/notes.txt");
    std::fs::create_dir_all(std::path::Path::new(temp_path.as_str()).parent().unwrap()).unwrap();
    std::fs::write(temp_path.as_str(), b"edited").unwrap();
    let id = register_conflict_session(&workspace, &mut cx, remote_path.clone(), temp_path.clone());
    while channels.command_rx.try_recv().is_ok() {}

    // Reconnect without changing host/user/auth: the epoch bumps but the
    // captured ConnectionKey is identical.
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.connect_with(test_settings(), None, window, cx);
    });
    while channels.command_rx.try_recv().is_ok() {}
    complete_connect(&workspace, &mut cx, &channels, SessionId(2), 2);

    workspace.update(&mut cx, |workspace, cx| {
        assert!(workspace.owns_tab(TabId(1)));
        let found = cx
            .resources()
            .edit_sessions
            .find_active(&manual_connection_key(), &remote_path)
            .expect("same-target reconnect must keep the session findable");
        assert_eq!(found.id, id, "session survives the reconnect");
        assert_eq!(
            found.phase,
            EditPhase::RemoteConflict,
            "the conflict is preserved verbatim across the reconnect"
        );

        // The explicit upload path still works off the preserved session:
        // resolve as overwrite and expect an upload command carrying the
        // REFRESHED epoch.
        workspace.resolve_edit_conflict(id, ConflictChoice::Overwrite, cx);
    });
    let command = channels
        .command_rx
        .try_recv()
        .expect("overwrite must enqueue a StartTransfer upload");
    let AppCommand::StartTransfer(command) = command else {
        panic!("expected StartTransfer for upload, got {command:?}");
    };
    assert_eq!(command.direction, TransferDirection::Upload);
    assert_eq!(command.session_epoch, 2, "upload must carry the new epoch");
    assert_eq!(
        command.profile_id,
        ProfileId(0),
        "transfer routing keeps the manual ProfileId(0) placeholder"
    );
    assert_eq!(
        command.destination,
        TransferEndpoint::Remote(remote_path.clone())
    );
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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
                        "The server folder wasn’t found.",
                        "This folder no longer exists. Go up one level or refresh.",
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

#[gpui_kit::test]
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
            io_path: RemotePath::new("/home/tester/doc.txt"),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            connection_key: manual_connection_key(),
            local_temp_path: LocalPath::new("/tmp/edits/doc.txt"),
            phase: EditPhase::Editing,
            remote_snapshot: snapshot,
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
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

#[gpui_kit::test]
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
    // The explicit upload check flags a conflict when `listing != baseline`. After
    // adoption the session no longer diverges from the same listing, so the
    // next explicit upload would proceed instead of popping RemoteConflict.
    let diverged = after != listed;
    assert!(
        !diverged,
        "adopted baseline must not diverge from the listing (no false conflict)"
    );
}

#[gpui_kit::test]
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
    // The explicit divergence check (baseline != current listing) still
    // fires, so the RemoteConflict path remains reachable.
    assert_ne!(
        after, changed,
        "the preserved baseline still diverges from the changed listing"
    );
    let diverged = after != changed;
    assert!(
        diverged,
        "a genuine remote change must remain detectable as divergence"
    );
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
            Some("This location wasn’t found.")
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
            Some("Enter a full folder path.")
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
            Some("This location isn’t a folder.")
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn remote_symlink_entry_is_resolved_on_open_instead_of_edited(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest");
    workspace.update_in(&mut cx, |workspace, window, cx| {
        let tab = workspace.active_tab_mut().expect("active tab");
        tab.remote.entries = vec![RemoteEntry {
            name: "latest".to_string(),
            path: link_path.clone(),
            kind: FileKind::Symlink,
            size: Some(12),
            permissions: Some(0o777),
            modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            link_target: None,
        }];
        tab.selection.selected_paths = vec![EntryPath::Remote(link_path.clone())];
        workspace.focus_pane(PaneSide::Remote, window, cx);
        workspace.open_selected_entry(window, cx);
    });

    assert!(
        matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::ResolveRemoteEntry {
                tab_id: TabId(1),
                session_epoch: 1,
                path,
            }) if path == link_path
        ),
        "a link must be resolved by the actor, not guessed from the listing"
    );
    workspace.read_with(&cx, |_workspace, cx| {
        assert!(
            cx.resources()
                .edit_sessions
                .find_active(&manual_connection_key(), &link_path)
                .is_none(),
            "a link is not edited before its target type is known"
        );
    });
}

#[gpui_kit::test]
fn resolved_remote_directory_link_navigates_by_its_listed_path(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest");
    let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                scope,
                RemoteEntryResolution {
                    path: link_path.clone(),
                    kind: FileKind::Directory,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: None,
                },
            )),
            window,
            cx,
        );
    });

    assert!(matches!(
        channels.command_rx.try_recv(),
        Ok(AppCommand::ReadRemoteDir { tab_id: TabId(1), path }) if path == link_path
    ));
    workspace.read_with(&cx, |workspace, _| {
        let tab = workspace.state.tabs.active_tab().expect("tab must exist");
        assert_eq!(tab.remote.path.as_ref(), Some(&link_path));
    });
}

#[gpui_kit::test]
fn resolved_remote_file_link_edits_the_write_through_target(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest.txt");
    let target_path = RemotePath::new("/home/tester/releases/2.txt");
    let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                scope,
                RemoteEntryResolution {
                    path: link_path.clone(),
                    kind: FileKind::File,
                    target: Some(target_path.clone()),
                    size: Some(2_048),
                    modified_at: Some(Timestamp::from_secs_since_epoch(120)),
                    error: None,
                },
            )),
            window,
            cx,
        );
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("a resolved file link must start an edit download");
    let AppCommand::StartTransfer(command) = command else {
        panic!("expected StartTransfer for edit download, got {command:?}");
    };
    assert_eq!(command.direction, TransferDirection::Download);
    assert_eq!(
        command.sources,
        vec![TransferEndpoint::Remote(target_path.clone())],
        "the download must follow the link, not copy the link itself"
    );
    workspace.read_with(&cx, |_workspace, cx| {
        let session = cx
            .resources()
            .edit_sessions
            .find_active(&manual_connection_key(), &target_path)
            .expect("session is keyed by the write-through path");
        assert_eq!(session.remote_path, link_path, "UI identity stays the link");
        assert_eq!(session.io_path, target_path);
    });
}

#[gpui_kit::test]
fn remote_entry_resolution_is_dropped_when_the_pane_moved_on(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest");
    let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        // The pane has navigated into a different directory since the request.
        workspace.active_tab_mut().expect("active tab").remote.path =
            Some(RemotePath::new("/home/tester/archive"));
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                scope,
                RemoteEntryResolution {
                    path: link_path.clone(),
                    kind: FileKind::Directory,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: None,
                },
            )),
            window,
            cx,
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "a resolution for a directory the pane left must not navigate"
    );
    workspace.read_with(&cx, |workspace, _| {
        let tab = workspace.state.tabs.active_tab().expect("tab must exist");
        assert_eq!(
            tab.remote.path.as_ref(),
            Some(&RemotePath::new("/home/tester/archive")),
            "the newer navigation wins"
        );
    });
}

#[gpui_kit::test]
fn remote_entry_resolution_for_a_non_active_tab_is_ignored(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        // A second, genuinely connected tab that is not the active one. Both
        // tabs pass the core scope guard, so this exercises the app's
        // active-tab check rather than the stale-event guard.
        let mut second = TabState::new(TabId(2), "second");
        second.session_epoch = 1;
        second.connection = ConnectionState::Connected {
            session_id: SessionId(2),
            session_epoch: 1,
            connected_at: Timestamp::from_secs_since_epoch(1),
        };
        second.remote.path = Some(RemotePath::new("/srv/other"));
        workspace.state.tabs.open_tab(second);
        workspace.state.tabs.active_tab_id = Some(TabId(1));
        workspace.begin_link_check(
            TabId(2),
            EntryPath::Remote(RemotePath::new("/srv/other/latest")),
            Some(1),
            cx,
        );
    });

    let scope = RemoteEventScope::new(TabId(2), SessionId(2), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                scope,
                RemoteEntryResolution {
                    path: RemotePath::new("/srv/other/latest"),
                    kind: FileKind::Directory,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: None,
                },
            )),
            window,
            cx,
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "the active tab must not navigate for another tab's resolution"
    );
    workspace.read_with(&cx, |workspace, _| {
        let active = workspace.state.tabs.active_tab().expect("active tab");
        assert_eq!(
            active.remote.path.as_ref(),
            Some(&RemotePath::new(TEST_REMOTE_ROOT))
        );
        let second = workspace.state.tabs.find_tab(TabId(2)).expect("second tab");
        assert_eq!(
            second.remote.path.as_ref(),
            Some(&RemotePath::new("/srv/other")),
            "the other tab keeps its own path"
        );
        assert_eq!(
            workspace.status_message_for_test(),
            None,
            "dropping the result still clears its own pending status"
        );
    });
}

#[gpui_kit::test]
fn dropped_entry_resolution_clears_the_pending_check_status(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest");
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_link_check(TabId(1), EntryPath::Remote(link_path.clone()), Some(1), cx);
    });

    // The pane left while the actor was still resolving.
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.active_tab_mut().expect("active tab").remote.path =
            Some(RemotePath::new("/home/tester/archive"));
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                RemoteEventScope::new(TabId(1), SessionId(1), 1),
                RemoteEntryResolution {
                    path: link_path,
                    kind: FileKind::Directory,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: None,
                },
            )),
            window,
            cx,
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test(),
            None,
            "a dropped resolution must not leave “Checking the link…” behind"
        );
    });
}

/// An older request's result must not clear the status of a newer request for
/// a different entry. (Whether the older result also navigates is unchanged
/// `applies` semantics: the pane's current path still matches its parent.)
#[gpui_kit::test]
fn older_link_result_does_not_clear_a_newer_check_status(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let first = RemotePath::new("/home/tester/first");
    let second = RemotePath::new("/home/tester/second");
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.begin_link_check(TabId(1), EntryPath::Remote(first.clone()), Some(1), cx);
        // A second request supersedes the first one's status.
        workspace.begin_link_check(TabId(1), EntryPath::Remote(second), Some(1), cx);
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                RemoteEventScope::new(TabId(1), SessionId(1), 1),
                RemoteEntryResolution {
                    path: first,
                    kind: FileKind::Directory,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: None,
                },
            )),
            window,
            cx,
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Checking the link…"),
            "the newer request's status must survive the older result"
        );
    });
}

#[gpui_kit::test]
fn cancelled_local_link_check_clears_its_status(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("symlink-stale");
    let target = fixture.join("releases").join("2");
    std::fs::create_dir_all(&target).expect("target dir");
    let link = fixture.join("latest");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).expect("create directory symlink");
    #[cfg(not(unix))]
    return;

    set_local_path_and_wait(&workspace, &mut cx, base.clone());
    workspace.update_in(&mut cx, |workspace, window, cx| {
        let tab = workspace.active_tab().expect("active tab");
        let index = tab
            .local
            .entries
            .iter()
            .position(|entry| entry.path.as_str() == link.to_string_lossy())
            .expect("link is listed");
        workspace.open_entry_at(PaneSide::Local, index, window, cx);
        // Leave the link's directory before the background stat completes.
        workspace.set_local_path(
            LocalPath::new(target.to_string_lossy().into_owned()),
            window,
            cx,
        );
    });
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test(),
            None,
            "a discarded local link result must clear its pending status"
        );
        let tab = workspace.state.tabs.active_tab().expect("tab must exist");
        assert_eq!(
            tab.local.path.as_ref().map(LocalPath::as_str),
            Some(target.to_string_lossy().as_ref()),
            "the pane stays where the user navigated, not the late link result"
        );
    });
}

#[gpui_kit::test]
fn stale_epoch_entry_resolution_is_dropped_by_the_state_guard(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        // Reconnect: the tab's epoch moves past the event's scope.
        workspace
            .active_tab_mut()
            .expect("active tab")
            .session_epoch = 2;
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                scope,
                RemoteEntryResolution {
                    path: RemotePath::new("/home/tester/latest"),
                    kind: FileKind::Directory,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: None,
                },
            )),
            window,
            cx,
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "a resolution from the superseded session must not navigate"
    );
}

#[gpui_kit::test]
fn opening_a_link_then_its_target_reuses_one_edit_session(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest.txt");
    let target_path = RemotePath::new("/home/tester/releases/2.txt");

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_edit_resolved(
            link_path.clone(),
            target_path.clone(),
            Some(2_048),
            Some(Timestamp::from_secs_since_epoch(100)),
            cx,
        );
    });
    let first = channels
        .command_rx
        .try_recv()
        .expect("opening the link starts a download");
    let AppCommand::StartTransfer(first) = first else {
        panic!("expected StartTransfer, got {first:?}");
    };
    assert_eq!(
        first.sources,
        vec![TransferEndpoint::Remote(target_path.clone())]
    );

    // Opening the real target afterwards must hit the same write-through
    // session, not create a second editor for one file.
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_edit_resolved(
            target_path.clone(),
            target_path.clone(),
            Some(2_048),
            Some(Timestamp::from_secs_since_epoch(100)),
            cx,
        );
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "the target must not start a second edit download"
    );
    workspace.read_with(&cx, |_workspace, cx| {
        let matching = cx
            .resources()
            .edit_sessions
            .sessions()
            .filter(|session| session.io_path == target_path)
            .count();
        assert_eq!(matching, 1, "one write-through session per target");
    });
}

#[gpui_kit::test]
fn repointed_link_does_not_create_a_second_session_on_the_same_path(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest.txt");
    let old_target = RemotePath::new("/home/tester/releases/1.txt");
    let new_target = RemotePath::new("/home/tester/releases/2.txt");

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_edit_resolved(link_path.clone(), old_target.clone(), Some(1_024), None, cx);
    });
    let _ = channels.command_rx.try_recv().expect("first edit download");

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.begin_edit_resolved(link_path.clone(), new_target.clone(), Some(2_048), None, cx);
    });
    assert!(
        channels.command_rx.try_recv().is_err(),
        "an open session bound to the old target must block a silent rebind"
    );
    workspace.read_with(&cx, |workspace, _cx| {
        assert!(
            workspace
                .status_message_for_test()
                .is_some_and(|message| message.contains("points somewhere else")),
            "the ambiguity must be explained, not silently resolved"
        );
    });
}

#[gpui_kit::test]
fn link_edit_conflict_compare_downloads_the_write_through_target(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest.txt");
    let target_path = RemotePath::new("/home/tester/releases/2.txt");
    let temp_dir = unique_temp_dir("link-compare");
    std::fs::create_dir_all(&temp_dir).expect("create compare dir");
    let temp_path = LocalPath::new(temp_dir.join("2.txt").to_string_lossy().to_string());
    std::fs::write(temp_path.as_str(), b"local").expect("write temp copy");

    let session_id = workspace.update_in(&mut cx, |_workspace, _window, cx| {
        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: link_path.clone(),
            io_path: target_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path.clone(),
            phase: EditPhase::RemoteConflict,
            remote_snapshot: RemoteSnapshot {
                size: Some(5),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: true,
            local_baseline: None,
        });
        id
    });

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.compare_edit_conflict(session_id, cx);
    });
    let command = channels
        .command_rx
        .try_recv()
        .expect("compare dispatches a download");
    let AppCommand::StartTransfer(command) = command else {
        panic!("expected StartTransfer, got {command:?}");
    };
    assert_eq!(
        command.sources,
        vec![TransferEndpoint::Remote(target_path)],
        "the comparison must fetch the file the link points at"
    );
}

#[gpui_kit::test]
fn refresh_does_not_resync_a_write_through_session_baseline(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest.txt");
    let target_path = RemotePath::new("/home/tester/releases/2.txt");
    let temp_dir = unique_temp_dir("link-resync");
    std::fs::create_dir_all(&temp_dir).expect("create resync dir");
    let temp_path = LocalPath::new(temp_dir.join("2.txt").to_string_lossy().to_string());
    std::fs::write(temp_path.as_str(), b"local").expect("write temp copy");

    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: link_path.clone(),
            io_path: target_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path.clone(),
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(100),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
    });

    let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        // The listing reports the link's own lstat values (small, different
        // mtime). They must not overwrite the target's baseline.
        workspace.active_tab_mut().expect("active tab").remote.path =
            Some(RemotePath::new(TEST_REMOTE_ROOT));
        workspace.handle_app_event(
            AppEvent::RemoteDirLoaded(RemoteScoped::new(
                scope,
                RemoteDirSnapshot {
                    path: RemotePath::new(TEST_REMOTE_ROOT),
                    entries: vec![RemoteEntry {
                        name: "latest.txt".to_string(),
                        path: link_path.clone(),
                        kind: FileKind::Symlink,
                        size: Some(12),
                        permissions: Some(0o777),
                        modified_at: Some(Timestamp::from_secs_since_epoch(999)),
                        link_target: None,
                    }],
                },
            )),
            window,
            cx,
        );
    });

    workspace.read_with(&cx, |_workspace, cx| {
        let session = cx
            .resources()
            .edit_sessions
            .find_active(&manual_connection_key(), &target_path)
            .expect("session survives the refresh");
        assert_eq!(
            session.remote_snapshot,
            RemoteSnapshot {
                size: Some(100),
                modified_at: Some(Timestamp::from_secs_since_epoch(100)),
            },
            "a link's own lstat values must not replace the target baseline"
        );
    });
}

#[gpui_kit::test]
fn entry_resolve_routing_failure_clears_its_pending_check(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest");
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.begin_link_check(TabId(1), EntryPath::Remote(link_path.clone()), Some(1), cx);
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolveFailed(macsftp_core::RemoteEntryResolveFailure {
                tab_id: TabId(1),
                session_epoch: 1,
                path: link_path.clone(),
                error: UserFacingError::new(
                    ErrorCode::ChannelClosed,
                    "Open Item Failed",
                    "The remote session is busy. Try again.",
                ),
            }),
            window,
            cx,
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert!(
            workspace
                .status_message_for_test()
                .is_some_and(|message| message.contains("Open Item Failed")),
            "a routing failure must replace the pending “Checking the link…” state"
        );
        assert!(
            !workspace.link_check_is_current(
                TabId(1),
                &EntryPath::Remote(link_path.clone()),
                Some(1)
            ),
            "the pending check must be cleared by its own failure"
        );
    });
}

#[gpui_kit::test]
fn same_path_failure_from_a_superseded_epoch_keeps_the_newer_check(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/latest");
    workspace.update_in(&mut cx, |workspace, window, cx| {
        // The user reopened the same path after a reconnect; the pending check
        // belongs to the new epoch.
        workspace.begin_link_check(TabId(1), EntryPath::Remote(link_path.clone()), Some(2), cx);
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolveFailed(macsftp_core::RemoteEntryResolveFailure {
                tab_id: TabId(1),
                session_epoch: 2,
                path: link_path.clone(),
                error: UserFacingError::new(ErrorCode::ChannelClosed, "Open Item Failed", "new"),
            }),
            window,
            cx,
        );
    });
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.begin_link_check(TabId(1), EntryPath::Remote(link_path.clone()), Some(3), cx);
        // A late failure from the superseded epoch names the same path.
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolveFailed(macsftp_core::RemoteEntryResolveFailure {
                tab_id: TabId(1),
                session_epoch: 2,
                path: link_path.clone(),
                error: UserFacingError::new(
                    ErrorCode::ChannelClosed,
                    "Open Item Failed",
                    "stale same-path failure",
                ),
            }),
            window,
            cx,
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Checking the link…"),
            "a same-path failure from another epoch must not clear the newer check"
        );
    });
}

#[gpui_kit::test]
fn unresolved_remote_link_reports_an_error_without_moving_the_pane(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    let link_path = RemotePath::new("/home/tester/broken");
    let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::RemoteEntryResolved(RemoteScoped::new(
                scope,
                RemoteEntryResolution {
                    path: link_path.clone(),
                    kind: FileKind::Symlink,
                    target: None,
                    size: None,
                    modified_at: None,
                    error: Some(UserFacingError::new(
                        ErrorCode::NotFound,
                        "Server Link Unavailable",
                        "The link target does not exist.",
                    )),
                },
            )),
            window,
            cx,
        );
    });

    workspace.read_with(&cx, |workspace, _| {
        assert!(
            workspace
                .status_message
                .as_ref()
                .is_some_and(|message| message.contains("Server Link Unavailable")),
            "the failure must be visible in the status bar"
        );
        let tab = workspace.state.tabs.active_tab().expect("tab must exist");
        assert_eq!(
            tab.remote.path.as_ref(),
            Some(&RemotePath::new(TEST_REMOTE_ROOT)),
            "a broken link must not navigate the pane"
        );
    });
    assert!(channels.command_rx.try_recv().is_err());
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn connect_component_inputs_support_clipboard_and_mixed_secret_focus(cx: &mut TestAppContext) {
    use crate::workspace::connect_form::ConnectField;
    let (workspace, mut cx, channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_connect_form(window, cx)
    });
    cx.simulate_input("server.example");
    cx.simulate_keystrokes("cmd-a cmd-c tab cmd-a cmd-v");
    workspace.read_with(&cx, |workspace, _| {
        let form = workspace
            .connect_form_ui
            .form
            .as_ref()
            .expect("Connect form must remain open");
        assert_eq!(form.host.value(), "server.example");
        assert_eq!(
            form.port.value(),
            "server.example",
            "clipboard paste must replace the selection in the focused field"
        );
        assert_eq!(form.focused_field, ConnectField::Port);
    });
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("2222");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("deploy");
    workspace.update(&mut cx, |_workspace, cx| cx.notify());
    cx.simulate_keystrokes("tab s e c r e t");
    workspace.read_with(&cx, |workspace, _| {
        let form = workspace
            .connect_form_ui
            .form
            .as_ref()
            .expect("Connect form must remain open");
        assert_eq!(form.focused_field, ConnectField::Password);
        assert_eq!(form.password.value(), "secret");
        assert!(
            !form.inputs.contains_key(&ConnectField::Password),
            "secrets must never enter a component input"
        );
        assert!(!form.inputs.contains_key(&ConnectField::Passphrase));
    });
    cx.simulate_keystrokes("shift-tab cmd-a");
    cx.simulate_input("new-user");
    cx.simulate_keystrokes("enter");
    let command = channels
        .command_rx
        .try_recv()
        .expect("Enter must submit the existing Connect action");
    let AppCommand::ConnectTab(connect) = command else {
        panic!("Connect form must submit ConnectTab");
    };
    assert_eq!(connect.settings.host, "server.example");
    assert_eq!(connect.settings.port, 2222);
    assert_eq!(connect.settings.username, "new-user");
}

#[gpui_kit::test]
fn profile_component_input_preserves_invalid_draft_and_clears_error_on_edit(
    cx: &mut TestAppContext,
) {
    use crate::workspace::profiles::{ProfileEditorField, SettingsSection};
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, cx| {
        workspace.surface = WorkspaceSurface::Settings;
        workspace.settings.section = SettingsSection::Profiles;
        workspace.start_new_profile(cx);
    });
    cx.simulate_input("example.com");
    cx.simulate_keystrokes("tab cmd-a");
    cx.simulate_input("99999");
    cx.simulate_keystrokes("enter");
    workspace.read_with(&cx, |workspace, cx| {
        let editor = workspace
            .settings
            .profile_editor
            .as_ref()
            .expect("invalid draft must remain editable");
        assert_eq!(editor.port.value(), "99999");
        assert_eq!(editor.focused_field, ProfileEditorField::Port);
        assert!(editor.error.is_some());
        assert!(
            cx.resources().profiles.profiles().is_empty(),
            "invalid forms must not persist a profile"
        );
    });
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("2200");
    workspace.read_with(&cx, |workspace, _| {
        let editor = workspace
            .settings
            .profile_editor
            .as_ref()
            .expect("draft remains open");
        assert_eq!(editor.port.value(), "2200");
        assert!(
            editor.error.is_none(),
            "editing an ordinary field must clear the stale form error"
        );
    });
}

#[gpui_kit::test]
fn ordinary_modal_input_keeps_selection_across_redraw_and_supports_undo(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_go_to_path(window, cx)
    });
    cx.simulate_input("/目录/path");
    cx.simulate_keystrokes("cmd-a");
    workspace.update(&mut cx, |_workspace, cx| cx.notify());
    cx.update(|_window, cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("/replacement".into()))
    });
    cx.simulate_keystrokes("cmd-v");
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.go_to_path.input.value(), "/replacement")
    });
    cx.simulate_keystrokes("cmd-z");
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.go_to_path.input.value(), "/目录/path")
    });
    cx.simulate_keystrokes("escape");
    workspace.read_with(&cx, |workspace, _| assert!(!workspace.go_to_path.open));
}

#[gpui_kit::test]
fn ordinary_input_preserves_ime_composition_across_parent_redraw(cx: &mut TestAppContext) {
    use super::TextInputTarget;
    use gpui_kit::EntityInputHandler as _;
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_go_to_path(window, cx)
    });
    let input = workspace.read_with(&cx, |workspace, _| {
        workspace
            .text_inputs
            .get(&TextInputTarget::GoToPath)
            .expect("open path dialog must own a component input")
            .state()
            .clone()
    });
    input.update_in(&mut cx, |input, window, cx| {
        input.replace_and_mark_text_in_range(None, "zhong", None, window, cx)
    });
    workspace.update(&mut cx, |_workspace, cx| cx.notify());
    input.update_in(&mut cx, |input, window, cx| {
        assert!(
            input.marked_text_range(window, cx).is_some(),
            "unrelated redraw must preserve the IME preedit range"
        );
        input.replace_text_in_range(None, "中文", window, cx);
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.go_to_path.input.value(), "中文")
    });
}

#[gpui_kit::test]
fn ordinary_input_theme_change_preserves_composition_and_focus(cx: &mut TestAppContext) {
    use gpui_kit::EntityInputHandler as _;
    use gpui_kit::Focusable as _;
    let (workspace, mut cx, _) = init_workspace(cx);
    cx.dispatch_action(OpenSettings);
    let input = workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.settings.section = SettingsSection::Editor;
        cx.notify();
        workspace.focus_external_editor(window, cx);
        workspace.settings.external_editor_input.clone()
    });
    input.update_in(&mut cx, |input, window, cx| {
        input.replace_and_mark_text_in_range(None, "zhong", None, window, cx);
    });
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.set_appearance(AppearancePreference::Light, window, cx);
    });
    input.update_in(&mut cx, |input, window, cx| {
        assert!(input.focus_handle(cx).is_focused(window));
        assert!(
            input.marked_text_range(window, cx).is_some(),
            "theme repaint must preserve preedit"
        );
        input.replace_text_in_range(None, "中文", window, cx);
    });
    workspace.read_with(&cx, |_workspace, cx| {
        assert_eq!(
            cx.resources().config.config().external_editor.as_deref(),
            Some("中文")
        );
    });
}

#[gpui_kit::test]
fn ordinary_input_readonly_and_disabled_survive_parent_redraw(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    cx.dispatch_action(OpenSettings);
    let input = workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.settings.section = SettingsSection::Editor;
        cx.notify();
        workspace.focus_external_editor(window, cx);
        workspace.settings.external_editor_input.clone()
    });
    cx.simulate_input("vim");
    input.update(&mut cx, |input, cx| input.set_readonly(true, cx));
    workspace.update(&mut cx, |_workspace, cx| cx.notify());
    cx.simulate_keystrokes("cmd-a cmd-c backspace");
    cx.simulate_input("replacement");
    cx.update(|_window, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("vim".into()),
            "read-only fields remain copyable"
        );
    });
    input.read_with(&cx, |input, _| assert_eq!(input.value(), "vim"));
    input.update(&mut cx, |input, cx| {
        input.set_readonly(false, cx);
        input.set_disabled(true, cx);
    });
    workspace.update(&mut cx, |_workspace, cx| cx.notify());
    cx.simulate_keystrokes("backspace cmd-v");
    cx.simulate_input("replacement");
    input.read_with(&cx, |input, _| {
        assert!(
            input.presentation().is_disabled(),
            "rendering must not reset disabled state"
        );
        assert_eq!(input.value(), "vim");
    });
    input.update(&mut cx, |input, cx| input.set_disabled(false, cx));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("code");
    workspace.read_with(&cx, |_workspace, cx| {
        assert_eq!(
            cx.resources().config.config().external_editor.as_deref(),
            Some("code")
        );
    });
}

#[gpui_kit::test]
fn replaced_connect_form_ignores_changes_from_its_old_component(cx: &mut TestAppContext) {
    use crate::workspace::connect_form::{ConnectField, ConnectForm};
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_connect_form(window, cx)
    });
    let old_input = workspace.read_with(&cx, |workspace, _| {
        workspace
            .connect_form_ui
            .form
            .as_ref()
            .expect("Connect form must be open")
            .inputs
            .get(&ConnectField::Host)
            .expect("host field must own a component input")
            .state()
            .clone()
    });
    workspace.update(&mut cx, |workspace, cx| {
        let mut replacement = ConnectForm::empty();
        replacement.host.set_value("replacement.example");
        workspace.connect_form_ui.form = Some(replacement);
        cx.notify();
    });
    old_input.update_in(&mut cx, |input, window, cx| {
        input.set_value("stale.example", window, cx);
        cx.emit(gpui_kit::base::input::InputEvent::Change);
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace
                .connect_form_ui
                .form
                .as_ref()
                .expect("replacement form must remain open")
                .host
                .value(),
            "replacement.example"
        )
    });
}

#[gpui_kit::test]
fn open_connect_form_resets_profile_picker(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        ws.open_connect_form(window, cx);
        let form = ws.connect_form_ui.form.as_mut().expect("form opens");
        form.profile_picker_open = true;
        form.profile_picker_filter.set_value("partial");
        ws.close_connect_form(window, cx);
        ws.open_connect_form(window, cx);
        let form = ws.connect_form_ui.form.as_ref().expect("form reopens");
        assert!(
            !form.profile_picker_open,
            "reopening connect form must reset profile_picker_open"
        );
        assert!(
            form.profile_picker_filter.value().is_empty(),
            "reopening connect form must clear profile_picker_filter"
        );
    });
}

#[gpui_kit::test]
fn connect_picker_select_profile_prefills_form(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);

    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_connect_form(window, cx);
    });
    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        save_settings_fixture(
            cx,
            ProfileId(1),
            "Work",
            &ConnectionSettings {
                host: "example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "hunter2".into(),
                },
                route: macsftp_core::ResolvedConnectionRoute::Direct,
            },
        );
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

#[gpui_kit::test]
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
        save_profile_fixture(cx, work);
        save_profile_fixture(cx, home);

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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
        save_profile_fixture(cx, work);
        save_profile_fixture(cx, home);
        let settings = ConnectionSettings {
            host: "work.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "secret".into(),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, ProfileId(1), "Work", &settings);

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

#[gpui_kit::test]
fn editing_target_on_profile_form_clears_saved_password_and_detaches_profile(
    cx: &mut TestAppContext,
) {
    use gpui_kit::EntityInputHandler as _;
    let (workspace, mut cx, channels) = init_workspace(cx);

    let host_input = workspace.update_in(&mut cx, |workspace, window, cx| {
        let work = macsftp_core::ConnectionProfile::new(
            ProfileId(1),
            "Work",
            "work.example.com",
            "alex",
            AuthMethod::Password {
                secret_ref: macsftp_core::SecretRef::keychain_ref(ProfileId(1), "password"),
            },
        );
        save_profile_fixture(cx, work);
        let settings = ConnectionSettings {
            host: "work.example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "work-super-secret".into(),
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };
        save_settings_fixture(cx, ProfileId(1), "Work", &settings);

        workspace.open_connect_form(window, cx);
        workspace.select_connect_profile(ProfileId(1), cx);
        workspace.prepare_connect_inputs(window, cx);

        let form = workspace.connect_form_ui.form.as_ref().expect("form");
        assert_eq!(form.source_profile_id, Some(ProfileId(1)));
        assert_eq!(form.password.value(), "work-super-secret");

        form.inputs
            .get(&crate::workspace::connect_form::ConnectField::Host)
            .expect("host input")
            .state()
            .clone()
    });

    // Edit the Host field to a different server
    host_input.update_in(&mut cx, |input, window, cx| {
        let len = input.text().len();
        input.replace_text_in_range(Some(0..len), "evil.example.com", window, cx);
    });

    workspace.update_in(&mut cx, |workspace, window, cx| {
        // The form must have cleared the password and detached from the profile
        let form = workspace.connect_form_ui.form.as_ref().expect("form");
        assert_eq!(
            form.source_profile_id, None,
            "editing host must detach from profile"
        );
        assert!(
            form.password.value().is_empty(),
            "editing host must clear the saved password so it never travels to a different server"
        );

        workspace.submit_connect_form(window, cx);
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("submit must send ConnectTab");
    match command {
        macsftp_core::AppCommand::ConnectTab(cmd) => {
            assert_eq!(cmd.settings.host, "evil.example.com");
            assert_eq!(cmd.profile_id, ProfileId(0), "must not link to profile 1");
            assert!(
                matches!(
                    cmd.pool_identity,
                    macsftp_core::ConnectionPoolIdentity::Ephemeral(_)
                ),
                "must use ephemeral pool identity"
            );
            match &cmd.settings.auth {
                AuthCredential::Password { password } => {
                    assert_ne!(
                        password.as_str(),
                        "work-super-secret",
                        "must never send saved password to changed host"
                    );
                }
                _ => panic!("expected password auth"),
            }
        }
        _ => panic!("expected ConnectTab"),
    }
}

#[gpui_kit::test]
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

#[gpui_kit::test]
fn connect_manual_entry_clears_profile_link_and_secrets(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);

    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_connect_form(window, cx);
    });
    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        save_settings_fixture(
            cx,
            ProfileId(1),
            "Work",
            &ConnectionSettings {
                host: "example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: AuthCredential::Password {
                    password: "hunter2".into(),
                },
                route: macsftp_core::ResolvedConnectionRoute::Direct,
            },
        );
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

#[gpui_kit::test]
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

#[test]
fn connect_form_builds_interactive_and_agent_credentials() {
    let mut form = ConnectForm::empty();
    form.host.set_value("server.example");
    form.username.set_value("alex");
    form.set_auth_method(AuthMethodKind::KeyboardInteractive);
    let interactive = form
        .build_settings()
        .expect("keyboard-interactive requires no stored secret");
    assert!(matches!(
        interactive.auth,
        AuthCredential::KeyboardInteractive
    ));

    form.set_auth_method(AuthMethodKind::SshAgent);
    form.agent_socket.set_value("/tmp/test-agent.sock");
    let agent = form
        .build_settings()
        .expect("agent socket is optional but valid when provided");
    assert!(matches!(
        agent.auth,
        AuthCredential::SshAgent {
            socket_path: Some(ref path)
        } if path == "/tmp/test-agent.sock"
    ));
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn window_release_notifies_runtime_for_all_remaining_tabs(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    cx.cx.update(|cx| {
        cx.on_window_closed(|cx, _| crate::session_coordinator::checkpoint_after_window_closed(cx))
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

#[gpui_kit::test]
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
            AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot { plan, root_job })),
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
            AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot { plan, root_job })),
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

#[gpui_kit::test]
fn local_directory_symlink_navigates_into_its_target(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("symlink-dir");
    let target = fixture.join("releases").join("2");
    std::fs::create_dir_all(&target).expect("target dir");
    std::fs::write(target.join("note.txt"), b"hi").expect("target file");
    let link = fixture.join("latest");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).expect("create directory symlink");
    #[cfg(not(unix))]
    return;

    set_local_path_and_wait(&workspace, &mut cx, base);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        let tab = workspace.active_tab().expect("active tab");
        let index = tab
            .local
            .entries
            .iter()
            .position(|entry| entry.kind == FileKind::Symlink)
            .expect("the listing keeps the link typed as a symlink");
        workspace.open_entry_at(PaneSide::Local, index, window, cx);
    });
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, _| {
        let tab = workspace.state.tabs.active_tab().expect("tab must exist");
        assert_eq!(
            tab.local.path.as_ref().map(LocalPath::as_str),
            Some(link.to_string_lossy().as_ref()),
            "the pane enters the link by its own path, not the target's"
        );
    });
}

#[gpui_kit::test]
fn local_file_symlink_is_not_opened_as_a_folder(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("symlink-file");
    std::fs::write(fixture.join("note.txt"), b"hi").expect("target file");
    let link = fixture.join("latest.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(fixture.join("note.txt"), &link).expect("create file symlink");
    #[cfg(not(unix))]
    return;

    install_system_path_seams();
    OPENED_SYSTEM_PATHS.with(|paths| paths.borrow_mut().clear());

    set_local_path_and_wait(&workspace, &mut cx, base.clone());
    workspace.update_in(&mut cx, |workspace, window, cx| {
        let tab = workspace.active_tab().expect("active tab");
        let index = tab
            .local
            .entries
            .iter()
            .position(|entry| entry.path.as_str() == link.to_string_lossy())
            .expect("link is listed");
        workspace.open_entry_at(PaneSide::Local, index, window, cx);
    });
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, _| {
        let tab = workspace.state.tabs.active_tab().expect("tab must exist");
        assert_eq!(
            tab.local.path.as_ref(),
            Some(&base),
            "a file link must not navigate"
        );
    });
    OPENED_SYSTEM_PATHS.with(|paths| {
        assert_eq!(
            *paths.borrow(),
            vec![link.to_string_lossy().to_string()],
            "a link to a file opens the file itself, following the link"
        );
    });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
fn explicit_filter_reduces_visible_local_entries(cx: &mut TestAppContext) {
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

/// Type one printable key through the real key path (`key_char` is set the
/// way the platform sets it for letters).
fn type_letter(cx: &mut VisualTestContext, key: &str) {
    let mut keystroke = gpui_kit::Keystroke::parse(key).expect("printable key parses");
    keystroke.key_char = Some(key.to_string());
    cx.simulate_event(gpui_kit::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui_kit::KeyUpEvent { keystroke });
}

fn selected_local_name(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> String {
    workspace.read_with(cx, |workspace, cx| {
        let index = workspace
            .selected_index(PaneSide::Local, cx)
            .expect("a local row is selected");
        match workspace
            .entry_path_at(PaneSide::Local, index, cx)
            .expect("the selected row maps to a path")
        {
            EntryPath::Local(path) => path
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
            EntryPath::Remote(_) => panic!("local path expected"),
        }
    })
}

fn type_select_prefix(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    typed: &str,
    now: Instant,
) {
    workspace.update_in(cx, |workspace, _window, cx| {
        workspace.extend_type_select(PaneSide::Local, typed, now, cx);
    });
}

#[gpui_kit::test]
fn typing_letters_jumps_selection_without_filtering(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("type-select-keys");
    std::fs::write(fixture.join("alpha.txt"), b"a").expect("write alpha");
    std::fs::write(fixture.join("beta.txt"), b"b").expect("write beta");
    std::fs::write(fixture.join("brand.txt"), b"c").expect("write brand");

    set_local_path_and_wait(&workspace, &mut cx, base);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.focus_pane(PaneSide::Local, window, cx);
    });

    type_letter(&mut cx, "b");

    assert_eq!(
        selected_local_name(&workspace, &cx),
        "beta.txt",
        "a typed letter selects the first matching entry"
    );
    workspace.read_with(&cx, |workspace, cx| {
        assert_eq!(
            workspace.entry_count(PaneSide::Local, cx),
            3,
            "type-select must not narrow the list"
        );
        assert!(
            workspace.filter_query(PaneSide::Local).is_empty(),
            "type-select must not fill the filter field"
        );
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("file-row", 1usize)).selected(),
            Some(true),
            "the jumped row reports its selection to accessibility"
        );
    });

    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn type_select_accumulates_the_prefix_and_cycles_matches(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("type-select-prefix");
    std::fs::write(fixture.join("alpha.txt"), b"a").expect("write alpha");
    std::fs::write(fixture.join("beta.txt"), b"b").expect("write beta");
    std::fs::write(fixture.join("brand.txt"), b"c").expect("write brand");

    set_local_path_and_wait(&workspace, &mut cx, base);
    let start = Instant::now();

    type_select_prefix(&workspace, &mut cx, "b", start);
    assert_eq!(selected_local_name(&workspace, &cx), "beta.txt");

    // "br" continues the same session while the keystrokes stay close.
    type_select_prefix(&workspace, &mut cx, "r", start + Duration::from_millis(50));
    assert_eq!(
        selected_local_name(&workspace, &cx),
        "brand.txt",
        "letters typed in quick succession extend one prefix"
    );

    // A pause starts a new prefix; the same letter then scans past the
    // current row to the next match.
    type_select_prefix(
        &workspace,
        &mut cx,
        "b",
        start + Duration::from_millis(2000),
    );
    assert_eq!(
        selected_local_name(&workspace, &cx),
        "beta.txt",
        "a stale prefix restarts and scans past the selected row"
    );

    // Nothing matches "bz", so the session falls back to the typed letter.
    type_select_prefix(
        &workspace,
        &mut cx,
        "z",
        start + Duration::from_millis(2100),
    );
    assert_eq!(
        selected_local_name(&workspace, &cx),
        "beta.txt",
        "unmatched prefixes leave the selection alone"
    );
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.pane_type_select(PaneSide::Local).prefix(),
            "z",
            "an unmatched prefix restarts on the character just typed"
        );
    });

    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn file_list_rows_header_and_status_bar_expose_a11y_roles(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("a11y-roles");
    std::fs::write(fixture.join("alpha.txt"), b"a").expect("write alpha");
    std::fs::create_dir(fixture.join("docs")).expect("create docs");

    set_local_path_and_wait(&workspace, &mut cx, base);
    cx.update(|window, cx| {
        window.render_frame(cx);

        let folder = window.find(("file-row", 0usize));
        let folder_label = folder.label().expect("folder rows expose a label");
        assert!(
            folder_label.contains("docs") && folder_label.contains(", folder"),
            "folders sort first and read as folders: {folder_label}"
        );

        let row = window.find(("file-row", 1usize));
        assert_eq!(row.role(), Some(gpui_kit::Role::ListItem));
        let row_label = row.label().expect("rows expose a label");
        assert!(
            row_label.contains("alpha.txt") && row_label.contains(", file"),
            "row label names the entry and its kind: {row_label}"
        );

        let list = window.find("local-file-list");
        assert_eq!(list.role(), Some(gpui_kit::Role::List));
        assert_eq!(list.label(), Some("Local files"));

        let header = window.within("local-file-pane").find("sort-name");
        assert_eq!(header.role(), Some(gpui_kit::Role::Button));
        let header_label = header.label().expect("sort columns expose a label");
        assert!(
            header_label.starts_with("Sort by Name"),
            "sort column label: {header_label}"
        );

        let status = window.find("status-bar");
        assert_eq!(status.role(), Some(gpui_kit::Role::Status));
        let status_label = status.label().expect("the status bar exposes a label");
        assert!(
            status_label.contains("Connected"),
            "status label carries the connection state: {status_label}"
        );
    });

    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn local_directory_respects_tab_sort_by_size(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("macsftp-sort-{}-{}", std::process::id(), seq));
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

#[gpui_kit::test]
fn sort_actions_set_field_and_direction_explicitly(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    // The default sort is Name ascending. An explicit command is a no-op
    // there, where the column header's click-to-toggle would flip to Z→A.
    cx.dispatch_action(SortByNameAscending);
    cx.run_until_parked();
    workspace.read_with(&cx, |workspace, _| {
        let sort = &workspace.active_tab().unwrap().sort;
        assert_eq!(sort.field, FileSortField::Name);
        assert_eq!(sort.direction, SortDirection::Ascending);
    });

    let (fixture, base) = temp_local_fixture("sort-actions");
    std::fs::write(fixture.join("alpha.txt"), b"a").unwrap();
    std::fs::write(fixture.join("beta.txt"), b"12345").unwrap();
    cx.dispatch_action(SortBySizeDescending);
    cx.run_until_parked();
    set_local_path_and_wait(&workspace, &mut cx, base);
    workspace.read_with(&cx, |workspace, _| {
        let sort = &workspace.active_tab().unwrap().sort;
        assert_eq!(sort.field, FileSortField::Size);
        assert_eq!(sort.direction, SortDirection::Descending);
        let names: Vec<_> = workspace
            .active_tab()
            .unwrap()
            .local
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["beta.txt", "alpha.txt"], "largest first");
    });

    // Dispatching again re-sorts the listing in place, not just the state.
    cx.dispatch_action(SortBySizeAscending);
    cx.run_until_parked();
    workspace.read_with(&cx, |workspace, _| {
        let names: Vec<_> = workspace
            .active_tab()
            .unwrap()
            .local
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["alpha.txt", "beta.txt"], "smallest first");
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn window_title_follows_the_active_tab(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    cx.dispatch_action(NewTab);
    cx.run_until_parked();
    workspace.update(&mut cx, |workspace, _| {
        // A fresh tab is named for what it is; a connected one carries the
        // connection name. The native title (Mission Control, the Window
        // menu) is the same string the tab bar shows, and the app name stands
        // in while no tab is open (§12.7).
        assert_eq!(workspace.active_tab().unwrap().title, "New Connection");
        assert_eq!(workspace.window_title(), "New Connection");

        workspace.active_tab_mut().unwrap().title = "example.com".to_string();
        assert_eq!(workspace.window_title(), "example.com");

        let first_tab = workspace.state.tabs.tabs[0].id;
        workspace.state.tabs.active_tab_id = Some(first_tab);
        assert_eq!(workspace.window_title(), "New Connection");

        workspace.state.tabs.active_tab_id = None;
        assert_eq!(workspace.window_title(), crate::APP_NAME);
    });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
                    "The item couldn’t be deleted.",
                    "You don’t have permission.",
                ),
            },
            window,
            cx,
        );
        assert!(
            workspace
                .status_message
                .as_ref()
                .is_some_and(|m| m.contains("The item couldn’t be deleted."))
        );
    });
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
            io_path: RemotePath::new("/srv/doc.txt"),
            tab_id: TabId(2),
            session_epoch: 1,
            profile_id: ProfileId(1),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path.clone(),
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(6),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
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

#[gpui_kit::test]
fn window_closed_callback_reaps_orphaned_edit_session(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    let app = cx.cx.clone();
    cx.update(|_window, cx| {
        cx.on_window_closed(|cx, _| crate::workspace::cleanup_orphaned_edit_sessions(cx))
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
            io_path: RemotePath::new("/srv/doc.txt"),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(1),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path,
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(6),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
fn show_in_finder_without_a_local_selection_says_so(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("show-in-finder");
    set_local_path_and_wait(&workspace, &mut cx, base);

    workspace.update_in(&mut cx, |ws, window, cx| {
        assert!(
            ws.selected_index(PaneSide::Local, cx).is_none(),
            "the fixture folder is empty"
        );
        ws.open_command_palette(window, cx);
        ws.palette.input.set_value("show in finder");
        ws.palette.selected = 0;
        ws.execute_palette_selected(window, cx);
    });
    workspace.read_with(&cx, |ws, _| {
        assert_eq!(
            ws.status_message_for_test().as_deref(),
            Some("Select one file or folder to show in Finder.")
        );
    });

    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
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

#[gpui_kit::test]
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

#[gpui_kit::test]
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
            route: macsftp_core::ResolvedConnectionRoute::Direct,
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
#[gpui_kit::test]
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
        let detail = crate::rate_sampler::format_running_detail(1_000_000, Some(2_000_000), &snap);
        assert!(
            detail.contains("MB/s") || detail.contains("KB/s"),
            "detail should include speed: {detail}"
        );
        assert!(
            detail.contains("Time Remaining"),
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
    assert_eq!(STATUS_BUSY_TRY_AGAIN, "Busy. Try again in a moment.");
    assert_eq!(
        STATUS_CONNECTION_SERVICE_UNAVAILABLE,
        "The connection service is unavailable."
    );
}

/// Regenerates `docs/visual-evidence/workspace-ux` from this checkout through
/// gpui's headless Metal renderer: the real scene, text system and rasterizer,
/// but no on-screen window, so captures are deterministic and do not need
/// screen-recording permission.
///
/// Ignored by default because it writes repository files. Run it explicitly:
/// `cargo test -p macsftp-app regenerate_workspace_ux_evidence -- --ignored --nocapture`.
/// It writes PNGs (the renderer hands back RGBA); convert them to the committed
/// JPEGs with `sips -s format jpeg` as the evidence README describes.
#[test]
#[ignore = "writes docs/visual-evidence/workspace-ux; run explicitly with -- --ignored"]
fn regenerate_workspace_ux_evidence() {
    use gpui_kit::HeadlessAppContext;

    let output_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/visual-evidence/workspace-ux");
    std::fs::create_dir_all(&output_dir).expect("evidence directory must be writable");

    let app_paths = temp_app_paths();
    // A short fixed path keeps the breadcrumb trail fully readable at a 360 pt
    // pane; the capture asserts the folder is empty before using it.
    let fixture = std::path::PathBuf::from("/tmp/test");
    std::fs::create_dir_all(&fixture).expect("empty local fixture must be creatable");
    assert!(
        std::fs::read_dir(&fixture)
            .expect("local fixture must be readable")
            .next()
            .is_none(),
        "/tmp/test must be an empty folder for the capture; delete its contents"
    );

    let config = macsftp_storage::ConfigStore::with_defaults(app_paths.config_file.clone());
    let mut session_coordinator =
        SessionCoordinator::new(SessionStore::open_or_empty(app_paths.session_file.clone()));
    let window_session_id = session_coordinator.allocate_window_id();
    session_coordinator.register_window(window_session_id);

    // `MacPlatform` may only be built on the main thread; its text system may
    // not. Building it directly gives the same `font-kit` text system the app
    // runs with (gpui-kit enables that feature for shipped builds).
    let mut cx = HeadlessAppContext::with_platform(
        std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
        std::sync::Arc::new(crate::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        // The evidence shows settled surfaces. Motion is sampled per frame, so
        // a headless capture would otherwise catch a fade part way through.
        cx.set_reduce_motion(true);
        Theme::one_light().install(cx);
        app_actions::init(cx);
        cx.set_global(crate::resources::AppResources::load_for_test(
            app_paths, config,
        ));
        cx.set_global(session_coordinator);
        cx.set_global(crate::resources::SharedTransfers::default());
    });

    let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
    let client = RuntimeClient::new(channels.command_tx.clone());
    let mut workspace_slot: Option<Entity<Workspace>> = None;
    let (window, _) = cx
        .update(|cx| {
            gpui_kit::open_window(
                gpui_kit::WindowOptions {
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: Default::default(),
                        size: size(px(720.0), px(480.0)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let workspace =
                        cx.new(|cx| Workspace::new(client, window_session_id, None, window, cx));
                    workspace_slot = Some(workspace.clone());
                    cx.new(|cx| gpui_kit::base::Root::new(workspace, window, cx))
                },
            )
        })
        .expect("headless window must open");
    let workspace = workspace_slot.expect("root builder must capture the workspace entity");

    let capture = |cx: &mut HeadlessAppContext, name: &str| {
        cx.update_window(window, |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .expect("frame draw must succeed");
        let path = output_dir.join(name);
        cx.capture_screenshot(window)
            .expect("headless Metal renderer must produce an image")
            .save(&path)
            .expect("evidence image must be saved as PNG");
        println!("wrote {}", path.display());
    };

    // Capture 1 (light): fresh window, no recents, centered first-connection card.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_appearance(AppearancePreference::Light, window, cx);
                workspace.set_local_path(
                    LocalPath::new(fixture.to_string_lossy().into_owned()),
                    window,
                    cx,
                );
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    cx.run_until_parked();
    capture(&mut cx, "light-720x480.png");

    // Capture 2 (dark): the connection entry carries keyboard focus, reached by
    // Tab traversal the way a keyboard user gets there. The focus ring is the
    // only element painting a half-alpha border, so finding that quad in the
    // painted scene proves the entry is focused.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_appearance(AppearancePreference::Dark, window, cx);
            })
            .expect("workspace must be live");
    });
    cx.update_window(window, |_, window, cx| {
        let focus_ring = gpui_kit::Hsla {
            a: 0.5,
            ..cx.global::<Theme>().colors.border_focused
        };
        let mut focused = false;
        for _ in 0..24 {
            window.dispatch_keystroke(
                gpui_kit::Keystroke::parse("tab").expect("tab must be a valid keystroke"),
                cx,
            );
            window.draw(cx).clear(cx);
            if window
                .painted_quads()
                .iter()
                .any(|quad| quad.border_color == focus_ring)
            {
                focused = true;
                break;
            }
        }
        assert!(
            focused,
            "Tab traversal must focus the connection entry: no focus ring in the painted scene"
        );
    })
    .expect("window update must succeed");
    capture(&mut cx, "dark-720x480.png");

    // Capture 3 (light): six recent connections, scrolled to the last row.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_appearance(AppearancePreference::Light, window, cx);
            })
            .expect("workspace must be live");
        workspace.update(cx, |_workspace, cx| {
            for index in 1..=6 {
                cx.resources_mut()
                    .recents
                    .upsert(macsftp_storage::RecentEntryInput {
                        host: format!("demo-{index}.example.com"),
                        port: 22,
                        username: "demo".into(),
                        profile_id: None,
                        display_name: Some(format!("Demo Server {index}")),
                        last_remote_path: None,
                        last_connected_at: index,
                    })
                    .expect("seed recent connection fixture");
            }
            cx.notify();
        });
    });
    cx.update_window(window, |_, window, cx| {
        window.blur(cx);
        for _ in 0..4 {
            window.dispatch_event(
                gpui_kit::PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: point(px(540.0), px(180.0)),
                    delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
                    ..Default::default()
                }),
                cx,
            );
        }
    })
    .expect("window update must succeed");
    cx.run_until_parked();
    capture(&mut cx, "light-recents-scroll-720x480.png");

    // Capture 4 (light): a classified connection failure. The remote pane leads
    // with the error title and keeps its message on a second line, next to the
    // single-line empty state in the local pane.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, _window, cx| {
                let tab = workspace
                    .active_tab_mut()
                    .expect("the capture window has a tab");
                tab.connection = ConnectionState::Failed {
                    error: macsftp_core::UserFacingError::new(
                        macsftp_core::ErrorCode::NetworkTimeout,
                        "The server stopped responding",
                        "No answer for 30 seconds while listing /var/www. \
                         The network may have dropped.",
                    ),
                };
                cx.notify();
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    capture(&mut cx, "light-failed-connection-720x480.png");

    // Capture 5 (light): the command palette over the ordinary workspace — the
    // scrim token and the popover step of the elevation scale.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                let tab = workspace
                    .active_tab_mut()
                    .expect("the capture window has a tab");
                tab.connection = ConnectionState::Empty;
                workspace.open_command_palette(window, cx);
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    capture(&mut cx, "light-palette-720x480.png");

    // Capture 6 (light): two tabs. The inactive tab keeps its close button
    // hidden until the pointer reaches it, while the active tab shows it.
    // A fresh tab opens the user's home folder, so pin it to the fixture:
    // otherwise the capture embeds this machine's home listing.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.cancel_active_modal(window, cx);
                workspace.open_new_tab(window, cx);
                workspace.set_local_path(
                    LocalPath::new(fixture.to_string_lossy().into_owned()),
                    window,
                    cx,
                );
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    cx.run_until_parked();
    capture(&mut cx, "light-two-tabs-720x480.png");

    // Capture 7 (light): Finder-style type-select. Typing "b" jumps the
    // selection to the first matching name and leaves the rest of the list
    // visible — the filter bar stays empty, unlike the old type-to-filter.
    //
    // The capture must read the same on every run, so this folder has a fixed
    // path instead of a per-run temp directory. It is the test's own fixture:
    // it only writes into an empty folder (creating it when missing and leaving
    // it empty again), so it can neither overwrite nor delete an entry it did
    // not create.
    const TYPE_SELECT_FIXTURE: &str = "/tmp/macsftp-type-select";
    const TYPE_SELECT_ENTRIES: [&str; 5] = [
        "alpha.txt",
        "beta-report.pdf",
        "brand-audit.log",
        "charlie.txt",
        "docs",
    ];
    let type_select_fixture = std::path::PathBuf::from(TYPE_SELECT_FIXTURE);
    std::fs::create_dir_all(&type_select_fixture).expect("type-select fixture must be creatable");
    let leftovers: Vec<String> = std::fs::read_dir(&type_select_fixture)
        .expect("fixture must be readable")
        .map(|entry| {
            entry
                .expect("fixture entries must be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "{TYPE_SELECT_FIXTURE} is not empty ({leftovers:?}); the capture writes only into an \
         empty folder and leaves it empty, so move its contents away first"
    );
    for name in TYPE_SELECT_ENTRIES {
        let path = type_select_fixture.join(name);
        if name == "docs" {
            std::fs::create_dir_all(&path).expect("fixture folder");
        } else {
            std::fs::write(&path, b"evidence").expect("fixture file");
        }
    }
    // The listing carries a "Last Modified" column at minute precision and the
    // entries are written fresh on every run, so their real times would change
    // the pixels from run to run and break the byte-identical rerun guarantee.
    // Pin them to one instant (the column renders a UTC civil time); the folder
    // is included because writing the entries inside it bumps its time.
    let fixed_mtime =
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_767_323_045); // 2026-01-02 03:04:05 UTC
    for name in TYPE_SELECT_ENTRIES {
        let path = type_select_fixture.join(name);
        let handle = if name == "docs" {
            std::fs::File::open(&path).expect("fixture folder must be openable")
        } else {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("fixture file must be openable for writing")
        };
        handle
            .set_times(std::fs::FileTimes::new().set_modified(fixed_mtime))
            .expect("fixture times must be settable");
    }
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_local_path(
                    LocalPath::new(type_select_fixture.to_string_lossy().into_owned()),
                    window,
                    cx,
                );
                workspace.focus_pane(PaneSide::Local, window, cx);
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        let mut keystroke = gpui_kit::Keystroke::parse("b").expect("keystroke parses");
        keystroke.key_char = Some("b".into());
        window.dispatch_keystroke(keystroke, cx);
    })
    .expect("window update must succeed");
    cx.run_until_parked();
    capture(&mut cx, "light-type-select-720x480.png");
    for name in TYPE_SELECT_ENTRIES {
        let path = type_select_fixture.join(name);
        if name == "docs" {
            std::fs::remove_dir_all(&path).expect("fixture folder removal");
        } else {
            std::fs::remove_file(&path).expect("fixture file removal");
        }
    }

    // Capture 8 (light): Settings → General. One surface, three sections, and
    // the immediate-effect switches that replaced the old dead-end prompts.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.surface = WorkspaceSurface::Settings;
                workspace.settings.section = SettingsSection::General;
                window.focus(&workspace.workspace_focus, cx);
                cx.notify();
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    capture(&mut cx, "light-settings-general-720x480.png");

    // Capture 9 (dark): Settings → Editor. The editor page owns the external
    // editor field and the association rules that used to crowd General.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_appearance(AppearancePreference::Dark, window, cx);
                workspace.settings.section = SettingsSection::Editor;
                window.focus(&workspace.workspace_focus, cx);
                cx.notify();
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    capture(&mut cx, "dark-settings-editor-720x480.png");

    // Capture 10 (light): Settings → Saved Connections. Two profiles give the
    // list/editor split real content; passwords stay in the in-memory fixture
    // keychain and never reach the rendered frame.
    cx.update(|cx| {
        workspace.update(cx, |_workspace, cx| {
            for (index, (name, host, username)) in [
                ("Production", "prod.example.com", "deploy"),
                ("Staging", "staging.example.com", "alex"),
            ]
            .into_iter()
            .enumerate()
            {
                cx.resources_mut()
                    .profiles
                    .save_request(ProfileSaveRequest {
                        profile_id: ProfileId(index as u64 + 1),
                        name: name.into(),
                        host: host.into(),
                        port: 22,
                        username: username.into(),
                        auth: ProfileAuthUpdate::Password {
                            password: Some("fixture-password".into()),
                        },
                        route: macsftp_core::ConnectionRoute::Direct,
                        default_remote_path: None,
                    })
                    .expect("seed profile fixture");
            }
        });
    });
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_appearance(AppearancePreference::Light, window, cx);
                workspace.set_settings_section(SettingsSection::Profiles, cx);
                window.focus(&workspace.workspace_focus, cx);
                cx.notify();
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    capture(&mut cx, "light-settings-saved-connections-720x480.png");

    // Capture 11 (dark): Settings → General. The switch track and knob tokens
    // must keep their contrast when the surface and controls both go dark.
    cx.update(|cx| {
        workspace
            .downgrade()
            .update_in(cx, |workspace, window, cx| {
                workspace.set_appearance(AppearancePreference::Dark, window, cx);
                workspace.settings.section = SettingsSection::General;
                window.focus(&workspace.workspace_focus, cx);
                cx.notify();
            })
            .expect("workspace must be live");
    });
    cx.run_until_parked();
    capture(&mut cx, "dark-settings-general-720x480.png");
}

/// Local pane with a real, sorted fixture listing; returns the fixture dir so
/// the caller can clean it up.
fn workspace_with_local_files(
    cx: &mut TestAppContext,
    label: &str,
    names: &[&str],
) -> (std::path::PathBuf, Entity<Workspace>, VisualTestContext) {
    let (fixture, base) = temp_local_fixture(label);
    for name in names {
        std::fs::write(fixture.join(name), b"x").expect("fixture file");
    }
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.simulate_resize(size(px(900.0), px(600.0)));
    set_local_path_and_wait(&workspace, &mut cx, base);
    (fixture, workspace, cx)
}

fn right_click_local_pane(cx: &mut VisualTestContext, position: gpui_kit::Point<gpui_kit::Pixels>) {
    cx.simulate_event(gpui_kit::MouseDownEvent {
        position,
        button: gpui_kit::MouseButton::Right,
        click_count: 1,
        ..Default::default()
    });
    cx.simulate_event(gpui_kit::MouseUpEvent {
        position,
        button: gpui_kit::MouseButton::Right,
        click_count: 1,
        ..Default::default()
    });
    cx.run_until_parked();
}

fn local_visible_entry(
    workspace: &Entity<Workspace>,
    cx: &VisualTestContext,
    visible_index: usize,
) -> EntryPath {
    workspace
        .read_with(cx, |workspace, cx| {
            workspace.entry_path_at(PaneSide::Local, visible_index, cx)
        })
        .unwrap_or_else(|| panic!("fixture lists a row at visible index {visible_index}"))
}

/// Right-click selects the row under the pointer before the menu opens, so
/// every menu command targets exactly what was clicked.
#[gpui_kit::test]
fn right_click_selects_the_row_under_the_pointer(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) =
        workspace_with_local_files(cx, "context-menu-row", &["a.txt", "b.txt", "c.txt"]);
    let row_height = workspace.read_with(&cx, |_workspace, cx| cx.theme().sizes.file_row_height);
    let expected = local_visible_entry(&workspace, &cx, 1);
    let viewport = cx
        .debug_bounds("local-list-container-viewport")
        .expect("local list viewport is laid out");

    right_click_local_pane(
        &mut cx,
        point(viewport.center().x, viewport.top() + row_height * 1.5),
    );

    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.focused_side, PaneSide::Local);
        let tab = workspace.active_tab().expect("active tab");
        assert_eq!(tab.selection.selected_paths, vec![expected]);
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

/// A right click collapses a multi-selection onto the clicked row; the menu
/// commands then act on that single row (Transmit semantics).
#[gpui_kit::test]
fn right_click_collapses_a_multi_selection_to_the_clicked_row(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) = workspace_with_local_files(
        cx,
        "context-menu-collapse",
        &["a.txt", "b.txt", "c.txt", "d.txt"],
    );
    cx.dispatch_action(SelectAllEntries);
    cx.run_until_parked();
    workspace.read_with(&cx, |workspace, _| {
        let tab = workspace.active_tab().expect("active tab");
        assert_eq!(tab.selection.selected_paths.len(), 4, "all rows selected");
    });

    let row_height = workspace.read_with(&cx, |_workspace, cx| cx.theme().sizes.file_row_height);
    let expected = local_visible_entry(&workspace, &cx, 2);
    let viewport = cx
        .debug_bounds("local-list-container-viewport")
        .expect("local list viewport is laid out");
    right_click_local_pane(
        &mut cx,
        point(viewport.center().x, viewport.top() + row_height * 2.5),
    );

    workspace.read_with(&cx, |workspace, _| {
        let tab = workspace.active_tab().expect("active tab");
        assert_eq!(tab.selection.selected_paths, vec![expected]);
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

/// A right click below the last row opens the blank-area menu without
/// disturbing the selection.
#[gpui_kit::test]
fn right_click_on_blank_space_keeps_the_selection(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) =
        workspace_with_local_files(cx, "context-menu-blank", &["a.txt", "b.txt"]);
    let expected = local_visible_entry(&workspace, &cx, 0);
    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.select_index(PaneSide::Local, 0, cx);
    });
    cx.run_until_parked();

    let viewport = cx
        .debug_bounds("local-list-container-viewport")
        .expect("local list viewport is laid out");
    right_click_local_pane(
        &mut cx,
        point(viewport.center().x, viewport.bottom() - px(8.0)),
    );

    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(workspace.focused_side, PaneSide::Local);
        let tab = workspace.active_tab().expect("active tab");
        assert_eq!(tab.selection.selected_paths, vec![expected]);
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn copy_path_copies_the_right_clicked_entry(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) =
        workspace_with_local_files(cx, "copy-path-entry", &["a.txt", "b.txt", "c.txt"]);
    let row_height = workspace.read_with(&cx, |_workspace, cx| cx.theme().sizes.file_row_height);
    let expected = local_visible_entry(&workspace, &cx, 1);
    let expected_text = match expected {
        EntryPath::Local(path) => path.as_str().to_string(),
        EntryPath::Remote(_) => panic!("local fixture must select a local path"),
    };
    let viewport = cx
        .debug_bounds("local-list-container-viewport")
        .expect("local list viewport is laid out");

    right_click_local_pane(
        &mut cx,
        point(viewport.center().x, viewport.top() + row_height * 1.5),
    );
    cx.dispatch_action(CopyPath);
    cx.run_until_parked();

    cx.update(|_window, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(expected_text.clone()),
            "Copy Path copies the entry's own path, not its folder"
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some(format!("Path copied: {expected_text}").as_str())
        );
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn copy_path_copies_a_multi_selection_one_path_per_line(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) =
        workspace_with_local_files(cx, "copy-path-multi", &["a.txt", "b.txt", "c.txt"]);
    cx.dispatch_action(SelectAllEntries);
    cx.run_until_parked();
    let expected: Vec<String> = (0..3)
        .map(|index| match local_visible_entry(&workspace, &cx, index) {
            EntryPath::Local(path) => path.as_str().to_string(),
            EntryPath::Remote(_) => panic!("local fixture must list local paths"),
        })
        .collect();

    cx.dispatch_action(CopyPath);
    cx.run_until_parked();

    cx.update(|_window, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(expected.join("\n")),
            "every selected entry contributes one clipboard line"
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("3 paths copied")
        );
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

/// With nothing selected the toolbar button and shortcut still have a useful
/// meaning: the path of the folder the pane is showing.
#[gpui_kit::test]
fn copy_path_falls_back_to_the_pane_folder_when_nothing_is_selected(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) =
        workspace_with_local_files(cx, "copy-path-empty", &["a.txt"]);
    workspace.read_with(&cx, |workspace, _| {
        assert!(
            workspace
                .active_tab()
                .expect("active tab")
                .selection
                .selected_paths
                .is_empty(),
            "a freshly listed folder has no selection"
        );
    });

    cx.dispatch_action(CopyPath);
    cx.run_until_parked();

    let folder = LocalPath::new(fixture.to_string_lossy().into_owned());
    cx.update(|_window, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(folder.as_str().to_string()),
            "an empty selection copies the current folder"
        );
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn copy_path_copies_a_remote_entry_even_while_disconnected(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    let scope = connect_and_land_on_root(&workspace, &mut cx, &channels);
    let remote_path = RemotePath::new(format!("{TEST_REMOTE_ROOT}/doc.txt"));
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.handle_app_event(
            AppEvent::RemoteDirLoaded(RemoteScoped::new(
                scope,
                RemoteDirSnapshot {
                    path: RemotePath::new(TEST_REMOTE_ROOT),
                    entries: vec![RemoteEntry {
                        name: "doc.txt".to_string(),
                        path: remote_path.clone(),
                        kind: FileKind::File,
                        size: Some(10),
                        permissions: Some(0o644),
                        modified_at: None,
                        link_target: None,
                    }],
                },
            )),
            window,
            cx,
        );
        workspace.focused_side = PaneSide::Remote;
        workspace.select_index(PaneSide::Remote, 0, cx);
    });
    cx.run_until_parked();

    cx.dispatch_action(CopyPath);
    cx.run_until_parked();

    cx.update(|_window, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(remote_path.as_str().to_string()),
            "Copy Path needs no live session to read a listed remote path"
        );
    });
}

#[gpui_kit::test]
fn remote_row_shows_dirty_indicator_when_locally_modified(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    cx.run_until_parked();

    let remote_path = RemotePath::new("/home/tester/notes.txt");
    let session_dir = std::env::temp_dir().join(format!(
        "macsftp-dirty-indicator-{}-{}",
        std::process::id(),
        "test"
    ));
    std::fs::create_dir_all(&session_dir).expect("create session dir");
    let temp_file = session_dir.join("notes.txt");
    std::fs::write(&temp_file, b"content").expect("write temp file");
    let temp_path = LocalPath::new(temp_file.to_string_lossy().to_string());

    let session_id = workspace.update_in(&mut cx, |workspace, _window, cx| {
        let tab = workspace.active_tab_mut().expect("active tab should exist");
        tab.remote.entries = vec![RemoteEntry {
            name: "notes.txt".to_string(),
            path: remote_path.clone(),
            kind: FileKind::File,
            size: Some(100),
            permissions: Some(0o644),
            modified_at: None,
            link_target: None,
        }];
        tab.selection.selected_paths = vec![EntryPath::Remote(remote_path.clone())];

        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path,
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(7),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: false,
            local_baseline: None,
        });
        id
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let row = window.within("remote-file-pane").find(("file-row", 0usize));
        let label = row.label().expect("row exposes a label");
        assert!(
            !label.starts_with("●"),
            "unmodified edit session must not show dirty dot: {label}"
        );
    });

    // Mark session dirty
    workspace.update_in(&mut cx, |_workspace, _window, cx| {
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
            session.local_modified = true;
        }
        cx.refresh_windows();
    });

    cx.update(|window, cx| {
        window.render_frame(cx);
        let row = window.within("remote-file-pane").find(("file-row", 0usize));
        let label = row.label().expect("row exposes a label");
        assert!(
            label.starts_with("●"),
            "locally modified edit session must show dirty dot: {label}"
        );
    });

    std::fs::remove_dir_all(&session_dir).ok();
}

#[gpui_kit::test]
fn upload_edited_file_action_dispatches_check(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    cx.run_until_parked();

    let remote_path = RemotePath::new("/home/tester/notes.txt");
    let session_dir = std::env::temp_dir().join(format!(
        "macsftp-upload-action-{}-{}",
        std::process::id(),
        "test"
    ));
    std::fs::create_dir_all(&session_dir).expect("create session dir");
    let temp_file = session_dir.join("notes.txt");
    std::fs::write(&temp_file, b"content").expect("write temp file");
    let temp_path = LocalPath::new(temp_file.to_string_lossy().to_string());

    let session_id = workspace.update_in(&mut cx, |workspace, _window, cx| {
        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path,
            phase: EditPhase::Editing,
            remote_snapshot: RemoteSnapshot {
                size: Some(7),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: true,
            local_baseline: None,
        });
        let tab = workspace.active_tab_mut().expect("active tab");
        tab.selection.selected_paths = vec![EntryPath::Remote(remote_path.clone())];
        id
    });

    cx.dispatch_action(crate::app_actions::UploadEditedFile);

    let command = channels
        .command_rx
        .try_recv()
        .expect("UploadEditedFile action must dispatch remote check");
    let AppCommand::CheckRemoteEditSnapshot(cmd) = command else {
        panic!("expected CheckRemoteEditSnapshot, got {command:?}");
    };
    assert_eq!(cmd.edit_session_id, session_id);
    assert_eq!(cmd.path, remote_path);

    std::fs::remove_dir_all(&session_dir).ok();
}

#[gpui_kit::test]
fn settings_editor_rules_ui_add_and_remove(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        ws.settings.section = SettingsSection::General;
        ws.settings.new_rule_pattern.update(cx, |input, cx| {
            input.set_value("*.rs", window, cx);
        });
        ws.settings.new_rule_app.update(cx, |input, cx| {
            input.set_value("RustRover", window, cx);
        });
        ws.add_editor_rule(window, cx);

        let rules = cx.resources().config.config().editor_rules.clone();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "*.rs");
        assert_eq!(rules[0].app_name, "RustRover");

        ws.remove_editor_rule(0, cx);
        let rules_after = cx.resources().config.config().editor_rules.clone();
        assert!(rules_after.is_empty(), "rule should be removed");
    });
}

#[gpui_kit::test]
fn compare_edit_conflict_dispatches_download_transfer(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    cx.run_until_parked();

    let remote_path = RemotePath::new("/home/tester/notes.txt");
    let session_dir = std::env::temp_dir().join(format!(
        "macsftp-compare-conflict-{}-{}",
        std::process::id(),
        "test"
    ));
    std::fs::create_dir_all(&session_dir).expect("create session dir");
    let temp_file = session_dir.join("notes.txt");
    std::fs::write(&temp_file, b"content").expect("write temp file");
    let temp_path = LocalPath::new(temp_file.to_string_lossy().to_string());

    let session_id = workspace.update_in(&mut cx, |_ws, _window, cx| {
        let id = cx.resources_mut().edit_sessions.next_id();
        cx.resources_mut().edit_sessions.register(EditSession {
            id,
            remote_path: remote_path.clone(),
            io_path: remote_path.clone(),
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: ProfileId(0),
            connection_key: manual_connection_key(),
            local_temp_path: temp_path.clone(),
            phase: EditPhase::RemoteConflict,
            remote_snapshot: RemoteSnapshot {
                size: Some(7),
                modified_at: None,
            },
            pending_check_id: None,
            checking_local_mtime: None,
            local_modified: true,
            local_baseline: None,
        });
        id
    });

    workspace.update_in(&mut cx, |ws, _window, cx| {
        ws.compare_edit_conflict(session_id, cx);
        assert!(
            ws.modal_inputs.conflict_compare_loading,
            "starting compare must set conflict_compare_loading"
        );
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("compare_edit_conflict must dispatch StartTransfer");
    let AppCommand::StartTransfer(cmd) = command else {
        panic!("expected StartTransfer, got {command:?}");
    };
    assert_eq!(cmd.tab_id, TabId(1));
    assert_eq!(cmd.sources, vec![TransferEndpoint::Remote(remote_path)]);
    let expected_dest = crate::workspace::conflict_remote_path(&temp_path);
    assert_eq!(cmd.destination, TransferEndpoint::Local(expected_dest));

    std::fs::remove_dir_all(&session_dir).ok();
}

/// A drag carries the whole pane selection, so dragging one row out of a
/// multi-selection moves every selected file instead of silently dropping all
/// but the row under the pointer.
#[gpui_kit::test]
fn drag_payload_expands_to_the_selection_when_the_row_is_selected(cx: &mut TestAppContext) {
    let (fixture, workspace, mut cx) =
        workspace_with_local_files(cx, "drag-payload", &["a.txt", "b.txt", "c.txt"]);
    cx.dispatch_action(SelectAllEntries);
    cx.run_until_parked();

    let selected: Vec<EntryPath> = (0..3)
        .map(|index| local_visible_entry(&workspace, &cx, index))
        .collect();
    let dragged = selected[1].clone();
    let payload = DragPayload::new(dragged.clone(), PaneSide::Local, &selected);
    assert_eq!(
        payload.paths, selected,
        "dragging a selected row carries the whole selection"
    );
    assert_eq!(payload.path, dragged, "the payload keeps the grabbed row");

    // A row outside the selection drags alone.
    let unselected = EntryPath::Local(LocalPath::new("/tmp/elsewhere.txt"));
    let lone = DragPayload::new(unselected.clone(), PaneSide::Local, &selected);
    assert_eq!(lone.paths, vec![unselected]);
    let _ = std::fs::remove_dir_all(&fixture);
}

#[gpui_kit::test]
fn drag_payload_with_an_empty_selection_carries_just_the_row(cx: &mut TestAppContext) {
    let (fixture, workspace, cx) = workspace_with_local_files(cx, "drag-payload-empty", &["a.txt"]);
    let row = local_visible_entry(&workspace, &cx, 0);
    let payload = DragPayload::new(row.clone(), PaneSide::Local, &[]);
    assert_eq!(payload.paths, vec![row]);
    let _ = std::fs::remove_dir_all(&fixture);
}

/// Dropping several local files on the remote pane uploads all of them.
#[gpui_kit::test]
fn dropping_a_multi_selection_uploads_every_selected_file(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    let (fixture, base) = temp_local_fixture("drop-multi-upload");
    for name in ["one.txt", "two.txt"] {
        std::fs::write(fixture.join(name), b"x").expect("fixture file");
    }
    set_local_path_and_wait(&workspace, &mut cx, base.clone());
    cx.dispatch_action(SelectAllEntries);
    cx.run_until_parked();
    let selected = workspace.update(&mut cx, |workspace, _| {
        workspace
            .active_tab()
            .expect("active tab")
            .selection
            .selected_paths
            .clone()
    });
    assert_eq!(selected.len(), 2, "both files are selected");

    // The drop handler is a thin adapter over `begin_upload`; driving the same
    // expansion it performs keeps the assertion on the command that reaches the
    // runtime.
    let payload = DragPayload::new(selected[0].clone(), PaneSide::Local, &selected);
    workspace.update(&mut cx, |workspace, cx| {
        workspace.begin_upload(payload.local_sources(), cx);
    });

    let command = channels
        .command_rx
        .try_recv()
        .expect("dropping the selection must enqueue a StartTransfer");
    match command {
        AppCommand::StartTransfer(command) => {
            assert_eq!(
                command.sources.len(),
                2,
                "both dragged files reach the transfer command"
            );
            assert!(matches!(command.destination, TransferEndpoint::Remote(_)));
        }
        other => panic!("expected StartTransfer, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&fixture);
}

/// Downloading several server files queues one plan per source: the remote
/// planner resolves a single remote tree against one local destination, so a
/// multi-source command would be rejected at planning time.
#[gpui_kit::test]
fn multi_file_download_queues_one_plan_per_source(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        let tab = workspace.active_tab_mut().expect("active tab");
        tab.local.path = Some(LocalPath::new("/tmp/downloads"));
        tab.remote.entries = ["one.txt", "two.txt", "three.txt"]
            .iter()
            .map(|name| RemoteEntry {
                name: (*name).to_string(),
                path: RemotePath::new(format!("{TEST_REMOTE_ROOT}/{name}")),
                kind: FileKind::File,
                size: Some(1),
                permissions: Some(0o644),
                modified_at: None,
                link_target: None,
            })
            .collect();
        tab.selection.selected_paths = tab
            .remote
            .entries
            .iter()
            .map(|entry| EntryPath::Remote(entry.path.clone()))
            .collect();
        workspace.download_selection(cx);
    });

    let mut destinations = Vec::new();
    while let Ok(command) = channels.command_rx.try_recv() {
        if let AppCommand::StartTransfer(command) = command {
            assert_eq!(command.direction, TransferDirection::Download);
            assert_eq!(
                command.sources.len(),
                1,
                "each download plan carries exactly one remote source"
            );
            assert!(
                matches!(command.destination, TransferEndpoint::Local(_)),
                "the destination is the local folder"
            );
            match command.destination {
                TransferEndpoint::Local(path) => destinations.push(path.as_str().to_string()),
                TransferEndpoint::Remote(_) => {
                    panic!("a download destination is local")
                }
            }
        }
    }
    assert_eq!(
        destinations,
        vec![
            "/tmp/downloads/one.txt".to_string(),
            "/tmp/downloads/two.txt".to_string(),
            "/tmp/downloads/three.txt".to_string()
        ],
        "every selected file gets its own download, named after the source"
    );
}

/// A folder download still targets `<local folder>/<folder name>`: the remote
/// planner recreates the tree from that root.
#[gpui_kit::test]
fn folder_download_keeps_the_selected_folder_name(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);
    let source = RemotePath::new("/home/tester/project");

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        let tab = workspace.active_tab_mut().expect("active tab");
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
        .expect("a folder download is enqueued");
    let AppCommand::StartTransfer(command) = command else {
        panic!("expected StartTransfer");
    };
    assert_eq!(
        command.destination,
        TransferEndpoint::Local(LocalPath::new("/tmp/downloads/project"))
    );
}

/// Double-clicking (or pressing Enter on) a local file hands it to its default
/// application — the Finder convention. Before this, the row did nothing at all.
#[gpui_kit::test]
fn opening_a_local_file_hands_it_to_the_system(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    install_system_path_seams();
    OPENED_SYSTEM_PATHS.with(|paths| paths.borrow_mut().clear());
    let (fixture, base) = temp_local_fixture("open-local-file");
    std::fs::write(fixture.join("note.txt"), b"hi").expect("fixture file");
    set_local_path_and_wait(&workspace, &mut cx, base);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.focus_pane(PaneSide::Local, window, cx);
    });
    cx.run_until_parked();

    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_entry_at(PaneSide::Local, 0, window, cx);
    });
    cx.run_until_parked();

    let expected = fixture.join("note.txt").display().to_string();
    OPENED_SYSTEM_PATHS.with(|paths| {
        assert_eq!(
            *paths.borrow(),
            vec![expected],
            "opening a local file requests the system default application"
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.active_tab().expect("tab").local.path,
            Some(base_for_pane(&fixture)),
            "opening a file must not navigate the pane"
        );
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

fn base_for_pane(fixture: &std::path::Path) -> LocalPath {
    LocalPath::new(fixture.to_string_lossy().into_owned())
}

/// Opening a remote file still edits a temp copy; it must never reach the
/// system open path, because a download keeps the remote exec bit.
#[gpui_kit::test]
fn opening_a_remote_file_never_reaches_the_system_opener(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    install_system_path_seams();
    OPENED_SYSTEM_PATHS.with(|paths| paths.borrow_mut().clear());
    connect_and_drain(&workspace, &mut cx, &channels);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        workspace.active_tab_mut().expect("tab").remote.entries = vec![RemoteEntry {
            name: "run.sh".to_string(),
            path: RemotePath::new("/home/tester/run.sh"),
            kind: FileKind::File,
            size: Some(4),
            permissions: Some(0o755),
            modified_at: None,
            link_target: None,
        }];
        workspace.focused_side = PaneSide::Remote;
        workspace.select_index(PaneSide::Remote, 0, cx);
    });
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_entry_at(PaneSide::Remote, 0, window, cx);
    });
    cx.run_until_parked();

    OPENED_SYSTEM_PATHS.with(|paths| {
        assert!(
            paths.borrow().is_empty(),
            "a remote file must be edited in a temp copy, not executed: {:?}",
            paths.borrow()
        );
    });
    assert!(
        matches!(
            channels.command_rx.try_recv(),
            Ok(AppCommand::StartTransfer(_))
        ),
        "the remote open starts an edit download instead"
    );
}

/// A status line reports one action's outcome; it must not outlive that moment.
///
/// The fixture's home-folder listing is allowed to settle first: its completed
/// read retires its own loading line, and that path must not touch a message
/// set afterwards.
#[gpui_kit::test]
fn a_status_line_expires_after_its_lifetime(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    workspace.update(&mut cx, |workspace, cx| {
        workspace.show_status("Path copied: /tmp/a".into(), cx);
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Path copied: /tmp/a")
        );
    });

    cx.cx
        .executor()
        .advance_clock(crate::workspace::view_state::StatusLine::LIFETIME);
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test(),
            None,
            "the line is gone once its lifetime elapses"
        );
    });
}

/// A newer line must survive an older line's timer firing.
#[gpui_kit::test]
fn a_newer_status_line_is_not_cleared_by_an_older_timer(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.run_until_parked();
    workspace.update(&mut cx, |workspace, cx| {
        workspace.show_status("first".into(), cx);
    });
    cx.cx
        .executor()
        .advance_clock(crate::workspace::view_state::StatusLine::LIFETIME / 2);
    workspace.update(&mut cx, |workspace, cx| {
        workspace.show_status("second".into(), cx);
    });

    // Past the first line's deadline (t=6s), still inside the second line's
    // (shown at t=3s, deadline t=9s). Only the generation check keeps the older
    // timer from clearing the newer line.
    cx.cx.executor().advance_clock(
        crate::workspace::view_state::StatusLine::LIFETIME / 2 + Duration::from_millis(1),
    );
    cx.run_until_parked();
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("second"),
            "the older expiry must not clear the newer message"
        );
    });
}

/// A completed download offers reveal, and the button reveals the file it wrote.
#[gpui_kit::test]
fn a_completed_download_reveals_its_destination(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    install_system_path_seams();
    REVEALED_PATHS.with(|paths| paths.borrow_mut().clear());
    let destination = LocalPath::new("/tmp/downloads/report.pdf");
    workspace.update(&mut cx, |_workspace, cx| {
        seed_finished_transfer(
            cx,
            TransferId(70),
            TransferDirection::Download,
            TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf")),
            TransferEndpoint::Local(destination.clone()),
        );
    });
    cx.run_until_parked();

    workspace.update(&mut cx, |workspace, cx| {
        workspace.reveal_transfer_destination(TransferId(70), cx);
    });
    cx.run_until_parked();

    REVEALED_PATHS.with(|paths| {
        assert_eq!(
            *paths.borrow(),
            vec![destination.as_str().to_string()],
            "reveal points at the downloaded file"
        );
    });
}

/// An upload's destination lives on the server, so revealing it must say so
/// rather than opening a meaningless local path.
#[gpui_kit::test]
fn revealing_a_remote_destination_reports_instead_of_opening_it(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    install_system_path_seams();
    REVEALED_PATHS.with(|paths| paths.borrow_mut().clear());
    workspace.update(&mut cx, |_workspace, cx| {
        seed_finished_transfer(
            cx,
            TransferId(71),
            TransferDirection::Upload,
            TransferEndpoint::Local(LocalPath::new("/tmp/dest/report.pdf")),
            TransferEndpoint::Remote(RemotePath::new("/srv/report.pdf")),
        );
    });
    cx.run_until_parked();

    workspace.update(&mut cx, |workspace, cx| {
        workspace.reveal_transfer_destination(TransferId(71), cx);
    });
    cx.run_until_parked();

    REVEALED_PATHS.with(|paths| {
        assert!(
            paths.borrow().is_empty(),
            "a remote destination is never revealed locally"
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("This file is on the server.")
        );
    });
}

/// Publish a plan whose single job is already `Completed`.
fn seed_finished_transfer(
    cx: &mut gpui_kit::App,
    id: TransferId,
    direction: TransferDirection,
    source: TransferEndpoint,
    destination: TransferEndpoint,
) {
    use crate::resources::ActiveTransfers as _;
    use macsftp_core::{
        ConflictPolicy, MetadataPolicy, Timestamp, TransferJob, TransferPlan, TransferPlanId,
        TransferPlanSnapshot, TransferPlanState, TransferState,
    };
    let job = TransferJob {
        id,
        direction,
        source,
        destination,
        state: TransferState::Queued,
        metadata_policy: MetadataPolicy::default(),
        conflict_policy: ConflictPolicy::default(),
        warnings: Vec::new(),
        created_at: Timestamp::from_secs_since_epoch(1),
    };
    let plan = TransferPlan {
        id: TransferPlanId(id.0),
        root_job_id: id,
        source_root: job.source.clone(),
        destination_root: job.destination.clone(),
        state: TransferPlanState::Queued,
        planned_count: 1,
        total_bytes: None,
        child_jobs: Vec::new(),
        conflict_policy: job.conflict_policy.clone(),
    };
    cx.apply_transfer_event(
        &AppEvent::TransferPlanStarted(Box::new(TransferPlanSnapshot {
            plan,
            root_job: job,
        })),
        Timestamp::from_secs_since_epoch(1),
    );
    cx.apply_transfer_event(
        &AppEvent::TransferCompleted { transfer_id: id },
        Timestamp::from_secs_since_epoch(2),
    );
}

/// Go to Folder opens pre-filled with the pane's current path, selected, so
/// navigating to a sibling directory means typing one segment instead of the
/// whole path.
#[gpui_kit::test]
fn go_to_path_prefills_the_current_folder(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("goto-prefill");
    set_local_path_and_wait(&workspace, &mut cx, base.clone());
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.focus_pane(PaneSide::Local, window, cx);
        workspace.open_go_to_path(window, cx);
    });
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.go_to_path.open);
        assert_eq!(
            workspace.go_to_path.input.value(),
            fixture.to_string_lossy(),
            "the dialog starts from the folder being shown"
        );
        assert!(
            !workspace.go_to_path.select_on_first_render,
            "the selection was applied to the fresh field"
        );
    });

    // The same path round-trips: pressing Enter with the prefill navigates to
    // where the pane already is and does not error.
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.submit_go_to_path(window, cx);
    });
    cx.run_until_parked();
    workspace.read_with(&cx, |workspace, _| {
        assert!(!workspace.go_to_path.open);
        assert_eq!(
            workspace.active_tab().expect("tab").local.path,
            Some(base),
            "the pane stays on the pre-filled folder"
        );
    });
    let _ = std::fs::remove_dir_all(&fixture);
}

/// Submitting an edited prefill navigates to the edited path: the selection
/// must have left the field editable, not read-only.
#[gpui_kit::test]
fn go_to_path_prefill_is_editable(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = temp_local_fixture("goto-edit");
    let sibling = fixture.join("other");
    std::fs::create_dir_all(&sibling).expect("sibling dir");
    set_local_path_and_wait(&workspace, &mut cx, base.clone());
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.focus_pane(PaneSide::Local, window, cx);
        workspace.open_go_to_path(window, cx);
    });
    cx.run_until_parked();

    // Simulate the user editing the selected prefill down to the fixture root
    // and pressing Enter.
    workspace.update(&mut cx, |workspace, _cx| {
        workspace
            .go_to_path
            .input
            .set_value(fixture.to_string_lossy().into_owned());
    });
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.submit_go_to_path(window, cx);
    });
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.active_tab().expect("tab").local.path,
            Some(base_for_pane(&fixture)),
            "the edited prefill navigated to the fixture root"
        );
    });
    let _ = std::fs::remove_dir_all(&fixture);
    let _ = std::fs::remove_dir_all(&sibling);
}

/// The password field starts masked and its Show/Hide toggle flips the mask
/// without touching the stored value. A fresh form always starts masked.
#[gpui_kit::test]
fn password_reveal_toggles_the_mask(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_connect_form(window, cx);
    });
    cx.run_until_parked();

    workspace.update(&mut cx, |workspace, _cx| {
        let form = workspace
            .connect_form_ui
            .form
            .as_mut()
            .expect("the connect form opened");
        form.password.set_value("s3cret-password");
        assert!(!form.show_password, "a fresh form starts masked");
    });
    cx.run_until_parked();

    // Masked by default: the toggle reads "Show".
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("connect-reveal", 3usize)).label(),
            Some("Show"),
            "a fresh form offers Show, not Hide"
        );
    });

    // Show: the flag flips, the value stays, the toggle reads "Hide".
    workspace.update(&mut cx, |workspace, _cx| {
        let form = workspace.connect_form_ui.form.as_mut().expect("form");
        form.show_password = true;
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("connect-reveal", 3usize)).label(),
            Some("Hide"),
            "the revealed field offers Hide"
        );
    });

    // Hide again: the value survives, the mask returns.
    workspace.update(&mut cx, |workspace, _cx| {
        let form = workspace.connect_form_ui.form.as_mut().expect("form");
        form.show_password = false;
        assert_eq!(form.password.value(), "s3cret-password");
    });

    // A form opened from a saved profile starts masked too, even when the
    // previous form was left revealed.
    workspace.update(&mut cx, |workspace, _cx| {
        let form = workspace.connect_form_ui.form.as_mut().expect("form");
        form.show_password = true;
    });
    workspace.update_in(&mut cx, |workspace, window, cx| {
        workspace.open_connect_form(window, cx);
    });
    cx.run_until_parked();
    workspace.read_with(&cx, |workspace, _| {
        let form = workspace.connect_form_ui.form.as_ref().expect("form");
        assert!(
            !form.show_password,
            "a reopened form starts masked, never inheriting the reveal"
        );
    });
}

/// The recent-connections context menu targets the exact row: reconnecting,
/// copying its endpoint, or forgetting that one entry — nothing disabled,
/// because none of it needs a session.
#[gpui_kit::test]
fn recent_connections_menu_targets_the_row(cx: &mut TestAppContext) {
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
    cx.run_until_parked();

    // The successful connect registered the recent entry this menu targets.
    let recent_id = workspace.read_with(&cx, |_, cx| {
        cx.resources()
            .recents
            .entries()
            .first()
            .expect("a successful connect registers a recent")
            .id
    });

    workspace.update(&mut cx, |workspace, cx| {
        workspace.forget_recent_connection(recent_id, cx);
    });
    cx.run_until_parked();

    workspace.read_with(&cx, |workspace, cx| {
        assert!(
            cx.resources().recents.entries().is_empty(),
            "forgetting removes the targeted entry"
        );
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Removed from Recent Connections.")
        );
    });

    // Forgetting an already-gone id is a no-op, not an error.
    workspace.update(&mut cx, |workspace, cx| {
        workspace.forget_recent_connection(recent_id + 100, cx);
    });
    cx.run_until_parked();
    workspace.read_with(&cx, |_, cx| {
        assert!(
            cx.resources().recents.entries().is_empty(),
            "the no-op forget leaves the store alone"
        );
    });
}

/// Copying a recent connection's endpoint puts `user@host:port` on the
/// clipboard — the string a terminal needs — without connecting.
#[gpui_kit::test]
fn copying_a_recent_endpoint_copies_user_host_port(cx: &mut TestAppContext) {
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
    cx.run_until_parked();

    let recent_id = workspace.read_with(&cx, |_, cx| {
        cx.resources()
            .recents
            .entries()
            .first()
            .expect("recent registered")
            .id
    });
    cx.update(|_window, cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("".into())));

    workspace.update(&mut cx, |workspace, cx| {
        workspace.copy_recent_endpoint(recent_id, cx);
    });
    cx.update(|_window, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("tester@mock.example.com:22".to_string()),
            "the endpoint is user@host:port"
        );
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.status_message_for_test().as_deref(),
            Some("Endpoint copied: tester@mock.example.com:22")
        );
    });
}

/// The divider between the panes sets how much width each side gets, and the
/// split survives a window resize because it is stored as a ratio.
#[gpui_kit::test]
fn pane_divider_sets_the_split_between_the_panes(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    let local_before = cx
        .debug_bounds("local-file-pane")
        .expect("local pane is laid out");
    let remote_before = cx
        .debug_bounds("remote-file-pane")
        .expect("remote pane is laid out");
    let divider = cx
        .debug_bounds("pane-divider")
        .expect("the divider is laid out");
    assert!(
        divider.left() >= local_before.right() - px(1.0)
            && divider.right() <= remote_before.left() + px(1.0),
        "the divider sits between the panes: {divider:?} {local_before:?} {remote_before:?}"
    );
    let total = local_before.size.width + remote_before.size.width;
    let local_share = local_before.size.width / total;
    assert!(
        (local_share - 0.5).abs() < 0.02,
        "the panes start evenly split: {local_share}"
    );

    // Dragging the divider gives the local pane less room.
    workspace.update(&mut cx, |workspace, cx| {
        workspace.pane_split_ratio = 0.3;
        cx.notify();
    });
    cx.run_until_parked();
    let local_after = cx
        .debug_bounds("local-file-pane")
        .expect("local pane is laid out");
    let remote_after = cx
        .debug_bounds("remote-file-pane")
        .expect("remote pane is laid out");
    let total_after = local_after.size.width + remote_after.size.width;
    let share = local_after.size.width / total_after;
    assert!(
        (share - 0.3).abs() < 0.02,
        "the divider's ratio drives the split: {share}"
    );

    // The ratio survives a window resize.
    cx.simulate_resize(size(px(900.0), px(600.0)));
    cx.run_until_parked();
    let local_resized = cx
        .debug_bounds("local-file-pane")
        .expect("local pane is laid out");
    let remote_resized = cx
        .debug_bounds("remote-file-pane")
        .expect("remote pane is laid out");
    let total_resized = local_resized.size.width + remote_resized.size.width;
    let share_resized = local_resized.size.width / total_resized;
    assert!(
        (share_resized - 0.3).abs() < 0.02,
        "the ratio survives a resize: {share_resized}"
    );
}

/// The divider stops where a pane would become unusable.
#[gpui_kit::test]
fn pane_divider_stops_at_the_usable_limits(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    cx.run_until_parked();

    workspace.update(&mut cx, |workspace, cx| {
        workspace.pane_split_ratio = 0.0;
        cx.notify();
    });
    cx.run_until_parked();
    let local = cx
        .debug_bounds("local-file-pane")
        .expect("local pane is laid out");
    let remote = cx
        .debug_bounds("remote-file-pane")
        .expect("remote pane is laid out");
    assert!(
        local.size.width > px(0.0) && remote.size.width > px(0.0),
        "neither pane collapses: {local:?} {remote:?}"
    );
    assert!(
        local.size.width / (local.size.width + remote.size.width) >= 0.2,
        "the local pane keeps a usable share"
    );
}

/// A download whose source cannot be named still queues the other sources and
/// says how many started, instead of cancelling the ones already queued.
#[gpui_kit::test]
fn a_download_with_an_unusable_name_keeps_the_other_sources(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    connect_and_drain(&workspace, &mut cx, &channels);

    workspace.update_in(&mut cx, |workspace, _window, cx| {
        let tab = workspace.active_tab_mut().expect("active tab");
        tab.local.path = Some(LocalPath::new("/tmp/downloads"));
        // A name with a control character cannot become a local file name.
        tab.remote.entries = vec![
            RemoteEntry {
                name: "good.txt".to_string(),
                path: RemotePath::new("/home/tester/good.txt"),
                kind: FileKind::File,
                size: Some(1),
                permissions: Some(0o644),
                modified_at: None,
                link_target: None,
            },
            RemoteEntry {
                name: "bad\u{1}name.txt".to_string(),
                path: RemotePath::new("/home/tester/bad\u{1}name.txt"),
                kind: FileKind::File,
                size: Some(1),
                permissions: Some(0o644),
                modified_at: None,
                link_target: None,
            },
        ];
        tab.selection.selected_paths = tab
            .remote
            .entries
            .iter()
            .map(|entry| EntryPath::Remote(entry.path.clone()))
            .collect();
        workspace.download_selection(cx);
    });

    let mut queued = 0;
    while let Ok(command) = channels.command_rx.try_recv() {
        if matches!(command, AppCommand::StartTransfer(_)) {
            queued += 1;
        }
    }
    assert_eq!(queued, 1, "the usable source is still downloaded");
    workspace.read_with(&cx, |workspace, _| {
        let message = workspace.status_message_for_test().unwrap_or_default();
        assert!(
            message.contains("1 could not be started"),
            "the user is told one item did not start: {message:?}"
        );
    });
}
