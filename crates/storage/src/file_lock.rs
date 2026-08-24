use std::fs::File;
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
        let lock_file = File::create(Path::new(&lock_path))?;
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
