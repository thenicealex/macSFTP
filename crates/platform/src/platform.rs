use macsftp_core::{FileKind, LocalEntry, LocalPath, Timestamp};

use std::os::unix::fs::PermissionsExt;

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
    pub transfer_history_file: LocalPath,
    pub residual_temp_file: LocalPath,
    pub log_file: LocalPath,
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
            transfer_history_file: LocalPath::new(format!(
                "{app_support_dir}/transfer_history.json"
            )),
            residual_temp_file: LocalPath::new(format!("{app_support_dir}/residual_temp.json")),
            log_file: LocalPath::new(format!("{log_dir}/macsftp.log")),
        }
    }

    /// Create the app support and log directories so writes (known_hosts,
    /// profiles, transfer history, logs) succeed on first launch.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        for file in [
            &self.config_file,
            &self.profiles_file,
            &self.known_hosts_file,
            &self.transfer_history_file,
            &self.residual_temp_file,
            &self.log_file,
        ] {
            if let Some(parent) = std::path::Path::new(file.as_str()).parent() {
                std::fs::create_dir_all(parent)?;
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

#[cfg(test)]
mod tests {
    use macsftp_core::{FileKind, LocalPath, Timestamp};

    use super::{AppPaths, LocalEntryDraft, core_crate_name, crate_name, read_local_directory};

    #[test]
    fn links_core_crate() {
        assert_eq!(crate_name(), "macsftp-platform");
        assert_eq!(core_crate_name(), "macsftp-core");
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
}
