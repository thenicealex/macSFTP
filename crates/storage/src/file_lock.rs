use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rustix::fd::AsFd;
use rustix::fs::{FlockOperation, flock};

/// An exclusive advisory lock (`flock(2)`) held on a sidecar `{path}.lock`
/// file for the duration of one store transaction. The lock file itself
/// carries no data; it only serializes writers across `ProfileStore`
/// instances and processes.
pub(crate) struct FileLock {
    // Field order matters: the lock is released when the descriptor closes,
    // which happens after `flock(LOCK_UN)` in `Drop`.
    lock_file: File,
}

impl FileLock {
    /// Acquire an exclusive advisory lock on `<file_path>.lock`, blocking
    /// until any other holder releases it. Creating the sidecar never
    /// truncates an existing lock file, so concurrent openers stay safe.
    pub(crate) fn acquire(file_path: &Path) -> std::io::Result<Self> {
        let mut lock_path = file_path.as_os_str().to_owned();
        lock_path.push(".lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let lock_file = options.open(Path::new(&lock_path))?;
        flock(lock_file.as_fd(), FlockOperation::LockExclusive)?;
        Ok(Self { lock_file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Err(error) = flock(self.lock_file.as_fd(), FlockOperation::Unlock) {
            // Releasing a held flock on a still-open descriptor cannot fail
            // in practice; surface anything unexpected so a stuck lock stays
            // diagnosable instead of silently leaked.
            tracing::warn!(%error, "macsftp-storage: failed to release profile lock");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_LOCK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn file_lock_preserves_content_and_sets_mode_0600() {
        use std::os::unix::fs::PermissionsExt as _;

        let seq = TEST_LOCK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("macsftp-lock-test-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create test lock directory");

        // 1. Newly created lock file gets mode 0o600.
        let target_new = dir.join("new_store.json");
        let lock_path_new = dir.join("new_store.json.lock");
        {
            let lock = FileLock::acquire(&target_new).expect("acquire lock on new file");
            drop(lock);
        }
        let mode = std::fs::metadata(&lock_path_new)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        // 2. Existing lock file with content is not truncated on acquire.
        let target_existing = dir.join("existing_store.json");
        let lock_path_existing = dir.join("existing_store.json.lock");
        std::fs::write(&lock_path_existing, b"sentinel").expect("write sentinel lock file");

        {
            let lock = FileLock::acquire(&target_existing).expect("acquire lock on existing file");
            let content = std::fs::read(&lock_path_existing).expect("read lock file");
            assert_eq!(
                content, b"sentinel",
                "lock acquisition must not truncate existing file"
            );
            drop(lock);
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
