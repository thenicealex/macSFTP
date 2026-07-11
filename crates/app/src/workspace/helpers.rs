#![allow(unused_imports)]
use std::path::Path;

use macsftp_core::{
    AuthCredential, AuthMethod, ConnectionState, LocalPath, ProfileId, RemotePath, SecretRef,
    TabState,
};

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

/// `SecretRef`s a fresh save would create for `profile_id` given the
/// in-form `auth`. Mirrors `SecretRef::keychain_ref` (plan §11).
pub(crate) fn secret_refs_for_settings(
    profile_id: ProfileId,
    auth: &AuthCredential,
) -> Vec<SecretRef> {
    match auth {
        AuthCredential::Password { .. } => vec![SecretRef::keychain_ref(profile_id, "password")],
        AuthCredential::PrivateKey { passphrase, .. } => match passphrase {
            Some(_) => vec![SecretRef::keychain_ref(profile_id, "passphrase")],
            None => Vec::new(),
        },
    }
}

/// `SecretRef`s already stored for a saved profile's `auth`.
pub(crate) fn secret_refs_for_profile(auth: &AuthMethod) -> Vec<SecretRef> {
    match auth {
        AuthMethod::Password { secret_ref } => vec![secret_ref.clone()],
        AuthMethod::PrivateKey { passphrase_ref, .. } => match passphrase_ref {
            Some(secret_ref) => vec![secret_ref.clone()],
            None => Vec::new(),
        },
    }
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
