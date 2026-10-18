mod diagnostics;

pub use diagnostics::{prune_log_files, write_crash_marker};

use macsftp_core::{FileKind, LocalEntry, LocalPath, Timestamp};

use notify::Watcher;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Deep link to the macOS Local Network privacy pane. GPUI owns URL opening;
/// this crate owns the platform-specific destination.
pub const MACOS_LOCAL_NETWORK_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork";

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
    pub edits_dir: LocalPath,
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
            edits_dir: LocalPath::new(format!("{app_support_dir}/edits")),
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

/// Delete the entire edits directory and recreate it empty. Startup calls this
/// to discard edit sessions from the previous run (sessions do not persist
/// across launches). NotFound is treated as success.
pub fn clear_edits_dir(edits_dir: &LocalPath) -> std::io::Result<()> {
    match std::fs::remove_dir_all(edits_dir.as_str()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(edits_dir.as_str())?;
    std::fs::set_permissions(edits_dir.as_str(), std::fs::Permissions::from_mode(0o700))
}

/// Remove one remote-edit session's temp directory (`<edits>/<run>/<id>/`).
/// NotFound is treated as success so callers can best-effort clean up without
/// distinguishing "already gone" from a real error. Any other failure is
/// returned so the caller can log or record a residual (audit APP-EDIT-003).
pub fn cleanup_edit_session_dir(session_dir: &LocalPath) -> std::io::Result<()> {
    match std::fs::remove_dir_all(session_dir.as_str()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

/// Construct a command to open a temporary edit file via macOS `open`.
///
/// When editor is `Some(app)`, use `open -a <app>` (the user pinned a specific
/// application, which forces that handler).
///
/// When editor is `None`, use `open -t` to open with the default **text
/// editor** rather than the file's default associated application. This is a
/// security boundary, not a preference: a bare `open <file>` dispatches by
/// content type through LaunchServices, so a remote file named `evil.command`,
/// `evil.terminal`, `evil.webloc`, or `evil.workflow` would be *executed* (or
/// navigate to an attacker URL) on a single click rather than edited — and the
/// download preserves the remote exec bit, so the file is already +x. `-t`
/// always routes to a text editor and never executes, closing that vector
/// regardless of the extension or mode. Editing a non-text file this way is a
/// deliberate tradeoff; a user who needs another app pins one via
/// `external_editor` (the `Some` branch).
///
/// Extracted for unit testing argument wiring without spawning.
pub fn build_open_command(temp: &LocalPath, editor: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    match editor {
        None => {
            cmd.args(["-t", temp.as_str()]);
        }
        Some(app) => {
            cmd.args(["-a", app, temp.as_str()]);
        }
    }
    cmd
}

/// Open a temporary file for editing. `open` returns immediately, not tying
/// the subprocess lifecycle to the app, which naturally satisfies app-level monitoring.
pub fn open_in_editor(temp: &LocalPath, editor: Option<&str>) -> std::io::Result<()> {
    build_open_command(temp, editor).spawn().map(|_| ())
}

/// External diff tools supported for visual conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTool {
    FileMerge(std::path::PathBuf),
    VsCode(std::path::PathBuf),
}

impl DiffTool {
    pub fn name(&self) -> &'static str {
        match self {
            Self::FileMerge(_) => "FileMerge (opendiff)",
            Self::VsCode(_) => "Visual Studio Code",
        }
    }

    /// Launch the diff tool comparing `local_path` and `remote_snapshot_path`.
    /// Uses `spawn()` ONLY (NEVER `output()`, as `opendiff` blocks the GUI).
    ///
    /// The child is reaped on a detached thread: `opendiff` stays alive for as
    /// long as its FileMerge window is open, and dropping a `Child` without
    /// waiting would leave a zombie for the rest of the app's lifetime.
    pub fn launch(
        &self,
        local_path: &std::path::Path,
        remote_path: &std::path::Path,
    ) -> std::io::Result<()> {
        let mut command = match self {
            Self::FileMerge(bin) => {
                let mut cmd = std::process::Command::new(bin);
                cmd.arg(local_path).arg(remote_path);
                cmd
            }
            Self::VsCode(bin) => {
                let mut cmd = std::process::Command::new(bin);
                cmd.arg("--diff").arg(local_path).arg(remote_path);
                cmd
            }
        };
        // Start the reaper first: if the thread cannot be created, the child
        // has not been spawned yet and nothing leaks.
        let (child_tx, child_rx) = std::sync::mpsc::channel::<std::process::Child>();
        std::thread::Builder::new()
            .name("macsftp-diff-reaper".into())
            .spawn(move || {
                while let Ok(mut child) = child_rx.recv() {
                    match child.wait() {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                "diff tool process could not be reaped"
                            );
                        }
                    }
                }
            })
            .map_err(std::io::Error::other)?;
        let child = command.spawn()?;
        if let Err(error) = child_tx.send(child) {
            let mut child = error.0;
            if let Err(kill_error) = child.kill() {
                tracing::warn!(error = %kill_error, "could not kill the diff tool process");
            }
            if let Err(wait_error) = child.wait() {
                tracing::warn!(error = %wait_error, "could not reap the diff tool process");
            }
            return Err(std::io::Error::other("diff tool reaper is unavailable"));
        }
        Ok(())
    }
}

pub const DEFAULT_DIFF_CANDIDATES: &[&str] = &[
    "/usr/bin/opendiff",
    "/usr/local/bin/code",
    "/opt/homebrew/bin/code",
];

pub fn default_diff_candidates() -> Vec<std::path::PathBuf> {
    DEFAULT_DIFF_CANDIDATES
        .iter()
        .map(std::path::PathBuf::from)
        .collect()
}

/// Detect an available diff tool from candidate absolute paths.
/// First existing candidate wins.
pub fn detect_diff_tool(candidates: &[std::path::PathBuf]) -> Option<DiffTool> {
    for candidate in candidates {
        if candidate.is_file() {
            let file_name = candidate.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if file_name == "opendiff" {
                return Some(DiffTool::FileMerge(candidate.clone()));
            } else if file_name == "code" {
                return Some(DiffTool::VsCode(candidate.clone()));
            }
        }
    }
    None
}

/// Detect the default diff tool installed on this macOS system.
pub fn detect_default_diff_tool() -> Option<DiffTool> {
    detect_diff_tool(&default_diff_candidates())
}

/// Returns true if the file name matches temporary editor artifacts
/// that must be filtered out (`.*.swp`, `*~`, `*.tmp`, `4913`).
pub fn is_temp_artifact(file_name: &str) -> bool {
    if file_name == "4913" {
        return true;
    }
    if file_name.ends_with('~') || file_name.ends_with(".tmp") {
        return true;
    }
    if file_name.starts_with('.') && file_name.ends_with(".swp") {
        return true;
    }
    false
}

/// Parse an event path under `watch_root`, extracting `(session_id, full_path)`
/// when the path has the format `<watch_root>/<session_id>/<file_name>`
/// and `file_name` is not an editor temporary artifact.
pub fn parse_edit_event_path(
    watch_root: &std::path::Path,
    path: &std::path::Path,
) -> Option<(macsftp_core::EditSessionId, String)> {
    parse_edit_event_path_under(watch_root, watch_root, path)
}

/// Same as [`parse_edit_event_path`], but strips `watched_root` (which may be
/// canonicalized) and reports the path under `report_root` (the configured
/// root). FSEvents resolves symlinks, so the reported path must be rebuilt
/// from the configured root or it will not match an edit session's path.
fn parse_edit_event_path_under(
    report_root: &std::path::Path,
    watched_root: &std::path::Path,
    path: &std::path::Path,
) -> Option<(macsftp_core::EditSessionId, String)> {
    let relative = path
        .strip_prefix(watched_root)
        .ok()
        .map(|p| p.to_path_buf())
        .or_else(|| {
            let canon_root = watched_root.canonicalize().ok()?;
            let canon_path = path.canonicalize().ok()?;
            canon_path
                .strip_prefix(canon_root)
                .ok()
                .map(|p| p.to_path_buf())
        })?;
    let mut components = relative.components();
    let session_component = components.next()?.as_os_str().to_str()?;
    let file_component = components.next()?.as_os_str().to_str()?;
    if components.next().is_some() {
        return None;
    }
    if is_temp_artifact(file_component) {
        return None;
    }
    let session_num: u64 = session_component.parse().ok()?;
    // Rebuild the path from the configured root instead of returning the
    // watcher's path: FSEvents reports resolved paths (`/private/var/...`),
    // while edit sessions store paths built from `edits_dir`. Returning the
    // resolved form would make every equality check against a session path
    // fail, silently disabling dirty detection.
    Some((
        macsftp_core::EditSessionId(session_num),
        report_root.join(&relative).to_string_lossy().to_string(),
    ))
}

struct DebounceState {
    pending: std::collections::HashMap<macsftp_core::EditSessionId, (String, Instant)>,
    stopped: bool,
    /// Bumped by the notify callback. The worker compares it after emitting to
    /// detect an insert that happened while it was not holding the lock; a
    /// plain `notify_one` before `wait` starts would otherwise be lost and the
    /// save would stay undelivered until some unrelated event arrived.
    generation: u64,
}

/// Process-wide watcher on `<edits_dir>/<edit_run_id>/` monitoring for
/// external editor modifications.
pub struct EditWatcher {
    _watcher: notify::RecommendedWatcher,
    state: Arc<(Mutex<DebounceState>, Condvar)>,
    worker_handle: Option<std::thread::JoinHandle<()>>,
}

impl EditWatcher {
    /// Start watching the edit run directory with the default 700ms debounce.
    pub fn start(
        watch_root: std::path::PathBuf,
        on_modified: impl Fn(macsftp_core::EditSessionId, String) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        Self::start_with_debounce(watch_root, Duration::from_millis(700), on_modified)
    }

    /// Start watching `watch_root` recursively, debouncing events per session
    /// for `debounce_duration`.
    pub fn start_with_debounce(
        watch_root: std::path::PathBuf,
        debounce_duration: Duration,
        on_modified: impl Fn(macsftp_core::EditSessionId, String) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        // Ensure directory exists at startup before watching.
        std::fs::create_dir_all(&watch_root)?;
        let canonical_root = match watch_root.canonicalize() {
            Ok(root) => root,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "could not canonicalize the edit watch root; events may not match session paths"
                );
                watch_root.clone()
            }
        };

        let state = Arc::new((
            Mutex::new(DebounceState {
                pending: std::collections::HashMap::new(),
                stopped: false,
                generation: 0,
            }),
            Condvar::new(),
        ));

        let report_root = watch_root.clone();
        let watch_root_clone = canonical_root.clone();
        let state_clone = Arc::clone(&state);
        let mut watcher = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                let event = match res {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!(error = %error, "edit watcher reported an error");
                        return;
                    }
                };
                for path in event.paths {
                    if let Some((session_id, path_str)) =
                        parse_edit_event_path_under(&report_root, &watch_root_clone, &path)
                        && let Ok(mut guard) = state_clone.0.lock()
                    {
                        let deadline = Instant::now() + debounce_duration;
                        guard.pending.insert(session_id, (path_str, deadline));
                        guard.generation = guard.generation.wrapping_add(1);
                        state_clone.1.notify_one();
                    }
                }
            },
            notify::Config::default(),
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;

        watcher
            .watch(&canonical_root, notify::RecursiveMode::Recursive)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let state_worker = Arc::clone(&state);
        let on_modified = Arc::new(on_modified);
        let worker_handle = std::thread::Builder::new()
            .name("macsftp-edit-debouncer".into())
            .spawn(move || {
                loop {
                    let mut to_emit = Vec::new();
                    let (wait_opt, observed_generation) = {
                        let mut guard = match state_worker.0.lock() {
                            Ok(g) => g,
                            Err(_) => break,
                        };
                        if guard.stopped {
                            break;
                        }
                        let now = Instant::now();
                        let ready_ids: Vec<macsftp_core::EditSessionId> = guard
                            .pending
                            .iter()
                            .filter(|(_, (_, deadline))| *deadline <= now)
                            .map(|(id, _)| *id)
                            .collect();

                        for id in ready_ids {
                            if let Some((path, _)) = guard.pending.remove(&id) {
                                to_emit.push((id, path));
                            }
                        }

                        let earliest = guard.pending.values().map(|(_, d)| *d).min();
                        let wait_opt = match earliest {
                            None => None,
                            Some(d) if d > now => Some(d - now),
                            Some(_) => Some(Duration::from_millis(10)),
                        };
                        (wait_opt, guard.generation)
                    };

                    for (id, path) in to_emit {
                        on_modified(id, path);
                    }

                    let guard = match state_worker.0.lock() {
                        Ok(g) => g,
                        Err(_) => break,
                    };
                    if guard.stopped {
                        break;
                    }
                    // A save may have arrived after `wait_opt` was computed;
                    // recompute instead of sleeping on the stale decision.
                    if guard.generation != observed_generation {
                        continue;
                    }
                    match wait_opt {
                        None => {
                            if state_worker.1.wait(guard).is_err() {
                                break;
                            }
                        }
                        Some(timeout) => {
                            if state_worker.1.wait_timeout(guard, timeout).is_err() {
                                break;
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            _watcher: watcher,
            state,
            worker_handle: Some(worker_handle),
        })
    }
}

impl Drop for EditWatcher {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.0.lock() {
            guard.stopped = true;
            self.state.1.notify_all();
        }
        if let Some(handle) = self.worker_handle.take()
            && let Err(e) = handle.join()
        {
            tracing::error!("error joining edit debouncer thread: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::{FileKind, LocalPath, Timestamp};

    use super::{
        AppPaths, LocalEntryDraft, create_directory, delete_entry, read_local_directory,
        rename_entry,
    };

    /// Per-process unique sequence so parallel test invocations never share a
    /// temp directory (AGENTS.md §9; audit TEST-001).
    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "macsftp-platform-test-{label}-{}-{sequence}",
            std::process::id()
        ))
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
        let dir = unique_temp_dir("read_local");
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

    #[test]
    fn app_paths_expose_edits_dir_under_app_support() {
        let paths = super::AppPaths::from_home_dir("/Users/tester");
        assert_eq!(
            paths.edits_dir.as_str(),
            "/Users/tester/Library/Application Support/macSFTP/edits"
        );
    }

    #[test]
    fn clear_edits_dir_removes_contents_and_recreates() {
        let base = std::env::temp_dir().join(format!("macsftp-edits-clear-{}", std::process::id()));
        let edits = macsftp_core::LocalPath::new(base.to_string_lossy().to_string());
        std::fs::create_dir_all(base.join("session-1")).expect("create session dir");
        std::fs::write(base.join("session-1/a.txt"), b"x").expect("write temp file");
        super::clear_edits_dir(&edits).expect("clear edits dir");
        assert!(base.exists());
        assert_eq!(std::fs::read_dir(&base).expect("read edits dir").count(), 0);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn cleanup_edit_session_dir_removes_contents() {
        let session = LocalPath::new(
            unique_temp_dir("edit-session")
                .to_string_lossy()
                .to_string(),
        );
        std::fs::create_dir_all(format!("{}/nested", session.as_str()))
            .expect("create session dir");
        std::fs::write(format!("{}/nested/a.txt", session.as_str()), b"x")
            .expect("write temp file");
        super::cleanup_edit_session_dir(&session).expect("cleanup edit session dir");
        assert!(!std::path::Path::new(session.as_str()).exists());
    }

    #[test]
    fn cleanup_edit_session_dir_treats_missing_as_success() {
        let session = LocalPath::new(
            unique_temp_dir("edit-session-missing")
                .to_string_lossy()
                .to_string(),
        );
        super::cleanup_edit_session_dir(&session).expect("missing dir is not an error");
    }

    #[test]
    fn build_open_command_uses_text_editor_when_no_editor() {
        // The None branch must force `-t` (default TEXT editor), never a bare
        // `open <file>` that would dispatch by content type and EXECUTE a
        // remote-named .command/.terminal/.webloc/.workflow file.
        let cmd = super::build_open_command(&LocalPath::new("/tmp/edits/1/a.txt"), None);
        assert_eq!(cmd.get_program(), "/usr/bin/open");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["-t", "/tmp/edits/1/a.txt"]);
    }

    #[test]
    fn build_open_command_uses_named_editor() {
        let cmd = super::build_open_command(
            &LocalPath::new("/tmp/edits/1/a.txt"),
            Some("Visual Studio Code"),
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["-a", "Visual Studio Code", "/tmp/edits/1/a.txt"]);
    }

    #[test]
    fn is_temp_artifact_identifies_editor_files() {
        assert!(super::is_temp_artifact("4913"));
        assert!(super::is_temp_artifact(".test.swp"));
        assert!(super::is_temp_artifact(".main.py.swp"));
        assert!(super::is_temp_artifact("file.txt~"));
        assert!(super::is_temp_artifact("draft.tmp"));
        assert!(!super::is_temp_artifact("main.py"));
        assert!(!super::is_temp_artifact("test.swp"));
        assert!(!super::is_temp_artifact("Dockerfile"));
    }

    #[test]
    fn parse_edit_event_path_extracts_session_and_filters_artifacts() {
        let root = std::path::Path::new("/app/edits/run1");
        let valid = root.join("42/main.py");
        assert_eq!(
            super::parse_edit_event_path(root, &valid),
            Some((
                macsftp_core::EditSessionId(42),
                valid.to_string_lossy().to_string()
            ))
        );

        // Temp artifacts rejected
        let vim_swp = root.join("42/.main.py.swp");
        assert_eq!(super::parse_edit_event_path(root, &vim_swp), None);

        let vim_4913 = root.join("42/4913");
        assert_eq!(super::parse_edit_event_path(root, &vim_4913), None);

        let backup = root.join("42/main.py~");
        assert_eq!(super::parse_edit_event_path(root, &backup), None);

        // Not in watch root
        let outside = std::path::Path::new("/tmp/42/main.py");
        assert_eq!(super::parse_edit_event_path(root, outside), None);

        // Non-numeric session
        let bad_session = root.join("abc/main.py");
        assert_eq!(super::parse_edit_event_path(root, &bad_session), None);

        // Nested directory inside session
        let nested = root.join("42/subdir/main.py");
        assert_eq!(super::parse_edit_event_path(root, &nested), None);
    }

    #[test]
    fn detect_diff_tool_selects_first_matching_candidate() {
        let dir = unique_temp_dir("diff-tool-test");
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let code_bin = dir.join("code");
        let opendiff_bin = dir.join("opendiff");
        std::fs::write(&code_bin, b"#!/bin/sh\n").expect("write code");
        std::fs::write(&opendiff_bin, b"#!/bin/sh\n").expect("write opendiff");

        // opendiff first in candidates
        let candidates1 = vec![opendiff_bin.clone(), code_bin.clone()];
        assert_eq!(
            super::detect_diff_tool(&candidates1),
            Some(super::DiffTool::FileMerge(opendiff_bin.clone()))
        );

        // code first in candidates
        let candidates2 = vec![code_bin.clone(), opendiff_bin.clone()];
        assert_eq!(
            super::detect_diff_tool(&candidates2),
            Some(super::DiffTool::VsCode(code_bin.clone()))
        );

        // missing paths
        let missing = vec![dir.join("nonexistent1"), dir.join("nonexistent2")];
        assert_eq!(super::detect_diff_tool(&missing), None);

        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("cleanup error: {e}");
        }
    }

    #[test]
    fn edit_watcher_emits_debounced_event() {
        let dir = unique_temp_dir("watcher-test");
        let run_dir = dir.join("run1");
        let session_dir = run_dir.join("7");
        std::fs::create_dir_all(&session_dir).expect("create session dir");
        let target_file = session_dir.join("doc.txt");
        std::fs::write(&target_file, b"initial content").expect("write initial");

        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = super::EditWatcher::start_with_debounce(
            run_dir.clone(),
            std::time::Duration::from_millis(50),
            move |session_id, path| match tx.send((session_id, path)) {
                Ok(_) => {}
                Err(e) => eprintln!("send err: {e}"),
            },
        );
        let watcher = match watcher {
            Ok(w) => w,
            Err(e) => panic!("failed to start watcher: {e}"),
        };

        // Sleep briefly to ensure FSEvents is live
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Write modified content
        std::fs::write(&target_file, b"updated content").expect("write modified");

        let event = rx.recv_timeout(std::time::Duration::from_secs(3));
        assert!(event.is_ok(), "expected to receive debounced event");
        let (session_id, path) = match event {
            Ok((id, p)) => (id, p),
            Err(e) => panic!("failed to receive event: {e}"),
        };
        assert_eq!(session_id, macsftp_core::EditSessionId(7));
        // The watcher reports paths built from its configured root, not the
        // FSEvents-resolved path, so they compare equal to edit session paths.
        let expected_path = run_dir
            .join("7")
            .join("doc.txt")
            .to_string_lossy()
            .to_string();
        assert_eq!(path, expected_path);

        drop(watcher);
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("cleanup error: {e}");
        }
    }
}
