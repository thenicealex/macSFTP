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

/// Durably replace an application-owned data file without exposing its
/// metadata to other local users. The temporary file is created beside the
/// destination so rename remains atomic on the same filesystem.
pub fn write_private_file_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = temp_path_for(path);
    let result = (|| {
        let mut temp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)?;
        temp_file.write_all(contents)?;
        temp_file.sync_all()?;
        std::fs::rename(&temp_path, path)?;
        File::open(parent)?.sync_all()
    })();
    match result {
        Ok(()) => Ok(()),
        Err(write_error) => match std::fs::remove_file(&temp_path) {
            Ok(()) => Err(write_error),
            Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => {
                Err(write_error)
            }
            Err(cleanup_error) => Err(std::io::Error::new(
                write_error.kind(),
                format!(
                    "{write_error}; could not remove temporary file {}: {cleanup_error}",
                    temp_path.display()
                ),
            )),
        },
    }
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
}
