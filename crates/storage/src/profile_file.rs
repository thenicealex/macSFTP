use macsftp_core::{ConnectionProfile, LocalPath, ProfileId};
use serde::{Deserialize, Serialize};

use crate::write_private_file_atomically;

/// Versioned, non-secret representation persisted as `profiles.json`.
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
        write_private_file_atomically(std::path::Path::new(path.as_str()), json.as_bytes()).map_err(
            |error| StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            },
        )
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
    RecoveryRequired { path: String },
    Io { path: String, message: String },
    Parse { message: String },
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
            StorageError::Io { path, message } => {
                write!(formatter, "could not access {path}: {message}")
            }
            StorageError::Parse { message } => {
                write!(formatter, "could not parse profiles: {message}")
            }
        }
    }
}
