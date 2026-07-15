mod diagnostics;

pub use diagnostics::{prune_log_files, write_crash_marker};

use macsftp_core::{FileKind, LocalEntry, LocalPath, Timestamp};

use std::os::unix::fs::PermissionsExt;

/// Deep link to the macOS Local Network privacy pane. GPUI owns URL opening;
/// this crate owns the platform-specific destination.
pub const MACOS_LOCAL_NETWORK_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";

pub fn crate_name() -> &'static str {
    "macsftp-platform"
}

pub fn core_crate_name() -> &'static str {
    macsftp_core::crate_name()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_file: LocalPath,
    pub profiles_file: LocalPath,
    pub known_hosts_file: LocalPath,
    /// Legacy file produced before transfer history became process-only.
    /// Startup removes it instead of loading a cross-launch catalog.
    pub legacy_transfer_history_file: LocalPath,
    pub residual_temp_file: LocalPath,
    pub session_file: LocalPath,
    pub recents_file: LocalPath,
    pub log_file: LocalPath,
    pub crash_marker_file: LocalPath,
}

impl AppPaths {
    pub fn from_home_dir(home_dir: impl AsRef<str>) -> Self {
        let home_dir = trim_trailing_slashes(home_dir.as_ref());
        let app_support_dir = format!("{home_dir}/Library/Application Support/macSFTP");
        let log_dir = format!("{home_dir}/Library/Logs/macSFTP");

        Self {
            config_file: LocalPath::new(format!("{app_support_dir}/config.json")),
            profiles_file: LocalPath::new(format!("{app_support_dir}/profiles.json")),
            known_hosts_file: LocalPath::new(format!("{app_support_dir}/known_hosts")),
            legacy_transfer_history_file: LocalPath::new(format!(
                "{app_support_dir}/transfer_history.json"
            )),
            residual_temp_file: LocalPath::new(format!("{app_support_dir}/residual_temp.json")),
            session_file: LocalPath::new(format!("{app_support_dir}/session.json")),
            recents_file: LocalPath::new(format!("{app_support_dir}/recents.json")),
            log_file: LocalPath::new(format!("{log_dir}/macsftp.log")),
            crash_marker_file: LocalPath::new(format!("{log_dir}/last-crash.txt")),
        }
    }

    /// Create the app support and log directories so writes (known_hosts,
    /// profiles, residual temp records, logs) succeed on first launch.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        for file in [
            &self.config_file,
            &self.profiles_file,
            &self.known_hosts_file,
            &self.residual_temp_file,
            &self.session_file,
            &self.recents_file,
            &self.log_file,
            &self.crash_marker_file,
        ] {
            if let Some(parent) = std::path::Path::new(file.as_str()).parent() {
                std::fs::create_dir_all(parent)?;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEntryDraft {
    pub name: String,
    pub path: LocalPath,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified_at: Option<Timestamp>,
    pub link_target: Option<LocalPath>,
}

impl LocalEntryDraft {
    pub fn into_entry(self) -> LocalEntry {
        LocalEntry {
            name: self.name,
            path: self.path,
            kind: self.kind,
            size: self.size,
            permissions: self.permissions,
            modified_at: self.modified_at,
            link_target: self.link_target,
        }
    }
}

/// Read the real local filesystem directory at `path` and return its
/// entries as `LocalEntry`s. Directories, regular files, and symlinks are
/// classified via `DirEntry::file_type` (which does not follow symlinks, so
/// a symlink's own kind is preserved); size, permissions, and mtime come
/// from `metadata` (which does follow symlinks to report the target's
/// attributes when the link resolves). Broken symlinks still appear with
/// kind `Symlink` and `None` for the attributes that could not be read.
///
/// On a directory that cannot be opened (missing, not a directory, or
/// permission denied) the `std::io::Error` from `read_dir` is returned so
/// the caller can surface it. Individual unreadable entries are *not* fatal:
/// they are surfaced with `FileKind::Unknown` and best-effort attributes so
/// the listing stays complete.
pub fn read_local_directory(path: &LocalPath) -> std::io::Result<Vec<LocalEntry>> {
    let mut entries: Vec<LocalEntry> = Vec::new();
    for entry in std::fs::read_dir(path.as_str())? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                // A single unreadable entry should not sink the whole listing.
                tracing::warn!(error = %error, "skipping unreadable local entry in {}", path.as_str());
                continue;
            }
        };

        let name = entry.file_name().to_string_lossy().into_owned();
        let full_path = LocalPath::new(entry.path().to_string_lossy().into_owned());

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                entries.push(
                    LocalEntryDraft {
                        name,
                        path: full_path,
                        kind: FileKind::Unknown,
                        size: None,
                        permissions: None,
                        modified_at: None,
                        link_target: None,
                    }
                    .into_entry(),
                );
                continue;
            }
        };

        let kind = if file_type.is_dir() {
            FileKind::Directory
        } else if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_file() {
            FileKind::File
        } else {
            FileKind::Other
        };

        // `metadata` follows symlinks; for a live link this yields the
        // target's attributes, which is what a transfer cares about.
        let (size, permissions, modified_at) = match entry.metadata() {
            Ok(metadata) => (
                if metadata.is_file() {
                    Some(metadata.len())
                } else {
                    None
                },
                Some(metadata.permissions().mode()),
                metadata.modified().ok().map(Timestamp::from_system_time),
            ),
            Err(_) => (None, None, None),
        };

        let link_target = if file_type.is_symlink() {
            std::fs::read_link(entry.path())
                .ok()
                .map(|target| LocalPath::new(target.to_string_lossy().into_owned()))
        } else {
            None
        };

        entries.push(
            LocalEntryDraft {
                name,
                path: full_path,
                kind,
                size,
                permissions,
                modified_at,
                link_target,
            }
            .into_entry(),
        );
    }
    Ok(entries)
}

fn trim_trailing_slashes(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');

    if trimmed.is_empty() { "/" } else { trimmed }
}

/// Delete a local file or directory. Directories use recursive
/// `remove_dir_all` (phase 1: confirm-then-execute).
pub fn delete_entry(path: &LocalPath, is_dir: bool) -> std::io::Result<()> {
    if is_dir {
        std::fs::remove_dir_all(path.as_str())
    } else {
        std::fs::remove_file(path.as_str())
    }
}

/// Rename or move a local path.
pub fn rename_entry(from: &LocalPath, to: &LocalPath) -> std::io::Result<()> {
    std::fs::rename(from.as_str(), to.as_str())
}

/// Create a single directory under `parent` named `name` (not recursive).
pub fn create_directory(parent: &LocalPath, name: &str) -> std::io::Result<LocalPath> {
    let path = parent.join(name);
    std::fs::create_dir(path.as_str())?;
    Ok(path)
}

/// Map a local IO error into a user-facing error for Fs ops.
pub fn local_fs_error(title: &str, error: &std::io::Error) -> macsftp_core::UserFacingError {
    use macsftp_core::{ErrorCode, UserFacingError};
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::AlreadyExists => ErrorCode::FileExists,
        _ => ErrorCode::Unknown,
    };
    let mut user_error = UserFacingError::new(
        code,
        title,
        "Check the path and permissions, then try again.",
    )
    .with_retryable(true);
    user_error.detail = Some(error.to_string());
    user_error
}

#[cfg(test)]
mod tests {
    use macsftp_core::{FileKind, LocalPath, Timestamp};

    use super::{
        AppPaths, LocalEntryDraft, core_crate_name, crate_name, create_directory, delete_entry,
        read_local_directory, rename_entry,
    };

    #[test]
    fn links_core_crate() {
        assert_eq!(crate_name(), "macsftp-platform");
        assert_eq!(core_crate_name(), "macsftp-core");
    }

    #[test]
    fn local_network_settings_url_targets_macos_system_settings() {
        assert!(super::MACOS_LOCAL_NETWORK_SETTINGS_URL.starts_with("x-apple.systempreferences:"));
        assert!(super::MACOS_LOCAL_NETWORK_SETTINGS_URL.contains("Privacy_LocalNetwork"));
    }

    #[test]
    fn builds_expected_macos_app_paths() {
        let paths = AppPaths::from_home_dir("/Users/alex/");

        assert_eq!(
            paths.config_file.as_str(),
            "/Users/alex/Library/Application Support/macSFTP/config.json"
        );
        assert_eq!(
            paths.profiles_file.as_str(),
            "/Users/alex/Library/Application Support/macSFTP/profiles.json"
        );
        assert_eq!(
            paths.known_hosts_file.as_str(),
            "/Users/alex/Library/Application Support/macSFTP/known_hosts"
        );
        assert_eq!(
            paths.log_file.as_str(),
            "/Users/alex/Library/Logs/macSFTP/macsftp.log"
        );
        assert_eq!(
            paths.crash_marker_file.as_str(),
            "/Users/alex/Library/Logs/macSFTP/last-crash.txt"
        );
        assert_eq!(
            paths.session_file.as_str(),
            "/Users/alex/Library/Application Support/macSFTP/session.json"
        );
        assert_eq!(
            paths.recents_file.as_str(),
            "/Users/alex/Library/Application Support/macSFTP/recents.json"
        );
    }

    #[test]
    fn converts_local_entry_draft_to_core_entry() {
        let entry = LocalEntryDraft {
            name: "data".to_string(),
            path: LocalPath::new("/Users/alex/data"),
            kind: FileKind::Directory,
            size: None,
            permissions: Some(0o755),
            modified_at: Some(Timestamp::from_secs_since_epoch(10)),
            link_target: None,
        }
        .into_entry();

        assert_eq!(entry.name, "data");
        assert_eq!(entry.path.as_str(), "/Users/alex/data");
        assert_eq!(entry.kind, FileKind::Directory);
        assert_eq!(entry.permissions, Some(0o755));
    }

    #[test]
    fn reads_real_directory_entries() {
        let dir = std::env::temp_dir().join("macsftp_read_local_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let base = LocalPath::new(dir.to_string_lossy().into_owned());

        std::fs::write(dir.join("note.txt"), b"hello").expect("write note.txt");
        std::fs::create_dir(dir.join("subdir")).expect("create subdir");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("note.txt"), dir.join("link.txt"))
            .expect("create symlink");

        let entries = read_local_directory(&base).expect("directory is readable");
        let by_name: std::collections::HashMap<_, _> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();

        assert_eq!(by_name.get("note.txt"), Some(&FileKind::File));
        assert_eq!(by_name.get("subdir"), Some(&FileKind::Directory));
        assert_eq!(by_name.get("link.txt"), Some(&FileKind::Symlink));

        let note = entries
            .iter()
            .find(|e| e.name == "note.txt")
            .expect("note.txt present");
        assert_eq!(note.size, Some(5));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_returns_io_error() {
        let result = read_local_directory(&LocalPath::new(
            "/nonexistent/macsftp/path/that/does/not/exist",
        ));
        assert!(result.is_err());
    }

    fn temp_ops_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "macsftp-platform-ops-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp ops dir");
        dir
    }

    #[test]
    fn delete_file_and_empty_directory() {
        let dir = temp_ops_dir("delete-simple");
        let file = dir.join("note.txt");
        std::fs::write(&file, b"hi").expect("write file");
        let empty = dir.join("empty");
        std::fs::create_dir(&empty).expect("create empty");

        delete_entry(&LocalPath::new(file.to_string_lossy().into_owned()), false)
            .expect("delete file");
        assert!(!file.exists());

        delete_entry(&LocalPath::new(empty.to_string_lossy().into_owned()), true)
            .expect("delete empty dir");
        assert!(!empty.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_non_empty_directory_recursively() {
        let dir = temp_ops_dir("delete-recursive");
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).expect("nested");
        std::fs::write(nested.join("a.txt"), b"a").expect("write nested");

        delete_entry(&LocalPath::new(dir.to_string_lossy().into_owned()), true)
            .expect("recursive delete");
        assert!(!dir.exists());
    }

    #[test]
    fn rename_and_create_directory() {
        let dir = temp_ops_dir("rename-create");
        let base = LocalPath::new(dir.to_string_lossy().into_owned());
        let from = base.join("old.txt");
        std::fs::write(from.as_str(), b"x").expect("write");
        let to = base.join("new.txt");
        rename_entry(&from, &to).expect("rename");
        assert!(!std::path::Path::new(from.as_str()).exists());
        assert!(std::path::Path::new(to.as_str()).exists());

        let created = create_directory(&base, "folder").expect("create dir");
        assert!(std::path::Path::new(created.as_str()).is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_missing_path_returns_error() {
        let result = delete_entry(&LocalPath::new("/nonexistent/macsftp/delete-me"), false);
        assert!(result.is_err());
    }
}
