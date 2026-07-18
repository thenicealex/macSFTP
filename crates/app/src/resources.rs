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
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{App, Global};
use macsftp_core::{
    AppEvent, EditSessionStore, LocalPath, TabId, Timestamp, TransferId, TransferStore,
};
use macsftp_platform::AppPaths;
use macsftp_storage::{ConfigStore, ProfileStore, RecentsStore, ResidualTempStore};
use tracing::warn;

/// The disk-backed stores plus the app paths and the shared tab-id
/// counter, promoted out of `Workspace` so all windows share one instance
/// of each (previously each `Workspace::new` opened its own copy, which
/// would race on file writes once a second window existed).
pub struct AppResources {
    pub app_paths: AppPaths,
    pub config: ConfigStore,
    pub profiles: ProfileStore,
    pub residual_temps: ResidualTempStore,
    /// Recent successful connections (host/user/paths). No secrets.
    pub recents: RecentsStore,
    /// Active remote-edit sessions (begin_edit / edit watcher). Registered
    /// here so the store is process-wide and shared across windows. Empty at
    /// launch; the edits directory is cleared in `main()` before this is
    /// built.
    pub edit_sessions: EditSessionStore,
    /// Per-process namespace for remote-edit temp paths. `EditSessionId`
    /// restarts at one on every launch, so using it alone would reuse the same
    /// document URL while an external editor may still remember the previous
    /// run's deleted file.
    pub edit_run_id: String,
    /// Monotonic tab-id source, shared across every window so ids never
    /// collide (the runtime keys its session registry by `TabId`). An
    /// injected `Arc<AtomicU64>` rather than a `static`, so each test gets
    /// a fresh counter starting at 1 and stays isolated from other tests.
    next_tab_id: Arc<AtomicU64>,
}

impl Global for AppResources {}

impl AppResources {
    /// Load (or default-initialize) every store from `app_paths`. `config`
    /// is passed in because `main()` needs it earlier to pick the initial
    /// theme. ProfileStore owns the production Keychain boundary.
    pub fn load(app_paths: AppPaths, config: ConfigStore) -> Self {
        let profiles = ProfileStore::open_or_empty(app_paths.profiles_file.clone());
        Self::load_with_profiles(app_paths, config, profiles)
    }

    #[cfg(test)]
    pub fn load_for_test(app_paths: AppPaths, config: ConfigStore) -> Self {
        let profiles = ProfileStore::open_or_empty_memory(app_paths.profiles_file.clone());
        Self::load_with_profiles(app_paths, config, profiles)
    }

    fn load_with_profiles(
        app_paths: AppPaths,
        config: ConfigStore,
        profiles: ProfileStore,
    ) -> Self {
        if profiles.initial_error().is_some() {
            warn!("profiles.json could not be loaded; profile writes are blocked until recovery");
        }
        remove_legacy_transfer_history(&app_paths.legacy_transfer_history_file);
        let residual_temps = reconcile_local_residual_temps(ResidualTempStore::open_or_empty(
            app_paths.residual_temp_file.clone(),
        ));
        let recents = RecentsStore::open_or_empty(app_paths.recents_file.clone());
        if residual_temps.initial_error().is_some() {
            warn!(
                "residual_temp.json could not be loaded; residual writes are blocked until recovery"
            );
        }
        if recents.initial_error().is_some() {
            warn!("recents.json could not be loaded; recent writes are blocked until recovery");
        }
        Self {
            app_paths,
            config,
            profiles,
            residual_temps,
            recents,
            edit_sessions: EditSessionStore::new(),
            edit_run_id: next_edit_run_id(),
            next_tab_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Allocate the next process-unique tab id.
    pub fn next_tab_id(&self) -> TabId {
        TabId(self.next_tab_id.fetch_add(1, Ordering::Relaxed))
    }
}

fn next_edit_run_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let launched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{launched_at}-{sequence}", std::process::id())
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
/// created fresh via `Default`, and mutated once at the process event
/// boundary. Windows only read it and issue named user-action mutations.
///
/// `rates` is a view-side sliding-window sampler map (phase 2) keyed by
/// transfer id; progress events update it, and the drawer/detail read it.
#[derive(Default)]
pub struct SharedTransfers {
    store: TransferStore,
    rates: crate::rate_sampler::TransferRateBook,
}

impl Global for SharedTransfers {}

/// Read/write the shared transfer store (and rate book) anywhere a
/// `&App`/`&mut App` is reachable.
pub trait ActiveTransfers {
    fn transfers(&self) -> &TransferStore;
    fn rates(&self) -> &crate::rate_sampler::TransferRateBook;
    fn apply_transfer_event(&mut self, event: &AppEvent, now: Timestamp) -> bool;
    fn mark_transfer_cancelling(&mut self, transfer_id: TransferId) -> bool;
    fn remove_transfer_conflict(&mut self, request_id: macsftp_core::ConflictRequestId) -> bool;
    #[cfg(test)]
    fn observe_transfer_rate(
        &mut self,
        transfer_id: TransferId,
        bytes_done: u64,
        now: std::time::Instant,
    );
}

impl ActiveTransfers for App {
    fn transfers(&self) -> &TransferStore {
        &self.global::<SharedTransfers>().store
    }
    fn rates(&self) -> &crate::rate_sampler::TransferRateBook {
        &self.global::<SharedTransfers>().rates
    }

    fn apply_transfer_event(&mut self, event: &AppEvent, now: Timestamp) -> bool {
        let transfers = self.global_mut::<SharedTransfers>();
        match event {
            AppEvent::TransferRunning(snapshot) => {
                if let macsftp_core::TransferState::Running { bytes_done, .. } = snapshot.job.state
                {
                    transfers
                        .rates
                        .observe(snapshot.job.id, bytes_done, std::time::Instant::now());
                }
            }
            AppEvent::TransferProgress(progress) => transfers.rates.observe(
                progress.transfer_id,
                progress.bytes_done,
                std::time::Instant::now(),
            ),
            AppEvent::TransferCompleted { transfer_id }
            | AppEvent::TransferSkipped { transfer_id } => transfers.rates.clear(*transfer_id),
            AppEvent::TransferFailed(failure) => transfers.rates.clear(failure.transfer_id),
            _ => {}
        }
        transfers.store.apply_event(event, now)
    }

    fn mark_transfer_cancelling(&mut self, transfer_id: TransferId) -> bool {
        let changed = self
            .global_mut::<SharedTransfers>()
            .store
            .mark_cancelling(transfer_id);
        if changed {
            self.refresh_windows();
        }
        changed
    }

    fn remove_transfer_conflict(&mut self, request_id: macsftp_core::ConflictRequestId) -> bool {
        let changed = self
            .global_mut::<SharedTransfers>()
            .store
            .remove_conflict(request_id);
        if changed {
            self.refresh_windows();
        }
        changed
    }

    #[cfg(test)]
    fn observe_transfer_rate(
        &mut self,
        transfer_id: TransferId,
        bytes_done: u64,
        now: std::time::Instant,
    ) {
        self.global_mut::<SharedTransfers>()
            .rates
            .observe(transfer_id, bytes_done, now);
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

#[cfg(test)]
mod edit_tests {
    use super::AppResources;
    use macsftp_platform::AppPaths;
    use macsftp_storage::ConfigStore;

    #[test]
    fn app_resources_start_with_empty_edit_sessions() {
        let home = std::env::temp_dir().join(format!("macsftp-res-edit-{}", std::process::id()));
        let app_paths = AppPaths::from_home_dir(home.to_string_lossy().as_ref());
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        let resources = AppResources::load_for_test(app_paths, config);
        assert_eq!(resources.edit_sessions.editing_sessions().count(), 0);
    }

    #[test]
    fn app_resources_use_a_unique_edit_namespace_per_run() {
        let home =
            std::env::temp_dir().join(format!("macsftp-res-edit-run-{}", std::process::id()));
        let app_paths = AppPaths::from_home_dir(home.to_string_lossy().as_ref());
        let first = AppResources::load_for_test(
            app_paths.clone(),
            ConfigStore::with_defaults(app_paths.config_file.clone()),
        );
        let second = AppResources::load_for_test(
            app_paths.clone(),
            ConfigStore::with_defaults(app_paths.config_file.clone()),
        );

        assert_ne!(first.edit_run_id, second.edit_run_id);
    }
}
