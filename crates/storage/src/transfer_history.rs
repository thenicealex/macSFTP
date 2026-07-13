use std::io::ErrorKind;

use macsftp_core::{LocalPath, TransferHistoryId, TransferHistoryRecord};
use serde::{Deserialize, Serialize};

use super::StorageError;

/// On-disk envelope for residual transfer-history cleanup.
///
/// Product policy is **session-scoped** history (no cross-launch catalog).
/// The file only exists so quit/launch can wipe leftovers from older builds
/// that used to persist plans; the app never *loads* records for UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferHistoryFile {
    pub version: u32,
    pub records: Vec<TransferHistoryRecord>,
}

impl TransferHistoryFile {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            records: Vec::new(),
        }
    }

    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: TransferHistoryFile =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                Ok(parsed)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(TransferHistoryFile::new()),
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

impl Default for TransferHistoryFile {
    fn default() -> Self {
        Self::new()
    }
}

/// In-process transfer-history bag used for optional mid-session records
/// (tests / retry helpers). Does **not** restore a catalog across launches.
pub struct TransferHistoryStore {
    path: LocalPath,
    records: Vec<TransferHistoryRecord>,
    next_id: TransferHistoryId,
}

impl TransferHistoryStore {
    /// Product entry point: empty memory and best-effort delete of any residual
    /// on-disk file from older cross-session history builds.
    pub fn open_session(path: LocalPath) -> Self {
        match std::fs::remove_file(path.as_str()) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            // Leave a broken/unreadable residual for `clear_and_persist` on quit.
            Err(_) => {}
        }
        Self {
            path,
            records: Vec::new(),
            next_id: TransferHistoryId(1),
        }
    }

    /// Load records from disk (unit tests / diagnostics only — not used by app startup).
    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let file = TransferHistoryFile::load(&path)?;
        let next_id = file
            .records
            .iter()
            .map(|record| record.id.0)
            .max()
            .map(|max_id| TransferHistoryId(max_id + 1))
            .unwrap_or(TransferHistoryId(1));
        Ok(Self {
            path,
            records: file.records,
            next_id,
        })
    }

    /// Like [`Self::open`], but never fails (missing/corrupt → empty).
    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(_) => Self {
                path,
                records: Vec::new(),
                next_id: TransferHistoryId(1),
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn records(&self) -> &[TransferHistoryRecord] {
        &self.records
    }

    pub fn find(&self, id: TransferHistoryId) -> Option<&TransferHistoryRecord> {
        self.records.iter().find(|record| record.id == id)
    }

    pub fn allocate_id(&mut self) -> TransferHistoryId {
        let id = self.next_id;
        self.next_id = TransferHistoryId(self.next_id.0 + 1);
        id
    }

    /// Append mid-session records (tests inject unfinished/failed for retry).
    pub fn append(&mut self, new_records: Vec<TransferHistoryRecord>) {
        self.records.extend(new_records);
    }

    pub fn remove(&mut self, id: TransferHistoryId) -> bool {
        let before = self.records.len();
        self.records.retain(|record| record.id != id);
        self.records.len() != before
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Empty the store and write an empty file (session end / residual wipe).
    pub fn clear_and_persist(&mut self) -> Result<(), StorageError> {
        self.clear();
        self.save()
    }

    pub fn save(&self) -> Result<(), StorageError> {
        TransferHistoryFile {
            version: TransferHistoryFile::CURRENT_VERSION,
            records: self.records.clone(),
        }
        .save(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{
        ConflictPolicy, LocalPath, MetadataPolicy, Timestamp, TransferDirection, TransferEndpoint,
        TransferHistoryId, TransferHistoryRecord, TransferHistoryStatus,
    };

    use super::TransferHistoryStore;

    fn temp_history_path() -> LocalPath {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        LocalPath::new(format!(
            "{}/macsftp-history-{}-{}.json",
            std::env::temp_dir().display(),
            std::process::id(),
            seq
        ))
    }

    fn sample_record(id: u64, status: TransferHistoryStatus) -> TransferHistoryRecord {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_secs();
        TransferHistoryRecord {
            id: TransferHistoryId(id),
            profile_id: None,
            direction: TransferDirection::Upload,
            sources: vec![TransferEndpoint::Local(LocalPath::new("/tmp/a.txt"))],
            destination: TransferEndpoint::Remote(macsftp_core::RemotePath::new("/srv/a.txt")),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::Ask,
            status,
            started_at: Timestamp::from_secs_since_epoch(now),
            last_updated: Timestamp::from_secs_since_epoch(now),
        }
    }

    #[test]
    fn open_session_starts_empty_and_deletes_residual_file() {
        let path = temp_history_path();
        let mut seeded = TransferHistoryStore::open_or_empty(path.clone());
        seeded.append(vec![sample_record(1, TransferHistoryStatus::Completed)]);
        seeded.save().expect("seed residual file");
        assert!(std::path::Path::new(path.as_str()).exists());

        let session = TransferHistoryStore::open_session(path.clone());
        assert!(session.records().is_empty());
        assert!(
            !std::path::Path::new(path.as_str()).exists(),
            "residual cross-session file must be removed"
        );
    }

    #[test]
    fn history_round_trips_through_disk_for_diagnostics() {
        let path = temp_history_path();
        let mut store = TransferHistoryStore::open_or_empty(path.clone());
        store.append(vec![sample_record(
            1,
            TransferHistoryStatus::Failed {
                message: "boom".into(),
                retryable: true,
            },
        )]);
        store.save().expect("save");

        let mut reopened = TransferHistoryStore::open_or_empty(path);
        assert_eq!(reopened.records().len(), 1);
        assert_eq!(reopened.records()[0].id, TransferHistoryId(1));
        assert_eq!(reopened.allocate_id(), TransferHistoryId(2));
    }

    #[test]
    fn clear_and_persist_writes_empty_file() {
        let path = temp_history_path();
        let mut store = TransferHistoryStore::open_or_empty(path.clone());
        store.append(vec![sample_record(3, TransferHistoryStatus::Unfinished)]);
        store.save().expect("save");
        store.clear_and_persist().expect("clear");
        assert!(store.records().is_empty());
        let reopened = TransferHistoryStore::open_or_empty(path);
        assert!(reopened.records().is_empty());
    }

    #[test]
    fn remove_drops_record() {
        let path = temp_history_path();
        let mut store = TransferHistoryStore::open_or_empty(path);
        store.append(vec![sample_record(99, TransferHistoryStatus::Unfinished)]);
        assert!(store.remove(TransferHistoryId(99)));
        assert!(store.records().is_empty());
        assert!(!store.remove(TransferHistoryId(99)));
    }
}
