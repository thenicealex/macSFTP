mod known_hosts;
mod mock_actor;
mod physical_connection;
pub mod pool;
mod runtime;
mod session_actor;
mod transfer_planner;
mod trust;

pub use known_hosts::{
    HostKeyCheckResult, KnownHostsStore, fingerprint_sha256, host_pattern, key_algorithm,
};
pub use mock_actor::{MockRemoteSessionActor, MockSessionConfig, MockTransferJob};
pub use runtime::{
    BridgeChannels, EventReceiver, ProgressThrottle, RuntimeClient, RuntimeController,
    SessionBackend, test_event_channel,
};
pub use session_actor::{HostTrustConfig, RemoteSessionActor, RemoteSessionRequest};
pub use trust::{TrustRegistry, TrustRegistryEntry};

pub fn crate_name() -> &'static str {
    "macsftp-sftp"
}

pub fn linked_crates() -> (&'static str, &'static str) {
    (macsftp_core::crate_name(), macsftp_storage::crate_name())
}

#[cfg(test)]
mod tests {
    use super::{crate_name, linked_crates};

    #[test]
    fn links_core_and_storage_crates() {
        assert_eq!(crate_name(), "macsftp-sftp");
        assert_eq!(linked_crates(), ("macsftp-core", "macsftp-storage"));
    }
}
