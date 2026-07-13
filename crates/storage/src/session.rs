use macsftp_core::LocalPath;
use serde::{Deserialize, Serialize};

use super::StorageError;

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
pub struct SessionFile {
    pub version: u32,
    #[serde(default)]
    pub active_tab_index: usize,
    #[serde(default)]
    pub tabs: Vec<SessionTabSnapshot>,
}

impl SessionFile {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn empty() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            active_tab_index: 0,
            tabs: Vec::new(),
        }
    }

    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: SessionFile =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                if parsed.version > Self::CURRENT_VERSION {
                    return Err(StorageError::Parse {
                        message: format!(
                            "unsupported session version {} (max {})",
                            parsed.version,
                            Self::CURRENT_VERSION
                        ),
                    });
                }
                Ok(parsed)
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

pub struct SessionStore {
    path: LocalPath,
    file: SessionFile,
}

impl SessionStore {
    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let file = SessionFile::load(&path)?;
        Ok(Self { path, file })
    }

    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(_) => Self {
                path,
                file: SessionFile::empty(),
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn file(&self) -> &SessionFile {
        &self.file
    }

    pub fn replace(&mut self, file: SessionFile) {
        self.file = file;
    }

    pub fn save(&self) -> Result<(), StorageError> {
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
        });
        store.save().expect("save session");
        let reloaded = SessionStore::open(path).expect("reopen");
        assert_eq!(reloaded.file().active_tab_index, 1);
        assert_eq!(reloaded.file().tabs.len(), 2);
        assert_eq!(reloaded.file().tabs[0].profile_id, Some(3));
        assert_eq!(reloaded.file().tabs[1].port, 2222);
    }

    #[test]
    fn corrupt_json_open_or_empty_yields_empty() {
        let path = temp_path("corrupt");
        std::fs::write(path.as_str(), "{not json").expect("write corrupt");
        let store = SessionStore::open_or_empty(path);
        assert!(store.file().tabs.is_empty());
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
        assert!(store.file().tabs.is_empty());
    }

    #[test]
    fn serialized_session_has_no_secret_keys() {
        let path = temp_path("nosecret");
        let mut store = SessionStore::open_or_empty(path.clone());
        store.replace(SessionFile {
            version: SessionFile::CURRENT_VERSION,
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
}
