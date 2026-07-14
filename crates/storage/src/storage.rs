/// Versioned, non-secret application preferences stored in `config.json`.
pub mod config;
pub use config::{AppConfig, AppearancePreference, ConfigError, ConfigStore};

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

/// Restorable workspace session tabs (plan §18 / Phase 5): `session.json`.
pub mod session;
pub use session::{SessionFile, SessionStore, SessionTabSnapshot};

pub fn crate_name() -> &'static str {
    "macsftp-storage"
}

pub fn core_crate_name() -> &'static str {
    macsftp_core::crate_name()
}
