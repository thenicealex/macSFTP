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

use gpui_kit::{App, Global};
use macsftp_core::{
    AppEvent, LocalPath, RemoteEditLifecycle, SessionId, TabId, Timestamp, TransferId,
    TransferState, TransferStore,
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
    /// Active remote-edit sessions. Registered here so the store is
    /// process-wide and shared across windows. Empty at
    /// launch; the edits directory is cleared in `main()` before this is
    /// built.
    pub edit_sessions: RemoteEditLifecycle,
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
    /// Process-wide source of `SessionId`s. Every window shares it because a
    /// `SessionId` also forms the pool identity of a manual connection; a
    /// per-window counter would let two windows mint the same id and silently
    /// share one authenticated physical SSH connection.
    next_session_id: Arc<AtomicU64>,
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
        let edit_run_id = next_edit_run_id();
        let edits_dir = app_paths.edits_dir.clone();
        Self {
            app_paths,
            config,
            profiles,
            residual_temps,
            recents,
            edit_sessions: RemoteEditLifecycle::new(edits_dir, edit_run_id.clone()),
            edit_run_id,
            next_tab_id: Arc::new(AtomicU64::new(1)),
            next_session_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Allocate the next process-unique tab id.
    pub fn next_tab_id(&self) -> TabId {
        TabId(self.next_tab_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Allocate the next process-unique session id for a connection attempt.
    ///
    /// Authority lives here, not in a per-window `AppState`: manual
    /// connections use `ConnectionPoolIdentity::Ephemeral(session_id)`, so two
    /// windows reusing the same number would derive the same pool key and
    /// reuse one authenticated connection across independent credentials.
    pub fn next_session_id(&self) -> SessionId {
        SessionId(self.next_session_id.fetch_add(1, Ordering::Relaxed) + 1)
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
    fn clear_terminal_transfers(&mut self) -> bool;
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

    fn clear_terminal_transfers(&mut self) -> bool {
        let terminal_ids: Vec<TransferId> = {
            let store = &self.global::<SharedTransfers>().store;
            store
                .jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        TransferState::Completed
                            | TransferState::Skipped
                            | TransferState::Failed { .. }
                    )
                })
                .map(|job| job.id)
                .collect()
        };
        let removed = self.global_mut::<SharedTransfers>().store.clear_terminal();
        if removed {
            let transfers = self.global_mut::<SharedTransfers>();
            for id in &terminal_ids {
                transfers.rates.clear(*id);
            }
            self.refresh_windows();
        }
        removed
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
/// (current architecture §8). Runs once at startup while building [`AppResources`],
/// not per window. Moved verbatim from `Workspace`.
fn reconcile_local_residual_temps(mut store: ResidualTempStore) -> ResidualTempStore {
    let local: Vec<_> = store.local_records().cloned().collect();
    for record in local {
        if !record.path.contains(".macsftp-part-")
            || record.path.contains('\0')
            || record.path.chars().any(macsftp_core::is_bidi_or_control)
        {
            warn!(
                path = %record.path,
                "skipping local residual temp file that does not match .macsftp-part- pattern or contains forbidden characters"
            );
            store.remove(record.transfer_id, &record.path);
            continue;
        }
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
        assert!(resources.edit_sessions.session_tab_ids().is_empty());
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

    /// Manual connections derive their connection-pool key from this id. If
    /// two windows could mint the same number they would reuse one
    /// authenticated physical SSH connection across different credentials.
    #[test]
    fn session_ids_are_unique_across_windows() {
        let home =
            std::env::temp_dir().join(format!("macsftp-res-session-id-{}", std::process::id()));
        let app_paths = AppPaths::from_home_dir(home.to_string_lossy().as_ref());
        let resources = AppResources::load_for_test(
            app_paths.clone(),
            ConfigStore::with_defaults(app_paths.config_file.clone()),
        );

        // Both windows share this one process-level allocator.
        let first = resources.next_session_id();
        let second = resources.next_session_id();
        let third = resources.next_session_id();
        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_ne!(first, third);
    }

    #[test]
    fn reconcile_local_residual_temps_removes_existing_files_and_cleans_store() {
        use macsftp_core::{
            LocalPath, ResidualTempRecord, Timestamp, TransferDirection, TransferId,
        };
        use macsftp_storage::ResidualTempStore;

        let dir = std::env::temp_dir().join(format!(
            "macsftp-res-reconcile-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");

        let store_path = dir.join("residual_temps.json");
        let store_local_path = LocalPath::new(store_path.display().to_string());
        let mut store = ResidualTempStore::open_or_empty(store_local_path.clone());

        let existing_local_file = dir.join(".download.txt.macsftp-part-1");
        std::fs::write(&existing_local_file, b"partial download").expect("write local temp file");

        let missing_local_file = dir.join(".download.txt.macsftp-part-2");

        // Local record with existing file
        store.add(ResidualTempRecord {
            connection_key: "server:22".to_string(),
            direction: TransferDirection::Download,
            transfer_id: TransferId(1),
            path: existing_local_file.display().to_string(),
            created_at: Timestamp::from_secs_since_epoch(100),
        });

        // Local record with missing file
        store.add(ResidualTempRecord {
            connection_key: "server:22".to_string(),
            direction: TransferDirection::Download,
            transfer_id: TransferId(2),
            path: missing_local_file.display().to_string(),
            created_at: Timestamp::from_secs_since_epoch(101),
        });

        // Remote record (must NOT be removed by local reconciliation)
        store.add(ResidualTempRecord {
            connection_key: "server:22".to_string(),
            direction: TransferDirection::Upload,
            transfer_id: TransferId(3),
            path: "/remote/path/.upload.txt.macsftp-part-3".to_string(),
            created_at: Timestamp::from_secs_since_epoch(102),
        });
        store.save().expect("save initial store");

        let reconciled = super::reconcile_local_residual_temps(store);

        assert!(
            !existing_local_file.exists(),
            "local temp file must be deleted during reconciliation"
        );
        assert_eq!(
            reconciled.local_records().count(),
            0,
            "all local records should be cleared"
        );
        assert_eq!(
            reconciled.remote_records_for("server:22").count(),
            1,
            "remote records must be preserved"
        );

        // Verify persisted state matches
        let reloaded = ResidualTempStore::open_or_empty(store_local_path);
        assert_eq!(reloaded.local_records().count(), 0);
        assert_eq!(reloaded.remote_records_for("server:22").count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
