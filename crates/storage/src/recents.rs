use macsftp_core::LocalPath;
use serde::{Deserialize, Serialize};

use super::StorageError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_path: Option<String>,
    /// Unix seconds
    pub last_connected_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentsFile {
    pub version: u32,
    pub entries: Vec<RecentEntry>,
}

impl RecentsFile {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn empty() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            entries: Vec::new(),
        }
    }

    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: RecentsFile =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                if parsed.version > Self::CURRENT_VERSION {
                    return Err(StorageError::Parse {
                        message: format!(
                            "unsupported recents version {} (max {})",
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

/// Input without id (store assigns). Timestamp is caller-provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentEntryInput {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_id: Option<u64>,
    pub display_name: Option<String>,
    pub last_remote_path: Option<String>,
    pub last_connected_at: u64,
}

pub struct RecentsStore {
    path: LocalPath,
    file: RecentsFile,
    next_id: u64,
}

impl RecentsStore {
    pub const MAX_ENTRIES: usize = 20;

    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let file = RecentsFile::load(&path)?;
        let next_id = file
            .entries
            .iter()
            .map(|entry| entry.id)
            .max()
            .map(|max_id| max_id.saturating_add(1))
            .unwrap_or(1);
        Ok(Self {
            path,
            file,
            next_id,
        })
    }

    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(_) => Self {
                path,
                file: RecentsFile::empty(),
                next_id: 1,
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn entries(&self) -> &[RecentEntry] {
        &self.file.entries
    }

    /// Upsert by `(host, port, username, profile_id)`; move to front; cap 20; save.
    pub fn upsert(&mut self, entry: RecentEntryInput) -> Result<(), StorageError> {
        let dedupe_matches = |existing: &RecentEntry| {
            existing.host == entry.host
                && existing.port == entry.port
                && existing.username == entry.username
                && existing.profile_id == entry.profile_id
        };

        if let Some(index) = self.file.entries.iter().position(dedupe_matches) {
            let mut updated = self.file.entries.remove(index);
            updated.display_name = entry.display_name;
            updated.last_remote_path = entry.last_remote_path;
            updated.last_connected_at = entry.last_connected_at;
            self.file.entries.insert(0, updated);
        } else {
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            self.file.entries.insert(
                0,
                RecentEntry {
                    id,
                    host: entry.host,
                    port: entry.port,
                    username: entry.username,
                    profile_id: entry.profile_id,
                    display_name: entry.display_name,
                    last_remote_path: entry.last_remote_path,
                    last_connected_at: entry.last_connected_at,
                },
            );
        }

        if self.file.entries.len() > Self::MAX_ENTRIES {
            self.file.entries.truncate(Self::MAX_ENTRIES);
        }

        self.save()
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
            "{}/macsftp-recents-{}-{}-{}.json",
            std::env::temp_dir().display(),
            label,
            std::process::id(),
            seq
        ))
    }

    fn sample_input(host: &str, at: u64) -> RecentEntryInput {
        RecentEntryInput {
            host: host.into(),
            port: 22,
            username: "alex".into(),
            profile_id: None,
            display_name: Some(host.into()),
            last_remote_path: Some("/home/alex".into()),
            last_connected_at: at,
        }
    }

    #[test]
    fn upsert_dedupes_and_moves_to_front() {
        let path = temp_path("dedupe");
        let mut store = RecentsStore::open_or_empty(path);

        store
            .upsert(sample_input("a.example", 100))
            .expect("first upsert");
        store
            .upsert(sample_input("b.example", 200))
            .expect("second upsert");
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.entries()[0].host, "b.example");

        store
            .upsert(RecentEntryInput {
                host: "a.example".into(),
                port: 22,
                username: "alex".into(),
                profile_id: None,
                display_name: Some("A renamed".into()),
                last_remote_path: Some("/tmp".into()),
                last_connected_at: 300,
            })
            .expect("dedupe upsert");

        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.entries()[0].host, "a.example");
        assert_eq!(store.entries()[0].last_connected_at, 300);
        assert_eq!(
            store.entries()[0].display_name.as_deref(),
            Some("A renamed")
        );
        assert_eq!(store.entries()[0].last_remote_path.as_deref(), Some("/tmp"));
        assert_eq!(store.entries()[1].host, "b.example");
        // Same key twice alone would be len 1; with two hosts, dedupe keeps identity.
        let first_id = store.entries()[0].id;
        store
            .upsert(sample_input("a.example", 400))
            .expect("second same key");
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.entries()[0].id, first_id);
        assert_eq!(store.entries()[0].last_connected_at, 400);
    }

    #[test]
    fn upsert_caps_at_twenty() {
        let mut store = RecentsStore::open_or_empty(temp_path("cap"));
        for i in 0..25 {
            store
                .upsert(RecentEntryInput {
                    host: format!("h{i}"),
                    port: 22,
                    username: "u".into(),
                    profile_id: None,
                    display_name: None,
                    last_remote_path: None,
                    last_connected_at: i,
                })
                .expect("upsert");
        }
        assert_eq!(store.entries().len(), 20);
        assert_eq!(store.entries()[0].host, "h24");
    }

    #[test]
    fn corrupt_recents_open_or_empty() {
        let path = temp_path("corrupt");
        std::fs::write(path.as_str(), "{not json").expect("write corrupt");
        let store = RecentsStore::open_or_empty(path);
        assert!(store.entries().is_empty());
    }

    #[test]
    fn serialized_recents_has_no_secret_keys() {
        let path = temp_path("nosecret");
        let mut store = RecentsStore::open_or_empty(path.clone());
        store
            .upsert(RecentEntryInput {
                host: "h".into(),
                port: 22,
                username: "u".into(),
                profile_id: Some(1),
                display_name: Some("Prod".into()),
                last_remote_path: Some("/home/u".into()),
                last_connected_at: 1,
            })
            .expect("upsert");
        let raw = std::fs::read_to_string(path.as_str()).expect("read");
        for forbidden in ["password", "passphrase", "secret", "auth", "key_path"] {
            assert!(
                !raw.to_lowercase().contains(forbidden),
                "recents json must not contain {forbidden}: {raw}"
            );
        }
    }
}
