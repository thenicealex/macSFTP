use macsftp_core::{LocalPath, WindowSessionId};
use serde::{Deserialize, Serialize};

use super::StorageError;
use crate::write_private_file_atomically;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTabSnapshot {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<u64>,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWindowSnapshot {
    pub id: WindowSessionId,
    #[serde(default)]
    pub active_tab_index: usize,
    #[serde(default)]
    pub tabs: Vec<SessionTabSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    #[serde(default)]
    pub active_window_index: usize,
    #[serde(default)]
    pub windows: Vec<SessionWindowSnapshot>,
}

#[derive(Deserialize)]
struct SessionVersion {
    version: u32,
}

#[derive(Deserialize)]
struct LegacySessionFileV1 {
    #[serde(default)]
    active_tab_index: usize,
    #[serde(default)]
    tabs: Vec<SessionTabSnapshot>,
}

impl SessionFile {
    pub const CURRENT_VERSION: u32 = 2;

    pub fn empty() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            active_window_index: 0,
            windows: Vec::new(),
        }
    }

    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let version: SessionVersion =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                match version.version {
                    0 | 1 => {
                        let legacy: LegacySessionFileV1 =
                            serde_json::from_str(&contents).map_err(|error| {
                                StorageError::Parse {
                                    message: error.to_string(),
                                }
                            })?;
                        Ok(Self {
                            version: Self::CURRENT_VERSION,
                            active_window_index: 0,
                            windows: vec![SessionWindowSnapshot {
                                id: WindowSessionId(1),
                                active_tab_index: legacy.active_tab_index,
                                tabs: legacy.tabs,
                            }],
                        })
                    }
                    Self::CURRENT_VERSION => {
                        serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                            message: error.to_string(),
                        })
                    }
                    unsupported => Err(StorageError::Parse {
                        message: format!(
                            "unsupported session version {} (max {})",
                            unsupported,
                            Self::CURRENT_VERSION
                        ),
                    }),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(error) => Err(StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            }),
        }
    }

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

pub struct SessionStore {
    path: LocalPath,
    file: SessionFile,
    initial_error: Option<StorageError>,
}

impl SessionStore {
    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let file = SessionFile::load(&path)?;
        Ok(Self {
            path,
            file,
            initial_error: None,
        })
    }

    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(error) => Self {
                path,
                file: SessionFile::empty(),
                initial_error: Some(error),
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn file(&self) -> &SessionFile {
        &self.file
    }

    pub fn initial_error(&self) -> Option<&StorageError> {
        self.initial_error.as_ref()
    }

    pub fn replace(&mut self, file: SessionFile) {
        self.file = file;
    }

    pub fn save(&self) -> Result<(), StorageError> {
        if self.initial_error.is_some() {
            return Err(StorageError::RecoveryRequired {
                path: self.path.as_str().to_string(),
            });
        }
        self.file.save(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macsftp_core::LocalPath;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> LocalPath {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        LocalPath::new(format!(
            "{}/macsftp-session-{}-{}-{}.json",
            std::env::temp_dir().display(),
            label,
            std::process::id(),
            seq
        ))
    }

    #[test]
    fn session_round_trip_preserves_tabs_and_active_index() {
        let path = temp_path("roundtrip");
        let mut store = SessionStore::open_or_empty(path.clone());
        store.replace(SessionFile {
            version: SessionFile::CURRENT_VERSION,
            active_window_index: 0,
            windows: vec![SessionWindowSnapshot {
                id: WindowSessionId(7),
                active_tab_index: 1,
                tabs: vec![
                    SessionTabSnapshot {
                        title: "a.example".into(),
                        profile_id: Some(3),
                        host: "a.example".into(),
                        port: 22,
                        username: "alex".into(),
                        local_path: Some("/Users/alex".into()),
                        remote_path: Some("/home/alex".into()),
                    },
                    SessionTabSnapshot {
                        title: "b.example".into(),
                        profile_id: None,
                        host: "b.example".into(),
                        port: 2222,
                        username: "root".into(),
                        local_path: None,
                        remote_path: None,
                    },
                ],
            }],
        });
        store.save().expect("save session");
        let reloaded = SessionStore::open(path).expect("reopen");
        assert_eq!(reloaded.file().windows[0].id, WindowSessionId(7));
        assert_eq!(reloaded.file().windows[0].active_tab_index, 1);
        assert_eq!(reloaded.file().windows[0].tabs.len(), 2);
        assert_eq!(reloaded.file().windows[0].tabs[0].profile_id, Some(3));
        assert_eq!(reloaded.file().windows[0].tabs[1].port, 2222);
    }

    #[test]
    fn version_one_migrates_to_one_window() {
        let path = temp_path("v1");
        std::fs::write(
            path.as_str(),
            r#"{"version":1,"active_tab_index":1,"tabs":[{"title":"a","host":"a","port":22,"username":"u"},{"title":"b","host":"b","port":22,"username":"u"}]}"#,
        )
        .expect("write v1 session fixture");

        let store = SessionStore::open(path).expect("v1 session should migrate");
        assert_eq!(store.file().version, SessionFile::CURRENT_VERSION);
        assert_eq!(store.file().active_window_index, 0);
        assert_eq!(store.file().windows.len(), 1);
        assert_eq!(store.file().windows[0].id, WindowSessionId(1));
        assert_eq!(store.file().windows[0].active_tab_index, 1);
        assert_eq!(store.file().windows[0].tabs.len(), 2);
    }

    #[test]
    fn corrupt_json_open_or_empty_yields_empty() {
        let path = temp_path("corrupt");
        std::fs::write(path.as_str(), "{not json").expect("write corrupt");
        let store = SessionStore::open_or_empty(path.clone());
        assert!(store.file().windows.is_empty());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save(),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("read preserved corrupt session"),
            "{not json"
        );
    }

    #[test]
    fn unsupported_version_open_or_empty_yields_empty() {
        let path = temp_path("badver");
        std::fs::write(
            path.as_str(),
            r#"{"version":99,"active_tab_index":0,"tabs":[{"title":"x","host":"x","port":22,"username":"u"}]}"#,
        )
        .expect("write");
        let store = SessionStore::open_or_empty(path);
        assert!(store.file().windows.is_empty());
        assert!(store.initial_error().is_some());
    }

    #[test]
    fn serialized_session_has_no_secret_keys() {
        let path = temp_path("nosecret");
        let mut store = SessionStore::open_or_empty(path.clone());
        store.replace(SessionFile {
            version: SessionFile::CURRENT_VERSION,
            active_window_index: 0,
            windows: vec![SessionWindowSnapshot {
                id: WindowSessionId(1),
                active_tab_index: 0,
                tabs: vec![SessionTabSnapshot {
                    title: "h".into(),
                    profile_id: None,
                    host: "h".into(),
                    port: 22,
                    username: "u".into(),
                    local_path: None,
                    remote_path: None,
                }],
            }],
        });
        store.save().expect("save");
        let raw = std::fs::read_to_string(path.as_str()).expect("read");
        for forbidden in ["password", "passphrase", "secret", "auth", "key_path"] {
            assert!(
                !raw.to_lowercase().contains(forbidden),
                "session json must not contain {forbidden}: {raw}"
            );
        }
    }

    #[test]
    fn failed_save_preserves_previous_session_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = format!("{}.dir", temp_path("preserve-on-failure").as_str());
        std::fs::create_dir_all(&directory).expect("create protected session directory");
        let path = LocalPath::new(format!("{directory}/session.json"));
        let mut original = SessionFile::empty();
        original.active_window_index = 3;
        original.save(&path).expect("save original session");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500))
            .expect("make session directory read-only");

        let replacement = SessionFile::empty();
        assert!(replacement.save(&path).is_err());

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("restore session directory permissions");
        let reloaded = SessionFile::load(&path).expect("original session should remain readable");
        assert_eq!(reloaded.active_window_index, 3);
        std::fs::remove_dir_all(directory).expect("remove protected session directory");
    }
}
