//! Process-wide shared state, held as GPUI globals so every window reads
//! and writes the same instances.
//!
//! macSFTP runs exactly one `App` per process (single-threaded — see
//! gpui's `Global: 'static` marker trait, no `Send`/`Sync` bound). These
//! globals are set once in `main()` before any window opens, so a second
//! window (Cmd+N) shares them rather than opening its own duplicate copies
//! of the same on-disk files.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{App, Global};
use macsftp_core::{LocalPath, TabId, TransferStore};
use macsftp_platform::AppPaths;
use macsftp_storage::{
    ConfigStore, KeychainStore, ProfileStore, RecentsStore, ResidualTempStore, SessionStore,
};
use tracing::warn;

/// The disk-backed stores plus the app paths and the shared tab-id
/// counter, promoted out of `Workspace` so all windows share one instance
/// of each (previously each `Workspace::new` opened its own copy, which
/// would race on file writes once a second window existed).
pub struct AppResources {
    pub app_paths: AppPaths,
    pub config: ConfigStore,
    pub keychain: KeychainStore,
    pub profiles: ProfileStore,
    pub residual_temps: ResidualTempStore,
    /// Last-window tab layout (host/user/paths). No secrets.
    pub session: SessionStore,
    /// Recent successful connections (host/user/paths). No secrets.
    pub recents: RecentsStore,
    /// Monotonic tab-id source, shared across every window so ids never
    /// collide (the runtime keys its session registry by `TabId`). An
    /// injected `Arc<AtomicU64>` rather than a `static`, so each test gets
    /// a fresh counter starting at 1 and stays isolated from other tests.
    next_tab_id: Arc<AtomicU64>,
}

impl Global for AppResources {}

impl AppResources {
    /// Load (or default-initialize) every store from `app_paths`. `config`
    /// and `keychain` are passed in because `main()` needs `config` earlier
    /// (to pick the initial theme) and tests inject a different `keychain`
    /// backend (`new_memory()` vs `new_os()`).
    pub fn load(app_paths: AppPaths, config: ConfigStore, keychain: KeychainStore) -> Self {
        let profiles = ProfileStore::open_or_empty(app_paths.profiles_file.clone());
        remove_legacy_transfer_history(&app_paths.legacy_transfer_history_file);
        let residual_temps = reconcile_local_residual_temps(ResidualTempStore::open_or_empty(
            app_paths.residual_temp_file.clone(),
        ));
        let session = SessionStore::open_or_empty(app_paths.session_file.clone());
        let recents = RecentsStore::open_or_empty(app_paths.recents_file.clone());
        Self {
            app_paths,
            config,
            keychain,
            profiles,
            residual_temps,
            session,
            recents,
            next_tab_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Allocate the next process-unique tab id.
    pub fn next_tab_id(&self) -> TabId {
        TabId(self.next_tab_id.fetch_add(1, Ordering::Relaxed))
    }
}

fn remove_legacy_transfer_history(path: &LocalPath) {
    match std::fs::remove_file(path.as_str()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(path = %path.as_str(), error = %error, "could not remove legacy transfer history");
        }
    }
}

/// Read/write the shared resource bundle anywhere a `&App`/`&mut App` is
/// reachable (including from a `Context<Workspace>`, which derefs to
/// `App`). Mirrors `macsftp_ui::ActiveTheme`.
pub trait ActiveResources {
    fn resources(&self) -> &AppResources;
    fn resources_mut(&mut self) -> &mut AppResources;
}

impl ActiveResources for App {
    fn resources(&self) -> &AppResources {
        self.global::<AppResources>()
    }
    fn resources_mut(&mut self) -> &mut AppResources {
        self.global_mut::<AppResources>()
    }
}

/// The app-wide, shared transfer list (jobs/plans/pending conflicts),
/// visible identically in every window's transfer drawer. Kept separate
/// from [`AppResources`] because it is in-memory only (no disk path),
/// created fresh via `Default`, and mutated continuously by every window's
/// event-drain task.
///
/// `rates` is a view-side sliding-window sampler map (phase 2) keyed by
/// transfer id; progress events update it, and the drawer/detail read it.
#[derive(Default)]
pub struct SharedTransfers {
    pub store: TransferStore,
    pub rates: crate::rate_sampler::TransferRateBook,
}

impl Global for SharedTransfers {}

/// Read/write the shared transfer store (and rate book) anywhere a
/// `&App`/`&mut App` is reachable.
pub trait ActiveTransfers {
    fn transfers(&self) -> &TransferStore;
    fn transfers_mut(&mut self) -> &mut TransferStore;
    fn rates(&self) -> &crate::rate_sampler::TransferRateBook;
    fn rates_mut(&mut self) -> &mut crate::rate_sampler::TransferRateBook;
}

impl ActiveTransfers for App {
    fn transfers(&self) -> &TransferStore {
        &self.global::<SharedTransfers>().store
    }
    fn transfers_mut(&mut self) -> &mut TransferStore {
        &mut self.global_mut::<SharedTransfers>().store
    }
    fn rates(&self) -> &crate::rate_sampler::TransferRateBook {
        &self.global::<SharedTransfers>().rates
    }
    fn rates_mut(&mut self) -> &mut crate::rate_sampler::TransferRateBook {
        &mut self.global_mut::<SharedTransfers>().rates
    }
}

/// Remove any local `.macsftp-part-*` residuals left by a previous run
/// (plan M5/M6). Runs once at startup while building [`AppResources`],
/// not per window. Moved verbatim from `Workspace`.
fn reconcile_local_residual_temps(mut store: ResidualTempStore) -> ResidualTempStore {
    let local: Vec<_> = store.local_records().cloned().collect();
    for record in local {
        match std::fs::remove_file(&record.path) {
            Ok(()) => {
                store.remove(record.transfer_id, &record.path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                store.remove(record.transfer_id, &record.path);
            }
            Err(error) => {
                warn!(
                    path = %record.path,
                    error = %error,
                    "local residual temp file could not be removed at launch"
                );
            }
        }
    }
    if let Err(error) = store.save() {
        warn!(error = %error, "could not persist residual temp store at launch");
    }
    store
}
