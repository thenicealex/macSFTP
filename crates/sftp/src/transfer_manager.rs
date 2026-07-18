use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};

use macsftp_core::{
    AppEvent, ConflictDecision, ConflictPolicy, ConflictRequestId, ConnectionKey, ErrorCode,
    TransferFailure, TransferId, TransferJob, TransferPlanId, UserFacingError,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::pool::{ConnectionManager, SharedConnection};
use crate::session_actor::{
    ConflictWaiter, TransferConflictContext, TransferJobOutcome, open_transfer_sftp,
    run_transfer_job,
};

const MAX_CONCURRENT_TRANSFERS: usize = 4;

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
    running_plans: HashSet<TransferPlanId>,
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
            running_plans: HashSet::new(),
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
                    let completed_plan_id = self
                        .running
                        .remove(&completed.transfer_id)
                        .map(|running| {
                        self.running_plans.remove(&running.plan_id);
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
            } => self.enqueue(connection, plan_id, jobs),
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
                        self.enqueue(connection, route.plan_id, vec![route.job]);
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
                {
                    let _send_result = waiter.sender.send(decision);
                }
            }
        }
    }

    fn enqueue(
        &mut self,
        connection: Arc<SharedConnection>,
        plan_id: TransferPlanId,
        jobs: Vec<TransferJob>,
    ) {
        if let Some(job) = jobs.first()
            && let Ok(mut policies) = self.plan_policies.lock()
        {
            policies
                .entry(plan_id)
                .or_insert_with(|| job.conflict_policy.clone());
        }
        self.pending
            .extend(jobs.into_iter().map(|job| QueuedTransfer {
                connection: connection.clone(),
                plan_id,
                job,
            }));
    }

    fn cleanup_plan_if_idle(&mut self, plan_id: TransferPlanId) {
        let has_pending = self.pending.iter().any(|queued| queued.plan_id == plan_id);
        let has_running = self.running_plans.contains(&plan_id);
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
    }

    async fn start_queued(&mut self, complete_tx: &flume::Sender<CompletedTransfer>) {
        while self.running.len() < MAX_CONCURRENT_TRANSFERS {
            let Some(position) = self
                .pending
                .iter()
                .position(|queued| !self.running_plans.contains(&queued.plan_id))
            else {
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

            let transfer_sftp = match open_transfer_sftp(&queued.connection.handle).await {
                Ok(sftp) => sftp,
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
            let cancel = CancellationToken::new();
            self.running.insert(
                transfer_id,
                RunningTransfer {
                    plan_id: queued.plan_id,
                    cancel: cancel.clone(),
                },
            );
            self.running_plans.insert(queued.plan_id);
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
                    transfer_sftp,
                    job,
                    plan_id,
                    event_tx,
                    cancel,
                    conflict_context,
                    connection_key,
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
