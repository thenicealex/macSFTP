use macsftp_core::ResidualTempRecord;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::StorageError;
use crate::{file_lock::FileLock, write_private_file_atomically};

/// On-disk envelope for residual temporary-transfer files (plan M5/M6
/// residual). The versioned wrapper lets the record format evolve without
/// breaking older writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidualTempFile {
    pub version: u32,
    pub records: Vec<ResidualTempRecord>,
}

impl ResidualTempFile {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            records: Vec::new(),
        }
    }

    /// Read the residual-temp file from `path`. A missing file is an empty
    /// store (first launch); other IO or parse failures surface as
    /// `StorageError`. A file written by a newer release is rejected as
    /// `UnsupportedVersion` instead of being silently read and rewritten
    /// lossily.
    pub fn load(path: &macsftp_core::LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: ResidualTempFile =
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ResidualTempFile::new())
            }
            Err(error) => Err(StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            }),
        }
    }

    /// Atomically write the file via a temp file + rename, so a crash
    /// mid-write can't leave a truncated file.
    pub fn save(&self, path: &macsftp_core::LocalPath) -> Result<(), StorageError> {
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

impl Default for ResidualTempFile {
    fn default() -> Self {
        Self::new()
    }
}

/// Disk-backed residual-temp store. Owns the `residual_temp.json` path
/// and the in-memory `ResidualTempFile`, keeping them in sync. Records are
/// keyed by `(transfer_id, path)` so a temp file created in a previous
/// runtime session (which may reuse transfer ids) is reconciled by its
/// full identity, not just its id.
pub struct ResidualTempStore {
    path: macsftp_core::LocalPath,
    records: Vec<ResidualTempRecord>,
    initial_error: Option<StorageError>,
}

impl ResidualTempStore {
    /// Open the store at `path`, loading any existing records.
    pub fn open(path: macsftp_core::LocalPath) -> Result<Self, StorageError> {
        let file = ResidualTempFile::load(&path)?;
        Ok(Self {
            path,
            records: file.records,
            initial_error: None,
        })
    }

    /// Like `open`, but never fails: a missing or unreadable file yields
    /// an empty store. An unreadable file blocks writes so recovery data is
    /// not overwritten.
    pub fn open_or_empty(path: macsftp_core::LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(error) => Self {
                path,
                records: Vec::new(),
                initial_error: Some(error),
            },
        }
    }

    pub fn path(&self) -> &macsftp_core::LocalPath {
        &self.path
    }

    pub fn records(&self) -> &[ResidualTempRecord] {
        &self.records
    }

    pub fn initial_error(&self) -> Option<&StorageError> {
        self.initial_error.as_ref()
    }

    /// All local residual records (cleaned at launch without a connection).
    /// Local records use the `"local"` connection-key sentinel.
    pub fn local_records(&self) -> impl Iterator<Item = &ResidualTempRecord> {
        self.records
            .iter()
            .filter(|record| record.connection_key == "local")
    }

    /// All remote residual records for a given `host:port` connection key.
    pub fn remote_records_for(
        &self,
        connection_key: &str,
    ) -> impl Iterator<Item = &ResidualTempRecord> {
        self.records
            .iter()
            .filter(move |record| record.connection_key == connection_key)
    }

    /// Append a freshly created residual temp record.
    ///
    /// In-memory only; pair with [`Self::save`] or prefer
    /// [`Self::add_and_save`], which also survives concurrent instances.
    pub fn add(&mut self, record: ResidualTempRecord) {
        // De-dupe by full identity; a re-created temp for the same
        // transfer+path just refreshes the existing record.
        if !self
            .records
            .iter()
            .any(|r| r.transfer_id == record.transfer_id && r.path == record.path)
        {
            self.records.push(record);
        }
    }

    /// Remove the record matching `(transfer_id, path)`.
    ///
    /// In-memory only; pair with [`Self::save`] or prefer
    /// [`Self::remove_and_save`]. Returns true if a record was removed.
    pub fn remove(&mut self, transfer_id: macsftp_core::TransferId, path: &str) -> bool {
        let before = self.records.len();
        self.records
            .retain(|record| record.transfer_id != transfer_id || record.path != path);
        self.records.len() != before
    }

    /// Flush the in-memory records to disk atomically.
    pub fn save(&self) -> Result<(), StorageError> {
        if self.initial_error.is_some() {
            return Err(StorageError::RecoveryRequired {
                path: self.path.as_str().to_string(),
            });
        }
        ResidualTempFile {
            version: ResidualTempFile::CURRENT_VERSION,
            records: self.records.clone(),
        }
        .save(&self.path)
    }

    /// Register a record and persist it through a serialized read-modify-
    /// write cycle: hold the advisory lock, reload what other app instances
    /// wrote, apply, save. Losing a registration would leave a
    /// `.macsftp-part` file that launch cleanup can never identify.
    pub fn add_and_save(&mut self, record: ResidualTempRecord) -> Result<(), StorageError> {
        self.modify(|store| store.add(record))
    }

    /// Drop a registration and persist under the advisory lock. Returns
    /// whether a record was removed.
    pub fn remove_and_save(
        &mut self,
        transfer_id: macsftp_core::TransferId,
        path: &str,
    ) -> Result<bool, StorageError> {
        let mut removed = false;
        self.modify(|store| removed = store.remove(transfer_id, path))?;
        Ok(removed)
    }

    fn modify(&mut self, apply: impl FnOnce(&mut Self)) -> Result<(), StorageError> {
        if self.initial_error.is_some() {
            return Err(StorageError::RecoveryRequired {
                path: self.path.as_str().to_string(),
            });
        }
        let _lock =
            FileLock::acquire(Path::new(self.path.as_str())).map_err(|error| StorageError::Io {
                path: self.path.as_str().to_string(),
                message: error.to_string(),
            })?;
        // Reload so registrations another instance made since our last sync
        // survive this write instead of being clobbered by a stale snapshot.
        self.records = ResidualTempFile::load(&self.path)?.records;
        apply(self);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{LocalPath, ResidualTempRecord, Timestamp, TransferDirection, TransferId};

    use super::{ResidualTempFile, ResidualTempStore, StorageError};

    // Unique temp path per call so concurrent tests never clobber each
    // other's residual_temp.json (plan §9).
    fn temp_path() -> LocalPath {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        LocalPath::new(format!(
            "{}/macsftp-residual-{}-{}.json",
            std::env::temp_dir().display(),
            std::process::id(),
            seq
        ))
    }

    fn sample(is_remote: bool, transfer_id: u64, path: &str) -> ResidualTempRecord {
        ResidualTempRecord {
            connection_key: if is_remote {
                "example.com:22".to_string()
            } else {
                "local".to_string()
            },
            direction: TransferDirection::Upload,
            transfer_id: TransferId(transfer_id),
            path: path.to_string(),
            created_at: Timestamp::from_secs_since_epoch(0),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let path = temp_path();
        let mut store = ResidualTempStore::open_or_empty(path.clone());
        assert!(store.records().is_empty());

        store.add(sample(true, 1, "/srv/.a.txt.macsftp-part-1"));
        store.add(sample(false, 2, "/tmp/.b.txt.macsftp-part-2"));
        store.save().expect("save");

        let reopened = ResidualTempStore::open_or_empty(path);
        assert_eq!(reopened.records().len(), 2);
    }

    /// A residual file written by a NEWER release must not be silently read
    /// and rewritten with lossy contents; it is version-incompatible input,
    /// classified as UnsupportedVersion like the other stores.
    #[test]
    fn newer_residual_file_version_is_rejected() {
        let path = temp_path();
        std::fs::write(
            path.as_str(),
            r#"{"version": 99, "records": [], "future_field": true}"#,
        )
        .expect("write future residual fixture");
        let raw_before = std::fs::read(path.as_str()).expect("read fixture before");

        let error = ResidualTempStore::open(path.clone())
            .err()
            .expect("future version must be rejected");
        assert!(matches!(
            error,
            StorageError::UnsupportedVersion { found: 99, .. }
        ));
        assert_eq!(
            match error {
                StorageError::UnsupportedVersion { supported, .. } => supported,
                _ => unreachable!("checked above"),
            },
            ResidualTempFile::CURRENT_VERSION
        );

        // Writes stay blocked so recovery data is never overwritten.
        let mut blocked = ResidualTempStore::open_or_empty(path.clone());
        assert!(blocked.initial_error().is_some());
        assert!(matches!(
            blocked.add_and_save(sample(true, 1, "/srv/.a.txt.macsftp-part-1")),
            Err(StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read(path.as_str()).expect("read fixture after"),
            raw_before
        );
    }

    /// Two app instances must not erase each other's registrations: the
    /// second writer reloads under the advisory lock first. Losing a record
    /// would leave a `.macsftp-part` file that cleanup can never identify.
    #[test]
    fn concurrent_instances_do_not_lose_residual_records() {
        let path = temp_path();
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
        let mut first = ResidualTempStore::open_or_empty(path.clone());
        let mut second = ResidualTempStore::open_or_empty(path.clone());

        first
            .add_and_save(sample(true, 1, "/srv/.a.txt.macsftp-part-1"))
            .expect("first instance registers");
        // `second` opened before the seed; its stale snapshot must not wipe
        // what `first` recorded.
        second
            .add_and_save(sample(false, 2, "/tmp/.b.txt.macsftp-part-2"))
            .expect("second instance registers");

        let reloaded = ResidualTempStore::open_or_empty(path.clone());
        assert_eq!(reloaded.records().len(), 2, "both records must survive");

        let _ = std::fs::remove_file(path.as_str());
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
    }

    #[test]
    fn missing_file_loads_empty() {
        let store = ResidualTempStore::open_or_empty(temp_path());
        assert!(store.records().is_empty());
    }

    #[test]
    fn corrupt_file_blocks_writes_and_is_preserved() {
        let path = temp_path();
        std::fs::write(path.as_str(), "{not json").expect("write corrupt residual fixture");
        let store = ResidualTempStore::open_or_empty(path.clone());

        assert!(store.records().is_empty());
        assert!(store.initial_error().is_some());
        assert!(matches!(
            store.save(),
            Err(crate::StorageError::RecoveryRequired { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("read preserved residual fixture"),
            "{not json"
        );
    }

    #[test]
    fn local_and_remote_queries_filter_correctly() {
        let mut store = ResidualTempStore::open_or_empty(temp_path());
        store.add(sample(true, 1, "/srv/.a.txt.macsftp-part-1"));
        store.add(sample(false, 2, "/tmp/.b.txt.macsftp-part-2"));
        store.add(sample(true, 3, "/srv/.c.txt.macsftp-part-3"));

        let locals: Vec<_> = store.local_records().cloned().collect();
        assert_eq!(locals.len(), 1);
        assert_eq!(locals[0].transfer_id, TransferId(2));

        let remotes: Vec<_> = store
            .remote_records_for("example.com:22")
            .cloned()
            .collect();
        assert_eq!(remotes.len(), 2);
    }

    #[test]
    fn remove_drops_by_identity() {
        let mut store = ResidualTempStore::open_or_empty(temp_path());
        store.add(sample(true, 1, "/srv/.a.txt.macsftp-part-1"));
        assert!(store.remove(TransferId(1), "/srv/.a.txt.macsftp-part-1"));
        assert!(store.records().is_empty());
        // Removing a missing identity is a no-op.
        assert!(!store.remove(TransferId(1), "/srv/.a.txt.macsftp-part-1"));
    }

    #[test]
    fn add_dedupes_by_identity() {
        let mut store = ResidualTempStore::open_or_empty(temp_path());
        store.add(sample(true, 1, "/srv/.a.txt.macsftp-part-1"));
        store.add(sample(true, 1, "/srv/.a.txt.macsftp-part-1"));
        assert_eq!(store.records().len(), 1);
    }
}
