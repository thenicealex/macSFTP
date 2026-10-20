use std::path::Path;

use macsftp_core::{
    ConnectionKey, ConnectionState, LocalPath, ProfileId, RemotePath, TabState, is_bidi_or_control,
};
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

/// `(session_epoch, connection_key)` of a connected tab. The key is the one
/// captured at connect time — the same identity the connection pool used for
/// this physical connection. Remote-edit dedup keys on it rather than
/// `profile_id`, so two manual connections to different servers (or a saved
/// profile re-pointed at another host) never share one edit temp file.
pub(crate) fn connected_edit_session(tab: &TabState) -> Option<(u64, ConnectionKey)> {
    match (&tab.connection, tab.connection_key.as_deref()) {
        (ConnectionState::Connected { session_epoch, .. }, Some(connection_key)) => {
            Some((*session_epoch, connection_key.clone()))
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

/// Local destination for a single download: the chosen folder plus the
/// remote file name.
///
/// The name comes from the server, so it is checked before it reaches the
/// local filesystem: control or bidirectional formatting characters would let
/// a downloaded file disguise itself in Finder. Directory downloads get the
/// same check while their tree is planned.
pub(crate) fn append_local_name(
    directory: &LocalPath,
    source: &RemotePath,
) -> Result<LocalPath, &'static str> {
    let Some(name) = Path::new(source.as_str()).file_name() else {
        return Err("The selected download item has no usable name.");
    };
    let name = name.to_string_lossy();
    if name.chars().any(is_bidi_or_control) {
        return Err("The selected download item has a name that cannot be saved safely.");
    }
    Ok(LocalPath::new(format!(
        "{}/{}",
        directory.as_str().trim_end_matches('/'),
        name
    )))
}

#[cfg(test)]
mod tests {
    use super::append_local_name;
    use macsftp_core::{LocalPath, RemotePath};

    #[test]
    fn download_name_rejects_control_and_bidi_characters() {
        let directory = LocalPath::new("/Users/me/Downloads");
        for unsafe_name in ["/srv/evil\u{202E}txt.exe", "/srv/bell\u{0007}.txt"] {
            assert!(
                append_local_name(&directory, &RemotePath::new(unsafe_name)).is_err(),
                "{unsafe_name:?} must not become a local file name"
            );
        }
    }

    #[test]
    fn download_name_joins_plain_names() {
        let destination = append_local_name(
            &LocalPath::new("/Users/me/Downloads/"),
            &RemotePath::new("/srv/report.txt"),
        )
        .expect("a plain name must be accepted");
        assert_eq!(destination.as_str(), "/Users/me/Downloads/report.txt");
    }
}
