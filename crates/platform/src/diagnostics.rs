use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

struct LogFile {
    path: PathBuf,
    modified: SystemTime,
    bytes: u64,
}

/// Keep the newest diagnostic logs within both a file-count and byte budget.
/// Symlinks and non-files are ignored so cleanup never follows user-controlled
/// paths outside the log directory.
pub fn prune_log_files(
    directory: &Path,
    prefix: &str,
    max_files: usize,
    max_total_bytes: u64,
) -> std::io::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(prefix) {
            continue;
        }
        let metadata = entry.metadata()?;
        files.push(LogFile {
            path: entry.path(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            bytes: metadata.len(),
        });
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.modified));

    let mut kept_files = 0usize;
    let mut kept_bytes = 0u64;
    for file in files {
        let within_count = kept_files < max_files;
        let within_bytes = kept_bytes.saturating_add(file.bytes) <= max_total_bytes;
        if within_count && within_bytes {
            kept_files += 1;
            kept_bytes = kept_bytes.saturating_add(file.bytes);
        } else {
            std::fs::remove_file(file.path)?;
        }
    }
    Ok(())
}

/// Append a deliberately payload-free crash marker. Panic messages are not
/// included because third-party errors can contain hostnames or paths.
pub fn write_crash_marker(
    path: &Path,
    version: &str,
    source_file_name: Option<&str>,
    source_line: Option<u32>,
) -> std::io::Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    writeln!(
        file,
        "timestamp={timestamp} version={version} arch={} source={} line={}",
        std::env::consts::ARCH,
        source_file_name.unwrap_or("unknown"),
        source_line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )?;
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "macsftp-diagnostics-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn pruning_keeps_only_the_configured_number_of_logs() {
        let directory = temp_dir("prune");
        std::fs::create_dir_all(&directory).expect("create diagnostic fixture directory");
        for name in ["macsftp.log.1", "macsftp.log.2", "macsftp.log.3"] {
            std::fs::write(directory.join(name), name).expect("write diagnostic fixture");
        }

        prune_log_files(&directory, "macsftp.log", 1, 1024).expect("prune diagnostic logs");

        let remaining = std::fs::read_dir(&directory)
            .expect("read pruned directory")
            .count();
        assert_eq!(remaining, 1);
        std::fs::remove_dir_all(directory).expect("remove diagnostic fixture directory");
    }

    #[test]
    fn pruning_removes_a_single_log_larger_than_the_byte_budget() {
        let directory = temp_dir("oversized");
        std::fs::create_dir_all(&directory).expect("create diagnostic fixture directory");
        std::fs::write(directory.join("macsftp.log.1"), [0u8; 16])
            .expect("write oversized diagnostic fixture");

        prune_log_files(&directory, "macsftp.log", 7, 8).expect("prune diagnostic logs");

        let remaining = std::fs::read_dir(&directory)
            .expect("read pruned directory")
            .count();
        assert_eq!(remaining, 0);
        std::fs::remove_dir_all(directory).expect("remove diagnostic fixture directory");
    }

    #[test]
    fn crash_marker_does_not_need_a_panic_payload() {
        let directory = temp_dir("crash-marker");
        std::fs::create_dir_all(&directory).expect("create crash marker directory");
        let path = directory.join("last-crash.txt");

        write_crash_marker(&path, "0.1.0", Some("main.rs"), Some(42)).expect("write crash marker");
        let marker = std::fs::read_to_string(&path).expect("read crash marker");
        assert!(marker.contains("version=0.1.0"));
        assert!(marker.contains("source=main.rs"));
        assert!(!marker.contains("password"));
        std::fs::remove_dir_all(directory).expect("remove crash marker directory");
    }
}
