use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, Weak};
use std::time::SystemTime;

use macsftp_core::{
    AppEvent, ConflictDecision, ConflictPolicy, ConflictRequestId, ConnectionKey, ErrorCode,
    StartTransferCommand, TabId, Timestamp, TransferDirection, TransferEndpoint, TransferFailure,
    TransferId, TransferJob, TransferPlanId, TransferPlanSnapshot, UserFacingError,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::pool::{ConnectionManager, SharedConnection};
use crate::session_actor::RemoteSessionRequest;
use crate::transfer_job::{
    ConflictWaiter, PlanTransferFacts, TransferConflictContext, TransferJobOutcome,
    TransferSessionSlot, open_transfer_session, run_transfer_job,
};
use crate::transfer_planner::{new_plan, plan_local_upload};

pub(crate) const MAX_CONCURRENT_TRANSFERS: usize = 12;
pub(crate) const MAX_CONCURRENT_PER_PLAN: usize = 8;
/// OpenSSH counts each subsystem request against `MaxSessions` (10 by default)
/// and the browsing session holds one channel of its own. Staying at nine
/// leaves room for it and keeps a tenth slot free for anything the server
/// opens on its side; more transfer channels than this make channel opens fail
/// on servers that keep the default.
const MAX_CHANNELS_PER_CONNECTION: usize = 9;
/// Idle sessions held per plan. Jobs of one plan run at most
/// [`MAX_CONCURRENT_PER_PLAN`] at a time, so keeping that many is all a later
/// job can use; the rest are closed instead of pinning server processes.
const MAX_IDLE_SESSIONS_PER_PLAN: usize = MAX_CONCURRENT_PER_PLAN;
/// Narrow seam for looking up the live browsing session's request channel
/// for a `(TabId, session_epoch)` pair.
pub(crate) trait TransferSessionSource: Send + Sync + 'static {
    fn session_request_tx(
        &self,
        tab_id: TabId,
        session_epoch: u64,
    ) -> Option<flume::Sender<RemoteSessionRequest>>;
}

pub(crate) enum TransferManagerRequest {
    Start(StartTransferCommand),
    Cancel {
        transfer_id: TransferId,
        /// Plan the cancelled job belongs to, when the runtime knows it.
        plan_id: Option<TransferPlanId>,
    },
    Retry {
        transfer_id: TransferId,
    },
    ResolveConflict {
        request_id: ConflictRequestId,
        decision: ConflictDecision,
    },
}

struct PlanningPlan {
    root_job_id: TransferId,
    cancel: CancellationToken,
    cancelled_children: HashSet<TransferId>,
    retry_command: StartTransferCommand,
}

struct PlanningFinished {
    plan_id: TransferPlanId,
    outcome: PlanningOutcome,
}

enum PlanningOutcome {
    Cancelled,
    Failed,
    HandoffFailed {
        jobs: Vec<TransferJob>,
        error: UserFacingError,
    },
    Ready {
        connection: Arc<SharedConnection>,
        jobs: Vec<TransferJob>,
    },
}

pub(crate) struct QueuedTransfer {
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
    /// Set when the job finished cleanly and its session can serve later jobs
    /// of the same plan.
    reusable_session: Option<Arc<TransferSessionSlot>>,
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
    session_source: Arc<dyn TransferSessionSource>,
    next_plan_id: u64,
    next_transfer_id: Arc<AtomicU64>,
    next_conflict_id: Arc<AtomicU64>,
    planning_plans: HashMap<TransferPlanId, PlanningPlan>,
    planning_retries: HashMap<TransferId, StartTransferCommand>,
    pending: VecDeque<QueuedTransfer>,
    pending_per_plan: HashMap<TransferPlanId, usize>,
    pending_roots: HashSet<TransferId>,
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
    /// Sessions a plan has finished with, available to its queued jobs.
    /// Dropped as soon as the plan has no work left.
    idle_sessions: HashMap<TransferPlanId, Vec<Arc<TransferSessionSlot>>>,
    /// Verified directories and confirmed atomic hardlink support shared
    /// across concurrent workers of the same plan to eliminate redundant RTTs.
    plan_facts: HashMap<TransferPlanId, PlanTransferFacts>,
    plan_policies: Arc<Mutex<HashMap<TransferPlanId, ConflictPolicy>>>,
    conflict_waiters: Arc<Mutex<HashMap<ConflictRequestId, ConflictWaiter>>>,
}

impl TransferManager {
    pub(crate) fn new(
        event_tx: flume::Sender<AppEvent>,
        next_conflict_id: Arc<AtomicU64>,
        connection_manager: Arc<ConnectionManager>,
        session_source: Arc<dyn TransferSessionSource>,
    ) -> Self {
        Self {
            event_tx,
            connection_manager,
            session_source,
            next_plan_id: 1,
            next_transfer_id: Arc::new(AtomicU64::new(1)),
            next_conflict_id,
            planning_plans: HashMap::new(),
            planning_retries: HashMap::new(),
            pending: VecDeque::new(),
            pending_per_plan: HashMap::new(),
            pending_roots: HashSet::new(),
            running: HashMap::new(),
            running_per_plan: HashMap::new(),
            plan_root_jobs: HashMap::new(),
            plan_children_started: HashSet::new(),
            retry_routes: HashMap::new(),
            idle_sessions: HashMap::new(),
            plan_facts: HashMap::new(),
            plan_policies: Arc::new(Mutex::new(HashMap::new())),
            conflict_waiters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn run(mut self, request_rx: flume::Receiver<TransferManagerRequest>) {
        let (complete_tx, complete_rx) = flume::bounded(MAX_CONCURRENT_TRANSFERS * 2);
        let (planning_tx, planning_rx) = flume::bounded(64);
        loop {
            tokio::select! {
                request = request_rx.recv_async() => {
                    let Ok(request) = request else {
                        break;
                    };
                    self.handle_request(request, Some(&planning_tx)).await;
                    self.start_queued(&complete_tx).await;
                }
                finished = planning_rx.recv_async() => {
                    let Ok(finished) = finished else {
                        break;
                    };
                    self.handle_planning_finished(finished).await;
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

        for planning in self.planning_plans.values() {
            planning.cancel.cancel();
        }
        for running in self.running.values() {
            running.cancel.cancel();
        }
    }

    async fn handle_request(
        &mut self,
        request: TransferManagerRequest,
        planning_tx: Option<&flume::Sender<PlanningFinished>>,
    ) {
        match request {
            TransferManagerRequest::Start(command) => {
                self.start_transfer(command, planning_tx).await;
            }
            TransferManagerRequest::Cancel {
                transfer_id,
                plan_id,
            } => {
                if let Some((&_planning_plan_id, planning)) = self
                    .planning_plans
                    .iter_mut()
                    .find(|(id, plan)| plan.root_job_id == transfer_id || plan_id == Some(**id))
                {
                    if planning.root_job_id == transfer_id {
                        planning.cancel.cancel();
                    } else {
                        planning.cancelled_children.insert(transfer_id);
                    }
                    return;
                }
                if let Some(plan_id) = self.plan_of_root(transfer_id) {
                    self.cancel_plan(plan_id).await;
                    return;
                }
                if let Some(position) = self
                    .pending
                    .iter()
                    .position(|transfer| transfer.job.id == transfer_id)
                {
                    let plan_id = self.remove_pending(position).map(|queued| queued.plan_id);
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
                if let Some(command) = self.planning_retries.remove(&transfer_id) {
                    self.start_transfer(command, planning_tx).await;
                    return;
                }
                if let Some(route) = self.retry_routes.remove(&transfer_id) {
                    let connection = route
                        .connection
                        .upgrade()
                        .filter(|connection| !connection.connection_lost.is_cancelled())
                        .or_else(|| self.connection_manager.connected(&route.connection_key));
                    if let Some(connection) = connection {
                        self.enqueue(
                            connection,
                            route.plan_id,
                            vec![route.job],
                            false,
                            &HashSet::new(),
                        )
                        .await;
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

    async fn start_transfer(
        &mut self,
        command: StartTransferCommand,
        planning_tx: Option<&flume::Sender<PlanningFinished>>,
    ) {
        let request_tx = self
            .session_source
            .session_request_tx(command.tab_id, command.session_epoch);
        let transfer_connection_rx = request_tx.as_ref().and_then(|request_tx| {
            let (responder, response) = oneshot::channel();
            request_tx
                .try_send(RemoteSessionRequest::AcquireTransferConnection { responder })
                .ok()
                .map(|()| response)
        });
        let plan_id = TransferPlanId(self.next_plan_id);
        self.next_plan_id += 1;
        let root_job_id = TransferId(self.next_transfer_id.fetch_add(1, Ordering::Relaxed));
        let planning_cancel = CancellationToken::new();
        let (plan, root_job) =
            new_plan(&command, plan_id, root_job_id, Timestamp(SystemTime::now()));
        if let Err(error) = self
            .event_tx
            .send_async(AppEvent::TransferPlanStarted(Box::new(
                TransferPlanSnapshot {
                    plan,
                    root_job: root_job.clone(),
                },
            )))
            .await
        {
            warn!(error = %error, plan_id = plan_id.0, "transfer plan started event dropped");
            return;
        }
        self.planning_plans.insert(
            plan_id,
            PlanningPlan {
                root_job_id,
                cancel: planning_cancel.clone(),
                cancelled_children: HashSet::new(),
                retry_command: command.clone(),
            },
        );

        let Some(planning_tx) = planning_tx.cloned() else {
            return;
        };
        let event_tx = self.event_tx.clone();
        let next_transfer_id = self.next_transfer_id.clone();
        let cancel_after_task = planning_cancel.clone();
        let planning_task = match command.direction {
            TransferDirection::Upload => tokio::task::spawn_blocking(move || {
                plan_local_upload(
                    command,
                    plan_id,
                    root_job,
                    next_transfer_id,
                    event_tx,
                    planning_cancel,
                )
            }),
            TransferDirection::Download => tokio::spawn(async move {
                let Some(request_tx) = request_tx else {
                    emit_download_preparation_failed(&event_tx, plan_id).await;
                    return None;
                };
                let (responder, response) = oneshot::channel();
                if request_tx
                    .send_async(RemoteSessionRequest::PlanRemoteDownload {
                        command,
                        plan_id,
                        created_at: root_job.created_at,
                        next_transfer_id,
                        cancel: planning_cancel,
                        responder,
                    })
                    .await
                    .is_err()
                {
                    emit_download_preparation_failed(&event_tx, plan_id).await;
                    return None;
                }
                response.await.ok().flatten()
            }),
        };

        std::mem::drop(tokio::spawn(async move {
            let outcome = match planning_task.await {
                Ok(Some(jobs)) => {
                    if cancel_after_task.is_cancelled() {
                        PlanningOutcome::Cancelled
                    } else if let Some(connection_rx) = transfer_connection_rx {
                        tokio::select! {
                            _ = cancel_after_task.cancelled() => PlanningOutcome::Cancelled,
                            res = connection_rx => match res {
                                Ok(connection) => PlanningOutcome::Ready { connection, jobs },
                                Err(_) => PlanningOutcome::HandoffFailed {
                                    jobs,
                                    error: transfer_handoff_error(
                                        "The connection closed. Reconnect and try again.",
                                    ),
                                },
                            }
                        }
                    } else {
                        PlanningOutcome::HandoffFailed {
                            jobs,
                            error: transfer_handoff_error(
                                "The connected session has no transfer channel.",
                            ),
                        }
                    }
                }
                _ => {
                    if cancel_after_task.is_cancelled() {
                        PlanningOutcome::Cancelled
                    } else {
                        PlanningOutcome::Failed
                    }
                }
            };
            if let Err(error) = planning_tx
                .send_async(PlanningFinished { plan_id, outcome })
                .await
            {
                warn!(
                    error = %error,
                    plan_id = plan_id.0,
                    "transfer planning completion could not reach transfer manager"
                );
            }
        }));
    }

    async fn handle_planning_finished(&mut self, finished: PlanningFinished) {
        let Some(planning) = self.planning_plans.remove(&finished.plan_id) else {
            return;
        };
        let plan_id = finished.plan_id;
        match finished.outcome {
            PlanningOutcome::Cancelled => {
                emit_plan_cancelled(&self.event_tx, plan_id).await;
            }
            PlanningOutcome::Failed => {
                if planning.cancel.is_cancelled() {
                    emit_plan_cancelled(&self.event_tx, plan_id).await;
                } else {
                    self.planning_retries
                        .insert(planning.root_job_id, planning.retry_command);
                }
            }
            PlanningOutcome::HandoffFailed { jobs, error } => {
                if planning.cancel.is_cancelled() {
                    emit_plan_cancelled(&self.event_tx, plan_id).await;
                } else {
                    fail_planned_jobs(&self.event_tx, jobs, error).await;
                }
            }
            PlanningOutcome::Ready { connection, jobs } => {
                if planning.cancel.is_cancelled() {
                    emit_plan_cancelled(&self.event_tx, plan_id).await;
                } else {
                    self.enqueue(
                        connection,
                        plan_id,
                        jobs,
                        true,
                        &planning.cancelled_children,
                    )
                    .await;
                }
            }
        }
    }

    async fn enqueue(
        &mut self,
        connection: Arc<SharedConnection>,
        plan_id: TransferPlanId,
        jobs: Vec<TransferJob>,
        record_root: bool,
        cancelled_children: &HashSet<TransferId>,
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
            if let TransferEndpoint::Local(destination) = &root_job.destination {
                let facts = self.plan_facts.entry(plan_id).or_default();
                facts.set_destination_root(destination.clone());
            }
        }
        let mut skipped = Vec::new();
        for job in jobs {
            if cancelled_children.contains(&job.id) {
                skipped.push(job.id);
                continue;
            }
            self.push_pending(QueuedTransfer {
                connection: connection.clone(),
                plan_id,
                job,
            });
        }
        let skipped_any = !skipped.is_empty();
        for transfer_id in skipped {
            if let Err(error) = self
                .event_tx
                .send_async(AppEvent::TransferSkipped { transfer_id })
                .await
            {
                warn!(error = %error, "pre-enqueue transfer cancellation event dropped");
            }
        }
        if skipped_any {
            self.cleanup_plan_if_idle(plan_id);
        }
    }

    /// The plan a job is the root of, if this manager already knows the plan.
    ///
    /// A root only shows a cancel control while its plan row still reads
    /// "Preparing…", so a cancel aimed at a root means "cancel the plan".
    fn plan_of_root(&self, transfer_id: TransferId) -> Option<TransferPlanId> {
        self.plan_root_jobs
            .iter()
            .find(|(_, root_id)| **root_id == transfer_id)
            .map(|(plan_id, _)| *plan_id)
    }

    /// Cancel every job this plan owns right now, whether queued or running.
    ///
    /// Used when a cancel landed during the planning→enqueue handoff, or when
    /// the user cancelled a plan root. Cancelling only the root would let the
    /// remaining children finish and report a completed plan after the user
    /// asked to cancel it.
    ///
    /// The `TransferPlanCancelled` event is safe to send even late: the store
    /// only marks jobs that have not already reached a terminal state, so a
    /// cancel that loses the race with completion cannot rewrite finished
    /// work.
    async fn cancel_plan(&mut self, plan_id: TransferPlanId) {
        let cancelled_pending: Vec<TransferId> = self
            .pending
            .iter()
            .filter(|queued| queued.plan_id == plan_id)
            .map(|queued| queued.job.id)
            .collect();
        self.retain_pending_not_plan(plan_id);
        let cancelled_running: Vec<TransferId> = self
            .running
            .iter()
            .filter(|(_, running)| running.plan_id == plan_id)
            .map(|(transfer_id, _)| *transfer_id)
            .collect();
        for transfer_id in &cancelled_running {
            if let Some(running) = self.running.get(transfer_id) {
                running.cancel.cancel();
            }
        }
        let interrupted = !cancelled_pending.is_empty() || !cancelled_running.is_empty();
        if !interrupted {
            return;
        }
        // One plan-level event covers every job the store already knows
        // about; it also must be applied before any per-job terminal event,
        // otherwise the store can finalize the plan as `Completed` from the
        // last child transition and then refuse the cancellation.
        if let Err(error) = self
            .event_tx
            .send_async(AppEvent::TransferPlanCancelled { plan_id })
            .await
        {
            warn!(error = %error, "plan cancellation event dropped");
        }
        self.plan_root_jobs.remove(&plan_id);
        self.plan_children_started.remove(&plan_id);
        self.plan_facts.remove(&plan_id);
        self.cleanup_plan_if_idle(plan_id);
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
        if let (Some(plan_id), Some(session)) = (completed_plan_id, completed.reusable_session) {
            self.return_idle_session(plan_id, session);
        }
        if let Some(plan_id) = completed_plan_id {
            self.cleanup_plan_if_idle(plan_id);
        }
    }

    /// Takes a session the plan has finished with, if one matches the job's
    /// direction. Sessions are direction-specific: uploads run on the
    /// high-level SFTP session and downloads on the pipelined raw one.
    fn take_idle_session(
        &mut self,
        plan_id: TransferPlanId,
        direction: TransferDirection,
    ) -> Option<Arc<TransferSessionSlot>> {
        let sessions = self.idle_sessions.get_mut(&plan_id)?;
        let position = sessions
            .iter()
            .position(|session| session.direction() == direction)?;
        let session = sessions.remove(position);
        if sessions.is_empty() {
            self.idle_sessions.remove(&plan_id);
        }
        Some(session)
    }

    fn return_idle_session(&mut self, plan_id: TransferPlanId, session: Arc<TransferSessionSlot>) {
        let sessions = self.idle_sessions.entry(plan_id).or_default();
        if sessions.len() < MAX_IDLE_SESSIONS_PER_PLAN {
            sessions.push(session);
        }
    }

    pub(crate) fn push_pending(&mut self, queued: QueuedTransfer) {
        *self.pending_per_plan.entry(queued.plan_id).or_insert(0) += 1;
        if self.plan_root_jobs.get(&queued.plan_id) == Some(&queued.job.id) {
            self.pending_roots.insert(queued.job.id);
        }
        self.pending.push_back(queued);
    }

    pub(crate) fn remove_pending(&mut self, position: usize) -> Option<QueuedTransfer> {
        let queued = self.pending.remove(position)?;
        if let Some(count) = self.pending_per_plan.get_mut(&queued.plan_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.pending_per_plan.remove(&queued.plan_id);
            }
        }
        self.pending_roots.remove(&queued.job.id);
        Some(queued)
    }

    #[cfg(test)]
    pub(crate) fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_per_plan.clear();
        self.pending_roots.clear();
    }

    fn retain_pending_not_plan(&mut self, plan_id: TransferPlanId) {
        self.pending.retain(|queued| queued.plan_id != plan_id);
        self.pending_per_plan.remove(&plan_id);
        if let Some(root_id) = self.plan_root_jobs.get(&plan_id) {
            self.pending_roots.remove(root_id);
        }
    }

    fn cleanup_plan_if_idle(&mut self, plan_id: TransferPlanId) {
        let has_pending = self.pending_per_plan.contains_key(&plan_id);
        let has_running = self.running_per_plan.contains_key(&plan_id);
        let has_retry = if has_pending || has_running {
            false
        } else {
            self.retry_routes
                .values()
                .any(|route| route.plan_id == plan_id)
        };
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
            // Idle transfer sessions are only worth their server-side process
            // while the plan still has jobs to run.
            self.idle_sessions.remove(&plan_id);
            self.plan_facts.remove(&plan_id);
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
        if queued.connection.active_channels.load(Ordering::Relaxed) >= MAX_CHANNELS_PER_CONNECTION
        {
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
        let root_pending = self.pending_roots.contains(root_id);
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
        if self.pending.is_empty() {
            return None;
        }

        // Fast-path: find plans with remaining concurrency slots (< MAX_CONCURRENT_PER_PLAN)
        // and whether any plan is completely idle (0 running).
        let mut has_idle_plan = false;
        let mut admissible_plans = Vec::with_capacity(self.pending_per_plan.len());

        for &plan_id in self.pending_per_plan.keys() {
            let running = self.running_per_plan.get(&plan_id).copied().unwrap_or(0);
            if running == 0 {
                has_idle_plan = true;
            }
            if running < MAX_CONCURRENT_PER_PLAN {
                admissible_plans.push(plan_id);
            }
        }

        if admissible_plans.is_empty() {
            return None;
        }

        // Two phases keep planning fair: prefer a plan with nothing running,
        // then fill remaining slots up to the per-plan cap.
        if has_idle_plan {
            self.pending
                .iter()
                .position(|queued| {
                    admissible_plans.contains(&queued.plan_id)
                        && self
                            .running_per_plan
                            .get(&queued.plan_id)
                            .copied()
                            .unwrap_or(0)
                            == 0
                        && self.is_eligible(queued)
                })
                .or_else(|| {
                    self.pending.iter().position(|queued| {
                        admissible_plans.contains(&queued.plan_id) && self.is_eligible(queued)
                    })
                })
        } else {
            self.pending.iter().position(|queued| {
                admissible_plans.contains(&queued.plan_id) && self.is_eligible(queued)
            })
        }
    }

    async fn start_queued(&mut self, complete_tx: &flume::Sender<CompletedTransfer>) {
        while self.running.len() < MAX_CONCURRENT_TRANSFERS {
            let Some(position) = self.next_queued_position() else {
                return;
            };
            let Some(queued) = self.remove_pending(position) else {
                return;
            };
            let mut job = queued.job;
            job.conflict_policy = self
                .plan_policies
                .lock()
                .ok()
                .and_then(|policies| policies.get(&queued.plan_id).cloned())
                .unwrap_or_else(|| job.conflict_policy.clone());

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
            let idle_session = self.take_idle_session(plan_id, job.direction);
            let facts = self.plan_facts.entry(plan_id).or_default().clone();
            connection.active_channels.fetch_add(1, Ordering::Relaxed);
            std::mem::drop(tokio::spawn(async move {
                // Opening a transfer channel costs several round trips (channel
                // open, subsystem, SFTP init, limits). Awaiting it here would
                // serialize every start behind the previous one and cap
                // small-file throughput at roughly one file per setup latency,
                // so a job only opens a session when the plan has none idle
                // and reports the failure through the same completion path as
                // a job.
                let transfer_session = match idle_session {
                    Some(session) => session,
                    None => match open_transfer_session(&connection.handle, job.direction).await {
                        Ok(session) => Arc::new(TransferSessionSlot::with_facts(session, facts)),
                        Err(error) => {
                            let retryable = error.retryable;
                            if let Err(send_error) = event_tx
                                .send_async(AppEvent::TransferFailed(TransferFailure {
                                    transfer_id,
                                    error,
                                }))
                                .await
                            {
                                warn!(error = %send_error, "transfer channel failure event dropped");
                            }
                            connection.active_channels.fetch_sub(1, Ordering::Relaxed);
                            if let Ok(mut last_used) = connection.last_used.lock() {
                                *last_used = std::time::Instant::now();
                            }
                            let retry_route = retryable.then_some(RetryRoute {
                                connection_key: retry_connection.connection_key.clone(),
                                connection: Arc::downgrade(&retry_connection),
                                plan_id,
                                job: retry_job,
                            });
                            if let Err(send_error) = complete_tx
                                .send_async(CompletedTransfer {
                                    transfer_id,
                                    retry_route,
                                    reusable_session: None,
                                })
                                .await
                            {
                                warn!(
                                    error = %send_error,
                                    "transfer completion bookkeeping dropped"
                                );
                            }
                            return;
                        }
                    },
                };
                let outcome = run_transfer_job(
                    transfer_session.clone(),
                    job,
                    plan_id,
                    event_tx.clone(),
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
                // A reusable upload conflict skip happens before file I/O.
                // Other cancellations or failures may leave requests in flight.
                let reusable_session = matches!(
                    outcome,
                    TransferJobOutcome::Completed
                        | TransferJobOutcome::Skipped {
                            reusable_session: true
                        }
                )
                .then_some(transfer_session);
                if let Err(error) = complete_tx
                    .send_async(CompletedTransfer {
                        transfer_id,
                        retry_route,
                        reusable_session,
                    })
                    .await
                {
                    warn!(error = %error, "transfer completion bookkeeping dropped");
                }
            }));
        }
    }
}

async fn emit_download_preparation_failed(
    event_tx: &flume::Sender<AppEvent>,
    plan_id: TransferPlanId,
) {
    if let Err(error) = event_tx
        .send_async(AppEvent::TransferPlanFailed {
            plan_id,
            error: UserFacingError::new(
                ErrorCode::ChannelClosed,
                "Download Preparation Failed",
                "The connected session is no longer available. Reconnect and try again.",
            )
            .with_retryable(true),
        })
        .await
    {
        warn!(
            error = %error,
            plan_id = plan_id.0,
            "download preparation failure event dropped"
        );
    }
}

pub(crate) fn transfer_handoff_error(detail: &'static str) -> UserFacingError {
    UserFacingError::new(ErrorCode::ChannelClosed, "Transfer Couldn’t Start", detail)
}

async fn emit_plan_cancelled(event_tx: &flume::Sender<AppEvent>, plan_id: TransferPlanId) {
    if let Err(error) = event_tx
        .send_async(AppEvent::TransferPlanCancelled { plan_id })
        .await
    {
        warn!(
            error = %error,
            plan_id = plan_id.0,
            "plan cancellation event dropped"
        );
    }
}

pub(crate) async fn fail_planned_jobs(
    event_tx: &flume::Sender<AppEvent>,
    jobs: Vec<TransferJob>,
    error: UserFacingError,
) {
    for job in jobs {
        if let Err(send_error) = event_tx
            .send_async(AppEvent::TransferFailed(TransferFailure {
                transfer_id: job.id,
                error: error.clone(),
            }))
            .await
        {
            warn!(error = %send_error, "transfer handoff failure event dropped");
            return;
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

    struct NoopSessionSource;

    impl TransferSessionSource for NoopSessionSource {
        fn session_request_tx(
            &self,
            _tab_id: TabId,
            _session_epoch: u64,
        ) -> Option<flume::Sender<RemoteSessionRequest>> {
            None
        }
    }

    impl TransferManager {
        fn new_for_test(
            event_tx: flume::Sender<AppEvent>,
            next_conflict_id: Arc<AtomicU64>,
            connection_manager: Arc<ConnectionManager>,
        ) -> Self {
            Self::new(
                event_tx,
                next_conflict_id,
                connection_manager,
                Arc::new(NoopSessionSource),
            )
        }

        async fn enqueue_for_test(
            &mut self,
            connection: Arc<SharedConnection>,
            plan_id: TransferPlanId,
            jobs: Vec<TransferJob>,
            record_root: bool,
        ) {
            self.enqueue(connection, plan_id, jobs, record_root, &HashSet::new())
                .await;
        }

        async fn handle_test_request(&mut self, request: TransferManagerRequest) {
            self.handle_request(request, None).await;
        }
    }

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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        let plan_2 = TransferPlanId(2);

        // Plans are enqueued as a whole, root job first.
        manager
            .enqueue_for_test(
                connection.clone(),
                plan_1,
                (1..=4)
                    .map(|id| dummy_job(id, TransferDirection::Upload))
                    .collect(),
                true,
            )
            .await;
        manager
            .enqueue_for_test(
                connection.clone(),
                plan_2,
                vec![dummy_job(5, TransferDirection::Upload)],
                true,
            )
            .await;

        // Case A: Neither plan is running. Queue arrival order applies -> index 0 (job 1).
        assert_eq!(manager.next_queued_position(), Some(0));

        // Simulate starting plan 1's root (job 1): it leaves the queue.
        let Some(root_1) = manager.remove_pending(0) else {
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
        let Some(root_2) = manager.remove_pending(3) else {
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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(9);
        let jobs: Vec<TransferJob> = (1..=4)
            .map(|id| dummy_job(id, TransferDirection::Download))
            .collect();
        manager
            .enqueue_for_test(connection, plan_id, jobs, true)
            .await;

        // A folder plan's first job removes the destination tree; it must run
        // alone before any child starts.
        assert_eq!(manager.next_queued_position(), Some(0));
        let Some(root) = manager.remove_pending(0) else {
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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);
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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        let plan_2 = TransferPlanId(2);

        for i in 1..=5 {
            manager
                .enqueue_for_test(
                    connection.clone(),
                    plan_1,
                    vec![dummy_job(i, TransferDirection::Upload)],
                    true,
                )
                .await;
        }

        // Set Plan 1 to MAX_CONCURRENT_PER_PLAN (4)
        manager
            .running_per_plan
            .insert(plan_1, MAX_CONCURRENT_PER_PLAN);

        // All pending jobs belong to Plan 1, which is at the per-plan cap -> None
        assert_eq!(manager.next_queued_position(), None);

        // Enqueue a job for Plan 2
        manager
            .enqueue_for_test(
                connection.clone(),
                plan_2,
                vec![dummy_job(10, TransferDirection::Upload)],
                true,
            )
            .await;

        // Plan 2 is not at cap (0 running), so its position should be returned (index 5)
        assert_eq!(manager.next_queued_position(), Some(5));
    }

    #[tokio::test]
    async fn test_handle_completed_decrement_and_idle_cleanup() {
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

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
            reusable_session: None,
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
            reusable_session: None,
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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

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
            reusable_session: None,
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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

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
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        manager
            .enqueue_for_test(
                connection.clone(),
                plan_1,
                vec![dummy_job(100, TransferDirection::Upload)],
                true,
            )
            .await;
        assert_eq!(manager.pending.len(), 1);

        // Cancel the queued transfer
        manager
            .handle_test_request(TransferManagerRequest::Cancel {
                transfer_id: TransferId(100),
                plan_id: Some(TransferPlanId(1)),
            })
            .await;

        assert_eq!(manager.pending.len(), 0);
        let event = event_rx
            .try_recv()
            .expect("the cancelled plan must be reported");
        assert_eq!(event, AppEvent::TransferPlanCancelled { plan_id: plan_1 });
        // Since plan has no pending, running or retry, policy is cleaned up
        assert!(
            !manager
                .plan_policies
                .lock()
                .expect("mutex lock should succeed")
                .contains_key(&plan_1)
        );
    }

    fn dummy_start_command() -> StartTransferCommand {
        StartTransferCommand {
            tab_id: TabId(1),
            session_epoch: 1,
            profile_id: macsftp_core::ProfileId(1),
            direction: TransferDirection::Upload,
            sources: vec![TransferEndpoint::Local(LocalPath::new("/tmp/source"))],
            destination: TransferEndpoint::Remote(RemotePath::new("/remote/dest")),
            conflict_policy: ConflictPolicy::Ask,
            metadata_policy: MetadataPolicy::default(),
        }
    }

    #[tokio::test]
    async fn cancelling_the_plan_root_during_planning_cancels_the_whole_plan() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(1);
        manager.planning_plans.insert(
            plan_id,
            PlanningPlan {
                root_job_id: TransferId(100),
                cancel: CancellationToken::new(),
                cancelled_children: HashSet::new(),
                retry_command: dummy_start_command(),
            },
        );
        manager
            .handle_test_request(TransferManagerRequest::Cancel {
                transfer_id: TransferId(100),
                plan_id: Some(plan_id),
            })
            .await;
        assert!(
            event_rx.try_recv().is_err(),
            "cancelling while planning is still running waits for the planning finish"
        );

        manager
            .handle_planning_finished(PlanningFinished {
                plan_id,
                outcome: PlanningOutcome::Ready {
                    connection,
                    jobs: vec![
                        dummy_job(100, TransferDirection::Upload),
                        dummy_job(101, TransferDirection::Upload),
                        dummy_job(102, TransferDirection::Upload),
                    ],
                },
            })
            .await;

        assert!(
            manager.pending.is_empty(),
            "cancelling the root must stop the whole plan, not just the root job"
        );
        assert!(
            !manager.planning_plans.contains_key(&plan_id),
            "planning state must be removed once planning finishes"
        );
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert_eq!(
            events,
            vec![AppEvent::TransferPlanCancelled { plan_id }],
            "one plan event covers the root and every child"
        );
    }

    #[tokio::test]
    async fn cancelling_a_child_during_planning_skips_it_when_enqueued() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_1 = TransferPlanId(1);
        manager.planning_plans.insert(
            plan_1,
            PlanningPlan {
                root_job_id: TransferId(100),
                cancel: CancellationToken::new(),
                cancelled_children: HashSet::new(),
                retry_command: dummy_start_command(),
            },
        );

        // Cancel a child (101) while its plan is still being planned.
        manager
            .handle_test_request(TransferManagerRequest::Cancel {
                transfer_id: TransferId(101),
                plan_id: Some(plan_1),
            })
            .await;

        assert!(
            event_rx.try_recv().is_err(),
            "a planning-phase child cancel must not emit an event before enqueue"
        );

        manager
            .handle_planning_finished(PlanningFinished {
                plan_id: plan_1,
                outcome: PlanningOutcome::Ready {
                    connection,
                    jobs: vec![
                        dummy_job(100, TransferDirection::Upload),
                        dummy_job(101, TransferDirection::Upload),
                    ],
                },
            })
            .await;

        // Only the cancelled child is skipped; the root still runs.
        assert_eq!(manager.pending.len(), 1);
        assert_eq!(manager.pending[0].job.id, TransferId(100));

        let event = event_rx
            .try_recv()
            .expect("the skipped job must be reported once it reaches the queue");
        assert_eq!(
            event,
            AppEvent::TransferSkipped {
                transfer_id: TransferId(101)
            }
        );
    }

    #[tokio::test]
    async fn failed_planning_drops_child_cancellations_and_records_retry() {
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(1);
        manager.planning_plans.insert(
            plan_id,
            PlanningPlan {
                root_job_id: TransferId(100),
                cancel: CancellationToken::new(),
                cancelled_children: HashSet::from([TransferId(101)]),
                retry_command: dummy_start_command(),
            },
        );

        manager
            .handle_planning_finished(PlanningFinished {
                plan_id,
                outcome: PlanningOutcome::Failed,
            })
            .await;

        assert!(
            !manager.planning_plans.contains_key(&plan_id),
            "failed planning must drop per-plan cancellation records"
        );
        assert!(
            manager.planning_retries.contains_key(&TransferId(100)),
            "failed planning must retain the command for retry"
        );
        assert!(
            event_rx.try_recv().is_err(),
            "planning failure bookkeeping must not emit spurious events"
        );
    }

    #[tokio::test]
    async fn cancel_plan_stops_queued_jobs_and_reports_once() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(7);
        manager
            .enqueue_for_test(
                connection,
                plan_id,
                vec![
                    dummy_job(500, TransferDirection::Upload),
                    dummy_job(501, TransferDirection::Upload),
                    dummy_job(502, TransferDirection::Upload),
                ],
                true,
            )
            .await;
        assert_eq!(manager.pending.len(), 3);

        manager.cancel_plan(plan_id).await;

        assert!(manager.pending.is_empty(), "the whole plan must stop");
        assert!(
            !manager.plan_root_jobs.contains_key(&plan_id),
            "the plan must not keep root bookkeeping after cancellation"
        );
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert_eq!(
            events,
            vec![AppEvent::TransferPlanCancelled { plan_id }],
            "one plan event covers every queued job"
        );
    }

    #[tokio::test]
    async fn cancel_of_an_untracked_transfer_does_not_rewrite_its_state() {
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        manager
            .handle_test_request(TransferManagerRequest::Cancel {
                transfer_id: TransferId(100),
                plan_id: Some(TransferPlanId(1)),
            })
            .await;

        assert!(
            event_rx.try_recv().is_err(),
            "cancel of an untracked transfer must not emit TransferSkipped"
        );
        assert!(manager.pending.is_empty());
        assert!(manager.running.is_empty());
    }

    #[tokio::test]
    async fn retry_enqueues_previously_failed_route() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(1);
        manager.retry_routes.insert(
            TransferId(100),
            RetryRoute {
                connection_key: ConnectionKey {
                    host: "127.0.0.1".to_string(),
                    port: server.port,
                    username: server.username.clone(),
                    identity: ConnectionPoolIdentity::Ephemeral(SessionId(1)),
                },
                connection: Arc::downgrade(&connection),
                plan_id,
                job: dummy_job(100, TransferDirection::Upload),
            },
        );

        manager
            .handle_test_request(TransferManagerRequest::Cancel {
                transfer_id: TransferId(100),
                plan_id: Some(TransferPlanId(1)),
            })
            .await;
        manager
            .handle_test_request(TransferManagerRequest::Retry {
                transfer_id: TransferId(100),
            })
            .await;

        assert_eq!(manager.pending.len(), 1, "retry must enqueue the job");
        assert_eq!(manager.pending[0].job.id, TransferId(100));
        assert!(
            event_rx.try_recv().is_err(),
            "a retried transfer must not be reported as skipped"
        );
    }

    #[tokio::test]
    async fn idle_sessions_are_reused_per_plan_and_direction() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let upload = open_transfer_session(&connection.handle, TransferDirection::Upload)
            .await
            .expect("upload session should open against the test server");
        let session = Arc::new(TransferSessionSlot::new(upload));
        let plan_id = TransferPlanId(1);

        let other_plan = TransferPlanId(2);
        assert!(
            manager
                .take_idle_session(other_plan, TransferDirection::Upload)
                .is_none(),
            "a plan must not see another plan's sessions"
        );

        manager.return_idle_session(plan_id, session.clone());
        assert!(
            manager
                .take_idle_session(plan_id, TransferDirection::Download)
                .is_none(),
            "a download job must not take an upload session"
        );
        let taken = manager
            .take_idle_session(plan_id, TransferDirection::Upload)
            .expect("the plan's upload session should be reusable");
        assert!(Arc::ptr_eq(&taken, &session));

        // A plan that still has queued work keeps its session.
        manager.return_idle_session(plan_id, taken.clone());
        manager.push_pending(QueuedTransfer {
            connection: connection.clone(),
            plan_id,
            job: dummy_job(7, TransferDirection::Upload),
        });
        manager.cleanup_plan_if_idle(plan_id);
        assert!(
            manager.idle_sessions.contains_key(&plan_id),
            "a plan with pending jobs must keep its idle sessions"
        );

        manager.clear_pending();
        manager.cleanup_plan_if_idle(plan_id);
        assert!(
            manager.idle_sessions.is_empty(),
            "an idle plan must release its pooled sessions"
        );
    }

    #[tokio::test]
    async fn per_connection_channel_cap_blocks_new_jobs() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_id = TransferPlanId(1);
        manager
            .enqueue_for_test(
                connection.clone(),
                plan_id,
                vec![dummy_job(1, TransferDirection::Upload)],
                true,
            )
            .await;

        connection
            .active_channels
            .store(MAX_CHANNELS_PER_CONNECTION, Ordering::Relaxed);
        let queued = manager.pending.front().expect("job should be queued");
        assert!(
            !manager.is_eligible(queued),
            "no job may start once the connection is at its channel cap"
        );

        connection
            .active_channels
            .store(MAX_CHANNELS_PER_CONNECTION - 1, Ordering::Relaxed);
        let queued = manager.pending.front().expect("job should be queued");
        assert!(
            manager.is_eligible(queued),
            "a connection below the cap may start another job"
        );
    }

    #[tokio::test]
    async fn large_queue_dispatch_is_constant_time_and_tracks_plan_state() {
        let Some(server) = SshTestServer::spawn() else {
            eprintln!("skipping test: sshd not available");
            return;
        };
        let connection = connect_test_server(&server).await;
        let (event_tx, _event_rx) = flume::bounded(64);
        let next_conflict_id = Arc::new(AtomicU64::new(1));
        let cm = Arc::new(ConnectionManager::new());
        let mut manager = TransferManager::new_for_test(event_tx, next_conflict_id, cm);

        let plan_download = TransferPlanId(10);
        let jobs: Vec<TransferJob> = (1..=3000)
            .map(|id| dummy_job(id, TransferDirection::Download))
            .collect();
        manager
            .enqueue_for_test(connection.clone(), plan_download, jobs, true)
            .await;

        assert_eq!(manager.pending.len(), 3000);
        assert_eq!(manager.pending_per_plan.get(&plan_download), Some(&3000));
        assert!(manager.pending_roots.contains(&TransferId(1)));

        // First position must be the root (id 1)
        assert_eq!(manager.next_queued_position(), Some(0));

        // Start the root
        let root = manager.remove_pending(0).expect("root should be present");
        assert_eq!(root.job.id, TransferId(1));
        assert!(!manager.pending_roots.contains(&TransferId(1)));
        assert_eq!(manager.pending_per_plan.get(&plan_download), Some(&2999));

        manager.running.insert(
            TransferId(1),
            RunningTransfer {
                plan_id: plan_download,
                cancel: CancellationToken::new(),
            },
        );
        manager.running_per_plan.insert(plan_download, 1);

        // While root is running, children must be blocked
        assert_eq!(manager.next_queued_position(), None);

        // Root finishes
        manager.running.remove(&TransferId(1));
        manager.running_per_plan.insert(plan_download, 0);

        // Children can now start
        for i in 0..MAX_CONCURRENT_PER_PLAN {
            assert_eq!(
                manager.next_queued_position(),
                Some(0),
                "child {i} should be immediately dispatched from the front"
            );
            let child = manager.remove_pending(0).expect("child should be queued");
            manager.running.insert(
                child.job.id,
                RunningTransfer {
                    plan_id: plan_download,
                    cancel: CancellationToken::new(),
                },
            );
            *manager.running_per_plan.entry(plan_download).or_insert(0) += 1;
        }

        // Plan 1 is now at MAX_CONCURRENT_PER_PLAN cap
        assert_eq!(manager.next_queued_position(), None);

        // Enqueue Plan 2 (upload with 500 files)
        let plan_upload = TransferPlanId(20);
        let upload_jobs: Vec<TransferJob> = (4000..4500)
            .map(|id| dummy_job(id, TransferDirection::Upload))
            .collect();
        manager
            .enqueue_for_test(connection.clone(), plan_upload, upload_jobs, true)
            .await;

        // Plan 2 has 0 running jobs and is prioritized by Phase 1
        let pos_plan_2 = manager
            .next_queued_position()
            .expect("idle plan 2 should be scheduled");
        let item_plan_2 = &manager.pending[pos_plan_2];
        assert_eq!(item_plan_2.plan_id, plan_upload);
        assert_eq!(item_plan_2.job.id, TransferId(4000));
    }
}
