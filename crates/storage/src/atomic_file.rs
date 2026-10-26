use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temp_path_for(path: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "macsftp-data".to_string());
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ))
}

/// Phase-aware failure of an atomic file replacement. Compensating actions
/// (such as rolling a Keychain secret back) are only safe before the rename
/// lands, so callers must be able to tell the two phases apart.
#[derive(Debug)]
pub enum AtomicWriteError {
    /// Failed at or before the rename: the destination still holds its
    /// previous contents.
    NotCommitted(std::io::Error),
    /// The rename landed but the parent directory could not be synced: the
    /// new contents are live while their durability is unproven.
    ReplacedButNotDurable(std::io::Error),
}

impl std::fmt::Display for AtomicWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCommitted(error) => {
                write!(formatter, "replacement was not committed: {error}")
            }
            Self::ReplacedButNotDurable(error) => write!(
                formatter,
                "contents were replaced but durability is unproven: {error}"
            ),
        }
    }
}

impl std::error::Error for AtomicWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotCommitted(error) | Self::ReplacedButNotDurable(error) => Some(error),
        }
    }
}

/// Durably replace an application-owned data file without exposing its
/// metadata to other local users. The temporary file is created beside the
/// destination so rename remains atomic on the same filesystem.
pub fn write_private_file_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    match write_private_file_phased(path, contents) {
        Ok(()) => Ok(()),
        Err(AtomicWriteError::NotCommitted(error)) => Err(error),
        Err(AtomicWriteError::ReplacedButNotDurable(error)) => Err(std::io::Error::new(
            error.kind(),
            format!(
                "{} (at {})",
                AtomicWriteError::ReplacedButNotDurable(error),
                path.display()
            ),
        )),
    }
}

pub(crate) fn write_private_file_phased(
    path: &Path,
    contents: &[u8],
) -> Result<(), AtomicWriteError> {
    write_phased_with_parent_sync(path, contents, File::sync_all)
}

/// Test seam: `sync_parent` stands in for the real parent-directory fsync so
/// durability-degraded commits can be simulated deterministically.
pub(crate) fn write_phased_with_parent_sync(
    path: &Path,
    contents: &[u8],
    sync_parent: impl FnOnce(&File) -> std::io::Result<()>,
) -> Result<(), AtomicWriteError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = temp_path_for(path);
    // Hold the parent directory open across the whole sequence so
    // permission-class failures surface before the rename instead of after
    // it (macOS supports fsync on a directory descriptor).
    let parent_directory = File::open(parent).map_err(AtomicWriteError::NotCommitted)?;

    let staging = (|| -> std::io::Result<()> {
        let mut temp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)?;
        temp_file.write_all(contents)?;
        temp_file.sync_all()
    })();
    if let Err(error) = staging {
        return Err(discard_temp_file(&temp_path, error));
    }

    if let Err(rename_error) = std::fs::rename(&temp_path, path) {
        return Err(discard_temp_file(&temp_path, rename_error));
    }
    // Past this point the destination already holds the new contents, so
    // failures can no longer mean "not committed".
    sync_parent(&parent_directory).map_err(AtomicWriteError::ReplacedButNotDurable)
}

fn discard_temp_file(temp_path: &Path, failure: std::io::Error) -> AtomicWriteError {
    match std::fs::remove_file(temp_path) {
        Ok(()) => AtomicWriteError::NotCommitted(failure),
        Err(remove_error) if remove_error.kind() == std::io::ErrorKind::NotFound => {
            AtomicWriteError::NotCommitted(failure)
        }
        Err(remove_error) => AtomicWriteError::NotCommitted(std::io::Error::new(
            failure.kind(),
            format!(
                "{failure}; could not remove temporary file {}: {remove_error}",
                temp_path.display()
            ),
        )),
    }
}

/// Append one line to an application-owned file without ever discarding what
/// is already there.
///
/// The read, append, and atomic replace happen while holding the store's
/// cross-process advisory lock, so two windows confirming an unknown host
/// cannot each read the old file and overwrite the other's entry. The file is
/// handled as bytes: a non-UTF-8 comment must not make the append unreadable.
pub fn append_private_line_atomically(path: &Path, line: &str) -> std::io::Result<()> {
    if line.contains('\n') || line.contains('\r') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "line cannot contain newline characters",
        ));
    }
    let _lock = crate::file_lock::FileLock::acquire(path)?;
    let mut content = match std::fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if content.last().is_some_and(|byte| *byte != b'\n') {
        content.push(b'\n');
    }
    content.extend_from_slice(line.as_bytes());
    content.push(b'\n');
    write_private_file_atomically(path, &content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(label: &str) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "macsftp-atomic-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn atomic_write_replaces_content_with_private_permissions() {
        let directory = temp_dir("replace");
        std::fs::create_dir_all(&directory).expect("create atomic fixture directory");
        let path = directory.join("state.json");
        write_private_file_atomically(&path, b"first").expect("write first state");
        write_private_file_atomically(&path, b"second").expect("replace state");

        assert_eq!(std::fs::read(&path).expect("read state"), b"second");
        let mode = std::fs::metadata(&path)
            .expect("read state metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(directory).expect("remove atomic fixture directory");
    }

    #[test]
    fn failed_write_preserves_existing_destination() {
        let directory = temp_dir("failure");
        std::fs::create_dir_all(&directory).expect("create failure fixture directory");
        let path = directory.join("state.json");
        write_private_file_atomically(&path, b"stable").expect("write stable state");
        let missing_parent_path = directory.join("missing").join("state.json");

        assert!(write_private_file_atomically(&missing_parent_path, b"new").is_err());
        assert_eq!(std::fs::read(&path).expect("read stable state"), b"stable");
        std::fs::remove_dir_all(directory).expect("remove failure fixture directory");
    }

    #[test]
    fn parent_fsync_failure_reports_replacement_without_durability() {
        let directory = temp_dir("not-durable");
        std::fs::create_dir_all(&directory).expect("create not-durable fixture directory");
        let path = directory.join("state.json");
        write_private_file_atomically(&path, b"old").expect("write old state");

        let result = write_phased_with_parent_sync(&path, b"new", |_parent| {
            Err(std::io::Error::other("injected fsync failure"))
        });

        match result {
            Err(AtomicWriteError::ReplacedButNotDurable(_)) => {}
            other => panic!("expected ReplacedButNotDurable, got {other:?}"),
        }
        // The rename landed before the failing sync, so the destination
        // already holds the new contents.
        assert_eq!(std::fs::read(&path).expect("read replaced state"), b"new");
        std::fs::remove_dir_all(directory).expect("remove not-durable fixture directory");
    }

    #[test]
    fn pre_rename_failure_reports_not_committed() {
        let directory = temp_dir("not-committed");
        std::fs::create_dir_all(&directory).expect("create not-committed fixture directory");
        let path = directory.join("state.json");
        write_private_file_atomically(&path, b"old").expect("write old state");

        // A missing temporary-file parent makes the staging open fail before
        // any rename can happen.
        let nested_missing = directory.join("nope").join("state.json");
        let staged_failure = write_phased_with_parent_sync(&nested_missing, b"new", File::sync_all);

        match staged_failure {
            Err(AtomicWriteError::NotCommitted(_)) => {}
            other => panic!("expected NotCommitted, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&path).expect("read preserved state"),
            b"old",
            "destination must keep its previous contents when the commit never happened"
        );
        std::fs::remove_dir_all(directory).expect("remove not-committed fixture directory");
    }

    #[test]
    fn rename_failure_cleans_up_staging_file() {
        let directory = temp_dir("rename-failure");
        std::fs::create_dir_all(&directory).expect("create rename-failure directory");
        let dest_dir = directory.join("existing_dir");
        std::fs::create_dir(&dest_dir).expect("create conflicting directory");

        let result = write_phased_with_parent_sync(&dest_dir, b"content", File::sync_all);
        match result {
            Err(AtomicWriteError::NotCommitted(_)) => {}
            other => panic!("expected NotCommitted error, got {other:?}"),
        }

        let entries: Vec<_> = std::fs::read_dir(&directory)
            .expect("read_dir")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "only the existing_dir should remain; staging file must be cleaned up"
        );
        assert_eq!(entries[0].file_name(), "existing_dir");
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn append_private_line_preserves_non_utf8_bytes_and_is_idempotent_per_call() {
        let directory = temp_dir("append-line");
        std::fs::create_dir_all(&directory).expect("create append fixture directory");
        let path = directory.join("known_hosts");
        let mut original = b"pinned.example ssh-ed25519 AAAA\n# caf".to_vec();
        original.push(0xE9);
        original.push(b'\n');
        std::fs::write(&path, &original).expect("write fixture");

        append_private_line_atomically(&path, "new.example ssh-ed25519 BBBB")
            .expect("first append");
        append_private_line_atomically(&path, "other.example ssh-ed25519 CCCC")
            .expect("second append");

        let written = std::fs::read(&path).expect("read appended file");
        assert!(
            written.starts_with(&original),
            "existing bytes must survive verbatim"
        );
        assert!(
            written.ends_with(b"new.example ssh-ed25519 BBBB\nother.example ssh-ed25519 CCCC\n")
        );

        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "appended trust data stays private");

        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn append_private_line_rejects_newlines() {
        let directory = temp_dir("reject-newlines");
        std::fs::create_dir_all(&directory).expect("create directory");
        let path = directory.join("known_hosts");
        assert!(append_private_line_atomically(&path, "line1\nline2").is_err());
        assert!(append_private_line_atomically(&path, "line1\rline2").is_err());
        std::fs::remove_dir_all(&directory).ok();
    }
}
