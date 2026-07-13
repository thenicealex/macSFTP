mod app_actions;
mod assets;
mod m7_regression;
mod palette_commands;
mod rate_sampler;
mod resources;
mod workspace;

use gpui::{
    App, AppContext, Application, Bounds, Global, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowOptions, px, size,
};
use macsftp_core::RuntimeBridgeConfig;
use macsftp_platform::AppPaths;
use macsftp_sftp::{HostTrustConfig, RuntimeController, SessionBackend};
use macsftp_storage::{AppearancePreference, ConfigStore, KeychainStore};
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
use crate::resources::{AppResources, SharedTransfers};
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
    let appender = tracing_appender::rolling::never(directory, file_name.as_str());
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

fn main() {
    let app = Application::new().with_assets(Assets);
    // Native macOS behavior: the app stays alive with no windows open;
    // clicking the Dock icon opens a fresh window. gpui only fires this
    // when zero windows are open. Registered on the builder (not `App`);
    // the callback runs later, once the globals it reads are set.
    app.on_reopen(|cx: &mut App| {
        if let Err(error) = open_workspace_window(cx) {
            error!(error = ?error, "failed to reopen window");
        }
    });
    app.run(|cx: &mut App| {
        app_actions::init(cx);

        // Host trust files (plan §10): read the user's known_hosts if
        // present, write only to the app-owned file.
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let app_paths = AppPaths::from_home_dir(&home_dir);
        // Start logging early so directory-creation and every later
        // diagnostic land in the log file, not just stderr.
        let log_guard = init_logging(app_paths.log_file.as_str());
        if let Err(error) = app_paths.ensure_directories() {
            warn!(error = %error, "could not create app directories");
        }
        let config_store = match ConfigStore::open(app_paths.config_file.clone()) {
            Ok(store) => store,
            Err(error) => {
                warn!(error = %error, "could not load app config; using defaults");
                ConfigStore::with_defaults_after_error(app_paths.config_file.clone(), error)
            }
        };
        let initial_theme = match config_store.config().appearance {
            AppearancePreference::System => Theme::for_appearance(cx.window_appearance()),
            AppearancePreference::Light => Theme::light(),
            AppearancePreference::Dark => Theme::dark(),
        };
        cx.set_global(initial_theme);
        let user_known_hosts = std::path::PathBuf::from(format!("{home_dir}/.ssh/known_hosts"));
        let trust_config = HostTrustConfig::new(
            std::path::PathBuf::from(app_paths.known_hosts_file.as_str()),
            user_known_hosts.exists().then_some(user_known_hosts),
        );

        let controller = RuntimeController::start(
            RuntimeBridgeConfig::default(),
            SessionBackend::Real(trust_config),
        );
        cx.set_global(RuntimeHandle {
            controller: Some(controller),
            _log_guard: log_guard,
        });
        cx.on_app_quit(|cx| {
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
        cx.set_global(AppResources::load(
            app_paths,
            config_store,
            KeychainStore::new_os(),
        ));
        cx.set_global(SharedTransfers::default());

        // Cmd+N (and File > New Window) opens another independent window.
        // Registered at the App level so it fires regardless of which
        // window — if any — is focused.
        cx.on_action(|_: &NewWindow, cx: &mut App| {
            if let Err(error) = open_workspace_window(cx) {
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

        if let Err(error) = open_workspace_window(cx) {
            error!(error = ?error, "failed to open macSFTP window");
            cx.quit();
        }
    });
}

/// Open a new top-level workspace window. Shared by the initial launch,
/// the `NewWindow` action (Cmd+N / File > New Window), and the Dock-icon
/// reopen hook so all three construct a window identically.
///
/// Each window mints its own `RuntimeClient` and broadcast `EventReceiver`
/// from the one process-wide [`RuntimeController`], and shares the
/// `AppResources`/`SharedTransfers` globals with every other window.
fn open_workspace_window(cx: &mut App) -> gpui::Result<()> {
    let (runtime_client, event_receiver) = {
        let handle = cx.global::<RuntimeHandle>();
        let controller = handle
            .controller
            .as_ref()
            .expect("runtime controller must be running while opening a window");
        (controller.client(), controller.event_receiver())
    };

    // First window restores the previous session layout; Cmd+N windows start empty.
    let restore_session = cx.windows().is_empty();

    // The first window (including one reopened after the last closed) opens
    // centered at the default size. Additional windows pass `None` so gpui
    // cascades them off the active window instead of stacking exactly on
    // top of it.
    let window_bounds = if restore_session {
        Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1200.0), px(800.0)),
            cx,
        )))
    } else {
        None
    };

    cx.open_window(
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
                Workspace::new(runtime_client, event_receiver, restore_session, window, cx)
            })
        },
    )?;
    cx.activate(true);
    Ok(())
}
