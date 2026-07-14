use std::collections::HashMap;
use std::sync::Mutex;

use macsftp_core::SecretRef;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// Keychain service name under which every macSFTP credential is stored.
/// The `account` field of each Keychain item is the `SecretRef` string,
/// so a given profile secret resolves to exactly one Keychain entry.
const SERVICE: &str = "macsftp";

/// `errSecItemNotFound` — returned by the Security framework when a
/// generic-password lookup or delete finds no matching entry.
const ERRSEC_ITEM_NOT_FOUND: i32 = -25300;

/// Errors from the credential store. Messages are sanitized — they never
/// contain the secret value, only the `SecretRef` handle or an OS status.
#[derive(Debug, Clone)]
pub enum KeychainError {
    /// No Keychain entry exists for the given `SecretRef`.
    NotFound,
    /// The underlying `security-framework` call failed.
    Os(String),
}

impl std::fmt::Display for KeychainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeychainError::NotFound => write!(f, "credential not found in Keychain"),
            KeychainError::Os(message) => write!(f, "Keychain error: {message}"),
        }
    }
}

impl std::error::Error for KeychainError {}

/// Pluggable credential backend. The app uses `OsKeychain`; tests inject
/// `MemoryKeychain` so they never touch the user's login Keychain.
trait KeychainBackend: Send + Sync {
    fn store(&self, secret_ref: &SecretRef, secret: &str) -> Result<(), KeychainError>;
    fn load(&self, secret_ref: &SecretRef) -> Result<Option<String>, KeychainError>;
    fn delete(&self, secret_ref: &SecretRef) -> Result<(), KeychainError>;
}

/// macOS login Keychain via `security-framework`'s generic-password
/// helpers (plan §11).
struct OsKeychain;

impl OsKeychain {
    fn new() -> Self {
        Self
    }
}

impl KeychainBackend for OsKeychain {
    fn store(&self, secret_ref: &SecretRef, secret: &str) -> Result<(), KeychainError> {
        // `set_generic_password` creates or overwrites the entry, so
        // re-saving a profile is idempotent.
        set_generic_password(SERVICE, secret_ref.as_str(), secret.as_bytes())
            .map_err(|error| KeychainError::Os(error.to_string()))
    }

    fn load(&self, secret_ref: &SecretRef) -> Result<Option<String>, KeychainError> {
        match get_generic_password(SERVICE, secret_ref.as_str()) {
            Ok(data) => Ok(Some(String::from_utf8_lossy(&data).into_owned())),
            // No matching entry: treat as "not found", not an error.
            Err(error) if error.code() == ERRSEC_ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(KeychainError::Os(error.to_string())),
        }
    }

    fn delete(&self, secret_ref: &SecretRef) -> Result<(), KeychainError> {
        match delete_generic_password(SERVICE, secret_ref.as_str()) {
            Ok(()) => Ok(()),
            // Already gone: nothing to do.
            Err(error) if error.code() == ERRSEC_ITEM_NOT_FOUND => Ok(()),
            Err(error) => Err(KeychainError::Os(error.to_string())),
        }
    }
}

/// In-memory backend for tests. Never touches the OS Keychain, so
/// `cargo test` never prompts for login Keychain access.
struct MemoryKeychain {
    secrets: Mutex<HashMap<String, String>>,
}

impl MemoryKeychain {
    fn new() -> Self {
        Self {
            secrets: Mutex::new(HashMap::new()),
        }
    }
}

impl KeychainBackend for MemoryKeychain {
    fn store(&self, secret_ref: &SecretRef, secret: &str) -> Result<(), KeychainError> {
        let mut map = self
            .secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.insert(secret_ref.as_str().to_string(), secret.to_string());
        Ok(())
    }

    fn load(&self, secret_ref: &SecretRef) -> Result<Option<String>, KeychainError> {
        let map = self
            .secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(map.get(secret_ref.as_str()).cloned())
    }

    fn delete(&self, secret_ref: &SecretRef) -> Result<(), KeychainError> {
        let mut map = self
            .secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.remove(secret_ref.as_str());
        Ok(())
    }
}

/// Owned handle to the credential store. Construct with `new_os()` in the
/// app and `new_memory()` in tests.
pub(crate) struct KeychainStore {
    backend: Box<dyn KeychainBackend>,
}

impl KeychainStore {
    /// Production backend backed by the macOS login Keychain.
    pub(crate) fn new_os() -> Self {
        Self {
            backend: Box::new(OsKeychain::new()),
        }
    }

    /// In-memory backend for tests; never touches the OS Keychain.
    pub(crate) fn new_memory() -> Self {
        Self {
            backend: Box::new(MemoryKeychain::new()),
        }
    }

    pub(crate) fn store(&self, secret_ref: &SecretRef, secret: &str) -> Result<(), KeychainError> {
        self.backend.store(secret_ref, secret)
    }

    pub(crate) fn load(&self, secret_ref: &SecretRef) -> Result<Option<String>, KeychainError> {
        self.backend.load(secret_ref)
    }

    pub(crate) fn delete(&self, secret_ref: &SecretRef) -> Result<(), KeychainError> {
        self.backend.delete(secret_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macsftp_core::SecretRef;

    #[test]
    fn memory_backend_round_trips_secret() {
        let store = KeychainStore::new_memory();
        let secret_ref = SecretRef::new("keychain:macsftp:1:password");

        assert_eq!(store.load(&secret_ref).expect("load"), None);

        store.store(&secret_ref, "hunter2").expect("store");
        assert_eq!(
            store.load(&secret_ref).expect("load"),
            Some("hunter2".to_string())
        );

        store.delete(&secret_ref).expect("delete");
        assert_eq!(store.load(&secret_ref).expect("load after delete"), None);
    }

    #[test]
    fn overwrite_replaces_previous_value() {
        let store = KeychainStore::new_memory();
        let secret_ref = SecretRef::new("keychain:macsftp:2:passphrase");

        store.store(&secret_ref, "first").expect("store");
        store.store(&secret_ref, "second").expect("store again");
        assert_eq!(
            store.load(&secret_ref).expect("load"),
            Some("second".to_string())
        );
    }

    // Touches the real macOS login Keychain and may prompt for access, so
    // it is opt-in. Run with: cargo test -- --ignored
    #[test]
    #[ignore = "touches the macOS login Keychain; run manually with cargo test -- --ignored"]
    fn os_backend_stores_loads_and_deletes() {
        let store = KeychainStore::new_os();
        let secret_ref = SecretRef::new("keychain:macsftp:test:password");
        let _ = store.delete(&secret_ref);

        store.store(&secret_ref, "secret-value").expect("store");
        assert_eq!(
            store.load(&secret_ref).expect("load"),
            Some("secret-value".to_string())
        );

        store.delete(&secret_ref).expect("delete");
        assert_eq!(store.load(&secret_ref).expect("load after delete"), None);
    }
}
