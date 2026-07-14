use macsftp_core::{
    AuthCredential, AuthMethod, AuthMethodKind, ConnectionProfile, ConnectionSettings, LocalPath,
    ProfileId, RemotePath, SecretRef,
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

/// macOS Keychain backend for credential persistence (plan §11/§18).
mod keychain;
pub use keychain::KeychainError;
use keychain::KeychainStore;

/// Versioned, non-secret application preferences stored in `config.json`.
pub mod config;
pub use config::{AppConfig, AppearancePreference, ConfigError, ConfigStore};

pub mod residual_temp;
pub use residual_temp::ResidualTempStore;

/// Restorable workspace session tabs (plan §18 / Phase 5): `session.json`.
pub mod session;
pub use session::{SessionFile, SessionStore, SessionTabSnapshot};

/// Recent successful connections (plan §18 / Phase 5): `recents.json`.
pub mod recents;
pub use recents::{RecentEntry, RecentEntryInput, RecentsFile, RecentsStore};

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
    keychain: KeychainStore,
}

/// Credential update requested by a profile editor. `None` means "keep the
/// existing credential" when the profile already uses the same auth method.
/// Secret strings are zeroized when the request is dropped.
pub enum ProfileAuthUpdate {
    Password {
        password: Option<String>,
    },
    PrivateKey {
        key_path: LocalPath,
        passphrase: Option<String>,
    },
}

impl std::fmt::Debug for ProfileAuthUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { password } => formatter
                .debug_struct("Password")
                .field("password", &password.as_ref().map(|_| "[REDACTED]"))
                .finish(),
            Self::PrivateKey {
                key_path: _,
                passphrase,
            } => formatter
                .debug_struct("PrivateKey")
                .field("key_path", &"[REDACTED]")
                .field("passphrase", &passphrase.as_ref().map(|_| "[REDACTED]"))
                .finish(),
        }
    }
}

impl Drop for ProfileAuthUpdate {
    fn drop(&mut self) {
        match self {
            Self::Password { password } => password.zeroize(),
            Self::PrivateKey { passphrase, .. } => passphrase.zeroize(),
        }
    }
}

/// Non-UI profile mutation accepted by the storage boundary.
pub struct ProfileSaveRequest {
    pub profile_id: ProfileId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: ProfileAuthUpdate,
    pub default_remote_path: Option<RemotePath>,
}

#[derive(Debug, Clone)]
pub enum ProfileMutationError {
    Storage(StorageError),
    Keychain(KeychainError),
    ProfileNotFound(ProfileId),
    ProfileIdOverflow,
    CredentialRequired(AuthMethodKind),
    StorageRollback {
        storage: StorageError,
        rollback: KeychainError,
    },
}

impl std::fmt::Display for ProfileMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
            Self::Keychain(error) => error.fmt(formatter),
            Self::ProfileNotFound(profile_id) => {
                write!(formatter, "profile {profile_id:?} was not found")
            }
            Self::ProfileIdOverflow => write!(formatter, "profile id space is exhausted"),
            Self::CredentialRequired(AuthMethodKind::Password) => {
                write!(formatter, "password is required")
            }
            Self::CredentialRequired(AuthMethodKind::PrivateKey) => {
                write!(formatter, "private-key credential is required")
            }
            Self::StorageRollback { storage, rollback } => write!(
                formatter,
                "{storage}; restoring the previous Keychain value also failed: {rollback}"
            ),
        }
    }
}

impl std::error::Error for ProfileMutationError {}

impl From<StorageError> for ProfileMutationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<KeychainError> for ProfileMutationError {
    fn from(error: KeychainError) -> Self {
        Self::Keychain(error)
    }
}

pub struct ProfileSaveOutcome {
    pub profile: ConnectionProfile,
    /// The profile and its active credential were committed, but an obsolete
    /// credential could not be removed. Callers should log these warnings.
    pub cleanup_warnings: Vec<KeychainError>,
}

pub struct ProfileDeleteOutcome {
    pub deleted: bool,
    /// The disk entry was deleted, but a now-unreferenced Keychain item could
    /// not be removed. Callers should log these warnings.
    pub cleanup_warnings: Vec<KeychainError>,
}

struct ResolvedProfileAuth<'a> {
    auth: AuthMethod,
    secret_update: Option<SecretUpdate<'a>>,
}

struct SecretUpdate<'a> {
    secret_ref: SecretRef,
    secret: &'a str,
}

impl ProfileStore {
    /// Open the store at `path`, creating the backing file on first save.
    /// A missing file starts with an empty profile set.
    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let profiles = ProfilesFile::load(&path)?;
        Ok(Self {
            path,
            profiles,
            keychain: KeychainStore::new_os(),
        })
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
                keychain: KeychainStore::new_os(),
            },
        }
    }

    /// Test-only behavior without touching the user's login Keychain. This is
    /// public because `macsftp-app` integration tests compile as a separate
    /// crate and need an isolated storage boundary.
    pub fn open_or_empty_memory(path: LocalPath) -> Self {
        let profiles = ProfilesFile::load(&path).unwrap_or_else(|_| ProfilesFile::new());
        Self {
            path,
            profiles,
            keychain: KeychainStore::new_memory(),
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

    pub fn next_profile_id(&self) -> Result<ProfileId, ProfileMutationError> {
        self.profiles
            .profiles
            .iter()
            .map(|profile| profile.id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .map(ProfileId)
            .ok_or(ProfileMutationError::ProfileIdOverflow)
    }

    /// Insert or update a profile and flush the whole store to disk.
    pub fn save_profile(
        &mut self,
        profile: ConnectionProfile,
    ) -> Result<ConnectionProfile, StorageError> {
        let mut next_profiles = self.profiles.clone();
        let saved = next_profiles.save_profile(profile)?;
        next_profiles.save(&self.path)?;
        self.profiles = next_profiles;
        Ok(saved)
    }

    /// Remove a profile by id and flush. Removing a non-existent id is a
    /// no-op that still succeeds (idempotent).
    pub fn delete_profile(&mut self, profile_id: ProfileId) -> Result<(), StorageError> {
        let mut next_profiles = self.profiles.clone();
        let before = next_profiles.profiles.len();
        next_profiles
            .profiles
            .retain(|profile| profile.id != profile_id);
        if next_profiles.profiles.len() == before {
            return Ok(());
        }
        next_profiles.save(&self.path)?;
        self.profiles = next_profiles;
        Ok(())
    }

    /// Save a complete connect-form value. Existing UI-only profile metadata
    /// is preserved when the same profile id is updated.
    pub fn save_connection_settings(
        &mut self,
        profile_id: ProfileId,
        name: String,
        settings: &ConnectionSettings,
    ) -> Result<ProfileSaveOutcome, ProfileMutationError> {
        let previous = self.find_profile(profile_id).cloned();
        let auth = match &settings.auth {
            AuthCredential::Password { password } => ProfileAuthUpdate::Password {
                password: Some(password.clone()),
            },
            AuthCredential::PrivateKey {
                key_path,
                passphrase,
            } => ProfileAuthUpdate::PrivateKey {
                key_path: LocalPath::new(key_path.clone()),
                passphrase: passphrase.clone(),
            },
        };
        let request = ProfileSaveRequest {
            profile_id,
            name,
            host: settings.host.clone(),
            port: settings.port,
            username: settings.username.clone(),
            auth,
            default_remote_path: previous
                .as_ref()
                .and_then(|profile| profile.default_remote_path.clone()),
        };
        self.save_request(request)
    }

    /// Coordinate Keychain and profiles.json with a compensating rollback. A
    /// failed disk write restores the prior Keychain value; obsolete
    /// credentials are only removed after the disk commit succeeds.
    pub fn save_request(
        &mut self,
        request: ProfileSaveRequest,
    ) -> Result<ProfileSaveOutcome, ProfileMutationError> {
        let previous = self.find_profile(request.profile_id).cloned();
        let resolved = Self::resolve_auth(&request, previous.as_ref())?;

        let previous_secret = match &resolved.secret_update {
            Some(update) => self.keychain.load(&update.secret_ref)?.map(Zeroizing::new),
            None => None,
        };
        if let Some(update) = &resolved.secret_update {
            self.keychain.store(&update.secret_ref, update.secret)?;
        }

        let mut profile = ConnectionProfile::new(
            request.profile_id,
            request.name.clone(),
            request.host.clone(),
            request.username.clone(),
            resolved.auth,
        );
        profile.port = request.port;
        profile.default_remote_path = request.default_remote_path.clone();
        if let Some(previous) = &previous {
            profile.group_id = previous.group_id;
            profile.last_local_path = previous.last_local_path.clone();
        }

        let saved = match self.save_profile(profile) {
            Ok(saved) => saved,
            Err(storage) => {
                if let Some(update) = &resolved.secret_update {
                    let rollback = match previous_secret.as_deref() {
                        Some(secret) => self.keychain.store(&update.secret_ref, secret),
                        None => self.keychain.delete(&update.secret_ref),
                    };
                    if let Err(rollback) = rollback {
                        return Err(ProfileMutationError::StorageRollback { storage, rollback });
                    }
                }
                return Err(ProfileMutationError::Storage(storage));
            }
        };

        let current_refs = secret_refs_for_auth(&saved.auth);
        let mut cleanup_warnings = Vec::new();
        if let Some(previous) = previous {
            for orphan in secret_refs_for_auth(&previous.auth) {
                if !current_refs.contains(&orphan)
                    && let Err(error) = self.keychain.delete(&orphan)
                {
                    cleanup_warnings.push(error);
                }
            }
        }

        Ok(ProfileSaveOutcome {
            profile: saved,
            cleanup_warnings,
        })
    }

    pub fn load_connection_settings(
        &self,
        profile_id: ProfileId,
    ) -> Result<ConnectionSettings, ProfileMutationError> {
        let profile = self
            .find_profile(profile_id)
            .ok_or(ProfileMutationError::ProfileNotFound(profile_id))?;
        let auth = match &profile.auth {
            AuthMethod::Password { secret_ref } => {
                let password = self.keychain.load(secret_ref)?.ok_or(
                    ProfileMutationError::CredentialRequired(AuthMethodKind::Password),
                )?;
                AuthCredential::Password { password }
            }
            AuthMethod::PrivateKey {
                key_path,
                passphrase_ref,
                ..
            } => {
                let passphrase = match passphrase_ref {
                    Some(secret_ref) => Some(self.keychain.load(secret_ref)?.ok_or(
                        ProfileMutationError::CredentialRequired(AuthMethodKind::PrivateKey),
                    )?),
                    None => None,
                };
                AuthCredential::PrivateKey {
                    key_path: key_path.as_str().to_string(),
                    passphrase,
                }
            }
        };
        Ok(ConnectionSettings {
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            auth,
        })
    }

    pub fn has_saved_secret(&self, profile_id: ProfileId) -> Result<bool, ProfileMutationError> {
        let profile = self
            .find_profile(profile_id)
            .ok_or(ProfileMutationError::ProfileNotFound(profile_id))?;
        let secret_ref = match &profile.auth {
            AuthMethod::Password { secret_ref } => Some(secret_ref),
            AuthMethod::PrivateKey { passphrase_ref, .. } => passphrase_ref.as_ref(),
        };
        match secret_ref {
            Some(secret_ref) => Ok(self.keychain.load(secret_ref)?.is_some()),
            None => Ok(false),
        }
    }

    pub fn delete_profile_and_credentials(
        &mut self,
        profile_id: ProfileId,
    ) -> Result<ProfileDeleteOutcome, ProfileMutationError> {
        let Some(previous) = self.find_profile(profile_id).cloned() else {
            return Ok(ProfileDeleteOutcome {
                deleted: false,
                cleanup_warnings: Vec::new(),
            });
        };
        self.delete_profile(profile_id)?;
        let mut cleanup_warnings = Vec::new();
        for secret_ref in secret_refs_for_auth(&previous.auth) {
            if let Err(error) = self.keychain.delete(&secret_ref) {
                cleanup_warnings.push(error);
            }
        }
        Ok(ProfileDeleteOutcome {
            deleted: true,
            cleanup_warnings,
        })
    }

    /// Remove the credential behind a profile while leaving its metadata.
    /// This is used by recovery tests and is also suitable for a future
    /// explicit "forget credential" action.
    pub fn delete_profile_credentials(
        &self,
        profile_id: ProfileId,
    ) -> Result<(), ProfileMutationError> {
        let profile = self
            .find_profile(profile_id)
            .ok_or(ProfileMutationError::ProfileNotFound(profile_id))?;
        for secret_ref in secret_refs_for_auth(&profile.auth) {
            self.keychain.delete(&secret_ref)?;
        }
        Ok(())
    }

    fn resolve_auth<'a>(
        request: &'a ProfileSaveRequest,
        previous: Option<&ConnectionProfile>,
    ) -> Result<ResolvedProfileAuth<'a>, ProfileMutationError> {
        match &request.auth {
            ProfileAuthUpdate::Password { password } => {
                let secret_ref = SecretRef::keychain_ref(request.profile_id, "password");
                match password.as_deref() {
                    Some(password) => Ok(ResolvedProfileAuth {
                        auth: AuthMethod::Password {
                            secret_ref: secret_ref.clone(),
                        },
                        secret_update: Some(SecretUpdate {
                            secret_ref,
                            secret: password,
                        }),
                    }),
                    None => match previous.map(|profile| &profile.auth) {
                        Some(AuthMethod::Password { secret_ref }) => Ok(ResolvedProfileAuth {
                            auth: AuthMethod::Password {
                                secret_ref: secret_ref.clone(),
                            },
                            secret_update: None,
                        }),
                        _ => Err(ProfileMutationError::CredentialRequired(
                            AuthMethodKind::Password,
                        )),
                    },
                }
            }
            ProfileAuthUpdate::PrivateKey {
                key_path,
                passphrase,
            } => {
                let (passphrase_ref, secret_update) = match passphrase.as_deref() {
                    Some(passphrase) => {
                        let secret_ref = SecretRef::keychain_ref(request.profile_id, "passphrase");
                        (
                            Some(secret_ref.clone()),
                            Some(SecretUpdate {
                                secret_ref,
                                secret: passphrase,
                            }),
                        )
                    }
                    None => match previous.map(|profile| &profile.auth) {
                        Some(AuthMethod::PrivateKey { passphrase_ref, .. }) => {
                            (passphrase_ref.clone(), None)
                        }
                        _ => (None, None),
                    },
                };
                Ok(ResolvedProfileAuth {
                    auth: AuthMethod::PrivateKey {
                        key_path: key_path.clone(),
                        remember_passphrase: passphrase_ref.is_some(),
                        passphrase_ref,
                    },
                    secret_update,
                })
            }
        }
    }
}

fn secret_refs_for_auth(auth: &AuthMethod) -> Vec<SecretRef> {
    match auth {
        AuthMethod::Password { secret_ref } => vec![secret_ref.clone()],
        AuthMethod::PrivateKey { passphrase_ref, .. } => passphrase_ref.iter().cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{
        AuthCredential, AuthMethod, ConnectionProfile, ConnectionSettings, LocalPath, ProfileId,
        SecretRef,
    };

    use super::{
        ProfileAuthUpdate, ProfileMutationError, ProfileSaveRequest, ProfileStore, ProfilesFile,
        StorageError, core_crate_name, crate_name,
    };

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

    fn password_settings(host: &str, password: &str) -> ConnectionSettings {
        ConnectionSettings {
            host: host.into(),
            port: 22,
            username: "alex".into(),
            auth: AuthCredential::Password {
                password: password.into(),
            },
        }
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
    fn failed_disk_save_keeps_profile_memory_unchanged() {
        let path = temp_profiles_path("memory-transaction");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        store
            .save_profile(password_profile(1, "Before"))
            .expect("seed profile");
        store.path = LocalPath::new("/nonexistent-dir-xyz/macsftp/profiles.json");

        let result = store.save_profile(password_profile(1, "After"));

        assert!(matches!(result, Err(StorageError::Io { .. })));
        let retained = store.find_profile(ProfileId(1)).expect("profile retained");
        assert_eq!(retained.name, "Before");
        assert_eq!(retained.revision, 1);
        cleanup(&path);
    }

    #[test]
    fn failed_profile_update_restores_previous_keychain_value() {
        let path = temp_profiles_path("credential-rollback");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        store
            .save_connection_settings(
                ProfileId(1),
                "Before".into(),
                &password_settings("before.example.com", "old-secret"),
            )
            .expect("seed profile and credential");
        store.path = LocalPath::new("/nonexistent-dir-xyz/macsftp/profiles.json");

        let result = store.save_connection_settings(
            ProfileId(1),
            "After".into(),
            &password_settings("after.example.com", "new-secret"),
        );

        assert!(matches!(result, Err(ProfileMutationError::Storage(_))));
        let retained = store
            .load_connection_settings(ProfileId(1))
            .expect("old profile and credential remain readable");
        assert_eq!(retained.host, "before.example.com");
        let AuthCredential::Password { password } = &retained.auth else {
            panic!("expected password auth");
        };
        assert_eq!(password, "old-secret");
        cleanup(&path);
    }

    #[test]
    fn auth_method_switch_removes_obsolete_secret_after_commit() {
        let path = temp_profiles_path("auth-switch-cleanup");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        store
            .save_connection_settings(
                ProfileId(1),
                "Server".into(),
                &password_settings("example.com", "old-secret"),
            )
            .expect("seed password profile");
        let old_ref = SecretRef::keychain_ref(ProfileId(1), "password");

        let outcome = store
            .save_request(ProfileSaveRequest {
                profile_id: ProfileId(1),
                name: "Server".into(),
                host: "example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: ProfileAuthUpdate::PrivateKey {
                    key_path: LocalPath::new("~/.ssh/id_ed25519"),
                    passphrase: None,
                },
                default_remote_path: None,
            })
            .expect("switch auth method");

        assert!(outcome.cleanup_warnings.is_empty());
        assert_eq!(
            store.keychain.load(&old_ref).expect("inspect old secret"),
            None,
            "obsolete password is removed only after the new profile commits"
        );
        cleanup(&path);
    }

    #[test]
    fn failed_new_profile_save_removes_unreferenced_secret() {
        let path = LocalPath::new("/nonexistent-dir-xyz/macsftp/profiles.json");
        let mut store = ProfileStore::open_or_empty_memory(path);
        let secret_ref = SecretRef::keychain_ref(ProfileId(7), "password");

        let result = store.save_connection_settings(
            ProfileId(7),
            "Server".into(),
            &password_settings("example.com", "temporary-secret"),
        );

        assert!(matches!(result, Err(ProfileMutationError::Storage(_))));
        assert_eq!(
            store
                .keychain
                .load(&secret_ref)
                .expect("inspect rolled-back secret"),
            None
        );
        assert!(store.profiles().is_empty());
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
