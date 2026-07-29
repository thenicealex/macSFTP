# Transfer Terminal Lifecycle Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Guarantee that every transfer child published to the global store reaches a terminal state when planning later fails/cancels or when a completed plan cannot reach `TransferManager`.

**Architecture:** Split ownership at the existing event boundary. `TransferStore` already knows every child accepted through `TransferPlanProgress`, so `TransferPlanFailed` and `TransferPlanCancelled` terminalize those published children atomically with the plan/root. After `TransferPlanCompleted`, the runtime owns the returned jobs until `TransferManagerRequest::Enqueue` succeeds; any earlier handoff failure emits one non-retryable `TransferFailed` per child. Planning retry remains attached to the root command, while child rows only expose Retry when a real retry route exists.

**Tech Stack:** Rust, Tokio, flume bounded channels, `macsftp-core` reducers, GPUI transfer drawer, Cargo tests.

---

## Scope and invariants

- PR title: `Guarantee transfer terminal lifecycle`
- Primary defect: `SFTP-TRANSFER-001`
- Files expected to change:
  - `crates/core/src/core.rs`
  - `crates/sftp/src/runtime.rs`
  - `crates/sftp/src/transfer_planner.rs` tests only if a deterministic local partial-plan fixture is needed
  - `crates/sftp/src/session_actor.rs` tests only if a deterministic remote partial-plan fixture is needed
  - `crates/app/src/workspace/transfer_render.rs`
- Required invariant: once a child appears in `TransferPlanProgress`, it must end in exactly one terminal state: `Completed`, `Skipped`, or `Failed`.
- A plan terminal event must terminalize children already known to core even if the planner returns no `Vec<TransferJob>`.
- A successful planner return transfers ownership to runtime; accepted `TransferManagerRequest::Enqueue` transfers ownership to the manager.
- Do not invent a rich partial-planner result solely to repeat information already held by `TransferStore`.
- Do not register fake child retry routes. A retry button is valid only when runtime/manager can execute it.

### Task 1: Prove partial-plan failure and cancellation in the core reducer

**Files:**
- Modify/Test: `crates/core/src/core.rs:760-843, 1018-1040`

**Step 1: Add a failing partial-plan failure test**

Create a plan, publish two queued children with `TransferPlanProgress`, then apply:

```rust
AppEvent::TransferPlanFailed {
    plan_id,
    error: planning_error.clone(),
}
```

Name the test:

```rust
#[test]
fn transfer_plan_failure_terminalizes_every_published_child()
```

Assert:
- the plan is `TransferPlanState::Failed` with the original error;
- the root is `TransferState::Failed` with the original retryable flag;
- both children are `TransferState::Failed` with the same error and retryable flag;
- applying the same event again returns `false` and changes nothing.

**Step 2: Add a failing partial-plan cancellation test**

Name it:

```rust
#[test]
fn transfer_plan_cancellation_skips_every_published_child()
```

Publish two children, apply `TransferPlanCancelled`, and assert plan cancellation plus `Skipped` root and children. Reapply the event and assert idempotence.

**Step 3: Run the tests to verify the gap**

```bash
cargo test -p macsftp-core transfer_plan_failure_terminalizes_every_published_child -- --nocapture
cargo test -p macsftp-core transfer_plan_cancellation_skips_every_published_child -- --nocapture
```

Expected before implementation: FAIL because only the root becomes terminal while both children remain `Queued`.

**Step 4: Keep the red tests local**

Do not commit deliberately failing tests unless the project explicitly permits red commits.

### Task 2: Terminalize published children with the plan event

**Files:**
- Modify: `crates/core/src/core.rs:829-843, 1018-1040`
- Test: `crates/core/src/core.rs` transfer-store test module

**Step 1: Extend the existing helper rather than adding a new event**

Change the helper to apply the plan terminal state to the root and every child ID already recorded on the plan:

```rust
fn set_plan_terminal_state(
    &mut self,
    plan_id: TransferPlanId,
    plan_state: TransferPlanState,
    job_state: TransferState,
) -> bool {
    let Some(plan_index) = self.plans.iter().position(|plan| plan.id == plan_id) else {
        return false;
    };
    let root_job_id = self.plans[plan_index].root_job_id;
    let child_job_ids = self.plans[plan_index].child_jobs.clone();

    let mut changed = false;
    if self.plans[plan_index].state != plan_state {
        self.plans[plan_index].state = plan_state;
        changed = true;
    }
    changed |= self.set_job_state(root_job_id, job_state.clone());
    for child_job_id in child_job_ids {
        changed |= self.set_job_state(child_job_id, job_state.clone());
    }
    changed
}
```

Use the existing failure error for all published children. Cancellation maps all published children to `Skipped`. No child terminal event is emitted separately because the global reducer is already applying the authoritative plan terminal event once.

**Step 2: Preserve event-order safety**

Do not let a late `TransferPlanProgress` resurrect a terminal plan. Add or extend a test named:

```rust
#[test]
fn transfer_plan_progress_after_terminal_event_is_ignored()
```

Before inserting progress children, require the plan to still be `Planning`. This protects against a progress event delayed behind failure/cancellation.

**Step 3: Run focused tests**

```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
```

Expected: all transfer-plan reducer tests PASS, including failure, cancellation, late progress rejection, and idempotence.

**Step 4: Commit**

```bash
git add crates/core/src/core.rs
git commit -m "Terminalize children when transfer planning ends"
```

### Task 3: Prove both planners can fail after publishing progress

**Files:**
- Test: `crates/sftp/src/transfer_planner.rs` test module
- Test: `crates/sftp/src/session_actor.rs` test module or `crates/sftp/tests/real_session.rs`

**Step 1: Add a deterministic local partial-failure test**

Name it:

```rust
#[test]
fn local_upload_failure_after_progress_emits_plan_failure()
```

Use unique paths containing the test label and `std::process::id()`. Provide two sources in order:
1. a valid file that publishes the first child immediately;
2. a missing/invalid source that fails later.

Assert the event order contains:
- `TransferPlanProgress` with the first child ID;
- `TransferPlanFailed` for the same plan;
- planner result `None`.

Do not assert child terminal events from the planner; core owns terminalization of already-published children.

**Step 2: Add deterministic cancellation coverage**

Name it:

```rust
#[test]
fn local_upload_cancellation_after_progress_emits_plan_cancelled()
```

Use a test-only synchronization point only if existing planner seams cannot deterministically cancel after first progress. Prefer controlling the event receiver/cancellation token over adding production hooks.

**Step 3: Cover the remote planner**

Add:

```rust
async fn remote_download_failure_after_progress_emits_plan_failure()
```

Use the real sshd fixture if a deterministic unit seam is unavailable. Publish at least one valid remote child, then make a later directory/source unreadable or absent. Assert progress precedes failure and the result is `None`. Add the cancellation counterpart when the existing cancellation token can be triggered deterministically.

**Step 4: Run planner tests**

```bash
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
```

Expected: PASS. These tests prove both producer paths exercise the core contract; they do not duplicate reducer assertions.

**Step 5: Commit**

```bash
git add crates/sftp/src/transfer_planner.rs crates/sftp/src/session_actor.rs crates/sftp/tests/real_session.rs
git commit -m "Test partial transfer planning termination"
```

Only add files that actually changed.

### Task 4: Compensate every post-planning handoff failure

**Files:**
- Modify/Test: `crates/sftp/src/runtime.rs:748-925`

**Step 1: Add a non-retryable handoff error**

A child rejected before manager ownership has no `RetryRoute`, so do not advertise a retry that cannot work:

```rust
fn transfer_handoff_error(detail: &'static str) -> UserFacingError {
    UserFacingError::new(
        ErrorCode::ChannelClosed,
        "Could not start transfer",
        detail,
    )
}
```

Keep details structural and path/credential-free.

**Step 2: Add one private compensation helper**

```rust
async fn fail_planned_jobs(
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
```

**Step 3: Replace the missing connection receiver return**

Replace:

```rust
let Some(connection_rx) = transfer_connection_rx else {
    return;
};
```

with compensation for all jobs. This covers missing/stale session, missing actor sender, and full/disconnected actor request queue because the current acquisition path collapses those cases to `None`.

**Step 4: Reuse the helper for a dropped connection responder**

Replace the existing inline failure loop with `fail_planned_jobs`.

**Step 5: Recover jobs from manager send failure**

Construct the request first and recover the rejected value from `SendError<T>`:

```rust
let request = TransferManagerRequest::Enqueue {
    connection,
    plan_id,
    jobs,
};
if let Err(send_error) = manager_tx.send_async(request).await {
    match send_error.0 {
        TransferManagerRequest::Enqueue { jobs, .. } => {
            fail_planned_jobs(
                &terminal_event_tx,
                jobs,
                transfer_handoff_error(
                    "The transfer service stopped before accepting the planned work. Start the transfer again.",
                ),
            )
            .await;
        }
        TransferManagerRequest::Cancel { .. }
        | TransferManagerRequest::Retry { .. }
        | TransferManagerRequest::ResolveConflict { .. } => {}
    }
}
```

Do not use `unwrap`, `expect`, or `unreachable!` for channel failure.

**Step 6: Add deterministic handoff tests**

Extract only a tiny private `handoff_planned_jobs` helper if needed. Add two-job tests:
- `handoff_without_connection_receiver_fails_all_jobs`
- `handoff_with_dropped_connection_responder_fails_all_jobs`
- `handoff_with_closed_manager_fails_all_jobs`

Assert exact IDs, order, `ErrorCode::ChannelClosed`, and `retryable == false`.

**Step 7: Run focused tests**

```bash
cargo test -p macsftp-sftp handoff_ -- --nocapture
cargo test -p macsftp-sftp completed_upload_plan_without_session_fails_every_planned_job -- --nocapture
```

Expected: PASS; no test waits for an absent child terminal event.

**Step 8: Commit**

```bash
git add crates/sftp/src/runtime.rs
git commit -m "Fail jobs rejected during transfer handoff"
```

### Task 5: Make Retry visibility match actual ownership

**Files:**
- Modify/Test: `crates/app/src/workspace/transfer_render.rs:358-490`

**Step 1: Add failing rendering-policy tests**

Extract a pure helper if needed:

```rust
fn can_retry_transfer(job: &TransferJob) -> bool {
    matches!(job.state, TransferState::Failed { retryable: true, .. })
}
```

Add:
- `retry_action_is_hidden_for_non_retryable_failure`
- `retry_action_is_shown_for_retryable_failure`

**Step 2: Gate the callback by `retryable`**

Replace the current `matches!(job.state, TransferState::Failed { .. })` condition with `can_retry_transfer(job)`.

This is required for correctness: post-planning handoff compensation deliberately has no retry route. Do not add a route that silently restarts unrelated work.

**Step 3: Preserve planning retry through the root**

When all children were terminalized by `TransferPlanFailed`, keep the failed root visible and hide its children if every child carries the exact same failure as the root. This preserves the existing root-ID entry in `planning_retries`; ordinary execution failures continue showing individual child rows.

Add:
- `partial_planning_failure_keeps_retryable_root_visible`
- `execution_failure_shows_terminal_child_rows`

Use a small pure classification helper rather than embedding another large boolean expression in rendering.

**Step 4: Run focused tests**

```bash
cargo test -p macsftp-app retry_action_ -- --nocapture
cargo test -p macsftp-app partial_planning_failure_keeps_retryable_root_visible -- --nocapture
cargo test -p macsftp-app execution_failure_shows_terminal_child_rows -- --nocapture
```

Expected with complete Xcode tools: PASS. If local GPUI compilation is blocked by missing `metal`, record the environment blocker and require CI evidence.

**Step 5: Visual verification**

Verify the transfer drawer in a narrow pane and short window:
- a planning failure shows one failed root with Retry only when retryable;
- a handoff failure shows failed child rows without Retry;
- cancellation shows no Retry.

This changes visible actions. Attach a screenshot or record an explicit visual verification note.

**Step 6: Commit**

```bash
git add crates/app/src/workspace/transfer_render.rs
git commit -m "Show retry only for routable transfer failures"
```

### Task 6: Run package and repository verification

**Step 1: Run focused non-GPUI checks**

```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
cargo test -p macsftp-sftp handoff_ -- --nocapture
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
cargo fmt --all --check
```

**Step 2: Run app tests where the toolchain permits**

```bash
cargo test -p macsftp-app retry_action_ -- --nocapture
cargo test -p macsftp-app partial_planning_failure -- --nocapture
cargo test -p macsftp-app execution_failure -- --nocapture
```

**Step 3: Run repository gates**

```bash
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
bash scripts/check.sh
```

Expected with a complete Xcode installation: PASS. If `xcrun` cannot find `metal`, report that as an environment blocker and attach passing core/SFTP evidence; do not claim the full gate passed.

## Acceptance checklist

- Local and remote planning failure/cancellation cannot leave a published child queued.
- Late progress cannot add or resurrect children after a plan is terminal.
- Missing/stale session, missing/full/disconnected actor queue, dropped connection responder, and stopped manager terminalize every returned job.
- Planning failure retry continues through the root command; cancelled work is not retryable.
- Pre-manager child failures are non-retryable and render no dead Retry action.
- Accepted manager enqueue and successful transfer behavior remain unchanged.
- No new global timeout, watchdog, or duplicate partial-plan data model is added.
