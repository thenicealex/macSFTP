/// Versioned, non-secret application preferences stored in `config.json`.
pub mod config;
pub use config::{AppConfig, AppearancePreference, ConfigError, ConfigStore};

mod atomic_file;
pub use atomic_file::{AtomicWriteError, write_private_file_atomically};

/// Cross-instance advisory locking for store write transactions.
mod file_lock;

/// macOS Keychain backend for credential persistence (plan §11/§18).
mod keychain;
pub use keychain::KeychainError;

mod profile_file;
pub use profile_file::{ProfilesFile, StorageError};

mod profiles;
pub use profiles::{
    ProfileAuthUpdate, ProfileDeleteOutcome, ProfileMutationError, ProfileSaveOutcome,
    ProfileSaveRequest, ProfileStore,
};

/// Recent successful connections (plan §18 / Phase 5): `recents.json`.
pub mod recents;
pub use recents::{RecentEntry, RecentEntryInput, RecentsFile, RecentsStore};

pub mod residual_temp;
pub use residual_temp::ResidualTempStore;

/// Restorable workspace windows and tabs (plan §15/§18): `session.json`.
pub mod session;
pub use session::{SessionFile, SessionStore, SessionTabSnapshot, SessionWindowSnapshot};

pub fn crate_name() -> &'static str {
    "macsftp-storage"
}

pub fn core_crate_name() -> &'static str {
    macsftp_core::crate_name()
}
