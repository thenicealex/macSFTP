use std::sync::Arc;
use std::time::Duration;

use macsftp_core::{
    AppEvent, DisconnectReason, HostKeyPrompt, LocalPath, RemoteEventScope, RemotePath,
    RemoteScoped, SessionId, TabConnected, TabDisconnected, TabId, TransferDirection,
    TransferEndpoint, TransferId, TransferProgress, TransferSnapshot, TrustDecision,
    TrustRequestId,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::ProgressThrottle;
use crate::trust::TrustRegistry;

/// Configuration for a mock remote session actor.
///
/// Defaults simulate a typical SFTP server: ed25519 host key, port 22,
/// home directory at `/home/user`.
#[derive(Debug, Clone)]
pub struct MockSessionConfig {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub algorithm: String,
    pub remote_root: RemotePath,
    /// Simulated transfer speed in bytes per second. Used by
    /// [`MockTransferJob`] to pace progress events.
    pub transfer_bytes_per_sec: u64,
}

impl Default for MockSessionConfig {
    fn default() -> Self {
        Self {
            host: "mock.example.com".to_string(),
            port: 22,
            fingerprint: "SHA256:mockfingerprint000000000000000000000000000".to_string(),
            algorithm: "ssh-ed25519".to_string(),
            remote_root: RemotePath::new("/home/user"),
            transfer_bytes_per_sec: 1_000_000,
        }
    }
}

/// A mock remote session actor that simulates the SFTP connect flow.
///
/// Lifecycle:
/// 1. Emits `HostKeyUnknown` with a trust request registered in the
///    [`TrustRegistry`].
/// 2. Awaits the user's decision via oneshot (resolved by `AcceptHostKey`
///    or `RejectHostKey` commands in the dispatch loop).
/// 3. On `TrustAndSave` → emits `TabConnected`.
///    On `Reject` / `TimedOut` / `RequestExpired` → emits `TabDisconnected`.
///
/// Cancellation: if the `CancellationToken` fires (tab closed, reconnect,
/// shutdown), the actor exits without emitting a final event — the
/// dispatch loop handles cleanup.
pub struct MockRemoteSessionActor {
    tab_id: TabId,
    session_id: SessionId,
    session_epoch: u64,
    trust_request_id: TrustRequestId,
    config: MockSessionConfig,
    event_tx: flume::Sender<AppEvent>,
    trust_registry: Arc<TrustRegistry>,
}

impl MockRemoteSessionActor {
    pub fn new(
        tab_id: TabId,
        session_id: SessionId,
        session_epoch: u64,
        trust_request_id: TrustRequestId,
        config: MockSessionConfig,
        event_tx: flume::Sender<AppEvent>,
        trust_registry: Arc<TrustRegistry>,
    ) -> Self {
        Self {
            tab_id,
            session_id,
            session_epoch,
            trust_request_id,
            config,
            event_tx,
            trust_registry,
        }
    }

    /// Run the mock connect flow to completion (or cancellation).
    pub async fn run(self, cancel: CancellationToken) {
        // 1. Create oneshot and register trust request.
        let (responder, decision_rx) = oneshot::channel();
        self.trust_registry.register(
            self.trust_request_id,
            crate::TrustRegistryEntry {
                tab_id: self.tab_id,
                session_epoch: self.session_epoch,
                responder,
            },
        );

        // 2. Emit HostKeyUnknown.
        let prompt = HostKeyPrompt {
            request_id: self.trust_request_id,
            tab_id: self.tab_id,
            session_id: self.session_id,
            session_epoch: self.session_epoch,
            host: self.config.host.clone(),
            port: self.config.port,
            algorithm: self.config.algorithm.clone(),
            fingerprint_sha256: self.config.fingerprint.clone(),
        };
        if self
            .event_tx
            .send_async(AppEvent::HostKeyUnknown(prompt))
            .await
            .is_err()
        {
            return; // event channel closed — shutdown
        }

        // 3. Await decision or cancellation.
        let scope = RemoteEventScope::new(self.tab_id, self.session_id, self.session_epoch);
        tokio::select! {
            decision = decision_rx => {
                match decision {
                    Ok(TrustDecision::TrustAndSave) => {
                        if let Err(error) = self
                            .event_tx
                            .send_async(AppEvent::TabConnected(RemoteScoped::new(
                                scope,
                                TabConnected {
                                    remote_root: self.config.remote_root.clone(),
                                },
                            )))
                            .await
                        {
                            // Event channel closed; the run ends here anyway.
                            tracing::warn!(error = %error, "mock actor event channel closed");
                        }
                    }
                    Ok(TrustDecision::Reject)
                    | Ok(TrustDecision::TimedOut)
                    | Ok(TrustDecision::RequestExpired) => {
                        if let Err(error) = self
                            .event_tx
                            .send_async(AppEvent::TabDisconnected(RemoteScoped::new(
                                scope,
                                TabDisconnected {
                                    reason: DisconnectReason::UserRequested,
                                },
                            )))
                            .await
                        {
                            // Event channel closed; the run ends here anyway.
                            tracing::warn!(error = %error, "mock actor event channel closed");
                        }
                    }
                    Err(_) => {
                        // Responder dropped without sending — actor was
                        // cancelled via registry cleanup. Exit silently.
                    }
                }
            }
            _ = cancel.cancelled() => {
                // Actor cancelled by dispatch loop (tab close / reconnect /
                // shutdown). No final event — dispatch loop handles cleanup.
            }
        }
    }
}

/// A mock transfer job that emits throttled `TransferProgress` events.
///
/// Simulates a file transfer by emitting progress at the configured
/// speed, respecting the `ProgressThrottle` (max 10 Hz by default per
/// ADR-002). The job completes when all bytes are transferred or the
/// cancellation token fires.
pub struct MockTransferJob {
    transfer_id: TransferId,
    total_bytes: u64,
    bytes_per_sec: u64,
    event_tx: flume::Sender<AppEvent>,
}

impl MockTransferJob {
    pub fn new(
        transfer_id: TransferId,
        total_bytes: u64,
        bytes_per_sec: u64,
        event_tx: flume::Sender<AppEvent>,
    ) -> Self {
        Self {
            transfer_id,
            total_bytes,
            bytes_per_sec,
            event_tx,
        }
    }

    /// Run the mock transfer, emitting progress events at most 10 Hz.
    /// Returns when the transfer completes or the cancel token fires.
    pub async fn run(self, cancel: CancellationToken) {
        // Emit TransferQueued.
        if self
            .event_tx
            .send_async(AppEvent::TransferQueued(self.snapshot(0)))
            .await
            .is_err()
        {
            return; // event channel closed — stop the mock transfer
        }

        let interval = Duration::from_millis(100); // 10 Hz
        let bytes_per_tick = self.bytes_per_sec.saturating_div(10).max(1);
        let mut throttle = ProgressThrottle::new(10);
        let mut bytes_done: u64 = 0;

        while bytes_done < self.total_bytes {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    bytes_done = bytes_done.saturating_add(bytes_per_tick).min(self.total_bytes);

                    let now = std::time::Instant::now();
                    if throttle.should_send(now)
                        && self
                            .event_tx
                            .send_async(AppEvent::TransferProgress(TransferProgress {
                                transfer_id: self.transfer_id,
                                bytes_done,
                                bytes_total: Some(self.total_bytes),
                            }))
                            .await
                            .is_err()
                    {
                        return; // event channel closed — stop the mock transfer
                    }
                }
                _ = cancel.cancelled() => {
                    // Transfer cancelled — don't emit completion.
                    return;
                }
            }
        }

        // Emit final progress (force-send by resetting throttle).
        throttle.reset();
        if self
            .event_tx
            .send_async(AppEvent::TransferProgress(TransferProgress {
                transfer_id: self.transfer_id,
                bytes_done: self.total_bytes,
                bytes_total: Some(self.total_bytes),
            }))
            .await
            .is_err()
        {
            return; // event channel closed — completion cannot be delivered
        }

        // Emit completion.
        if let Err(error) = self
            .event_tx
            .send_async(AppEvent::TransferCompleted {
                transfer_id: self.transfer_id,
            })
            .await
        {
            // Event channel closed; the run ends here regardless, but the
            // failure is worth recording for test diagnostics.
            tracing::warn!(error = %error, "mock transfer event channel closed");
        }
    }

    fn snapshot(&self, bytes_done: u64) -> TransferSnapshot {
        use macsftp_core::{ConflictPolicy, MetadataPolicy, TransferJob, TransferState};
        use std::time::UNIX_EPOCH;

        TransferSnapshot {
            job: TransferJob {
                id: self.transfer_id,
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/source")),
                destination: TransferEndpoint::Remote(RemotePath::new("/srv/dest")),
                state: TransferState::Running {
                    bytes_done,
                    bytes_total: Some(self.total_bytes),
                    started_at: macsftp_core::Timestamp(UNIX_EPOCH),
                },
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::default(),
                warnings: Vec::new(),
                created_at: macsftp_core::Timestamp(UNIX_EPOCH),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macsftp_core::AppEvent;
    use std::time::{Duration, Instant};

    /// Helper: create a mock actor with default config.
    fn mock_actor(
        tab_id: u64,
        session_id: u64,
        epoch: u64,
        trust_id: u64,
        registry: Arc<TrustRegistry>,
        event_tx: flume::Sender<AppEvent>,
    ) -> MockRemoteSessionActor {
        MockRemoteSessionActor::new(
            TabId(tab_id),
            SessionId(session_id),
            epoch,
            TrustRequestId(trust_id),
            MockSessionConfig::default(),
            event_tx,
            registry,
        )
    }

    #[test]
    fn mock_actor_accept_host_key_emits_tab_connected() {
        let registry = Arc::new(TrustRegistry::new());
        let (event_tx, event_rx) = flume::bounded::<AppEvent>(32);

        let actor = mock_actor(1, 10, 1, 100, registry.clone(), event_tx);
        let cancel = CancellationToken::new();

        // Use a helper runtime since we can't use #[tokio::test] (can't
        // drop a runtime inside an async context).
        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            // Spawn the actor.
            let actor_handle = tokio::spawn(actor.run(cancel));

            // Wait for HostKeyUnknown event.
            let event = event_rx.recv_async().await.expect("event should arrive");
            assert!(matches!(event, AppEvent::HostKeyUnknown(_)));

            // Resolve with TrustAndSave.
            registry.resolve(TrustRequestId(100), TrustDecision::TrustAndSave);

            // Wait for TabConnected.
            let event = event_rx.recv_async().await.expect("event should arrive");
            match event {
                AppEvent::TabConnected(scoped) => {
                    assert_eq!(scoped.scope.tab_id, TabId(1));
                    assert_eq!(scoped.scope.session_id, SessionId(10));
                    assert_eq!(scoped.scope.session_epoch, 1);
                }
                other => panic!("expected TabConnected, got {other:?}"),
            }

            // Actor should complete.
            actor_handle.await.expect("actor should complete");
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn mock_actor_reject_host_key_emits_tab_disconnected() {
        let registry = Arc::new(TrustRegistry::new());
        let (event_tx, event_rx) = flume::bounded::<AppEvent>(32);

        let actor = mock_actor(1, 10, 1, 100, registry.clone(), event_tx);
        let cancel = CancellationToken::new();

        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            let actor_handle = tokio::spawn(actor.run(cancel));

            // Wait for HostKeyUnknown.
            event_rx.recv_async().await.expect("event should arrive");

            // Reject.
            registry.resolve(TrustRequestId(100), TrustDecision::Reject);

            // Wait for TabDisconnected.
            let event = event_rx.recv_async().await.expect("event should arrive");
            match event {
                AppEvent::TabDisconnected(scoped) => {
                    assert_eq!(scoped.scope.tab_id, TabId(1));
                    assert_eq!(scoped.payload.reason, DisconnectReason::UserRequested);
                }
                other => panic!("expected TabDisconnected, got {other:?}"),
            }

            actor_handle.await.expect("actor should complete");
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn mock_actor_cancellation_exits_silently() {
        let registry = Arc::new(TrustRegistry::new());
        let (event_tx, event_rx) = flume::bounded::<AppEvent>(32);

        let actor = mock_actor(1, 10, 1, 100, registry.clone(), event_tx);
        let cancel = CancellationToken::new();

        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            let actor_handle = tokio::spawn(actor.run(cancel.clone()));

            // Wait for HostKeyUnknown.
            event_rx.recv_async().await.expect("event should arrive");

            // Cancel the actor.
            cancel.cancel();

            // Actor should exit without emitting another event.
            actor_handle.await.expect("actor should complete");

            // No more events should arrive (within a short window).
            let result =
                tokio::time::timeout(Duration::from_millis(100), event_rx.recv_async()).await;
            assert!(
                result.is_err() || matches!(result, Ok(Err(_))),
                "no event should arrive after cancellation"
            );
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn mock_actor_stale_reject_sends_request_expired() {
        let registry = Arc::new(TrustRegistry::new());
        let (event_tx, event_rx) = flume::bounded::<AppEvent>(32);

        let actor = mock_actor(1, 10, 1, 100, registry.clone(), event_tx);
        let cancel = CancellationToken::new();

        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            let actor_handle = tokio::spawn(actor.run(cancel));

            // Wait for HostKeyUnknown.
            event_rx.recv_async().await.expect("event should arrive");

            // Reject as stale (tab reconnected to epoch 2).
            let rejected = registry.reject_stale(TabId(1), 2);
            assert_eq!(rejected, 1, "stale request should be rejected");

            // Actor should emit TabDisconnected (RequestExpired path).
            let event = event_rx.recv_async().await.expect("event should arrive");
            assert!(
                matches!(event, AppEvent::TabDisconnected(_)),
                "stale rejection should result in TabDisconnected"
            );

            actor_handle.await.expect("actor should complete");
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn mock_transfer_job_emits_queued_progress_and_completed() {
        let (_event_tx, event_rx) = flume::bounded::<AppEvent>(64);
        let event_tx = _event_tx;

        // Small transfer: 1000 bytes at 10000 bytes/sec → ~1 tick.
        let job = MockTransferJob::new(TransferId(1), 1000, 10_000, event_tx);
        let cancel = CancellationToken::new();

        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            let job_handle = tokio::spawn(job.run(cancel));

            // Collect events until the job completes.
            let mut got_queued = false;
            let mut got_progress = false;
            let mut got_completed = false;

            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                let event =
                    match tokio::time::timeout(Duration::from_secs(1), event_rx.recv_async()).await
                    {
                        Ok(Ok(event)) => event,
                        _ => break,
                    };

                match event {
                    AppEvent::TransferQueued(_) => got_queued = true,
                    AppEvent::TransferProgress(p) => {
                        got_progress = true;
                        assert_eq!(p.transfer_id, TransferId(1));
                        assert!(p.bytes_done <= 1000);
                    }
                    AppEvent::TransferCompleted { transfer_id } => {
                        assert_eq!(transfer_id, TransferId(1));
                        got_completed = true;
                        break;
                    }
                    _ => {}
                }
            }

            assert!(got_queued, "should receive TransferQueued");
            assert!(got_progress, "should receive TransferProgress");
            assert!(got_completed, "should receive TransferCompleted");

            job_handle.await.expect("job should complete");
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn mock_actor_exits_when_event_channel_closed() {
        // Audit SFTP-MOCK-001: a dropped receiver must make the mock exit
        // promptly instead of silently continuing or deadlocking.
        let registry = Arc::new(TrustRegistry::new());
        let (event_tx, event_rx) = flume::bounded::<AppEvent>(1);
        drop(event_rx); // close the channel before the actor starts

        let actor = mock_actor(1, 10, 1, 100, registry.clone(), event_tx);
        let cancel = CancellationToken::new();
        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            // The very first HostKeyUnknown send fails, so run() must return
            // immediately without waiting on the trust decision.
            let actor_handle = tokio::spawn(actor.run(cancel));
            let finished = tokio::time::timeout(Duration::from_secs(1), actor_handle).await;
            assert!(
                finished.is_ok(),
                "mock actor must exit quickly when the event channel is closed"
            );
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn mock_transfer_job_exits_when_event_channel_closed() {
        // Audit SFTP-MOCK-001: same guarantee for the mock transfer job.
        let (event_tx, event_rx) = flume::bounded::<AppEvent>(1);
        drop(event_rx); // close the channel before the job starts

        let job = MockTransferJob::new(TransferId(1), 1000, 10_000, event_tx);
        let cancel = CancellationToken::new();
        let helper = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("helper runtime");

        helper.block_on(async {
            // TransferQueued send fails immediately; the job must not spin in
            // its progress loop or deadlock.
            let job_handle = tokio::spawn(job.run(cancel));
            let finished = tokio::time::timeout(Duration::from_secs(1), job_handle).await;
            assert!(
                finished.is_ok(),
                "mock transfer job must exit quickly when the event channel is closed"
            );
        });

        helper.shutdown_timeout(Duration::from_secs(1));
    }
}
