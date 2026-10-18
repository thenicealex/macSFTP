use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use macsftp_core::{
    AppEvent, ConflictDecision, ConflictPolicy, ConflictRequestId, ConnectionKey, ErrorCode,
    TransferDirection, TransferFailure, TransferId, TransferJob, TransferPlanId, UserFacingError,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::pool::{ConnectionManager, SharedConnection};
use crate::session_actor::{
    ConflictWaiter, TransferConflictContext, TransferJobOutcome, open_transfer_session,
    run_transfer_job,
};

pub(crate) const MAX_CONCURRENT_TRANSFERS: usize = 6;
pub(crate) const MAX_CONCURRENT_PER_PLAN: usize = 4;

pub(crate) enum TransferManagerRequest {
    Enqueue {
        connection: Arc<SharedConnection>,
        plan_id: TransferPlanId,
        jobs: Vec<TransferJob>,
    },
    Cancel {
        transfer_id: TransferId,
    },
    Retry {
        transfer_id: TransferId,
    },
    ResolveConflict {
        request_id: ConflictRequestId,
        decision: ConflictDecision,
    },
}

struct QueuedTransfer {
    connection: Arc<SharedConnection>,
    plan_id: TransferPlanId,
    job: TransferJob,
}

struct RunningTransfer {
    plan_id: TransferPlanId,
    cancel: CancellationToken,
}

struct CompletedTransfer {
    transfer_id: TransferId,
    retry_route: Option<RetryRoute>,
}

struct RetryRoute {
    connection_key: ConnectionKey,
    connection: Weak<SharedConnection>,
    plan_id: TransferPlanId,
    job: TransferJob,
}

pub(crate) struct TransferManager {
    event_tx: flume::Sender<AppEvent>,
    connection_manager: Arc<ConnectionManager>,
    next_conflict_id: Arc<AtomicU64>,
    pending: VecDeque<QueuedTransfer>,
    running: HashMap<TransferId, RunningTransfer>,
    running_per_plan: HashMap<TransferPlanId, usize>,
    /// First job a planner emitted for a plan. For folder transfers this is the
    /// directory job that removes and recreates the destination, so it must
    /// finish before any child of that plan starts.
    plan_root_jobs: HashMap<TransferPlanId, TransferId>,
    /// Plans whose non-root job has started at least once. A retried root must
    /// not clear the destination again after children wrote into it.
    plan_children_started: HashSet<TransferPlanId>,
    retry_routes: HashMap<TransferId, RetryRoute>,
    plan_policies: Arc<Mutex<HashMap<TransferPlanId, ConflictPolicy>>>,
    conflict_waiters: Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
}

impl TransferManager {
    pub(crate) fn new(
        event_tx: flume::Sender<AppEvent>,
        next_conflict_id: Arc<AtomicU64>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            event_tx,
            connection_manager,
            next_conflict_id,
            pending: VecDeque::new(),
            running: HashMap::new(),
            running_per_plan: HashMap::new(),
            plan_root_jobs: HashMap::new(),
            plan_children_started: HashSet::new(),
            retry_routes: HashMap::new(),
            plan_policies: Arc::new(Mutex::new(HashMap::new())),
            conflict_waiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn run(mut self, request_rx: flume::Receiver<TransferManagerRequest>) {
        let (complete_tx, complete_rx) = flume::bounded(MAX_CONCURRENT_TRANSFERS * 2);
        loop {
            tokio::select! {
                request = request_rx.recv_async() => {
                    let Ok(request) = request else {
                        break;
                    };
                    self.handle_request(request).await;
                    self.start_queued(&complete_tx).await;
                }
                completed = complete_rx.recv_async() => {
                    let Ok(completed) = completed else {
                        break;
                    };
                    self.handle_completed(completed);
                    self.start_queued(&complete_tx).await;
                }
            }
        }

        for running in self.running.values() {
            running.cancel.cancel();
        }
    }

    async fn handle_request(&mut self, request: TransferManagerRequest) {
        match request {
            TransferManagerRequest::Enqueue {
                connection,
                plan_id,
                jobs,
            } => self.enqueue(connection, plan_id, jobs, true),
            TransferManagerRequest::Cancel { transfer_id } => {
                if let Some(position) = self
                    .pending
                    .iter()
                    .position(|transfer| transfer.job.id == transfer_id)
                {
                    let plan_id = self.pending.remove(position).map(|queued| queued.plan_id);
                    if let Err(error) = self
                        .event_tx
                        .send_async(AppEvent::TransferSkipped { transfer_id })
                        .await
                    {
                        warn!(error = %error, "queued transfer cancellation event dropped");
                    }
                    if let Some(plan_id) = plan_id {
                        self.cleanup_plan_if_idle(plan_id);
                    }
                } else if let Some(running) = self.running.get(&transfer_id) {
                    running.cancel.cancel();
                }
            }
            TransferManagerRequest::Retry { transfer_id } => {
                if let Some(route) = self.retry_routes.remove(&transfer_id) {
                    let connection = route
                        .connection
                        .upgrade()
                        .filter(|connection| !connection.connection_lost.is_cancelled())
                        .or_else(|| self.connection_manager.connected(&route.connection_key));
                    if let Some(connection) = connection {
                        self.enqueue(connection, route.plan_id, vec![route.job], false);
                    } else {
                        self.retry_routes.insert(transfer_id, route);
                        let error = UserFacingError::new(
                            ErrorCode::ChannelClosed,
                            "Reconnect before retrying",
                            "The previous connection is no longer available. Reconnect this profile and retry the transfer.",
                        )
                        .with_retryable(true);
                        if let Err(send_error) = self
                            .event_tx
                            .send_async(AppEvent::TransferFailed(TransferFailure {
                                transfer_id,
                                error,
                            }))
                            .await
                        {
                            warn!(error = %send_error, "transfer retry failure event dropped");
                        }
                    }
                }
            }
            TransferManagerRequest::ResolveConflict {
                request_id,
                decision,
            } => {
                if let Ok(mut waiters) = self.conflict_waiters.lock()
                    && let Some(waiter) = waiters.remove(&request_id)
                    && waiter.sender.send(decision).is_err()
                {
                    warn!("conflict decision receiver was dropped");
                }
            }
        }
    }

    fn enqueue(
        &mut self,
        connection: Arc<SharedConnection>,
        plan_id: TransferPlanId,
        jobs: Vec<TransferJob>,
        record_root: bool,
    ) {
        if let Some(job) = jobs.first()
            && let Ok(mut policies) = self.plan_policies.lock()
        {
            policies
                .entry(plan_id)
                .or_insert_with(|| job.conflict_policy.clone());
        }
        // A retried single job must never be mistaken for the plan root: the
        // root mapping survives the plan's lifetime for exactly this reason.
        if record_root && let Some(root_job) = jobs.first() {
            self.plan_root_jobs.entry(plan_id).or_insert(root_job.id);
        }
        self.pending
            .extend(jobs.into_iter().map(|job| QueuedTransfer {
                connection: connection.clone(),
                plan_id,
                job,
            }));
    }

    fn handle_completed(&mut self, completed: CompletedTransfer) {
        let completed_plan_id = self.running.remove(&completed.transfer_id).map(|running| {
            if let Some(count) = self.running_per_plan.get_mut(&running.plan_id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.running_per_plan.remove(&running.plan_id);
                }
            }
            running.plan_id
        });
        if let Some(route) = completed.retry_route {
            self.retry_routes.insert(completed.transfer_id, route);
        } else {
            self.retry_routes.remove(&completed.transfer_id);
        }
        if let Some(plan_id) = completed_plan_id {
            self.cleanup_plan_if_idle(plan_id);
        }
    }

    fn cleanup_plan_if_idle(&mut self, plan_id: TransferPlanId) {
        let has_pending = self.pending.iter().any(|queued| queued.plan_id == plan_id);
        let has_running = self.running_per_plan.contains_key(&plan_id);
        let has_retry = self
            .retry_routes
            .values()
            .any(|route| route.plan_id == plan_id);
        if !has_pending
            && !has_running
            && !has_retry
            && let Ok(mut policies) = self.plan_policies.lock()
        {
            policies.remove(&plan_id);
        }
        if !has_pending && !has_running && !has_retry {
            self.plan_root_jobs.remove(&plan_id);
            self.plan_children_started.remove(&plan_id);
        }
    }

    /// Whether `queued` may start now. For downloads, the first job of a plan
    /// (the folder job) removes the destination tree and must run alone before
    /// any child. Upload folder jobs only create directories, so their plans
    /// keep normal intra-plan concurrency.
    fn is_eligible(&self, queued: &QueuedTransfer) -> bool {
        let running = self
            .running_per_plan
            .get(&queued.plan_id)
            .copied()
            .unwrap_or(0);
        if running >= MAX_CONCURRENT_PER_PLAN {
            return false;
        }
        if queued.job.direction != TransferDirection::Download {
            return true;
        }
        let Some(root_id) = self.plan_root_jobs.get(&queued.plan_id) else {
            return running == 0;
        };
        if queued.job.id == *root_id {
            return running == 0;
        }
        let root_pending = self
            .pending
            .iter()
            .any(|other| other.plan_id == queued.plan_id && other.job.id == *root_id);
        !root_pending && !self.running.contains_key(root_id)
    }

    /// Whether this job may clear its plan's destination before transferring.
    /// Only the plan root may, and only before any child of that plan started:
    /// a retried root must not delete output that already completed. Calling
    /// this for a non-root job records that the plan's tree is now in use.
    fn take_destination_clear_permission(
        &mut self,
        plan_id: TransferPlanId,
        transfer_id: TransferId,
    ) -> bool {
        let is_plan_root = self
            .plan_root_jobs
            .get(&plan_id)
            .is_some_and(|root_id| *root_id == transfer_id);
        let may_clear = is_plan_root && !self.plan_children_started.contains(&plan_id);
        if !is_plan_root {
            self.plan_children_started.insert(plan_id);
        }
        may_clear
    }

    fn next_queued_position(&self) -> Option<usize> {
        // Two phases keep planning fair: prefer a plan with nothing running,
        // then fill remaining slots up to the per-plan cap.
        self.pending
            .iter()
            .position(|queued| {
                self.running_per_plan
                    .get(&queued.plan_id)
                    .copied()
                    .unwrap_or(0)
                    == 0
                    && self.is_eligible(queued)
            })
            .or_else(|| {
                self.pending
                    .iter()
                    .position(|queued| self.is_eligible(queued))
            })
    }

    async fn start_queued(&mut self, complete_tx: &flume::Sender<CompletedTransfer>) {
        while self.running.len() < MAX_CONCURRENT_TRANSFERS {
            let Some(position) = self.next_queued_position() else {
                return;
            };
            let Some(queued) = self.pending.remove(position) else {
                return;
            };
            let mut job = queued.job;
            job.conflict_policy = self
                .plan_policies
                .lock()
                .ok()
                .and_then(|policies| policies.get(&queued.plan_id).cloned())
                .unwrap_or_else(|| job.conflict_policy.clone());

            let transfer_session =
                match open_transfer_session(&queued.connection.handle, job.direction).await {
                    Ok(session) => session,
                    Err(error) => {
                        let transfer_id = job.id;
                        let retryable = error.retryable;
                        if let Err(send_error) = self
                            .event_tx
                            .send_async(AppEvent::TransferFailed(TransferFailure {
                                transfer_id,
                                error,
                            }))
                            .await
                        {
                            warn!(error = %send_error, "transfer channel failure event dropped");
                            return;
                        }
                        if retryable {
                            self.retry_routes.insert(
                                transfer_id,
                                RetryRoute {
                                    connection_key: queued.connection.connection_key.clone(),
                                    connection: Arc::downgrade(&queued.connection),
                                    plan_id: queued.plan_id,
                                    job,
                                },
                            );
                        } else {
                            self.cleanup_plan_if_idle(queued.plan_id);
                        }
                        continue;
                    }
                };

            let transfer_id = job.id;
            let may_clear_destination =
                self.take_destination_clear_permission(queued.plan_id, transfer_id);
            let cancel = CancellationToken::new();
            self.running.insert(
                transfer_id,
                RunningTransfer {
                    plan_id: queued.plan_id,
                    cancel: cancel.clone(),
                },
            );
            *self.running_per_plan.entry(queued.plan_id).or_insert(0) += 1;
            let event_tx = self.event_tx.clone();
            let complete_tx = complete_tx.clone();
            let conflict_context = TransferConflictContext {
                waiters: self.conflict_waiters.clone(),
                next_id: self.next_conflict_id.clone(),
                plan_policies: self.plan_policies.clone(),
            };
            let connection_key = queued.connection.host_port_string.clone();
            let connection = queued.connection;
            let retry_connection = connection.clone();
            let retry_job = job.clone();
            let plan_id = queued.plan_id;
            connection.active_channels.fetch_add(1, Ordering::Relaxed);
            std::mem::drop(tokio::spawn(async move {
                let outcome = run_transfer_job(
                    transfer_session,
                    job,
                    plan_id,
                    event_tx,
                    cancel,
                    conflict_context,
                    connection_key,
                    may_clear_destination,
                )
                .await;
                connection.active_channels.fetch_sub(1, Ordering::Relaxed);
                if let Ok(mut last_used) = connection.last_used.lock() {
                    *last_used = std::time::Instant::now();
                }
                let retry_route = matches!(outcome, TransferJobOutcome::Failed { retryable: true })
                    .then_some(RetryRoute {
                        connection_key: retry_connection.connection_key.clone(),
                        connection: Arc::downgrade(&retry_connection),
                        plan_id,
                        job: retry_job,
                    });
                if let Err(error) = complete_tx
                    .send_async(CompletedTransfer {
                        transfer_id,
                        retry_route,
                    })
                    .await
                {
                    warn!(error = %error, "transfer completion bookkeeping dropped");
                }
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostTrustConfig;
    use crate::known_hosts::KnownHostsStore;
    use crate::trust::TrustRegistry;
    use macsftp_core::{
        AuthCredential, ConflictPolicy, ConnectionKey, ConnectionPoolIdentity, ConnectionSettings,
        LocalPath, MetadataPolicy, RemoteEventScope, RemotePath, SessionId, TabId, Timestamp,
        TransferDirection, TransferEndpoint, TransferId, TransferJob, TransferPlanId,
        TransferState,
    };
    use macsftp_test_support::SshTestServer;

    async fn connect_test_server(server: &SshTestServer) -> Arc<SharedConnection> {
        let (event_tx, _event_rx) = flume::bounded(64);
        let app_known_hosts_path = server.fixture_dir.join("app_known_hosts");
        let line = format!("[127.0.0.1]:{} {}\n", server.port, server.host_public_key);
        std::fs::write(&app_known_hosts_path, line).expect("app known_hosts must be prefilled");

        let known_hosts = Arc::new(Mutex::new(KnownHostsStore::load(
            &app_known_hosts_path,
            None,
        )));
        let trust_config = Arc::new(HostTrustConfig::new(app_known_hosts_path.clone(), None));
        let trust_registry = Arc::new(TrustRegistry::new());

        let settings = ConnectionSettings {
            host: "127.0.0.1".to_string(),
            port: server.port,
            username: server.username.clone(),
            auth: AuthCredential::PrivateKey {
                key_path: server.client_key_path.display().to_string(),
                passphrase: None,
            },
            route: macsftp_core::ResolvedConnectionRoute::Direct,
        };

        let cm = Arc::new(ConnectionManager::new());
        let scope = RemoteEventScope::new(TabId(1), SessionId(1), 1);
        let mut connect_rx = cm.get_or_connect(
            &settings,
            &ConnectionPoolIdentity::Ephemeral(SessionId(1)),
            &scope,
            known_hosts,
            trust_config,
            trust_registry,
            event_tx,
        );

        match connect_rx
            .recv()
            .await
            .expect("connect response receiver must produce a result")
        {
            Ok(shared) => shared,
            Err(e) => panic!("connect failed: {e:?}"),
        }
    }

    fn dummy_job(id: u64, direction: TransferDirection) -> TransferJob {
        TransferJob {
            id: TransferId(id),
            direction,
            source: TransferEndpoint::Local(LocalPath::new(format!("/tmp/src-{id}"))),
            destination: TransferEndpoint::Remote(RemotePath::new(format!("/tmp/dest-{id}"))),
            state: TransferState::Queued,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::OverwriteAll,
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(0),
        }
    }

    #[tokio::test]
    async fn test_next_queued_position_fair_scheduling() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        let plan_2 = TransferPlanId(2);

        // Plans are enqueued as a whole, root job first.
        manager.enqueue(
            connection.clone(),
            plan_1,
            (1..=4)
                .map(|id| dummy_job(id, TransferDirection::Upload))
                .collect(),
            true,
        );
        manager.enqueue(
            connection.clone(),
            plan_2,
            vec![dummy_job(5, TransferDirection::Upload)],
            true,
        );

        // Case A: Neither plan is running. Queue arrival order applies -> index 0 (job 1).
        assert_eq!(manager.next_queued_position(), Some(0));

        // Simulate starting plan 1's root (job 1): it leaves the queue.
        let Some(root_1) = manager.pending.remove(0) else {
            panic!("plan 1 root must be queued");
        };
        manager.running.insert(
            root_1.job.id,
            RunningTransfer {
                plan_id: plan_1,
                cancel: CancellationToken::new(),
            },
        );
        manager.running_per_plan.insert(plan_1, 1);

        // Case B: Plan 1 has 1 job running, Plan 2 has 0 running.
        // Phase 1 prioritizes Plan 2 because it has 0 running jobs, even though
        // its root sits after plan 1's children.
        assert_eq!(manager.next_queued_position(), Some(3));

        // The root finished; one of its children is now the running job.
        manager.running.remove(&TransferId(1));
        // Plan 2's root starts running as well.
        let Some(root_2) = manager.pending.remove(3) else {
            panic!("plan 2 root must be queued");
        };
        manager.running.insert(
            root_2.job.id,
            RunningTransfer {
                plan_id: plan_2,
                cancel: CancellationToken::new(),
            },
        );
        manager.running_per_plan.insert(plan_2, 1);

        // Case C: Phase 1 finds no idle plan; phase 2 picks plan 1's first
        // child, whose root already completed.
        assert_eq!(manager.next_queued_position(), Some(0));
    }

    #[tokio::test]
    async fn test_plan_root_blocks_children_until_it_completes() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(9);
        let jobs: Vec<TransferJob> = (1..=4)
            .map(|id| dummy_job(id, TransferDirection::Download))
            .collect();
        manager.enqueue(connection, plan_id, jobs, true);

        // A folder plan's first job removes the destination tree; it must run
        // alone before any child starts.
        assert_eq!(manager.next_queued_position(), Some(0));
        let Some(root) = manager.pending.remove(0) else {
            panic!("root job must be queued");
        };
        manager.running.insert(
            root.job.id,
            RunningTransfer {
                plan_id,
                cancel: CancellationToken::new(),
            },
        );
        manager.running_per_plan.insert(plan_id, 1);
        assert_eq!(
            manager.next_queued_position(),
            None,
            "children must wait while the plan root is running"
        );

        // Root finished: its children may now fill the per-plan window.
        manager.running.remove(&root.job.id);
        manager.running_per_plan.insert(plan_id, 0);
        assert_eq!(manager.next_queued_position(), Some(0));
    }

    #[test]
    fn retried_plan_root_may_not_clear_destination_after_children_started() {
        let (event_tx, _event_rx) = flume::bounded(1);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);
        let plan_id = TransferPlanId(4);
        manager.plan_root_jobs.insert(plan_id, TransferId(1));

        assert!(
            manager.take_destination_clear_permission(plan_id, TransferId(1)),
            "the root may clear the destination before any child starts"
        );
        assert!(
            !manager.take_destination_clear_permission(plan_id, TransferId(2)),
            "children never clear the destination"
        );
        assert!(
            !manager.take_destination_clear_permission(plan_id, TransferId(1)),
            "a retried root must not delete output written by children"
        );
    }

    #[tokio::test]
    async fn test_per_plan_concurrency_limit() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        let plan_2 = TransferPlanId(2);

        for i in 1..=5 {
            manager.enqueue(
                connection.clone(),
                plan_1,
                vec![dummy_job(i, TransferDirection::Upload)],
                true,
            );
        }

        // Set Plan 1 to MAX_CONCURRENT_PER_PLAN (4)
        manager
            .running_per_plan
            .insert(plan_1, MAX_CONCURRENT_PER_PLAN);

        // All pending jobs belong to Plan 1, which is at the per-plan cap -> None
        assert_eq!(manager.next_queued_position(), None);

        // Enqueue a job for Plan 2
        manager.enqueue(
            connection.clone(),
            plan_2,
            vec![dummy_job(10, TransferDirection::Upload)],
            true,
        );

        // Plan 2 is not at cap (0 running), so its position should be returned (index 5)
        assert_eq!(manager.next_queued_position(), Some(5));
    }

    #[tokio::test]
    async fn test_handle_completed_decrement_and_idle_cleanup() {
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        manager
            .plan_policies
            .lock()
            .expect("mutex lock should succeed")
            .insert(plan_1, ConflictPolicy::OverwriteAll);

        // Simulate 2 running jobs for Plan 1
        manager.running.insert(
            TransferId(1),
            RunningTransfer {
                plan_id: plan_1,
                cancel: CancellationToken::new(),
            },
        );
        manager.running.insert(
            TransferId(2),
            RunningTransfer {
                plan_id: plan_1,
                cancel: CancellationToken::new(),
            },
        );
        manager.running_per_plan.insert(plan_1, 2);

        // Job 1 completes without retry
        manager.handle_completed(CompletedTransfer {
            transfer_id: TransferId(1),
            retry_route: None,
        });

        assert!(!manager.running.contains_key(&TransferId(1)));
        assert_eq!(manager.running_per_plan.get(&plan_1), Some(&1));
        // Plan 1 still has running job 2, so policy is retained
        assert!(
            manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_1)
        );

        // Job 2 completes without retry
        manager.handle_completed(CompletedTransfer {
            transfer_id: TransferId(2),
            retry_route: None,
        });

        assert!(!manager.running.contains_key(&TransferId(2)));
        // Counter reached 0, so key is removed from running_per_plan
        assert!(!manager.running_per_plan.contains_key(&plan_1));
        // Plan 1 is now completely idle (no pending, no running, no retry), so policy is cleaned up!
        assert!(
            !manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_1)
        );
    }

    #[tokio::test]
    async fn test_handle_completed_retry_route_preserves_policy() {
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        manager
            .plan_policies
            .lock()
            .expect("mutex lock should succeed")
            .insert(plan_1, ConflictPolicy::OverwriteAll);

        manager.running.insert(
            TransferId(1),
            RunningTransfer {
                plan_id: plan_1,
                cancel: CancellationToken::new(),
            },
        );
        manager.running_per_plan.insert(plan_1, 1);

        let retry = RetryRoute {
            connection_key: ConnectionKey {
                host: "127.0.0.1".to_string(),
                port: 22,
                username: "user".to_string(),
                identity: ConnectionPoolIdentity::Ephemeral(SessionId(1)),
            },
            connection: Weak::new(),
            plan_id: plan_1,
            job: dummy_job(1, TransferDirection::Upload),
        };

        // Job completes with retry route
        manager.handle_completed(CompletedTransfer {
            transfer_id: TransferId(1),
            retry_route: Some(retry),
        });

        assert!(!manager.running.contains_key(&TransferId(1)));
        assert!(!manager.running_per_plan.contains_key(&plan_1));
        assert!(manager.retry_routes.contains_key(&TransferId(1)));
        // Because a retry route exists for Plan 1, the policy must NOT be removed!
        assert!(
            manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_1)
        );
    }

    #[tokio::test]
    async fn test_cleanup_plan_if_idle_conditions() {
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(99);

        // Condition 1: Running exists -> policy retained
        manager
            .plan_policies
            .lock()
            .expect("mutex lock should succeed")
            .insert(plan_id, ConflictPolicy::OverwriteAll);
        manager.running_per_plan.insert(plan_id, 1);
        manager.cleanup_plan_if_idle(plan_id);
        assert!(
            manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_id)
        );

        // Condition 2: Running cleared, but retry route exists -> policy retained
        manager.running_per_plan.remove(&plan_id);
        manager.retry_routes.insert(
            TransferId(999),
            RetryRoute {
                connection_key: ConnectionKey {
                    host: "localhost".to_string(),
                    port: 22,
                    username: "user".to_string(),
                    identity: ConnectionPoolIdentity::Ephemeral(SessionId(1)),
                },
                connection: Weak::new(),
                plan_id,
                job: dummy_job(999, TransferDirection::Upload),
            },
        );
        manager.cleanup_plan_if_idle(plan_id);
        assert!(
            manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_id)
        );

        // Condition 3: Retry route cleared -> policy removed
        manager.retry_routes.remove(&TransferId(999));
        manager.cleanup_plan_if_idle(plan_id);
        assert!(
            !manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_id)
        );
    }

    #[tokio::test]
    async fn test_queued_transfer_cancellation() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        manager.enqueue(
            connection.clone(),
            plan_1,
            vec![dummy_job(100, TransferDirection::Upload)],
            true,
        );
        assert_eq!(manager.pending.len(), 1);

        // Cancel the queued transfer
        manager
            .handle_request(TransferManagerRequest::Cancel {
                transfer_id: TransferId(100),
            })
            .await;

        assert_eq!(manager.pending.len(), 0);
        let event = event_rx
            .try_recv()
            .expect("TransferSkipped event should be emitted");
        assert_eq!(
            event,
            AppEvent::TransferSkipped {
                transfer_id: TransferId(100)
            }
        );
        // Since plan has no pending, running or retry, policy is cleaned up
        assert!(
            !manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_1)
        );
    }
}
