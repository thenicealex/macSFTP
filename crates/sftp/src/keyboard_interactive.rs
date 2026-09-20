use std::collections::HashMap;
use std::sync::Mutex;

use macsftp_core::{KeyboardInteractiveRequestId, KeyboardInteractiveResponse, TabId};
use tokio::sync::oneshot;

pub struct KeyboardInteractiveRegistryEntry {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub responder: oneshot::Sender<Option<KeyboardInteractiveResponse>>,
}

#[derive(Default)]
pub struct KeyboardInteractiveRegistry {
    requests: Mutex<HashMap<KeyboardInteractiveRequestId, KeyboardInteractiveRegistryEntry>>,
}

impl KeyboardInteractiveRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        request_id: KeyboardInteractiveRequestId,
        entry: KeyboardInteractiveRegistryEntry,
    ) {
        self.lock().insert(request_id, entry);
    }

    pub fn resolve(&self, response: KeyboardInteractiveResponse) -> bool {
        let entry = self.lock().remove(&response.request_id);
        match entry {
            Some(entry) => {
                let _send_result = entry.responder.send(Some(response));
                true
            }
            None => false,
        }
    }

    pub fn cancel(&self, request_id: KeyboardInteractiveRequestId) -> bool {
        let entry = self.lock().remove(&request_id);
        match entry {
            Some(entry) => {
                let _send_result = entry.responder.send(None);
                true
            }
            None => false,
        }
    }

    pub fn reject_stale(&self, tab_id: TabId, current_epoch: u64) -> usize {
        self.reject_where(|entry| entry.tab_id == tab_id && entry.session_epoch != current_epoch)
    }

    pub fn reject_all_for_tab(&self, tab_id: TabId) -> usize {
        self.reject_where(|entry| entry.tab_id == tab_id)
    }

    pub fn reject_all(&self) {
        self.reject_where(|_| true);
    }

    pub fn pending_count(&self) -> usize {
        self.lock().len()
    }

    fn reject_where(&self, predicate: impl Fn(&KeyboardInteractiveRegistryEntry) -> bool) -> usize {
        let mut requests = self.lock();
        let ids = requests
            .iter()
            .filter_map(|(id, entry)| predicate(entry).then_some(*id))
            .collect::<Vec<_>>();
        let count = ids.len();
        for id in ids {
            if let Some(entry) = requests.remove(&id) {
                let _send_result = entry.responder.send(None);
            }
        }
        count
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        HashMap<KeyboardInteractiveRequestId, KeyboardInteractiveRegistryEntry>,
    > {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl std::fmt::Debug for KeyboardInteractiveRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyboardInteractiveRegistry")
            .field("pending", &self.pending_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_delivers_responses_once() {
        let registry = KeyboardInteractiveRegistry::new();
        let request_id = KeyboardInteractiveRequestId(7);
        let (responder, receiver) = oneshot::channel();
        registry.register(
            request_id,
            KeyboardInteractiveRegistryEntry {
                tab_id: TabId(2),
                session_epoch: 3,
                responder,
            },
        );

        assert!(registry.resolve(KeyboardInteractiveResponse {
            request_id,
            responses: vec!["answer".into()],
        }));
        let response = receiver
            .await
            .expect("registered keyboard-interactive receiver remains live")
            .expect("resolve sends a response");
        assert_eq!(response.responses, vec!["answer"]);
        assert!(!registry.cancel(request_id));
    }

    #[tokio::test]
    async fn reconnect_expires_only_stale_rounds() {
        let registry = KeyboardInteractiveRegistry::new();
        let mut receivers = Vec::new();
        for (id, tab_id, epoch) in [(1, 1, 1), (2, 1, 2), (3, 2, 1)] {
            let (responder, receiver) = oneshot::channel();
            registry.register(
                KeyboardInteractiveRequestId(id),
                KeyboardInteractiveRegistryEntry {
                    tab_id: TabId(tab_id),
                    session_epoch: epoch,
                    responder,
                },
            );
            receivers.push(receiver);
        }

        assert_eq!(registry.reject_stale(TabId(1), 2), 1);
        assert!(
            receivers
                .remove(0)
                .await
                .expect("stale receiver wakes")
                .is_none()
        );
        assert_eq!(registry.pending_count(), 2);
    }
}
