use std::time::{Duration, SystemTime};

use macsftp_core::{LocalPath, TransferHistoryId, TransferHistoryRecord};
use serde::{Deserialize, Serialize};

use super::StorageError;

/// On-disk envelope for transfer history (plan §18). Mirrors
/// `ProfilesFile`: a versioned wrapper around the record list so the
/// file format can evolve without breaking older writes.
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

    /// Read the history file from `path`. A missing file is an empty
    /// store (first launch); other IO or parse failures surface as
    /// `StorageError`.
    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: TransferHistoryFile =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                Ok(parsed)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(TransferHistoryFile::new())
            }
            Err(error) => Err(StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            }),
        }
    }

    /// Atomically write the history file via a temp file + rename, so a
    /// crash mid-write can't leave a truncated file.
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

/// Retention policy (plan §18): keep completed/failed history for 7 days
/// or at most 100 entries, whichever is the smaller set.
const MAX_HISTORY_RECORDS: usize = 100;
const RETENTION_DAYS: u64 = 7;
const RETENTION_SECONDS: u64 = RETENTION_DAYS * 24 * 60 * 60;

/// Disk-backed transfer-history store. Owns the `transfer_history.json`
/// path and the in-memory `TransferHistoryFile`, keeping them in sync.
pub struct TransferHistoryStore {
    path: LocalPath,
    records: Vec<TransferHistoryRecord>,
    next_id: TransferHistoryId,
}

impl TransferHistoryStore {
    /// Open the store at `path`, loading any existing records.
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

    /// Like `open`, but never fails: a missing or unreadable history
    /// file yields an empty store that still remembers `path`, so the
    /// next save recovers.
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

    /// Allocate a fresh history id for a new record.
    pub fn allocate_id(&mut self) -> TransferHistoryId {
        let id = self.next_id;
        self.next_id = TransferHistoryId(self.next_id.0 + 1);
        id
    }

    /// Append freshly persisted records (e.g. in-flight transfers
    /// captured at app close). Callers are responsible for de-duping
    /// within a session.
    pub fn append(&mut self, new_records: Vec<TransferHistoryRecord>) {
        self.records.extend(new_records);
    }

    /// Remove a record by id (e.g. after it is retried). Returns true if
    /// a record was removed.
    pub fn remove(&mut self, id: TransferHistoryId) -> bool {
        let before = self.records.len();
        self.records.retain(|record| record.id != id);
        self.records.len() != before
    }

    /// Drop every in-memory record. Callers that need the empty set on
    /// disk should follow with [`Self::save`].
    pub fn clear(&mut self) {
        self.records.clear();
    }

    /// Enforce the retention policy: drop records older than
    /// `RETENTION_DAYS` and cap the remainder at `MAX_HISTORY_RECORDS`,
    /// keeping the most recently updated.
    pub fn prune(&mut self, now: macsftp_core::Timestamp) {
        let cutoff = now
            .0
            .checked_sub(Duration::from_secs(RETENTION_SECONDS))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.records
            .retain(|record| record.last_updated.0 >= cutoff);
        if self.records.len() > MAX_HISTORY_RECORDS {
            self.records
                .sort_by_key(|record| std::cmp::Reverse(record.last_updated));
            self.records.truncate(MAX_HISTORY_RECORDS);
        }
    }

    /// Flush the in-memory records to disk atomically.
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

    // Unique temp path per call so concurrent tests never clobber each
    // other's transfer_history.json (plan §9).
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

    fn sample_record(
        id: u64,
        status: TransferHistoryStatus,
        age_secs: u64,
    ) -> TransferHistoryRecord {
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
            started_at: Timestamp::from_secs_since_epoch(now - age_secs),
            last_updated: Timestamp::from_secs_since_epoch(now - age_secs),
        }
    }

    #[test]
    fn history_round_trips_through_disk() {
        let path = temp_history_path();
        let mut store = TransferHistoryStore::open_or_empty(path.clone());
        assert!(store.records().is_empty());

        store.append(vec![sample_record(1, TransferHistoryStatus::Unfinished, 0)]);
        store.save().expect("save");

        let mut reopened = TransferHistoryStore::open_or_empty(path);
        assert_eq!(reopened.records().len(), 1);
        assert_eq!(reopened.records()[0].id, TransferHistoryId(1));
        // allocate_id continues after the loaded max.
        assert_eq!(reopened.allocate_id(), TransferHistoryId(2));
    }

    #[test]
    fn missing_file_loads_empty() {
        let path = temp_history_path();
        let store = TransferHistoryStore::open_or_empty(path);
        assert!(store.records().is_empty());
    }

    #[test]
    fn prune_drops_old_and_caps_count() {
        let path = temp_history_path();
        let mut store = TransferHistoryStore::open_or_empty(path.clone());
        // 5 old (30 days) + 5 fresh.
        for id in 1..=10u64 {
            let age = if id <= 5 { 30 * 24 * 3600 } else { 60 };
            store.append(vec![sample_record(
                id,
                TransferHistoryStatus::Completed,
                age,
            )]);
        }
        let now = Timestamp::from_secs_since_epoch(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after the Unix epoch")
                .as_secs(),
        );
        store.prune(now);
        // Old ones gone; fresh five remain.
        assert_eq!(store.records().len(), 5);
        assert!(
            store
                .records()
                .iter()
                .all(|r| matches!(r.status, TransferHistoryStatus::Completed))
        );
    }

    #[test]
    fn remove_drops_record() {
        let path = temp_history_path();
        let mut store = TransferHistoryStore::open_or_empty(path.clone());
        store.append(vec![sample_record(
            99,
            TransferHistoryStatus::Unfinished,
            0,
        )]);
        assert!(store.remove(TransferHistoryId(99)));
        assert!(store.records().is_empty());
        // Removing a missing id is a no-op.
        assert!(!store.remove(TransferHistoryId(99)));
    }
}
