use macsftp_core::{ConnectionProfile, LocalPath, ProfileId};
use serde::{Deserialize, Serialize};

/// macOS Keychain backend for credential persistence (plan §11/§18).
pub mod keychain;
pub use keychain::{KeychainError, KeychainStore};

/// Versioned, non-secret application preferences stored in `config.json`.
pub mod config;
pub use config::{AppConfig, AppearancePreference, ConfigError, ConfigStore};

/// On-disk transfer history (plan §18): persists in-flight and finished
/// transfers so they survive app restarts and can be retried.
pub mod transfer_history;
pub use transfer_history::TransferHistoryStore;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilesFile {
    pub version: u32,
    pub profiles: Vec<ConnectionProfile>,
}

impl ProfilesFile {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            profiles: Vec::new(),
        }
    }

    pub fn find_profile(&self, profile_id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.id == profile_id)
    }

    pub fn save_profile(
        &mut self,
        mut profile: ConnectionProfile,
    ) -> Result<ConnectionProfile, StorageError> {
        match self
            .profiles
            .iter()
            .position(|stored_profile| stored_profile.id == profile.id)
        {
            Some(index) => {
                let next_revision = self.profiles[index].revision.checked_add(1).ok_or(
                    StorageError::RevisionOverflow {
                        profile_id: profile.id,
                    },
                )?;
                profile.revision = next_revision;
                self.profiles[index] = profile.clone();
                Ok(profile)
            }
            None => {
                profile.revision = 1;
                self.profiles.push(profile.clone());
                Ok(profile)
            }
        }
    }

    /// Read `profiles.json` from `path`. A missing file is treated as an
    /// empty store (first launch); other IO or parse failures surface as
    /// `StorageError`.
    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: ProfilesFile =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                Ok(parsed)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ProfilesFile::new()),
            Err(error) => Err(StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            }),
        }
    }

    /// Atomically write `profiles.json` to `path` via a temp file + rename,
    /// so a crash mid-write can't leave a truncated file.
    pub fn save(&self, path: &LocalPath) -> Result<(), StorageError> {
        let json = serde_json::to_string_pretty(self).map_err(|error| StorageError::Parse {
            message: error.to_string(),
        })?;
        let path_str = path.as_str();
        let temp_path = format!("{path_str}.tmp");
        std::fs::write(&temp_path, &json).map_err(|error| StorageError::Io {
            path: temp_path.clone(),
            message: error.to_string(),
        })?;
        std::fs::rename(&temp_path, path_str).map_err(|error| StorageError::Io {
            path: path_str.to_string(),
            message: error.to_string(),
        })?;
        Ok(())
    }
}

impl Default for ProfilesFile {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    RevisionOverflow { profile_id: ProfileId },
    Io { path: String, message: String },
    Parse { message: String },
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::RevisionOverflow { profile_id } => {
                write!(f, "profile {profile_id:?} revision overflowed")
            }
            StorageError::Io { path, message } => {
                write!(f, "could not access {path}: {message}")
            }
            StorageError::Parse { message } => write!(f, "could not parse profiles: {message}"),
        }
    }
}

/// Disk-backed profile store: owns the `profiles.json` path and the
/// in-memory `ProfilesFile`, keeping them in sync on every write.
pub struct ProfileStore {
    path: LocalPath,
    profiles: ProfilesFile,
}

impl ProfileStore {
    /// Open the store at `path`, creating the backing file on first save.
    /// A missing file starts with an empty profile set.
    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let profiles = ProfilesFile::load(&path)?;
        Ok(Self { path, profiles })
    }

    /// Like `open`, but never fails: a missing or unreadable
    /// `profiles.json` (corrupt, permission denied) yields an empty store
    /// that still remembers `path`, so the next save recovers. The app
    /// keeps running instead of dying on a bad profile file.
    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(_) => Self {
                path,
                profiles: ProfilesFile::new(),
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn profiles(&self) -> &[ConnectionProfile] {
        &self.profiles.profiles
    }

    pub fn find_profile(&self, profile_id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles.find_profile(profile_id)
    }

    /// Insert or update a profile and flush the whole store to disk.
    pub fn save_profile(
        &mut self,
        profile: ConnectionProfile,
    ) -> Result<ConnectionProfile, StorageError> {
        let saved = self.profiles.save_profile(profile)?;
        self.profiles.save(&self.path)?;
        Ok(saved)
    }

    /// Remove a profile by id and flush. Removing a non-existent id is a
    /// no-op that still succeeds (idempotent).
    pub fn delete_profile(&mut self, profile_id: ProfileId) -> Result<(), StorageError> {
        let before = self.profiles.profiles.len();
        self.profiles
            .profiles
            .retain(|profile| profile.id != profile_id);
        if self.profiles.profiles.len() == before {
            return Ok(());
        }
        self.profiles.save(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{
        AuthCredential, AuthMethod, ConnectionProfile, ConnectionSettings, LocalPath, ProfileId,
        SecretRef,
    };

    use super::{ProfileStore, ProfilesFile, StorageError, core_crate_name, crate_name};

    // Unique temp path per call so concurrent tests never clobber each
    // other's profiles.json (plan §9: parallel tests must not share paths).
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_profiles_path(tag: &str) -> LocalPath {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let thread = format!("{:?}", std::thread::current().id());
        let dir = std::env::temp_dir();
        LocalPath::new(format!(
            "{}/macsftp-test-{tag}-{pid}-{thread}-{seq}.json",
            dir.display()
        ))
    }

    fn cleanup(path: &LocalPath) {
        let _ = std::fs::remove_file(path.as_str());
        let _ = std::fs::remove_file(format!("{}.tmp", path.as_str()));
    }

    fn password_profile(id: u64, name: &str) -> ConnectionProfile {
        ConnectionProfile::new(
            ProfileId(id),
            name,
            "example.com",
            "alex",
            AuthMethod::Password {
                secret_ref: SecretRef::new(format!("profile-{id}-password")),
            },
        )
    }

    #[test]
    fn links_core_crate() {
        assert_eq!(crate_name(), "macsftp-storage");
        assert_eq!(core_crate_name(), "macsftp-core");
    }

    #[test]
    fn new_profiles_file_has_current_version() {
        let profiles = ProfilesFile::new();

        assert_eq!(profiles.version, ProfilesFile::CURRENT_VERSION);
        assert!(profiles.profiles.is_empty());
    }

    #[test]
    fn save_new_profile_sets_revision_to_one() {
        let mut profiles = ProfilesFile::new();
        let mut profile = password_profile(1, "Production");
        profile.revision = 47;

        let saved = match profiles.save_profile(profile) {
            Ok(saved) => saved,
            Err(error) => panic!("new profile should save successfully: {error:?}"),
        };

        assert_eq!(saved.revision, 1);
        assert_eq!(profiles.find_profile(ProfileId(1)), Some(&saved));
    }

    #[test]
    fn save_existing_profile_increments_revision() {
        let mut profiles = ProfilesFile::new();
        let first_saved = match profiles.save_profile(password_profile(1, "Production")) {
            Ok(saved) => saved,
            Err(error) => panic!("initial profile should save successfully: {error:?}"),
        };
        let mut updated = first_saved.clone();
        updated.name = "Production Updated".to_string();

        let second_saved = match profiles.save_profile(updated) {
            Ok(saved) => saved,
            Err(error) => panic!("updated profile should save successfully: {error:?}"),
        };

        assert_eq!(second_saved.revision, first_saved.revision + 1);
        assert_eq!(second_saved.name, "Production Updated");
        assert_eq!(profiles.profiles.len(), 1);
    }

    #[test]
    fn save_existing_profile_reports_revision_overflow() {
        let mut profiles = ProfilesFile {
            version: ProfilesFile::CURRENT_VERSION,
            profiles: vec![ConnectionProfile {
                revision: u64::MAX,
                ..password_profile(1, "Production")
            }],
        };

        let result = profiles.save_profile(password_profile(1, "Production Updated"));

        assert_eq!(
            result,
            Err(StorageError::RevisionOverflow {
                profile_id: ProfileId(1)
            })
        );
    }

    #[test]
    fn missing_file_loads_as_empty_store() {
        let path = temp_profiles_path("missing");
        cleanup(&path);

        let file = ProfilesFile::load(&path).expect("missing file is an empty store");
        assert_eq!(file.version, ProfilesFile::CURRENT_VERSION);
        assert!(file.profiles.is_empty());

        cleanup(&path);
    }

    #[test]
    fn profile_store_round_trips_through_disk() {
        let path = temp_profiles_path("store-roundtrip");
        cleanup(&path);

        let mut store = ProfileStore::open(path.clone()).expect("open empty store");
        let saved = store
            .save_profile(password_profile(1, "Production"))
            .expect("save profile");
        assert_eq!(saved.revision, 1);

        // Fresh store reading the same file on disk.
        let reloaded = ProfileStore::open(path.clone()).expect("reopen store");
        assert_eq!(reloaded.profiles().len(), 1);
        let found = reloaded
            .find_profile(ProfileId(1))
            .expect("profile present");
        assert_eq!(found.name, "Production");
        assert_eq!(found.host, "example.com");
        assert_eq!(found.revision, 1);

        cleanup(&path);
    }

    #[test]
    fn revision_increments_survive_reload() {
        let path = temp_profiles_path("revision");
        cleanup(&path);

        let mut store = ProfileStore::open(path.clone()).expect("open store");
        store
            .save_profile(password_profile(2, "First"))
            .expect("save first");
        store
            .save_profile(password_profile(2, "Second"))
            .expect("save second");

        let reloaded = ProfileStore::open(path.clone()).expect("reopen store");
        let found = reloaded.find_profile(ProfileId(2)).expect("present");
        assert_eq!(found.revision, 2, "two saves => revision 2");
        assert_eq!(found.name, "Second");

        cleanup(&path);
    }

    #[test]
    fn profile_file_never_contains_the_secret() {
        let path = temp_profiles_path("no-secret");
        cleanup(&path);

        let settings = ConnectionSettings {
            host: "example.com".into(),
            port: 2222,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: "hunter2-do-not-leak".into(),
            },
        };
        let profile =
            ConnectionProfile::from_connection_settings(ProfileId(9), "Staging", &settings);

        let mut store = ProfileStore::open(path.clone()).expect("open store");
        store.save_profile(profile).expect("save profile");

        let raw = std::fs::read_to_string(path.as_str()).expect("read profiles file");
        assert!(
            raw.contains("keychain:macsftp:9:password"),
            "stub keychain ref should be persisted, got: {raw}"
        );
        assert!(
            !raw.contains("hunter2-do-not-leak"),
            "secret must never be written to disk, got: {raw}"
        );

        cleanup(&path);
    }

    #[test]
    fn save_to_uncreatable_path_returns_io_error() {
        let path = LocalPath::new("/nonexistent-dir-xyz/macsftp/profiles.json");
        let file = ProfilesFile::new();
        let result = file.save(&path);
        assert!(
            matches!(result, Err(StorageError::Io { .. })),
            "expected Io error, got {result:?}"
        );
    }
}
