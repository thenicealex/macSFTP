mod app_actions;
mod assets;
mod event_coordinator;
mod m7_regression;
mod palette_commands;
mod rate_sampler;
mod resources;
mod session_coordinator;
mod workspace;

use gpui::{
    App, AppContext, Application, Bounds, Global, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowHandle, WindowOptions, px, size,
};
use macsftp_core::RuntimeBridgeConfig;
use macsftp_platform::{AppPaths, prune_log_files, write_crash_marker};
use macsftp_sftp::{HostTrustConfig, RuntimeController, SessionBackend};
use macsftp_storage::{AppearancePreference, ConfigStore, SessionStore, SessionWindowSnapshot};
use macsftp_ui::Theme;
use std::path::Path;
use tracing::{error, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::app_actions::{
    CloseTab, CopyVersionInfo, DownloadSelection, FocusLocalPane, FocusRemotePane, HideApplication,
    HideOtherApplications, MinimizeWindow, NewTab, NewWindow, OpenLogFolder, OpenSettings, Quit,
    ReconnectTab, RefreshPane, ShowAbout, ShowAllApplications, ShowTransferDrawer,
    ToggleHiddenFiles, UploadSelection, ZoomWindow,
};
use crate::assets::Assets;
use crate::event_coordinator::{AppEventCoordinator, present_orphaned_transfer_conflicts};
use crate::resources::{AppResources, SharedTransfers};
use crate::session_coordinator::{
    SessionCoordinator, checkpoint_after_window_closed, checkpoint_before_quit,
};
use crate::workspace::Workspace;

/// Owns the Tokio runtime controller for the app's lifetime so it can
/// be shut down explicitly (with timeout) when the app quits (ADR-002).
struct RuntimeHandle {
    controller: Option<RuntimeController>,
    /// Keeps the tracing file-appender worker alive for the app's
    /// lifetime; dropping it flushes buffered log lines.
    _log_guard: WorkerGuard,
}

impl Global for RuntimeHandle {}

/// Install the global tracing subscriber. All diagnostics from the `sftp`
/// runtime, actor, and `app` UI flow through here.
///
/// - A non-blocking file appender writes to `log_path` (thread-safe for
///   the GPUI executor and the Tokio runtime both writing concurrently).
/// - `stderr` mirrors output for local development.
/// - Level is controlled by `RUST_LOG` (e.g. `RUST_LOG=debug`),
///   defaulting to `info`.
///
/// Returns the [`WorkerGuard`] the caller must keep alive for the process.
fn init_logging(log_path: &str) -> WorkerGuard {
    let path = Path::new(log_path);
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "macsftp.log".to_string());
    if let Err(error) = prune_log_files(directory, file_name.as_str(), 7, 50 * 1024 * 1024) {
        eprintln!("macSFTP could not prune old diagnostic logs: {error}");
    }
    let appender = tracing_appender::rolling::daily(directory, file_name.as_str());
    let (non_blocking_writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = fmt::layer()
        .with_writer(non_blocking_writer)
        .with_ansi(false)
        .with_target(false);

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_target(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();

    guard
}

fn install_panic_hook(crash_marker_path: &str) {
    let crash_marker_path = crash_marker_path.to_string();
    std::panic::set_hook(Box::new(move |panic_info| {
        let location = panic_info.location();
        let source_file_name = location.and_then(|location| {
            Path::new(location.file())
                .file_name()
                .and_then(|name| name.to_str())
        });
        let source_line = location.map(std::panic::Location::line);
        if let Err(error) = write_crash_marker(
            Path::new(&crash_marker_path),
            env!("CARGO_PKG_VERSION"),
            source_file_name,
            source_line,
        ) {
            eprintln!("macSFTP encountered an error and could not write a crash marker: {error}");
        } else {
            eprintln!("macSFTP encountered an internal error; a crash marker was written");
        }
    }));
}

fn main() {
    let app = Application::new().with_assets(Assets);
    // Native macOS behavior: the app stays alive with no windows open;
    // clicking the Dock icon opens a fresh window. gpui only fires this
    // when zero windows are open. Registered on the builder (not `App`);
    // the callback runs later, once the globals it reads are set.
    app.on_reopen(|cx: &mut App| {
        if let Err(error) = open_workspace_window(cx, None) {
            error!(error = ?error, "failed to reopen window");
        }
    });
    app.run(|cx: &mut App| {
        app_actions::init(cx);

        // Host trust files (plan §10): read the user's known_hosts if
        // present, write only to the app-owned file.
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let app_paths = AppPaths::from_home_dir(&home_dir);
        if let Err(error) = app_paths.ensure_directories() {
            eprintln!("macSFTP could not create its application directories: {error}");
        }
        install_panic_hook(app_paths.crash_marker_file.as_str());
        let log_guard = init_logging(app_paths.log_file.as_str());
        let config_store = match ConfigStore::open(app_paths.config_file.clone()) {
            Ok(store) => store,
            Err(error) => {
                warn!(error = %error, "could not load app config; using defaults");
                ConfigStore::with_defaults_after_error(app_paths.config_file.clone(), error)
            }
        };
        let initial_theme = match config_store.config().appearance {
            AppearancePreference::System => Theme::for_appearance(cx.window_appearance()),
            AppearancePreference::Light => Theme::one_light(),
            AppearancePreference::Dark => Theme::one_dark(),
        };
        cx.set_global(initial_theme);
        let user_known_hosts = std::path::PathBuf::from(format!("{home_dir}/.ssh/known_hosts"));
        let trust_config = HostTrustConfig::new(
            std::path::PathBuf::from(app_paths.known_hosts_file.as_str()),
            user_known_hosts.exists().then_some(user_known_hosts),
        );

        let mut controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Real(trust_config),
        );
        let event_receiver = controller
            .take_event_receiver()
            .expect("runtime event receiver must be available at startup");
        cx.set_global(RuntimeHandle {
            controller: Some(controller),
            _log_guard: log_guard,
        });
        cx.on_app_quit(|cx| {
            checkpoint_before_quit(cx);
            if cx.has_global::<RuntimeHandle>()
                && let Some(controller) = cx.remove_global::<RuntimeHandle>().controller
            {
                controller.shutdown();
            }
            async {}
        })
        .detach();

        // Shared, process-wide app state: one instance per process, read
        // and written by every window (Cmd+N). Set before any window opens.
        cx.set_global(SessionCoordinator::new(SessionStore::open_or_empty(
            app_paths.session_file.clone(),
        )));
        cx.set_global(AppResources::load(app_paths, config_store));
        cx.set_global(SharedTransfers::default());
        let event_coordinator = AppEventCoordinator::start(event_receiver, cx);
        cx.set_global(event_coordinator);
        cx.on_window_closed(|cx| {
            checkpoint_after_window_closed(cx);
            present_orphaned_transfer_conflicts(cx);
        })
        .detach();

        // Cmd+N (and File > New Window) opens another independent window.
        // Registered at the App level so it fires regardless of which
        // window — if any — is focused.
        cx.on_action(|_: &NewWindow, cx: &mut App| {
            if let Err(error) = open_workspace_window(cx, None) {
                error!(error = ?error, "failed to open new window");
            }
        });

        cx.set_menus(vec![
            Menu {
                name: "macSFTP".into(),
                items: vec![
                    MenuItem::action("About macSFTP", ShowAbout),
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Hide macSFTP", HideApplication),
                    MenuItem::action("Hide Others", HideOtherApplications),
                    MenuItem::action("Show All", ShowAllApplications),
                    MenuItem::separator(),
                    MenuItem::action("Quit macSFTP", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                items: vec![
                    MenuItem::action("New Window", NewWindow),
                    MenuItem::action("New Connection", NewTab),
                    MenuItem::action("Close Tab", CloseTab),
                    MenuItem::separator(),
                    MenuItem::action("Upload Selection", UploadSelection),
                    MenuItem::action("Download Selection", DownloadSelection),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Focus Local Pane", FocusLocalPane),
                    MenuItem::action("Focus Remote Pane", FocusRemotePane),
                    MenuItem::action("Toggle Transfers", ShowTransferDrawer),
                    MenuItem::separator(),
                    MenuItem::action("Show Hidden Files", ToggleHiddenFiles),
                    MenuItem::separator(),
                    MenuItem::action("Refresh", RefreshPane),
                    MenuItem::action("Reconnect", ReconnectTab),
                ],
            },
            Menu {
                name: "Window".into(),
                items: vec![
                    MenuItem::action("Minimize", MinimizeWindow),
                    MenuItem::action("Zoom", ZoomWindow),
                ],
            },
            Menu {
                name: "Help".into(),
                items: vec![
                    MenuItem::action("Open Log Folder", OpenLogFolder),
                    MenuItem::action("Copy Version Info", CopyVersionInfo),
                ],
            },
        ]);

        if let Err(error) = restore_workspace_windows(cx) {
            error!(error = ?error, "failed to open macSFTP window");
            cx.quit();
        }
    });
}

/// Open a new top-level workspace window. Shared by the initial launch,
/// the `NewWindow` action (Cmd+N / File > New Window), and the Dock-icon
/// reopen hook so all three construct a window identically.
///
/// Each window mints its own `RuntimeClient` from the one process-wide
/// [`RuntimeController`] and shares `AppResources`/`SharedTransfers` with
/// every other window. Runtime events are consumed once by
/// [`AppEventCoordinator`].
fn open_workspace_window(
    cx: &mut App,
    restore_session: Option<SessionWindowSnapshot>,
) -> gpui::Result<WindowHandle<Workspace>> {
    let runtime_client = {
        let handle = cx.global::<RuntimeHandle>();
        let controller = handle
            .controller
            .as_ref()
            .expect("runtime controller must be running while opening a window");
        controller.client()
    };

    let is_first_window = cx.windows().is_empty();
    let window_session_id = restore_session
        .as_ref()
        .map(|snapshot| snapshot.id)
        .unwrap_or_else(|| cx.global_mut::<SessionCoordinator>().allocate_window_id());

    // The first window (including one reopened after the last closed) opens
    // centered at the default size. Additional windows pass `None` so gpui
    // cascades them off the active window instead of stacking exactly on
    // top of it.
    let window_bounds = if is_first_window {
        Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1200.0), px(800.0)),
            cx,
        )))
    } else {
        None
    };

    let window = cx.open_window(
        WindowOptions {
            window_bounds,
            window_min_size: Some(size(px(720.0), px(480.0))),
            titlebar: Some(TitlebarOptions {
                title: Some("macSFTP".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                Workspace::new(
                    runtime_client,
                    window_session_id,
                    restore_session,
                    window,
                    cx,
                )
            })
        },
    )?;
    cx.global_mut::<SessionCoordinator>()
        .register_window(window_session_id);
    present_orphaned_transfer_conflicts(cx);
    cx.activate(true);
    Ok(window)
}

fn restore_workspace_windows(cx: &mut App) -> gpui::Result<()> {
    let session = cx.global_mut::<SessionCoordinator>().take_restore_session();
    if session.windows.is_empty() {
        open_workspace_window(cx, None)?;
        return Ok(());
    }

    let active_window_index = session
        .active_window_index
        .min(session.windows.len().saturating_sub(1));
    let mut active_window = None;
    for (index, snapshot) in session.windows.into_iter().enumerate() {
        let window = open_workspace_window(cx, Some(snapshot))?;
        if index == active_window_index {
            active_window = Some(window);
        }
    }
    if let Some(window) = active_window {
        window.update(cx, |_workspace, window, _cx| window.activate_window())?;
    }
    cx.activate(true);
    Ok(())
}
