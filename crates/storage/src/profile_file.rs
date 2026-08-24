use macsftp_core::{AuthMethod, ConnectionProfile, LocalPath, ProfileId, SecretRef};
use serde::{Deserialize, Serialize};

use crate::atomic_file::{AtomicWriteError, write_private_file_phased};

/// Versioned, non-secret representation persisted as `profiles.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilesFile {
    pub version: u32,
    pub profiles: Vec<ConnectionProfile>,
    /// One past the highest profile id ever allocated. Persisted so ids are
    /// never reused after the highest-id profile is deleted (recents and
    /// session state hold `profile_id` as a stable identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_profile_id: Option<u64>,
}

impl ProfilesFile {
    pub const CURRENT_VERSION: u32 = 2;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            profiles: Vec::new(),
            next_profile_id: None,
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
    /// empty store (first launch); unsupported versions or structurally
    /// corrupt contents surface as `StorageError`. A v1 file is migrated in
    /// memory only — loading never rewrites the file on disk.
    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => Self::parse(&contents),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ProfilesFile::new()),
            Err(error) => Err(StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            }),
        }
    }

    fn parse(contents: &str) -> Result<Self, StorageError> {
        let parsed: ProfilesFile =
            serde_json::from_str(contents).map_err(|error| StorageError::Parse {
                message: error.to_string(),
            })?;
        if parsed.version == 0 || parsed.version > Self::CURRENT_VERSION {
            return Err(StorageError::UnsupportedVersion {
                found: parsed.version,
                supported: Self::CURRENT_VERSION,
            });
        }
        parsed.validate()?;
        let mut file = parsed;
        if file.next_profile_id.is_none() {
            // v1 files carry no high-water mark; derive one so existing ids
            // stay stable. The value is written out on the next save.
            file.next_profile_id = Some(file.next_profile_high_water());
        }
        file.version = Self::CURRENT_VERSION;
        Ok(file)
    }

    /// The reserved high-water mark: the next id to hand out. At least 1
    /// even when the store is empty, so deleting every profile never
    /// restarts the sequence.
    pub(crate) fn next_profile_high_water(&self) -> u64 {
        self.next_profile_id.unwrap_or_else(|| {
            self.profiles
                .iter()
                .map(|profile| profile.id.0)
                .max()
                .unwrap_or(0)
                + 1
        })
    }

    /// Reject structurally valid JSON that violates store invariants instead
    /// of silently repairing it (a silent repair would be rewritten back to
    /// disk and destroy the original evidence).
    fn validate(&self) -> Result<(), StorageError> {
        for (index, profile) in self.profiles.iter().enumerate() {
            if let Some(duplicate) = self.profiles[index + 1..]
                .iter()
                .find(|other| other.id == profile.id)
            {
                return Err(StorageError::Corrupt {
                    message: format!("duplicate profile id {}", duplicate.id.0),
                });
            }
            if profile.revision == 0 {
                return Err(StorageError::Corrupt {
                    message: format!("profile {} has revision 0", profile.id.0),
                });
            }
            validate_auth(&profile.auth, profile.id)?;
        }
        Ok(())
    }

    /// Atomically write `profiles.json` to `path` via a temp file + rename,
    /// so a crash mid-write can't leave a truncated file.
    pub fn save(&self, path: &LocalPath) -> Result<(), StorageError> {
        self.save_phased(path)
            .map_err(PhasedSaveError::into_storage_error)
    }

    /// Phase-aware variant of [`Self::save`]: callers that run compensating
    /// actions on failure must be able to tell a not-committed failure from a
    /// replaced-but-not-durable one.
    pub(crate) fn save_phased(&self, path: &LocalPath) -> Result<(), PhasedSaveError> {
        let json = self.serialize().map_err(PhasedSaveError::NotCommitted)?;
        write_private_file_phased(std::path::Path::new(path.as_str()), json.as_bytes())
            .map_err(|error| phased_error(path, error))
    }

    /// Test seam mirroring [`Self::save_phased`] whose parent-directory sync
    /// always fails: the staged write and rename are real, so the destination
    /// genuinely holds the new contents while durability stays unproven.
    #[cfg(test)]
    pub(crate) fn save_phased_with_failing_parent_sync(
        &self,
        path: &LocalPath,
    ) -> Result<(), PhasedSaveError> {
        let json = self.serialize().map_err(PhasedSaveError::NotCommitted)?;
        crate::atomic_file::write_phased_with_parent_sync(
            std::path::Path::new(path.as_str()),
            json.as_bytes(),
            |_parent_directory| std::io::Result::Err(std::io::Error::other("injected fsync failure")),
        )
        .map_err(|error| phased_error(path, error))
    }

    fn serialize(&self) -> Result<String, StorageError> {
        serde_json::to_string_pretty(self).map_err(|error| StorageError::Parse {
            message: error.to_string(),
        })
    }
}

fn phased_error(path: &LocalPath, error: AtomicWriteError) -> PhasedSaveError {
    match error {
        AtomicWriteError::NotCommitted(io) => PhasedSaveError::NotCommitted(StorageError::Io {
            path: path.as_str().to_string(),
            message: io.to_string(),
        }),
        AtomicWriteError::ReplacedButNotDurable(io) => {
            PhasedSaveError::ReplacedButNotDurable(StorageError::Io {
                path: path.as_str().to_string(),
                message: io.to_string(),
            })
        }
    }
}

impl Default for ProfilesFile {
    fn default() -> Self {
        Self::new()
    }
}

/// Check that a stored auth method points at its own profile's Keychain
/// entries (`keychain:macsftp:{profile_id}:{kind}`), matching
/// `SecretRef::keychain_ref` in core.
fn validate_auth(auth: &AuthMethod, profile_id: ProfileId) -> Result<(), StorageError> {
    let expected_prefix = format!("keychain:macsftp:{}:", profile_id.0);
    let check = |secret_ref: &SecretRef, kind: &str| -> Result<(), StorageError> {
        let expected = format!("{expected_prefix}{kind}");
        if secret_ref.as_str() != expected {
            return Err(StorageError::Corrupt {
                message: format!(
                    "profile {} has {} ref {:?}, expected {:?}",
                    profile_id.0,
                    kind,
                    secret_ref.as_str(),
                    expected
                ),
            });
        }
        Ok(())
    };
    match auth {
        AuthMethod::Password { secret_ref } => check(secret_ref, "password"),
        AuthMethod::PrivateKey {
            passphrase_ref,
            remember_passphrase,
            ..
        } => {
            if let Some(secret_ref) = passphrase_ref {
                check(secret_ref, "passphrase")?;
            }
            if *remember_passphrase != passphrase_ref.is_some() {
                return Err(StorageError::Corrupt {
                    message: format!(
                        "profile {} remember_passphrase={} but passphrase_ref present={}",
                        profile_id.0,
                        remember_passphrase,
                        passphrase_ref.is_some()
                    ),
                });
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    RevisionOverflow {
        profile_id: ProfileId,
    },
    RecoveryRequired {
        path: String,
    },
    UnsupportedVersion {
        found: u32,
        supported: u32,
    },
    Corrupt {
        message: String,
    },
    Io {
        path: String,
        message: String,
    },
    Parse {
        message: String,
    },
    /// A write transaction assumed an id was absent (new profile) or present
    /// (update/delete), but the freshly reloaded on-disk state disagreed —
    /// another store instance changed the file in between.
    ConcurrentModification {
        message: String,
    },
}

/// A `ProfilesFile::save` failure tagged with which phase of the atomic
/// replacement it happened in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PhasedSaveError {
    /// Failed at or before the rename: disk still holds the previous file.
    NotCommitted(StorageError),
    /// The rename landed but durability is unproven.
    ReplacedButNotDurable(StorageError),
}

impl PhasedSaveError {
    /// Which commit phase failed: compensating actions (such as rolling a
    /// Keychain secret back) are only safe when the write never committed.
    pub(crate) fn phase(&self) -> TransactionPhase {
        match self {
            Self::NotCommitted(_) => TransactionPhase::NotCommitted,
            Self::ReplacedButNotDurable(_) => TransactionPhase::ReplacedButNotDurable,
        }
    }

    pub(crate) fn into_storage_error(self) -> StorageError {
        match self {
            Self::NotCommitted(error) | Self::ReplacedButNotDurable(error) => error,
        }
    }

    pub(crate) fn into_durability_warning(self) -> crate::profiles::StorageWarning {
        let Self::ReplacedButNotDurable(error) = self else {
            unreachable!("durability warnings only come from replaced-but-not-durable errors")
        };
        crate::profiles::StorageWarning::DurabilityNotProven(error.to_string())
    }
}

/// The two failure phases of an atomic file replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionPhase {
    NotCommitted,
    ReplacedButNotDurable,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::RevisionOverflow { profile_id } => {
                write!(formatter, "profile {profile_id:?} revision overflowed")
            }
            StorageError::RecoveryRequired { path } => write!(
                formatter,
                "refusing to overwrite unreadable application data at {path}"
            ),
            StorageError::UnsupportedVersion { found, supported } => write!(
                formatter,
                "unsupported profiles version {found} (supported: {supported})"
            ),
            StorageError::Corrupt { message } => {
                write!(formatter, "profiles data is corrupt: {message}")
            }
            StorageError::ConcurrentModification { message } => {
                write!(formatter, "profiles changed concurrently: {message}")
            }
            StorageError::Io { path, message } => {
                write!(formatter, "could not access {path}: {message}")
            }
            StorageError::Parse { message } => {
                write!(formatter, "could not parse profiles: {message}")
            }
        }
    }
}
