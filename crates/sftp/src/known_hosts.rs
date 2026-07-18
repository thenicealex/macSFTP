use std::path::Path;

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use ssh_key::HashAlg;
use ssh_key::PublicKey;
use ssh_key::known_hosts::{Entry, HostPatterns, KnownHosts, Marker};
use tracing::warn;

/// Result of checking a server's host key against known_hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyCheckResult {
    /// The key matches a known_hosts entry for this host+port.
    Match,
    /// No entry exists for this host+port.
    NotFound,
    /// An entry exists but the key is different — potential MITM.
    Mismatch,
}

/// In-memory store of known_hosts entries loaded from multiple files.
///
/// Reads from both the app-owned known_hosts file and the user's
/// `~/.ssh/known_hosts`. Writes only to the app-owned file.
///
/// MVP subset (per `docs/gpui-russh-plan.md` §10):
/// - Supports plaintext host and `[host]:port` patterns.
/// - Supports common key types: ed25519, ecdsa, rsa.
/// - Supports plaintext and OpenSSH `|1|` hashed hostnames.
/// - Skips wildcards and `@cert-authority`; a matching `@revoked` entry
///   blocks the connection through the mismatch path.
pub struct KnownHostsStore {
    app_entries: Vec<Entry>,
    user_entries: Vec<Entry>,
}

impl KnownHostsStore {
    /// Load known_hosts from the app-owned file and optionally from
    /// `~/.ssh/known_hosts`.
    ///
    /// Parse failures on individual lines are skipped with a WARN log;
    /// the rest of the file is still loaded.
    pub fn load(app_path: &Path, user_path: Option<&Path>) -> Self {
        let app_entries = load_file(app_path);
        let user_entries = user_path.map(load_file).unwrap_or_default();
        Self {
            app_entries,
            user_entries,
        }
    }

    /// Create an empty store (for testing).
    pub fn empty() -> Self {
        Self {
            app_entries: Vec::new(),
            user_entries: Vec::new(),
        }
    }

    /// Check if a server's host key is known and matches.
    ///
    /// Checks app-owned entries first, then user entries. Returns
    /// `Mismatch` as soon as an entry for the host+port is found with
    /// a different key — this is a security-critical distinction from
    /// `NotFound`.
    pub fn check(&self, host: &str, port: u16, key: &PublicKey) -> HostKeyCheckResult {
        let pattern = host_pattern(host, port);
        let mut found_host = false;
        let mut found_matching_key = false;

        for entries in [&self.app_entries, &self.user_entries] {
            for entry in entries {
                if entry_matches_host(entry, &pattern) {
                    if entry.marker() == Some(&Marker::CertAuthority) {
                        continue;
                    }
                    found_host = true;
                    // Compare key material only: stored entries may carry
                    // a comment, wire keys never do, and `PublicKey`'s
                    // `PartialEq` would treat that as a different key.
                    let key_matches = entry.public_key().key_data() == key.key_data();
                    if entry.marker() == Some(&Marker::Revoked) && key_matches {
                        return HostKeyCheckResult::Mismatch;
                    }
                    if entry.marker().is_none() && key_matches {
                        found_matching_key = true;
                    }
                }
            }
        }
        if found_matching_key {
            HostKeyCheckResult::Match
        } else if found_host {
            HostKeyCheckResult::Mismatch
        } else {
            HostKeyCheckResult::NotFound
        }
    }

    /// Add a new trusted host key to the app-owned known_hosts file
    /// and to the in-memory store.
    ///
    /// Writes a plaintext `host` or `[host]:port` entry. Does not
    /// write hashed entries.
    pub fn add_trusted(
        &mut self,
        host: &str,
        port: u16,
        key: &PublicKey,
        app_path: &Path,
    ) -> Result<(), KnownHostsError> {
        let pattern = host_pattern(host, port);
        let key_str = key.to_string();
        let line = format!("{pattern} {key_str}");

        // Parse back to verify the line is valid.
        let entry: Entry = line.parse().map_err(|_| KnownHostsError::EntryFormat)?;

        // Append to file (create if missing).
        let existing = std::fs::read_to_string(app_path).unwrap_or_default();
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&line);
        content.push('\n');
        macsftp_storage::write_private_file_atomically(app_path, content.as_bytes())
            .map_err(KnownHostsError::Io)?;

        self.app_entries.push(entry);
        Ok(())
    }

    /// Fingerprint of the key currently stored for this host+port, if
    /// any — used to show the expected key in mismatch errors.
    pub fn expected_fingerprint(&self, host: &str, port: u16) -> Option<String> {
        let pattern = host_pattern(host, port);
        for entries in [&self.app_entries, &self.user_entries] {
            for entry in entries {
                if entry_matches_host(entry, &pattern) {
                    return Some(fingerprint_sha256(entry.public_key()));
                }
            }
        }
        None
    }

    /// Total number of loaded entries (app + user).
    pub fn entry_count(&self) -> usize {
        self.app_entries.len() + self.user_entries.len()
    }
}

/// Format the host pattern for a known_hosts entry.
///
/// Port 22 (the SSH default) is written as a bare hostname.
/// Non-default ports use the `[host]:port` bracketed form.
pub fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Compute the SHA256 fingerprint of a public key, formatted as
/// `SHA256:base64...` — the same format OpenSSH uses.
pub fn fingerprint_sha256(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Get the algorithm name of a public key (e.g. "ssh-ed25519").
pub fn key_algorithm(key: &PublicKey) -> String {
    key.algorithm().as_str().to_string()
}

/// Check if a known_hosts entry matches a host pattern.
///
fn entry_matches_host(entry: &Entry, pattern: &str) -> bool {
    match entry.host_patterns() {
        HostPatterns::Patterns(patterns) => patterns.iter().any(|p| p == pattern),
        HostPatterns::HashedName { salt, hash } => {
            let Ok(mut mac) = <Hmac<Sha1> as KeyInit>::new_from_slice(salt) else {
                return false;
            };
            mac.update(pattern.as_bytes());
            mac.verify_slice(hash).is_ok()
        }
    }
}

/// Load a known_hosts file, skipping unparseable content with a WARN.
/// A missing file is the normal first-launch state — no warning.
fn load_file(path: &Path) -> Vec<Entry> {
    let input = match std::fs::read_to_string(path) {
        Ok(input) => input,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            warn!(path = %path.display(), error = %error, "known_hosts: failed to read file");
            return Vec::new();
        }
    };

    input
        .lines()
        .enumerate()
        .filter_map(|(index, line)| match KnownHosts::new(line).next() {
            None => None,
            Some(Ok(entry)) => Some(entry),
            Some(Err(error)) => {
                warn!(
                    path = %path.display(),
                    line = index + 1,
                    error = %error,
                    "known_hosts: ignored unparseable line"
                );
                None
            }
        })
        .collect()
}

/// Errors from known_hosts operations.
#[derive(Debug)]
pub enum KnownHostsError {
    Io(std::io::Error),
    EntryFormat,
}

impl std::fmt::Display for KnownHostsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "known_hosts IO error: {error}"),
            Self::EntryFormat => write!(formatter, "known_hosts entry format error"),
        }
    }
}

impl std::error::Error for KnownHostsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::EntryFormat => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::public::{Ed25519PublicKey, KeyData};
    use std::io::Write;

    /// Deterministic ed25519 public key for testing. Distinct seeds
    /// produce distinct keys; no RNG dependency needed.
    fn test_key(seed: u8) -> PublicKey {
        let mut bytes = [0u8; Ed25519PublicKey::BYTE_SIZE];
        bytes[0] = seed;
        bytes[31] = seed.wrapping_add(1);
        PublicKey::from(KeyData::from(Ed25519PublicKey(bytes)))
    }

    fn write_temp_known_hosts(label: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "macsftp_test_known_hosts_{label}_{}.txt",
            std::process::id()
        ));
        let mut file = std::fs::File::create(&path).expect("create temp file");
        file.write_all(content.as_bytes()).expect("write temp file");
        path
    }

    fn hashed_pattern(pattern: &str) -> HostPatterns {
        let salt = b"macsftp-hashed-host-test".to_vec();
        let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(&salt)
            .expect("HMAC accepts an arbitrary OpenSSH known_hosts salt");
        mac.update(pattern.as_bytes());
        let bytes = mac.finalize().into_bytes();
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&bytes);
        HostPatterns::HashedName { salt, hash }
    }

    #[test]
    fn host_pattern_uses_bare_hostname_for_port_22() {
        assert_eq!(host_pattern("example.com", 22), "example.com");
    }

    #[test]
    fn host_pattern_uses_brackets_for_non_default_port() {
        assert_eq!(host_pattern("example.com", 2222), "[example.com]:2222");
    }

    #[test]
    fn hashed_host_entry_matches_only_the_original_host_pattern() {
        let key = test_key(77);
        let pattern = hashed_pattern("[hashed.example.com]:2222").to_string();
        let key_line = key.to_string();
        let path = write_temp_known_hosts("hashed", &format!("{pattern} {key_line}\n"));
        let store = KnownHostsStore::load(&path, None);

        assert_eq!(
            store.check("hashed.example.com", 2222, &key),
            HostKeyCheckResult::Match
        );
        assert_eq!(
            store.check("other.example.com", 2222, &key),
            HostKeyCheckResult::NotFound
        );
        std::fs::remove_file(path).expect("remove hashed known_hosts fixture");
    }

    #[test]
    fn revoked_matching_host_is_always_blocked() {
        let key = test_key(78);
        let key_line = key.to_string();
        let path = write_temp_known_hosts(
            "revoked",
            &format!("@revoked revoked.example.com {key_line}\n"),
        );
        let store = KnownHostsStore::load(&path, None);

        assert_eq!(
            store.check("revoked.example.com", 22, &key),
            HostKeyCheckResult::Mismatch
        );
        std::fs::remove_file(path).expect("remove revoked known_hosts fixture");
    }

    #[test]
    fn fingerprint_sha256_starts_with_prefix() {
        let key = test_key(1);
        let fp = fingerprint_sha256(&key);
        assert!(
            fp.starts_with("SHA256:"),
            "fingerprint should start with SHA256: prefix, got: {fp}"
        );
    }

    #[test]
    fn key_algorithm_returns_ssh_ed25519() {
        let key = test_key(2);
        assert_eq!(key_algorithm(&key), "ssh-ed25519");
    }

    #[test]
    fn empty_store_returns_not_found() {
        let store = KnownHostsStore::empty();
        let key = test_key(3);
        assert_eq!(
            store.check("example.com", 22, &key),
            HostKeyCheckResult::NotFound
        );
    }

    #[test]
    fn add_trusted_then_check_matches() {
        let mut store = KnownHostsStore::empty();
        let key = test_key(4);
        let path = std::env::temp_dir().join(format!(
            "macsftp_test_known_hosts_add_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        store
            .add_trusted("example.com", 22, &key, &path)
            .expect("add_trusted should succeed");

        assert_eq!(
            store.check("example.com", 22, &key),
            HostKeyCheckResult::Match
        );
        assert_eq!(store.entry_count(), 1);

        // Verify the file was written.
        let content = std::fs::read_to_string(&path).expect("read file");
        assert!(
            content.contains("example.com"),
            "file should contain the host pattern"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_detects_mismatch() {
        let mut store = KnownHostsStore::empty();
        let key1 = test_key(5);
        let path = std::env::temp_dir().join(format!(
            "macsftp_test_known_hosts_mismatch_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        store
            .add_trusted("example.com", 22, &key1, &path)
            .expect("add_trusted");

        let key2 = test_key(6);
        assert_eq!(
            store.check("example.com", 22, &key2),
            HostKeyCheckResult::Mismatch
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_with_non_default_port() {
        let mut store = KnownHostsStore::empty();
        let key = test_key(7);
        let path = std::env::temp_dir().join(format!(
            "macsftp_test_known_hosts_port_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        store
            .add_trusted("example.com", 2222, &key, &path)
            .expect("add_trusted");

        assert_eq!(
            store.check("example.com", 2222, &key),
            HostKeyCheckResult::Match
        );
        // Port 22 should NOT match.
        assert_eq!(
            store.check("example.com", 22, &key),
            HostKeyCheckResult::NotFound
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_file_parses_valid_entries() {
        let key = test_key(8);
        let key_line = key.to_string();
        let content = format!("example.com {key_line}\n[host2.com]:2222 {key_line}\n\n# comment\n");
        let path = write_temp_known_hosts("valid", &content);

        let store = KnownHostsStore::load(&path, None);
        assert!(store.entry_count() >= 2, "should parse at least 2 entries");

        assert_eq!(
            store.check("example.com", 22, &key),
            HostKeyCheckResult::Match
        );
        assert_eq!(
            store.check("host2.com", 2222, &key),
            HostKeyCheckResult::Match
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_file_skips_unparseable_lines() {
        // Mix of valid and invalid lines — invalid ones should be
        // skipped, valid ones should still load.
        let key = test_key(55);
        let key_line = key.to_string();
        let content =
            format!("example.com {key_line}\nthis is not valid\nanother.example.com {key_line}\n");
        let path = write_temp_known_hosts("unparseable", &content);

        let store = KnownHostsStore::load(&path, None);

        assert_eq!(store.entry_count(), 2);
        assert_eq!(
            store.check("example.com", 22, &key),
            HostKeyCheckResult::Match
        );
        assert_eq!(
            store.check("another.example.com", 22, &key),
            HostKeyCheckResult::Match
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn check_scans_all_keys_for_the_same_host_before_mismatch() {
        let stale_key = test_key(56);
        let current_key = test_key(57);
        let content = format!(
            "example.com {}\nexample.com {}\n",
            stale_key.to_string(),
            current_key.to_string()
        );
        let path = write_temp_known_hosts("multiple_keys", &content);
        let store = KnownHostsStore::load(&path, None);

        assert_eq!(
            store.check("example.com", 22, &current_key),
            HostKeyCheckResult::Match
        );

        std::fs::remove_file(path).expect("remove multiple-key known_hosts fixture");
    }

    #[test]
    fn revoked_entry_only_blocks_its_own_key() {
        let revoked_key = test_key(58);
        let current_key = test_key(59);
        let content = format!(
            "@revoked example.com {}\nexample.com {}\n",
            revoked_key.to_string(),
            current_key.to_string()
        );
        let path = write_temp_known_hosts("revoked_key_specific", &content);
        let store = KnownHostsStore::load(&path, None);

        assert_eq!(
            store.check("example.com", 22, &current_key),
            HostKeyCheckResult::Match
        );
        assert_eq!(
            store.check("example.com", 22, &revoked_key),
            HostKeyCheckResult::Mismatch
        );

        std::fs::remove_file(path).expect("remove revoked-key known_hosts fixture");
    }

    #[test]
    fn revoked_key_wins_even_when_a_normal_entry_appears_first() {
        let revoked_key = test_key(60);
        let content = format!(
            "example.com {}\n@revoked example.com {}\n",
            revoked_key.to_string(),
            revoked_key.to_string()
        );
        let path = write_temp_known_hosts("revoked_after_normal", &content);
        let store = KnownHostsStore::load(&path, None);

        assert_eq!(
            store.check("example.com", 22, &revoked_key),
            HostKeyCheckResult::Mismatch
        );

        std::fs::remove_file(path).expect("remove revoked-after-normal fixture");
    }

    #[test]
    fn check_matches_entry_with_comment() {
        // Entries in a user's known_hosts often carry a trailing
        // comment; the wire key never does. They must still match.
        let key = test_key(30);
        let content = format!("example.com {} some-comment@host\n", key.to_string());
        let path = write_temp_known_hosts("comment", &content);

        let store = KnownHostsStore::load(&path, None);
        assert_eq!(
            store.check("example.com", 22, &key),
            HostKeyCheckResult::Match,
            "a comment on the stored entry must not cause a mismatch"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn expected_fingerprint_returns_stored_key_fingerprint() {
        let mut store = KnownHostsStore::empty();
        let key = test_key(9);
        let path = std::env::temp_dir().join(format!(
            "macsftp_test_known_hosts_expected_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        assert_eq!(store.expected_fingerprint("example.com", 22), None);
        store
            .add_trusted("example.com", 22, &key, &path)
            .expect("add_trusted");
        assert_eq!(
            store.expected_fingerprint("example.com", 22),
            Some(fingerprint_sha256(&key))
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn add_trusted_appends_to_existing_file() {
        let key1 = test_key(10);
        let key2 = test_key(20);
        let path = std::env::temp_dir().join(format!(
            "macsftp_test_known_hosts_append_{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let mut store = KnownHostsStore::empty();
        store
            .add_trusted("host1.com", 22, &key1, &path)
            .expect("first add");
        store
            .add_trusted("host2.com", 22, &key2, &path)
            .expect("second add");

        assert_eq!(store.entry_count(), 2);
        assert_eq!(
            store.check("host1.com", 22, &key1),
            HostKeyCheckResult::Match
        );
        assert_eq!(
            store.check("host2.com", 22, &key2),
            HostKeyCheckResult::Match
        );

        let _ = std::fs::remove_file(&path);
    }
}
