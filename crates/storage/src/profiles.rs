use macsftp_core::{
    AuthCredential, AuthFingerprint, AuthMethod, AuthMethodKind, ConnectionProfile,
    ConnectionSettings, LocalPath, ProfileId, RemotePath, SecretRef,
};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::file_lock::FileLock;
use crate::keychain::{KeychainError, KeychainStore};
use crate::profile_file::{PhasedSaveError, ProfilesFile, StorageError, TransactionPhase};

/// Disk-backed profile store: owns the `profiles.json` path and the
/// in-memory `ProfilesFile`, keeping them in sync on every write.
pub struct ProfileStore {
    path: LocalPath,
    profiles: ProfilesFile,
    keychain: KeychainStore,
    initial_error: Option<StorageError>,
    /// Test-only injection point for the commit step, letting tests
    /// simulate a commit that lands without proving durability.
    #[cfg(test)]
    commit_save_override: Option<CommitSaveOverride>,
}

#[cfg(test)]
type CommitSaveOverride = Box<dyn FnMut(&ProfilesFile, &LocalPath) -> Result<(), PhasedSaveError>>;

/// Passphrase portion of a private-key profile update. Replaces an older
/// `Option<String>` that could only express "set and remember" or "keep
/// existing" — clearing a saved passphrase or choosing not to remember one
/// was inexpressible, so a mistyped passphrase stayed in the Keychain
/// forever.
pub enum PrivateKeyPassphraseUpdate {
    /// Leave the profile's current passphrase state untouched.
    KeepExisting,
    /// The key is not encrypted; drop any remembered passphrase.
    NoPassphrase,
    /// The key needs a passphrase, but it must not be persisted. The typed
    /// value is discarded here — it is never written anywhere.
    NotRemembered(String),
    /// Remember the passphrase in the Keychain.
    Remember(String),
}

impl std::fmt::Debug for PrivateKeyPassphraseUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeepExisting => formatter.write_str("KeepExisting"),
            Self::NoPassphrase => formatter.write_str("NoPassphrase"),
            Self::NotRemembered(_) => formatter.write_str("NotRemembered([REDACTED])"),
            Self::Remember(_) => formatter.write_str("Remember([REDACTED])"),
        }
    }
}

impl Drop for PrivateKeyPassphraseUpdate {
    fn drop(&mut self) {
        match self {
            Self::NotRemembered(secret) | Self::Remember(secret) => secret.zeroize(),
            Self::KeepExisting | Self::NoPassphrase => {}
        }
    }
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
        passphrase: PrivateKeyPassphraseUpdate,
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
                .field("passphrase", passphrase)
                .finish(),
        }
    }
}

impl Drop for ProfileAuthUpdate {
    fn drop(&mut self) {
        match self {
            Self::Password { password } => password.zeroize(),
            // The nested update zeroizes its own secret values.
            Self::PrivateKey { .. } => {}
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
    /// The profile and its active credential were committed, but follow-up
    /// work failed: an obsolete Keychain credential could not be removed or
    /// the disk write committed without proving durability. Callers should
    /// log these warnings.
    pub cleanup_warnings: Vec<StorageWarning>,
}

pub struct ProfileDeleteOutcome {
    pub deleted: bool,
    /// The disk entry was deleted, but a now-unreferenced Keychain item could
    /// not be removed. Callers should log these warnings.
    pub cleanup_warnings: Vec<KeychainError>,
}

/// Non-fatal problem surfaced after a successful commit.
#[derive(Debug, Clone)]
pub enum StorageWarning {
    /// The replacement landed on disk but the parent directory could not be
    /// synced, so durability is unproven (data survives process death but
    /// possibly not power loss).
    DurabilityNotProven(String),
    /// A leftover temporary file could not be removed.
    TempCleanup { path: String, message: String },
    /// An obsolete Keychain credential could not be removed.
    Keychain(KeychainError),
}

struct ResolvedProfileAuth<'a> {
    auth: AuthMethod,
    secret_update: Option<SecretUpdate<'a>>,
}

struct SecretUpdate<'a> {
    secret_ref: SecretRef,
    secret: &'a str,
}

enum TransactionKind {
    Save { profile: Box<ConnectionProfile> },
    Delete { profile_id: ProfileId },
}

enum TransactionOutcome {
    /// The change was applied and the file committed. `warnings` carries
    /// non-fatal follow-up problems (temp cleanup, unproven durability).
    Committed { warnings: Vec<StorageWarning> },
    /// Delete found nothing to remove (idempotent no-op).
    Unchanged,
}

impl std::fmt::Display for StorageWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DurabilityNotProven(message) => {
                write!(
                    formatter,
                    "profile committed without durability proof: {message}"
                )
            }
            Self::TempCleanup { path, message } => {
                write!(
                    formatter,
                    "could not remove temporary file {path}: {message}"
                )
            }
            Self::Keychain(error) => error.fmt(formatter),
        }
    }
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
            initial_error: None,
            #[cfg(test)]
            commit_save_override: None,
        })
    }

    /// Like `open`, but never fails: a missing or unreadable
    /// `profiles.json` yields an empty in-memory store. An unreadable file
    /// blocks writes so launch remains possible without destroying recovery
    /// data.
    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(error) => Self {
                path,
                profiles: ProfilesFile::new(),
                keychain: KeychainStore::new_os(),
                initial_error: Some(error),
                #[cfg(test)]
                commit_save_override: None,
            },
        }
    }

    /// Test-only behavior without touching the user's login Keychain. This is
    /// public because `macsftp-app` integration tests compile as a separate
    /// crate and need an isolated storage boundary.
    pub fn open_or_empty_memory(path: LocalPath) -> Self {
        match ProfilesFile::load(&path) {
            Ok(profiles) => Self {
                path,
                profiles,
                keychain: KeychainStore::new_memory(),
                initial_error: None,
                #[cfg(test)]
                commit_save_override: None,
            },
            Err(error) => Self {
                path,
                profiles: ProfilesFile::new(),
                keychain: KeychainStore::new_memory(),
                initial_error: Some(error),
                #[cfg(test)]
                commit_save_override: None,
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn profiles(&self) -> &[ConnectionProfile] {
        &self.profiles.profiles
    }

    pub fn initial_error(&self) -> Option<&StorageError> {
        self.initial_error.as_ref()
    }

    fn ensure_writable(&self) -> Result<(), StorageError> {
        if self.initial_error.is_some() {
            return Err(StorageError::RecoveryRequired {
                path: self.path.as_str().to_string(),
            });
        }
        Ok(())
    }

    pub fn find_profile(&self, profile_id: ProfileId) -> Option<&ConnectionProfile> {
        self.profiles.find_profile(profile_id)
    }

    /// Build the non-secret identity used by the SFTP connection pool. This
    /// intentionally reads only profile metadata; Keychain values never take
    /// part in connection reuse decisions.
    pub fn auth_fingerprint(&self, profile_id: ProfileId) -> Option<AuthFingerprint> {
        let profile = self.find_profile(profile_id)?;
        match &profile.auth {
            AuthMethod::Password { secret_ref } => Some(AuthFingerprint::password(
                secret_ref.clone(),
                profile.revision,
            )),
            AuthMethod::PrivateKey {
                key_path,
                passphrase_ref,
                ..
            } => {
                let path_hash = format!("{:x}", Sha256::digest(key_path.as_str().as_bytes()));
                Some(AuthFingerprint::private_key(
                    path_hash,
                    passphrase_ref.clone(),
                    profile.revision,
                ))
            }
        }
    }

    /// Allocate a fresh profile id from the persisted high-water mark. The
    /// mark only ever moves forward — deleting the highest-id profile (or all
    /// profiles) never causes ids to be reused, because recents and session
    /// state hold `profile_id` as a stable identity.
    pub fn next_profile_id(&self) -> Result<ProfileId, ProfileMutationError> {
        Ok(ProfileId(self.profiles.next_profile_high_water()))
    }

    /// Insert or update a profile and flush the whole store to disk.
    pub fn save_profile(
        &mut self,
        profile: ConnectionProfile,
    ) -> Result<ConnectionProfile, StorageError> {
        self.ensure_writable()?;
        let profile_id = profile.id;
        match self.commit_transaction(TransactionKind::Save {
            profile: Box::new(profile),
        }) {
            Ok(TransactionOutcome::Committed { .. }) => Ok(self
                .find_profile(profile_id)
                .expect("just-saved profile is present")
                .clone()),
            Ok(TransactionOutcome::Unchanged) => {
                unreachable!("save transactions always produce a saved profile")
            }
            Err((error, _warnings)) => Err(error),
        }
    }

    /// Remove a profile by id and flush. Removing a non-existent id is a
    /// no-op that still succeeds (idempotent).
    pub fn delete_profile(&mut self, profile_id: ProfileId) -> Result<(), StorageError> {
        self.ensure_writable()?;
        match self.commit_transaction(TransactionKind::Delete { profile_id }) {
            Ok(TransactionOutcome::Committed { .. }) | Ok(TransactionOutcome::Unchanged) => Ok(()),
            Err((error, _warnings)) => Err(error),
        }
    }

    /// Hold an advisory lock beside `profiles.json` for one write
    /// transaction so concurrent store instances cannot silently overwrite
    /// each other's changes.
    fn acquire_transaction_lock(&self) -> Result<FileLock, StorageError> {
        FileLock::acquire(std::path::Path::new(self.path.as_str())).map_err(|error| {
            StorageError::Io {
                path: format!("{}.lock", self.path.as_str()),
                message: error.to_string(),
            }
        })
    }

    /// One full write transaction: acquire the advisory lock, re-read the
    /// authoritative on-disk state, apply the change to that fresh state,
    /// and commit. The in-memory snapshot is only replaced once the commit
    /// landed. A commit whose durability is unproven still counts as
    /// committed — only its warnings say so — because compensating actions
    /// (such as rolling back a Keychain secret) would desynchronize disk and
    /// credentials.
    fn commit_transaction(
        &mut self,
        transaction: TransactionKind,
    ) -> Result<TransactionOutcome, (StorageError, Vec<StorageWarning>)> {
        let _lock = self
            .acquire_transaction_lock()
            .map_err(|error| (error, Vec::new()))?;
        let mut next_profiles =
            ProfilesFile::load(&self.path).map_err(|error| (error, Vec::new()))?;
        let warnings = cleanup_stale_temp_files(&self.path);
        // Apply the change to the freshly reloaded state.
        match &transaction {
            TransactionKind::Save { profile } => {
                // `self.profiles` is the caller's snapshot; the freshly
                // reloaded file is the authoritative state. An id that was
                // absent from the snapshot but present on disk means another
                // store instance created it in between: a save assuming
                // "new" must conflict instead of silently overwriting.
                if self.profiles.find_profile(profile.id).is_none()
                    && next_profiles.find_profile(profile.id).is_some()
                {
                    return Err((
                        StorageError::ConcurrentModification {
                            message: format!(
                                "profile {} was created by another store instance; reload and retry",
                                profile.id.0
                            ),
                        },
                        warnings,
                    ));
                }
                if next_profiles.find_profile(profile.id).is_none() {
                    if profile.id.0 < next_profiles.next_profile_high_water() {
                        return Err((
                            StorageError::ConcurrentModification {
                                message: format!(
                                    "profile id {} was already handed out by another store instance; reload and retry",
                                    profile.id.0
                                ),
                            },
                            warnings,
                        ));
                    }
                    // Adopting a brand-new id moves the high-water mark past
                    // it; ids already handed out stay reserved even before
                    // the profile itself exists on disk.
                    next_profiles.next_profile_id = Some(profile.id.0 + 1);
                }
                next_profiles
                    .save_profile((**profile).clone())
                    .map_err(|error| (error, warnings.clone()))?;
            }
            TransactionKind::Delete { profile_id } => {
                if next_profiles.find_profile(*profile_id).is_none() {
                    return Ok(TransactionOutcome::Unchanged);
                }
                next_profiles
                    .profiles
                    .retain(|profile| profile.id != *profile_id);
            }
        }
        match self.commit_save(&next_profiles, warnings) {
            Ok(warnings) => {
                self.profiles = next_profiles;
                Ok(TransactionOutcome::Committed { warnings })
            }
            Err(failure) => Err(failure),
        }
    }

    /// Commit the already-mutated file state. Split from
    /// `commit_transaction` so tests can inject a failing parent-directory
    /// sync and exercise the durability-degraded commit path.
    fn commit_save(
        &mut self,
        next_profiles: &ProfilesFile,
        warnings: Vec<StorageWarning>,
    ) -> Result<Vec<StorageWarning>, (StorageError, Vec<StorageWarning>)> {
        #[cfg(test)]
        if self.commit_save_override.is_some() {
            let mut save = self.commit_save_override.take().expect("override checked");
            let result =
                self.commit_save_with_sync(next_profiles, warnings, |file, path| save(file, path));
            self.commit_save_override = Some(save);
            return result;
        }
        self.commit_save_with_sync(next_profiles, warnings, ProfilesFile::save_phased)
    }

    fn commit_save_with_sync(
        &self,
        next_profiles: &ProfilesFile,
        warnings: Vec<StorageWarning>,
        save: impl FnOnce(&ProfilesFile, &LocalPath) -> Result<(), PhasedSaveError>,
    ) -> Result<Vec<StorageWarning>, (StorageError, Vec<StorageWarning>)> {
        match save(next_profiles, &self.path) {
            Ok(()) => Ok(warnings),
            Err(save_error) => match save_error.phase() {
                TransactionPhase::NotCommitted => Err((save_error.into_storage_error(), warnings)),
                TransactionPhase::ReplacedButNotDurable => {
                    let mut degraded_warnings = Vec::with_capacity(warnings.len() + 1);
                    degraded_warnings.extend(warnings);
                    degraded_warnings.push(save_error.into_durability_warning());
                    Ok(degraded_warnings)
                }
            },
        }
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
                passphrase: match &passphrase {
                    Some(passphrase) => PrivateKeyPassphraseUpdate::Remember(passphrase.clone()),
                    None => PrivateKeyPassphraseUpdate::KeepExisting,
                },
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

    /// Coordinate Keychain and profiles.json with a compensating rollback.
    /// A disk write that never committed (failed before the rename) restores
    /// the prior Keychain value; a write that replaced the file but could
    /// not prove durability keeps the new secret in sync with what is now on
    /// disk and only reports a durability warning. Obsolete credentials are
    /// only removed after the disk commit succeeds.
    pub fn save_request(
        &mut self,
        request: ProfileSaveRequest,
    ) -> Result<ProfileSaveOutcome, ProfileMutationError> {
        self.ensure_writable()?;
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
            resolved.auth.clone(),
        );
        profile.port = request.port;
        profile.default_remote_path = request.default_remote_path.clone();
        if let Some(previous) = &previous {
            profile.group_id = previous.group_id;
            profile.last_local_path = previous.last_local_path.clone();
        }

        let saved = match self.commit_transaction(TransactionKind::Save {
            profile: Box::new(profile),
        }) {
            Ok(TransactionOutcome::Committed { warnings }) => {
                // The commit landed (possibly without proven durability, in
                // which case the transaction already attached a warning).
                // Rolling back the Keychain here would leave disk and
                // credentials permanently inconsistent.
                let committed_profile = self
                    .find_profile(request.profile_id)
                    .expect("just-committed profile is present")
                    .clone();
                (committed_profile, warnings)
            }
            Ok(TransactionOutcome::Unchanged) => {
                return Err(ProfileMutationError::ProfileNotFound(request.profile_id));
            }
            Err((storage_error, _warnings)) => {
                // The rename never landed: disk still holds the previous
                // contents, so the new secret must not outlive them.
                if let Some(update) = &resolved.secret_update {
                    let rollback = match previous_secret.as_deref() {
                        Some(secret) => self.keychain.store(&update.secret_ref, secret),
                        None => self.keychain.delete(&update.secret_ref),
                    };
                    if let Err(rollback) = rollback {
                        return Err(ProfileMutationError::StorageRollback {
                            storage: storage_error,
                            rollback,
                        });
                    }
                }
                return Err(ProfileMutationError::Storage(storage_error));
            }
        };

        let current_refs = secret_refs_for_auth(&saved.0.auth);
        let mut cleanup_warnings = saved.1;
        if let Some(previous) = previous {
            for orphan in secret_refs_for_auth(&previous.auth) {
                if !current_refs.contains(&orphan)
                    && let Err(error) = self.keychain.delete(&orphan)
                {
                    cleanup_warnings.push(StorageWarning::Keychain(error));
                }
            }
        }

        Ok(ProfileSaveOutcome {
            profile: saved.0,
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
                let previous_passphrase = match previous.map(|profile| &profile.auth) {
                    Some(AuthMethod::PrivateKey {
                        has_passphrase,
                        passphrase_ref,
                        ..
                    }) => Some((*has_passphrase, passphrase_ref.clone())),
                    // No previous private-key state to keep.
                    _ => None,
                };
                let (has_passphrase, passphrase_ref, secret_update) = match passphrase {
                    PrivateKeyPassphraseUpdate::KeepExisting => match previous_passphrase {
                        Some((kept_has_passphrase, kept_ref)) => {
                            (kept_has_passphrase, kept_ref, None)
                        }
                        None => (false, None, None),
                    },
                    // Dropping the ref orphans any remembered Keychain item;
                    // post-commit orphan cleanup removes it.
                    PrivateKeyPassphraseUpdate::NoPassphrase => (false, None, None),
                    // The typed value is deliberately discarded here.
                    PrivateKeyPassphraseUpdate::NotRemembered(_) => (true, None, None),
                    PrivateKeyPassphraseUpdate::Remember(secret) => {
                        let secret_ref = SecretRef::keychain_ref(request.profile_id, "passphrase");
                        (
                            true,
                            Some(secret_ref.clone()),
                            Some(SecretUpdate { secret_ref, secret }),
                        )
                    }
                };
                Ok(ResolvedProfileAuth {
                    auth: AuthMethod::PrivateKey {
                        key_path: key_path.clone(),
                        has_passphrase,
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

/// Best-effort removal of this store's leftover atomic-write temp files
/// (named `.{file}.tmp-{pid}-{sequence}`, see `temp_path_for`). Removal
/// failures are reported as warnings instead of being silently dropped.
fn cleanup_stale_temp_files(path: &LocalPath) -> Vec<StorageWarning> {
    let mut warnings = Vec::new();
    let Some(file_name) = std::path::Path::new(path.as_str()).file_name() else {
        return warnings;
    };
    let prefix = format!(".{}.tmp-", file_name.to_string_lossy());
    let entries = match std::fs::read_dir(
        std::path::Path::new(path.as_str())
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    ) {
        Ok(entries) => entries,
        Err(_) => return warnings,
    };
    for entry in entries.flatten() {
        let candidate = entry.file_name().to_string_lossy().into_owned();
        if !candidate.starts_with(&prefix) {
            continue;
        }
        if let Err(error) = std::fs::remove_file(entry.path()) {
            warnings.push(StorageWarning::TempCleanup {
                path: entry.path().display().to_string(),
                message: error.to_string(),
            });
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{
        AuthCredential, AuthMethod, AuthMethodKind, ConnectionProfile, ConnectionSettings,
        LocalPath, ProfileId, SecretRef,
    };

    use crate::{ProfilesFile, StorageError, core_crate_name, crate_name};

    use super::{
        PrivateKeyPassphraseUpdate, ProfileAuthUpdate, ProfileMutationError, ProfileSaveRequest,
        ProfileStore, StorageWarning,
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
                secret_ref: SecretRef::keychain_ref(ProfileId(id), "password"),
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

    fn private_key_save_request(
        id: u64,
        passphrase: PrivateKeyPassphraseUpdate,
    ) -> ProfileSaveRequest {
        ProfileSaveRequest {
            profile_id: ProfileId(id),
            name: "Keyed".into(),
            host: "example.com".into(),
            port: 22,
            username: "alex".into(),
            auth: ProfileAuthUpdate::PrivateKey {
                key_path: LocalPath::new("~/.ssh/id_ed25519"),
                passphrase,
            },
            default_remote_path: None,
        }
    }

    fn stored_private_key_state(store: &ProfileStore, id: u64) -> (bool, Option<SecretRef>) {
        match &store
            .find_profile(ProfileId(id))
            .expect("profile exists")
            .auth
        {
            AuthMethod::PrivateKey {
                has_passphrase,
                passphrase_ref,
                ..
            } => (*has_passphrase, passphrase_ref.clone()),
            other => panic!("expected private-key auth, got {other:?}"),
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
    fn auth_fingerprint_changes_with_profile_revision_without_secret_values() {
        let path = temp_profiles_path("auth-fingerprint-revision");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        let first = store
            .save_profile(password_profile(1, "Production"))
            .expect("save first profile revision");
        let first_fingerprint = store
            .auth_fingerprint(first.id)
            .expect("saved profile has a fingerprint");

        let second = store
            .save_profile(first)
            .expect("save second profile revision");
        let second_fingerprint = store
            .auth_fingerprint(second.id)
            .expect("updated profile has a fingerprint");

        assert_eq!(first_fingerprint.method, AuthMethodKind::Password);
        assert_ne!(first_fingerprint, second_fingerprint);
        assert_eq!(second_fingerprint.profile_revision, 2);
        let debug = format!("{second_fingerprint:?}");
        assert!(!debug.contains("password-value"));
        cleanup(&path);
    }

    #[test]
    fn private_key_fingerprint_distinguishes_paths_without_retaining_them() {
        let path = temp_profiles_path("auth-fingerprint-key-path");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        let first_path = "/Users/alex/.ssh/first_ed25519";
        let second_path = "/Users/alex/.ssh/second_ed25519";
        let first = store
            .save_profile(ConnectionProfile::new(
                ProfileId(1),
                "First",
                "example.com",
                "alex",
                AuthMethod::PrivateKey {
                    key_path: LocalPath::new(first_path),
                    has_passphrase: false,
                    passphrase_ref: None,
                },
            ))
            .expect("save first key profile");
        let second = store
            .save_profile(ConnectionProfile::new(
                ProfileId(2),
                "Second",
                "example.com",
                "alex",
                AuthMethod::PrivateKey {
                    key_path: LocalPath::new(second_path),
                    has_passphrase: false,
                    passphrase_ref: None,
                },
            ))
            .expect("save second key profile");

        let first_fingerprint = store
            .auth_fingerprint(first.id)
            .expect("first key profile has a fingerprint");
        let second_fingerprint = store
            .auth_fingerprint(second.id)
            .expect("second key profile has a fingerprint");

        assert_ne!(first_fingerprint, second_fingerprint);
        let debug = format!("{first_fingerprint:?}");
        assert!(!debug.contains(first_path));
        assert!(!debug.contains(second_path));
        cleanup(&path);
    }

    #[test]
    fn save_existing_profile_reports_revision_overflow() {
        let mut profiles = ProfilesFile {
            version: ProfilesFile::CURRENT_VERSION,
            profiles: vec![ConnectionProfile {
                revision: u64::MAX,
                ..password_profile(1, "Production")
            }],
            next_profile_id: None,
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
    fn corrupt_profile_file_blocks_writes_and_is_preserved() {
        let path = temp_profiles_path("corrupt-write-block");
        cleanup(&path);
        std::fs::write(path.as_str(), "{not json").expect("write corrupt profile fixture");
        let mut store = ProfileStore::open_or_empty_memory(path.clone());

        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save_profile(password_profile(1, "Replacement")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("read preserved profile fixture"),
            "{not json"
        );
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
                    passphrase: PrivateKeyPassphraseUpdate::KeepExisting,
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

    // --- Batch A regression tests ---

    /// Build a v1 profiles.json body (no high-water mark field) with the
    /// given profile entries.
    fn v1_file_body(profiles_json: &str) -> String {
        format!(r#"{{"version": 1, "profiles": [{profiles_json}]}}"#)
    }

    fn v2_password_profile_json(id: u64, name: &str) -> String {
        format!(
            r#"{{"id": {id}, "revision": 1, "name": "{name}", "host": "example.com", "port": 22,
                "username": "alex",
                "auth": {{"Password": {{"secret_ref": "keychain:macsftp:{id}:password"}}}},
                "default_remote_path": null, "group_id": null, "last_local_path": null}}"#
        )
    }

    #[test]
    fn unsupported_version_blocks_writes_and_preserves_bytes() {
        let path = temp_profiles_path("unsupported-version");
        cleanup(&path);
        // Version 999 plus an unknown field: valid JSON that a future
        // release might write.
        let future_body = r#"{"version": 999, "profiles": [], "future_field": {"a": 1}}"#;
        std::fs::write(path.as_str(), future_body).expect("write future version fixture");
        let raw_before = std::fs::read(path.as_str()).expect("read fixture bytes before opening");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());

        assert!(
            matches!(
                store.initial_error(),
                Some(StorageError::UnsupportedVersion {
                    found: 999,
                    supported: ProfilesFile::CURRENT_VERSION
                })
            ),
            "initial error must name the unsupported version, got {:?}",
            store.initial_error()
        );
        assert!(
            store.profiles().is_empty(),
            "unsupported file must load as empty"
        );
        assert!(matches!(
            store.save_profile(password_profile(1, "Replacement")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        let raw_after = std::fs::read(path.as_str()).expect("read fixture bytes after writes");
        assert_eq!(
            raw_before, raw_after,
            "the original bytes must never be rewritten"
        );
        cleanup(&path);
    }

    #[test]
    fn duplicate_profile_ids_block_writes_and_preserve_bytes() {
        let path = temp_profiles_path("duplicate-ids");
        cleanup(&path);
        let body = v1_file_body(&format!(
            "{}, {}",
            v2_password_profile_json(1, "First"),
            v2_password_profile_json(1, "Second")
        ));
        std::fs::write(path.as_str(), body.clone()).expect("write duplicate id fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save_profile(password_profile(3, "Replacement")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("reread fixture"),
            body,
            "corrupt input must be preserved verbatim"
        );
        cleanup(&path);
    }

    #[test]
    fn zero_revision_blocks_writes_and_preserve_bytes() {
        let path = temp_profiles_path("zero-revision");
        cleanup(&path);
        let body = v1_file_body(
            &v2_password_profile_json(4, "Zero").replace("\"revision\": 1", "\"revision\": 0"),
        );
        std::fs::write(path.as_str(), body.clone()).expect("write zero revision fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save_profile(password_profile(5, "Replacement")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("reread fixture"),
            body
        );
        cleanup(&path);
    }

    #[test]
    fn mismatched_secret_ref_blocks_writes() {
        let path = temp_profiles_path("mismatched-ref");
        cleanup(&path);
        // Password ref points at another profile's Keychain entry.
        let body = v1_file_body(
            &v2_password_profile_json(6, "Borrowed")
                .replace("keychain:macsftp:6:password", "keychain:macsftp:7:password"),
        );
        std::fs::write(path.as_str(), body.clone()).expect("write mismatched ref fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save_profile(password_profile(8, "Replacement")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("reread fixture"),
            body
        );
        cleanup(&path);
    }

    fn legacy_private_key_profile_json(auth_json: &str) -> String {
        format!(
            r#"{{"id": 9, "revision": 1, "name": "Flagged", "host": "example.com", "port": 22,
                "username": "alex", "auth": {auth_json},
                "default_remote_path": null, "group_id": null, "last_local_path": null}}"#
        )
    }

    /// A remembered passphrase in a pre-tri-state file migrates in memory:
    /// the flag that used to be spelled `remember_passphrase: true` becomes
    /// `has_passphrase: true`, and the Keychain ref is kept untouched.
    #[test]
    fn legacy_remembered_passphrase_migrates_to_has_passphrase() {
        let path = temp_profiles_path("legacy-remember-migrates");
        cleanup(&path);
        let auth = r#"{"PrivateKey": {"key_path": "~/.ssh/id_ed25519",
            "passphrase_ref": "keychain:macsftp:9:passphrase", "remember_passphrase": true}}"#;
        let body = format!(
            r#"{{"version": 2, "profiles": [{}]}}"#,
            legacy_private_key_profile_json(auth)
        );
        std::fs::write(path.as_str(), body.clone()).expect("write legacy fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(store.initial_error().is_none(), "legacy file must load");
        match &store.find_profile(ProfileId(9)).expect("loaded").auth {
            AuthMethod::PrivateKey {
                has_passphrase,
                passphrase_ref,
                ..
            } => {
                assert!(*has_passphrase);
                assert_eq!(
                    passphrase_ref.as_ref().map(SecretRef::as_str),
                    Some("keychain:macsftp:9:passphrase")
                );
            }
            other => panic!("expected private-key auth, got {other:?}"),
        }

        // The next save rewrites the file in the current format.
        store
            .save_profile(password_profile(10, "Trigger Rewrite"))
            .expect("rewrite triggers format upgrade");
        let rewritten = std::fs::read_to_string(path.as_str()).expect("reread rewritten file");
        assert!(rewritten.contains(r#""version": 3"#));
        assert!(rewritten.contains(r#""has_passphrase": true"#));
        assert!(!rewritten.contains("remember_passphrase"));
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    /// A pre-tri-state file whose remember flag contradicted its refs used
    /// to be rejected as Corrupt. The tri-state model can represent every
    /// state that flag combination could mean, and the contradictory state
    /// carries no recoverable data (the ref was already absent), so loading
    /// it as "no passphrase" beats bricking the store over it.
    #[test]
    fn legacy_contradictory_remember_flag_loads_as_no_passphrase() {
        let path = temp_profiles_path("legacy-contradictory-flag");
        cleanup(&path);
        let auth = r#"{"PrivateKey": {"key_path": "~/.ssh/id_ed25519",
            "passphrase_ref": null, "remember_passphrase": true}}"#;
        let body = format!(
            r#"{{"version": 2, "profiles": [{}]}}"#,
            legacy_private_key_profile_json(auth)
        );
        std::fs::write(path.as_str(), body.clone()).expect("write legacy fixture");

        let store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(
            store.initial_error().is_none(),
            "contradiction must not brick"
        );
        assert_eq!(stored_private_key_state(&store, 9), (false, None));
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    /// Current-format files must still uphold the invariant the old flag
    /// check guaranteed: a Keychain ref without `has_passphrase` is corrupt
    /// and blocks writes instead of being silently repaired.
    #[test]
    fn v3_ref_without_has_passphrase_blocks_writes() {
        let path = temp_profiles_path("v3-ref-without-flag");
        cleanup(&path);
        let auth = r#"{"PrivateKey": {"key_path": "~/.ssh/id_ed25519",
            "has_passphrase": false,
            "passphrase_ref": "keychain:macsftp:9:passphrase"}}"#;
        let body = format!(
            r#"{{"version": 3, "profiles": [{}]}}"#,
            legacy_private_key_profile_json(auth)
        );
        std::fs::write(path.as_str(), body.clone()).expect("write v3 fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save_profile(password_profile(11, "Replacement")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("reread fixture"),
            body
        );
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    /// Regression for the profiles-audit tri-state defect: a saved
    /// passphrase could never be un-remembered because the update API only
    /// expressed "set and remember" or "keep existing".
    #[test]
    fn private_key_passphrase_not_remembered_drops_secret_and_keeps_flag() {
        let path = temp_profiles_path("passphrase-not-remembered");
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
        let mut store = ProfileStore::open_or_empty_memory(path.clone());

        store
            .save_request(private_key_save_request(
                1,
                PrivateKeyPassphraseUpdate::Remember("old-secret".into()),
            ))
            .expect("seed remembered passphrase");
        assert!(
            store
                .has_saved_secret(ProfileId(1))
                .expect("keychain query")
        );

        store
            .save_request(private_key_save_request(
                1,
                PrivateKeyPassphraseUpdate::NotRemembered("typed-secret".into()),
            ))
            .expect("switch to ask-every-time");

        // The profile keeps the fact that the key needs a passphrase, but no
        // secret may remain in the Keychain — including the newly typed one.
        assert_eq!(stored_private_key_state(&store, 1), (true, None));
        assert!(
            !store
                .has_saved_secret(ProfileId(1))
                .expect("keychain query")
        );
        let settings = store
            .load_connection_settings(ProfileId(1))
            .expect("settings load without a stored secret");
        let AuthCredential::PrivateKey { passphrase, .. } = &settings.auth else {
            panic!("expected private-key credential")
        };
        assert!(passphrase.is_none());

        // Disk agrees with memory.
        let reloaded = ProfileStore::open_or_empty_memory(path.clone());
        assert_eq!(stored_private_key_state(&reloaded, 1), (true, None));
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    #[test]
    fn private_key_no_passphrase_update_clears_remembered_secret() {
        let path = temp_profiles_path("passphrase-none");
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
        let mut store = ProfileStore::open_or_empty_memory(path.clone());

        store
            .save_request(private_key_save_request(
                1,
                PrivateKeyPassphraseUpdate::Remember("old-secret".into()),
            ))
            .expect("seed remembered passphrase");

        store
            .save_request(private_key_save_request(
                1,
                PrivateKeyPassphraseUpdate::NoPassphrase,
            ))
            .expect("declare key passphrase-less");

        assert_eq!(stored_private_key_state(&store, 1), (false, None));
        assert!(
            !store
                .has_saved_secret(ProfileId(1))
                .expect("keychain query")
        );
        let reloaded = ProfileStore::open_or_empty_memory(path.clone());
        assert_eq!(stored_private_key_state(&reloaded, 1), (false, None));
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    #[test]
    fn keep_existing_preserves_not_remembered_passphrase_state() {
        let path = temp_profiles_path("passphrase-keep");
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
        let mut store = ProfileStore::open_or_empty_memory(path.clone());

        store
            .save_request(private_key_save_request(
                1,
                PrivateKeyPassphraseUpdate::NotRemembered("typed-secret".into()),
            ))
            .expect("seed ask-every-time state");

        // An unrelated rename must not resurrect a Keychain ref or drop the
        // has-passphrase flag.
        store
            .save_request(ProfileSaveRequest {
                profile_id: ProfileId(1),
                name: "Renamed".into(),
                host: "example.com".into(),
                port: 22,
                username: "alex".into(),
                auth: ProfileAuthUpdate::PrivateKey {
                    key_path: LocalPath::new("~/.ssh/id_ed25519"),
                    passphrase: PrivateKeyPassphraseUpdate::KeepExisting,
                },
                default_remote_path: None,
            })
            .expect("rename with untouched passphrase policy");

        assert_eq!(stored_private_key_state(&store, 1), (true, None));
        assert_eq!(
            store.find_profile(ProfileId(1)).expect("kept").name,
            "Renamed"
        );
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    #[test]
    fn deleting_highest_id_keeps_next_id_above_water_mark() {
        let path = temp_profiles_path("high-water-delete");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        store
            .save_profile(password_profile(1, "One"))
            .expect("save profile one");
        store
            .save_profile(password_profile(2, "Two"))
            .expect("save profile two");
        assert_eq!(
            store.next_profile_id().expect("allocate next id"),
            ProfileId(3)
        );

        store.delete_profile(ProfileId(2)).expect("delete highest");
        assert_eq!(
            store.next_profile_id().expect("allocate after delete"),
            ProfileId(3),
            "deleting the highest profile must not recycle its id"
        );

        // Reload from disk: the high-water mark must have been persisted.
        let mut reloaded = ProfileStore::open_or_empty_memory(path.clone());
        assert_eq!(
            reloaded.next_profile_id().expect("allocate after reload"),
            ProfileId(3),
            "high-water mark must survive a reload"
        );

        reloaded.delete_profile(ProfileId(1)).expect("delete last");
        assert_eq!(
            reloaded.next_profile_id().expect("allocate after emptying"),
            ProfileId(3),
            "emptying the store must not restart ids at 1"
        );
        cleanup(&path);
    }

    #[test]
    fn v1_file_migrates_in_memory_and_saves_as_current_format() {
        let path = temp_profiles_path("v1-migration");
        cleanup(&path);
        let body = v1_file_body(&v2_password_profile_json(5, "Legacy"));
        std::fs::write(path.as_str(), body).expect("write v1 fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        assert!(
            store.initial_error().is_none(),
            "v1 must still open cleanly"
        );
        assert_eq!(store.profiles().len(), 1);
        assert_eq!(
            store.next_profile_id().expect("allocate after migration"),
            ProfileId(6),
            "in-memory migration must derive max+1"
        );

        store
            .save_profile(password_profile(6, "Added"))
            .expect("save into migrated store");

        let raw = std::fs::read_to_string(path.as_str()).expect("read saved store");
        assert!(
            raw.contains(r#""version": 3"#),
            "first save must persist as the current format, got: {raw}"
        );
        assert!(
            raw.contains("\"next_profile_id\": 7"),
            "high-water mark must be persisted, got: {raw}"
        );

        let reloaded = ProfilesFile::load(&path).expect("reload migrated file");
        assert_eq!(reloaded.version, ProfilesFile::CURRENT_VERSION);
        assert_eq!(reloaded.profiles.len(), 2);
        cleanup(&path);
    }

    #[test]
    fn concurrent_stores_do_not_silently_lose_updates() {
        let path = temp_profiles_path("two-stores");
        let lock_cleanup = path.as_str().to_string() + ".lock";
        cleanup(&path);
        let _ = std::fs::remove_file(&lock_cleanup);

        // Two stores over the same file, both opened before either saves.
        let mut first = ProfileStore::open_or_empty_memory(path.clone());
        let mut second = ProfileStore::open_or_empty_memory(path.clone());

        first
            .save_profile(password_profile(1, "FromFirst"))
            .expect("first store saves");
        second
            .save_profile(password_profile(2, "FromSecond"))
            .expect("second store saves");

        let reloaded = ProfileStore::open_or_empty_memory(path.clone());
        assert_eq!(
            reloaded.profiles().len(),
            2,
            "both stores' profiles must survive; no silent whole-file overwrite"
        );

        // A third store whose in-memory snapshot predates both writes still
        // believes id 1 is free. Inserting id 1 through it must conflict
        // instead of silently overwriting the other store's profile.
        let mut stale = ProfileStore::open_or_empty_memory(path.clone());
        stale.profiles = ProfilesFile {
            version: ProfilesFile::CURRENT_VERSION,
            profiles: Vec::new(),
            next_profile_id: None,
        };
        let conflict = stale.save_profile(ConnectionProfile::new(
            ProfileId(1),
            "StaleInsert",
            "example.com",
            "alex",
            AuthMethod::Password {
                secret_ref: SecretRef::keychain_ref(ProfileId(1), "password"),
            },
        ));
        assert!(
            matches!(conflict, Err(StorageError::ConcurrentModification { .. })),
            "stale new-profile save must surface a conflict, got {conflict:?}"
        );
        cleanup(&path);
        std::fs::remove_file(&lock_cleanup).expect("remove lock sidecar");
    }

    #[test]
    fn update_of_missing_profile_reports_conflict_after_reload() {
        let path = temp_profiles_path("stale-update");
        let lock_cleanup = path.as_str().to_string() + ".lock";
        cleanup(&path);
        let _ = std::fs::remove_file(&lock_cleanup);

        let mut first = ProfileStore::open_or_empty_memory(path.clone());
        let mut second = ProfileStore::open_or_empty_memory(path.clone());
        first
            .save_profile(password_profile(1, "Live"))
            .expect("seed profile in first store");
        // Second store was opened before the seed; delete through it sees
        // the fresh disk state and removes the profile.
        second
            .delete_profile(ProfileId(1))
            .expect("delete via second store");
        // A stale new-profile save through a store whose snapshot predates
        // everything (never saw id 1 at all) must fail instead of silently
        // recreating or colliding with an id another instance handed out.
        let mut stale = ProfileStore::open_or_empty_memory(path.clone());
        stale.profiles = ProfilesFile {
            version: ProfilesFile::CURRENT_VERSION,
            profiles: Vec::new(),
            next_profile_id: None,
        };
        let result = stale.save_profile(password_profile(1, "Stale"));
        assert!(matches!(
            result,
            Err(StorageError::ConcurrentModification { .. })
        ));
        assert!(
            ProfileStore::open_or_empty_memory(path.clone())
                .find_profile(ProfileId(1))
                .is_none()
        );
        cleanup(&path);
        std::fs::remove_file(&lock_cleanup).expect("remove lock sidecar");
    }

    #[test]
    fn cleanup_removes_stale_temp_files_with_real_prefix() {
        let path = temp_profiles_path("temp-cleanup");
        cleanup(&path);
        let dir = std::env::temp_dir();
        let file_name = std::path::Path::new(path.as_str())
            .file_name()
            .expect("fixture has a file name")
            .to_string_lossy()
            .into_owned();
        let stale = dir.join(format!(".{file_name}.tmp-12345-0"));
        std::fs::write(&stale, b"leftover").expect("write stale temp fixture");
        // An unrelated dot-file must not be touched.
        let unrelated = dir.join(format!(".{file_name}.other"));
        std::fs::write(&unrelated, b"keep me").expect("write unrelated fixture");

        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        store
            .save_profile(password_profile(1, "Trigger"))
            .expect("save triggers temp sweep");

        assert!(
            !stale.exists(),
            "stale temp file with the real prefix must be removed"
        );
        assert!(unrelated.exists(), "unrelated files must survive the sweep");
        std::fs::remove_file(&unrelated).expect("remove unrelated fixture");
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    /// A rename that landed but whose parent-directory sync failed must NOT
    /// roll the new secret back: disk already holds the new profile, so a
    /// rollback would leave credentials permanently inconsistent. The
    /// degraded durability is surfaced as a warning instead.
    #[test]
    fn replaced_but_not_durable_save_keeps_new_secret_and_warns() {
        use crate::profile_file::PhasedSaveError;

        fn failing_parent_sync_save(
            file: &ProfilesFile,
            path: &LocalPath,
        ) -> Result<(), PhasedSaveError> {
            // Real staged write + rename so the destination genuinely holds
            // the new contents; only the parent sync is reported as failed.
            file.save_phased_with_failing_parent_sync(path)
        }

        let path = temp_profiles_path("not-durable-secret");
        cleanup(&path);
        let mut store = ProfileStore::open_or_empty_memory(path.clone());
        store
            .save_connection_settings(
                ProfileId(1),
                "Before".into(),
                &password_settings("before.example.com", "old-secret"),
            )
            .expect("seed profile and credential");

        // Swap in the injected commit and re-run an update through the full
        // save_request flow.
        let outcome = {
            let request_profile_id = ProfileId(1);
            let previous = store.find_profile(request_profile_id).cloned();
            assert!(previous.is_some(), "seeded profile exists");

            // Drive save_request manually with the failing sync seam.
            store.commit_save_override = Some(Box::new(failing_parent_sync_save));
            let result = store.save_connection_settings(
                ProfileId(1),
                "After".into(),
                &password_settings("after.example.com", "new-secret"),
            );
            store.commit_save_override = None;
            result.expect("degraded-durability commit still succeeds")
        };

        assert!(
            outcome
                .cleanup_warnings
                .iter()
                .any(|warning| matches!(warning, StorageWarning::DurabilityNotProven(_))),
            "a degraded commit must surface a durability warning"
        );

        // Memory keeps the new state...
        let retained = store.find_profile(ProfileId(1)).expect("profile kept");
        assert_eq!(retained.name, "After");
        let settings = store
            .load_connection_settings(ProfileId(1))
            .expect("load updated credential");
        let AuthCredential::Password { password } = &settings.auth else {
            panic!("expected password auth");
        };
        assert_eq!(
            password, "new-secret",
            "the new secret must not be rolled back after a landed rename"
        );

        // ...and so does disk.
        let reloaded = ProfileStore::open_or_empty_memory(path.clone());
        let found = reloaded.find_profile(ProfileId(1)).expect("on disk");
        assert_eq!(found.name, "After");
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }
}
