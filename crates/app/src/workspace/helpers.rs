use std::path::Path;

use macsftp_core::{ConnectionState, LocalPath, ProfileId, RemotePath, TabState};
use macsftp_storage::RecentEntry;

/// Row label for a recent connection: `Name · user@host:port` or `user@host:port`.
pub(crate) fn format_recent_label(entry: &RecentEntry) -> String {
    let endpoint = format!("{}@{}:{}", entry.username, entry.host, entry.port);
    match entry.display_name.as_deref() {
        Some(name) if !name.is_empty() => format!("{name} · {endpoint}"),
        _ => endpoint,
    }
}

pub(crate) fn connection_in_flight(connection: &ConnectionState) -> bool {
    matches!(
        connection,
        ConnectionState::Connecting { .. }
            | ConnectionState::AwaitingHostKey { .. }
            | ConnectionState::AwaitingCredentials { .. }
            | ConnectionState::Reconnecting { .. }
    )
}

/// Expand a leading `~/` to the user's home directory.
pub(crate) fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{}/{rest}", home.trim_end_matches('/'));
    }
    path.to_string()
}

pub(crate) fn connected_transfer_session(tab: &TabState) -> Option<(u64, ProfileId)> {
    match tab.connection {
        ConnectionState::Connected { session_epoch, .. } => {
            Some((session_epoch, tab.profile_id.unwrap_or(ProfileId(0))))
        }
        _ => None,
    }
}

pub(crate) fn append_remote_name(directory: &RemotePath, source: &LocalPath) -> Option<RemotePath> {
    let name = Path::new(source.as_str()).file_name()?.to_string_lossy();
    Some(RemotePath::new(format!(
        "{}/{}",
        directory.as_str().trim_end_matches('/'),
        name
    )))
}

pub(crate) fn append_local_name(directory: &LocalPath, source: &RemotePath) -> Option<LocalPath> {
    let name = Path::new(source.as_str()).file_name()?.to_string_lossy();
    Some(LocalPath::new(format!(
        "{}/{}",
        directory.as_str().trim_end_matches('/'),
        name
    )))
}
