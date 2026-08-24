use macsftp_core::LocalPath;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::StorageError;
use crate::{file_lock::FileLock, write_private_file_atomically};

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
                if parsed.version == 0 || parsed.version > Self::CURRENT_VERSION {
                    return Err(StorageError::UnsupportedVersion {
                        found: parsed.version,
                        supported: Self::CURRENT_VERSION,
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
        write_private_file_atomically(std::path::Path::new(path.as_str()), json.as_bytes()).map_err(
            |error| StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            },
        )
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
    initial_error: Option<StorageError>,
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
            initial_error: None,
        })
    }

    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(error) => Self {
                path,
                file: RecentsFile::empty(),
                next_id: 1,
                initial_error: Some(error),
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn entries(&self) -> &[RecentEntry] {
        &self.file.entries
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

    /// Upsert by `(host, port, username, profile_id)`; move to front; cap 20; save.
    pub fn upsert(&mut self, entry: RecentEntryInput) -> Result<(), StorageError> {
        let _lock = self.acquire_write_lock()?;
        // Reload under the lock so entries another app instance wrote since
        // our last sync are merged into our view instead of being clobbered
        // by this full-file rewrite.
        self.file = RecentsFile::load(&self.path)?;
        self.reconcile_next_id();
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
        self.ensure_writable()?;
        self.file.save(&self.path)
    }

    /// Start one serialized read-modify-write cycle: hold the advisory lock
    /// until the returned guard drops, with the on-disk state freshly
    /// loaded. Same pattern as `ProfileStore` transactions.
    fn acquire_write_lock(&self) -> Result<FileLock, StorageError> {
        self.ensure_writable()?;
        FileLock::acquire(Path::new(self.path.as_str())).map_err(|error| StorageError::Io {
            path: self.path.as_str().to_string(),
            message: error.to_string(),
        })
    }

    /// Another instance may have inserted entries while our snapshot was
    /// stale; never hand out an id that collides with anything on disk.
    fn reconcile_next_id(&mut self) {
        let highest_on_disk = self
            .file
            .entries
            .iter()
            .map(|entry| entry.id)
            .max()
            .unwrap_or(0);
        if self.next_id <= highest_on_disk {
            self.next_id = highest_on_disk.saturating_add(1);
        }
    }

    /// Drop the profile linkage from every entry referencing `profile_id`,
    /// keeping the entries as plain manual-connection history (the cached
    /// display label stays). Called when a profile is deleted: the stale
    /// linkage would otherwise break the upsert dedupe key and turn the
    /// next manual reconnect into a duplicate entry. Returns how many
    /// entries were decoupled; the disk is only rewritten when something
    /// changed.
    pub fn forget_profile(&mut self, profile_id: u64) -> Result<usize, StorageError> {
        let _lock = self.acquire_write_lock()?;
        self.file = RecentsFile::load(&self.path)?;
        let mut decoupled = 0;
        for entry in &mut self.file.entries {
            if entry.profile_id == Some(profile_id) {
                entry.profile_id = None;
                decoupled += 1;
            }
        }
        if decoupled > 0 {
            self.save()?;
        }
        Ok(decoupled)
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
        let mut store = RecentsStore::open_or_empty(path.clone());
        assert!(store.entries().is_empty());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.upsert(sample_input("new.example", 1)),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("read preserved corrupt recents"),
            "{not json"
        );
    }

    /// Deleting a profile must not leave recents entries pointing at a
    /// dead profile id: the stale linkage breaks the upsert dedupe key, so
    /// a later manual reconnect to the same server creates a duplicate
    /// entry instead of updating the surviving one.
    #[test]
    fn forget_profile_decouples_entries_and_persists() {
        let path = temp_path("forget-profile");
        let mut store = RecentsStore::open_or_empty(path.clone());
        for (host, profile_id) in [
            ("with.example", Some(7u64)),
            ("bare.example", None),
            ("other.example", Some(7u64)),
            ("another.example", Some(9u64)),
        ] {
            store
                .upsert(RecentEntryInput {
                    host: host.into(),
                    port: 22,
                    username: "alex".into(),
                    profile_id,
                    display_name: Some("Prod".into()),
                    last_remote_path: Some("/home/alex".into()),
                    last_connected_at: 1,
                })
                .expect("seed entry");
        }

        let decoupled = store.forget_profile(7).expect("forget profile 7");

        assert_eq!(decoupled, 2);
        let linked: Vec<&str> = store
            .entries()
            .iter()
            .filter(|entry| entry.profile_id == Some(7))
            .map(|entry| entry.host.as_str())
            .collect();
        assert!(linked.is_empty(), "no entry may keep the deleted link");
        // The entries themselves survive as plain manual-connection history,
        // keeping their cached display label and other metadata.
        let former = store
            .entries()
            .iter()
            .find(|entry| entry.host == "with.example")
            .expect("decoupled entry is kept, not removed");
        assert_eq!(former.profile_id, None);
        assert_eq!(former.display_name.as_deref(), Some("Prod"));
        assert_eq!(
            store.entries()[0].profile_id,
            Some(9),
            "unrelated links stay untouched"
        );

        // The decoupling survives a reload.
        let reloaded = RecentsStore::open(path.clone()).expect("reload");
        assert!(
            reloaded
                .entries()
                .iter()
                .all(|entry| entry.profile_id != Some(7))
        );
    }

    #[test]
    fn forget_profile_without_matches_skips_rewrite() {
        let path = temp_path("forget-noop");
        let mut store = RecentsStore::open_or_empty(path.clone());
        store
            .upsert(sample_input("a.example", 1))
            .expect("seed entry");
        let before = std::fs::read_to_string(path.as_str()).expect("read before");

        let decoupled = store.forget_profile(42).expect("forget absent profile");

        assert_eq!(decoupled, 0);
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("read after"),
            before,
            "nothing changed on disk"
        );
    }

    /// Two store instances (two app processes) must not silently overwrite
    /// each other's entries: the second writer reloads under the advisory
    /// lock before mutating, so both upserts survive.
    #[test]
    fn concurrent_instances_do_not_silently_lose_recents() {
        let path = temp_path("concurrent");
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
        let mut first = RecentsStore::open_or_empty(path.clone());
        let mut second = RecentsStore::open_or_empty(path.clone());

        first
            .upsert(sample_input("a.example", 100))
            .expect("first instance upsert");
        // `second` was opened before the seed; its stale in-memory snapshot
        // must not erase what `first` wrote.
        second
            .upsert(sample_input("b.example", 200))
            .expect("second instance upsert");

        let reloaded = RecentsStore::open(path.clone()).expect("reload after both writes");
        let hosts: Vec<&str> = reloaded
            .entries()
            .iter()
            .map(|entry| entry.host.as_str())
            .collect();
        assert!(
            hosts.contains(&"a.example") && hosts.contains(&"b.example"),
            "both instances' entries must survive, got {hosts:?}"
        );

        // Ids stay unique across instances even after concurrent inserts.
        let mut ids: Vec<u64> = reloaded.entries().iter().map(|entry| entry.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), reloaded.entries().len(), "ids must not collide");

        let _ = std::fs::remove_file(path.as_str());
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    #[test]
    fn newer_recents_version_reports_unsupported_version() {
        let path = temp_path("future-version");
        std::fs::write(
            path.as_str(),
            r#"{"version": 99, "entries": [], "future_field": true}"#,
        )
        .expect("write future recents fixture");
        let raw_before = std::fs::read(path.as_str()).expect("read fixture before");

        // A file written by a NEWER release is not "corrupt": the recovery
        // guidance is fundamentally different (upgrade vs restore), so it
        // must be classified as UnsupportedVersion, not Parse.
        let error = RecentsStore::open(path.clone())
            .err()
            .expect("future version must be rejected");
        assert!(matches!(
            &error,
            StorageError::UnsupportedVersion { found: 99, .. }
        ));
        assert_eq!(
            match error {
                StorageError::UnsupportedVersion { supported, .. } => supported,
                _ => unreachable!("checked above"),
            },
            RecentsFile::CURRENT_VERSION
        );

        // The unreadable file still blocks writes instead of being replaced.
        let mut blocked = RecentsStore::open_or_empty(path.clone());
        assert!(blocked.initial_error().is_some());
        assert!(matches!(
            blocked.upsert(sample_input("x.example", 1)),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read(path.as_str()).expect("read fixture after"),
            raw_before,
            "future-version bytes must be preserved"
        );
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
