use std::collections::HashMap;
use std::sync::Mutex;

use macsftp_core::{TabId, TrustDecision, TrustRequestId};
use tokio::sync::oneshot;

/// A pending trust request entry in the [`TrustRegistry`].
///
/// The `responder` is the oneshot sender that the mock actor (or real
/// `russh` handshake) is awaiting. When the user makes a decision via
/// `AcceptHostKey` / `RejectHostKey`, the dispatch loop calls
/// [`TrustRegistry::resolve`] which sends the decision through this
/// channel.
pub struct TrustRegistryEntry {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub responder: oneshot::Sender<TrustDecision>,
}

/// Registry of pending host key trust requests.
///
/// Implements ADR-004: the SSH handshake (`check_server_key`) creates a
/// oneshot channel, registers it here, emits `HostKeyUnknown`, and
/// awaits the response. The dispatch loop resolves requests when the
/// user sends `AcceptHostKey` or `RejectHostKey`.
///
/// Stale requests (from a previous session epoch or a closed tab) are
/// rejected automatically so the waiting handshake unblocks and returns
/// `false` to `russh`.
///
/// Thread safety: uses `std::sync::Mutex` because all operations are
/// short HashMap insertions/removals. The oneshot sender is moved out
/// of the map before `.send()` is called, so no lock is held during
/// the actual send.
#[derive(Default)]
pub struct TrustRegistry {
    requests: Mutex<HashMap<TrustRequestId, TrustRegistryEntry>>,
}

impl TrustRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pending trust request. The `responder` will be
    /// consumed when the user makes a decision or when the request
    /// is rejected as stale.
    pub fn register(&self, request_id: TrustRequestId, entry: TrustRegistryEntry) {
        let mut requests = self.lock();
        requests.insert(request_id, entry);
    }

    /// Resolve a trust request by sending the decision to the waiting
    /// actor. Returns `true` if the request was found and resolved.
    ///
    /// Returns `false` if the request was already resolved, expired,
    /// or never existed — the caller should treat this as a no-op
    /// (e.g. the user clicked accept on an expired modal).
    pub fn resolve(&self, request_id: TrustRequestId, decision: TrustDecision) -> bool {
        let entry = {
            let mut requests = self.lock();
            requests.remove(&request_id)
        };
        match entry {
            Some(entry) => {
                // send() fails if the receiver was already dropped
                // (actor cancelled). That's fine — the decision is
                // just lost, which is the correct behavior.
                let _ = entry.responder.send(decision);
                true
            }
            None => false,
        }
    }

    /// Reject all trust requests for a tab whose `session_epoch` does
    /// not match `current_epoch`. Called when a tab reconnects — old
    /// session's pending host key prompts must not be confirmed into
    /// the new session.
    ///
    /// Returns the count of rejected requests for logging.
    pub fn reject_stale(&self, tab_id: TabId, current_epoch: u64) -> usize {
        let mut requests = self.lock();
        let stale_ids: Vec<TrustRequestId> = requests
            .iter()
            .filter(|(_, entry)| entry.tab_id == tab_id && entry.session_epoch != current_epoch)
            .map(|(id, _)| *id)
            .collect();

        let count = stale_ids.len();
        for id in stale_ids {
            if let Some(entry) = requests.remove(&id) {
                let _ = entry.responder.send(TrustDecision::RequestExpired);
            }
        }
        count
    }

    /// Reject all trust requests for a tab, regardless of epoch.
    /// Called when a tab is closed — no pending decision should
    /// outlive its tab.
    pub fn reject_all_for_tab(&self, tab_id: TabId) -> usize {
        let mut requests = self.lock();
        let tab_ids: Vec<TrustRequestId> = requests
            .iter()
            .filter(|(_, entry)| entry.tab_id == tab_id)
            .map(|(id, _)| *id)
            .collect();

        let count = tab_ids.len();
        for id in tab_ids {
            if let Some(entry) = requests.remove(&id) {
                let _ = entry.responder.send(TrustDecision::RequestExpired);
            }
        }
        count
    }

    /// Reject all pending trust requests. Called during runtime
    /// shutdown so no actor hangs waiting for a user decision.
    pub fn reject_all(&self) {
        let mut requests = self.lock();
        for (_, entry) in requests.drain() {
            let _ = entry.responder.send(TrustDecision::RequestExpired);
        }
    }

    /// Number of pending requests. For diagnostics and tests.
    pub fn pending_count(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<TrustRequestId, TrustRegistryEntry>> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl std::fmt::Debug for TrustRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.pending_count();
        formatter
            .debug_struct("TrustRegistry")
            .field("pending", &count)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tab_id: u64, epoch: u64) -> (TrustRegistryEntry, oneshot::Receiver<TrustDecision>) {
        let (tx, rx) = oneshot::channel();
        (
            TrustRegistryEntry {
                tab_id: TabId(tab_id),
                session_epoch: epoch,
                responder: tx,
            },
            rx,
        )
    }

    #[test]
    fn register_and_resolve_sends_decision() {
        let registry = TrustRegistry::new();
        let (entry, rx) = entry(1, 1);
        registry.register(TrustRequestId(10), entry);

        assert_eq!(registry.pending_count(), 1);

        let resolved = registry.resolve(TrustRequestId(10), TrustDecision::TrustAndSave);
        assert!(resolved, "request should be found and resolved");
        assert_eq!(registry.pending_count(), 0);

        let decision = rx.blocking_recv().expect("decision should be received");
        assert_eq!(decision, TrustDecision::TrustAndSave);
    }

    #[test]
    fn resolve_unknown_request_returns_false() {
        let registry = TrustRegistry::new();

        let resolved = registry.resolve(TrustRequestId(99), TrustDecision::Reject);
        assert!(
            !resolved,
            "resolving a non-existent request should return false"
        );
    }

    #[test]
    fn reject_stale_expires_old_epoch_requests() {
        let registry = TrustRegistry::new();

        // Old epoch request (epoch 1) for tab 1.
        let (old_entry, old_rx) = entry(1, 1);
        registry.register(TrustRequestId(1), old_entry);

        // Current epoch request (epoch 2) for tab 1.
        let (current_entry, current_rx) = entry(1, 2);
        registry.register(TrustRequestId(2), current_entry);

        // Reject stale for tab 1 with current epoch 2.
        let rejected = registry.reject_stale(TabId(1), 2);
        assert_eq!(rejected, 1, "only the old epoch request should be rejected");

        // Old request receives RequestExpired.
        let old_decision = old_rx.blocking_recv().expect("old request should receive");
        assert_eq!(old_decision, TrustDecision::RequestExpired);

        // Current request is still pending.
        assert_eq!(registry.pending_count(), 1);

        // Current request can still be resolved normally.
        let resolved = registry.resolve(TrustRequestId(2), TrustDecision::TrustAndSave);
        assert!(resolved);
        let current_decision = current_rx
            .blocking_recv()
            .expect("current request should receive");
        assert_eq!(current_decision, TrustDecision::TrustAndSave);
    }

    #[test]
    fn reject_all_for_tab_clears_all_requests_for_tab() {
        let registry = TrustRegistry::new();

        // Two requests for tab 1, one for tab 2.
        let (e1, rx1) = entry(1, 1);
        let (e2, rx2) = entry(1, 2);
        let (e3, _rx3) = entry(2, 1);
        registry.register(TrustRequestId(1), e1);
        registry.register(TrustRequestId(2), e2);
        registry.register(TrustRequestId(3), e3);

        let rejected = registry.reject_all_for_tab(TabId(1));
        assert_eq!(rejected, 2, "both requests for tab 1 should be rejected");
        assert_eq!(registry.pending_count(), 1, "tab 2 request should remain");

        rx1.blocking_recv()
            .expect("rx1 should receive RequestExpired");
        rx2.blocking_recv()
            .expect("rx2 should receive RequestExpired");
    }

    #[test]
    fn reject_all_clears_everything() {
        let registry = TrustRegistry::new();
        let (e1, rx1) = entry(1, 1);
        let (e2, rx2) = entry(2, 1);
        registry.register(TrustRequestId(1), e1);
        registry.register(TrustRequestId(2), e2);

        registry.reject_all();
        assert_eq!(registry.pending_count(), 0);

        rx1.blocking_recv().expect("rx1 should receive");
        rx2.blocking_recv().expect("rx2 should receive");
    }

    #[test]
    fn resolve_twice_second_returns_false() {
        let registry = TrustRegistry::new();
        let (entry, _rx) = entry(1, 1);
        registry.register(TrustRequestId(1), entry);

        assert!(registry.resolve(TrustRequestId(1), TrustDecision::Reject));
        assert!(
            !registry.resolve(TrustRequestId(1), TrustDecision::TrustAndSave),
            "second resolve should return false — request already consumed"
        );
    }

    #[test]
    fn reject_stale_does_not_touch_other_tabs() {
        let registry = TrustRegistry::new();
        let (e1, _rx1) = entry(1, 1);
        let (e2, _rx2) = entry(2, 1);
        registry.register(TrustRequestId(1), e1);
        registry.register(TrustRequestId(2), e2);

        // Reject stale for tab 1 with epoch 99 — tab 2 should be untouched.
        let rejected = registry.reject_stale(TabId(1), 99);
        assert_eq!(rejected, 1);
        assert_eq!(registry.pending_count(), 1, "tab 2 request should remain");
    }
}
