# Detailed Code Implementation Plan — High-Severity macSFTP Defects

**Scope:** Concrete, code-level repair plan for the three high-severity defects found in the 2026-07-28 audit. Three independent PRs, TDD-first (red test → minimal fix → focused run → commit → gate). No product source is modified by this document; it is the implementation contract.

**Merge order / dependencies**
- **PR 1** — `Guarantee transfer terminal lifecycle` (`SFTP-TRANSFER-001`). No dependency. Merge first.
- **PR 2** — `Scope pooled host-key mismatches` (`CORE-SFTP-001`). No product-logic dependency on PR 1, but edits the same `AppEvent`/`remote_scope()` site in `core.rs`. Merge second.
- **PR 3** — `Check live remote metadata before edit upload` (`APP-EDIT-001`). **Depends on PR 1** (handoff failures must yield transfer terminal events). Logically independent of PR 2; only the `core.rs` textual overlap matters. Merge last.

**Global rules (from `AGENTS.md`)**
- No `unwrap`/`expect`/panic on recoverable errors; no `let _ =` on fallible sends.
- No fabricated `SessionId`; stale/remote scope validation lives in `core`.
- Symlinks handled as links (`symlink_metadata`, not `metadata`).
- App/GPUI tests are blocked locally by missing Xcode `metal`; they must run in CI. Real-sshd integration tests require the fixture server in CI; an explicit skip is not evidence.
- Each commit is one logical change; run the matching subset of `scripts/check.sh`.

---

## PR 1 — Guarantee transfer terminal lifecycle

**Defect:** Jobs published by transfer planning can remain non-terminal if (a) planning later fails/cancels after streaming children, or (b) a completed plan cannot reach `TransferManager`.

**Files**
- `crates/core/src/core.rs` — reducer terminalization
- `crates/sftp/src/runtime.rs` — post-planning handoff compensation
- `crates/sftp/src/transfer_planner.rs`, `crates/sftp/src/session_actor.rs` — producer tests
- `crates/app/src/workspace/transfer_render.rs` — retry visibility

**Ownership split**
- Core already tracks every child published via `TransferPlanProgress` → `TransferPlanFailed`/`TransferPlanCancelled` terminalize those children atomically with the plan/root.
- After `TransferPlanCompleted`, runtime owns returned jobs until `TransferManagerRequest::Enqueue` succeeds; any earlier handoff failure emits one non-retryable `TransferFailed` per child.
- Do **not** invent a rich partial-planner result or fake child retry routes.

### T1.1 — Red tests: partial-plan failure / cancellation
In `crates/core/src/core.rs` transfer-store test module:

```rust
#[test]
fn transfer_plan_failure_terminalizes_every_published_child() {
    // create plan; publish two queued children via TransferPlanProgress;
    // apply AppEvent::TransferPlanFailed { plan_id, error: planning_error.clone() };
    // assert: plan == Failed(original error);
    //         root == Failed(original retryable flag);
    //         both children == Failed(same error, same retryable flag);
    //         reapply event -> returns false, no change.
}

#[test]
fn transfer_plan_cancellation_skips_every_published_child() {
    // publish two children; apply TransferPlanCancelled;
    // assert plan Cancelled, root + children == Skipped; reapply -> idempotent.
}
```
Run:
```bash
cargo test -p macsftp-core transfer_plan_failure_terminalizes_every_published_child -- --nocapture
cargo test -p macsftp-core transfer_plan_cancellation_skips_every_published_child -- --nocapture
```
Expected **before** fix: FAIL (children stay `Queued`).

### T1.2 — Terminalize published children
Extend `set_plan_terminal_state` so it applies the plan terminal state to the root **and every recorded child**:

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
Add a guard so a late `TransferPlanProgress` cannot resurrect a terminal plan: require plan still `Planning` before inserting children, plus test `transfer_plan_progress_after_terminal_event_is_ignored`.

```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
```
Commit: `git commit -m "Terminalize children when transfer planning ends"`.

### T1.3 — Producer tests: both planners fail after progress
- `local_upload_failure_after_progress_emits_plan_failure` — two sources; first valid (publishes child immediately), second missing/invalid. Assert event order: `TransferPlanProgress(child)` → `TransferPlanFailed(plan)`; planner result `None`.
- `local_upload_cancellation_after_progress_emits_plan_cancelled`.
- `remote_download_failure_after_progress_emits_plan_failure` (real sshd if no unit seam).
Do **not** assert child terminal events from the planner — core owns that.

```bash
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
```

### T1.4 — Post-planning handoff compensation (`runtime.rs`)
A child rejected before manager ownership has no `RetryRoute`, so mark it non-retryable:

```rust
fn transfer_handoff_error(detail: &'static str) -> UserFacingError {
    UserFacingError::new(ErrorCode::ChannelClosed, "Could not start transfer", detail)
}

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
- Replace the missing-connection-receiver `return` with `fail_planned_jobs(...)`.
- Reuse helper for the dropped connection responder.
- Recover jobs from manager send failure:

```rust
let request = TransferManagerRequest::Enqueue { connection, plan_id, jobs };
if let Err(send_error) = manager_tx.send_async(request).await {
    match send_error.0 {
        TransferManagerRequest::Enqueue { jobs, .. } => {
            fail_planned_jobs(
                &terminal_event_tx,
                jobs,
                transfer_handoff_error(
                    "The transfer service stopped before accepting the planned work. Start the transfer again.",
                ),
            ).await;
        }
        TransferManagerRequest::Cancel { .. }
        | TransferManagerRequest::Retry { .. }
        | TransferManagerRequest::ResolveConflict { .. } => {}
    }
}
```
No `unwrap`/`unreachable!`. Tests: `handoff_without_connection_receiver_fails_all_jobs`, `handoff_with_dropped_connection_responder_fails_all_jobs`, `handoff_with_closed_manager_fails_all_jobs` (assert exact IDs, `ErrorCode::ChannelClosed`, `retryable == false`).
```bash
cargo test -p macsftp-sftp handoff_ -- --nocapture
```

### T1.5 — Retry visibility (`transfer_render.rs`)
Extract a pure helper and gate the action on real ownership:

```rust
fn can_retry_transfer(job: &TransferJob) -> bool {
    matches!(job.state, TransferState::Failed { retryable: true, .. })
}
```
Replace the current `matches!(job.state, TransferState::Failed { .. })` condition with `can_retry_transfer(job)`. Keep planning-failure retry on the root `planning_retries` entry; if every child carries the exact same failure as the root, hide children and keep the failed root visible. Tests: `retry_action_is_hidden_for_non_retryable_failure`, `retry_action_is_shown_for_retryable_failure`, `partial_planning_failure_keeps_retryable_root_visible`, `execution_failure_shows_terminal_child_rows`. Visual-verify the drawer in a narrow pane.

### T1.6 — Gates
```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
cargo test -p macsftp-sftp handoff_ -- --nocapture
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
```

**PR 1 acceptance:** no published child can stay `Queued`; late progress can't resurrect a terminal plan; all handoff failure modes terminalize every returned job; planning retry survives on the root; pre-manager failures are non-retryable and show no dead Retry.

---

## PR 2 — Scope pooled host-key mismatches to logical sessions

**Defect:** `HostKeyMismatch` carries only `tab_id`; a mismatch from a pooled physical handshake can be scoped to the wrong (first) logical session, and other waiters get no event.

**Files**
- `crates/core/src/core.rs` — event payload + `remote_scope()`
- `crates/sftp/src/physical_connection.rs` — return scope-free details
- `crates/sftp/src/pool.rs` — emit one scoped event per logical waiter
- `crates/sftp/src/runtime.rs` — translate pooled failure per logical scope
- `crates/app/src/workspace/event_handling.rs`, `crates/app/src/workspace/tests.rs` — GPUI stale guard
- `crates/sftp/tests/real_session.rs` — single + two-waiter sshd tests

**Invariant:** one physical handshake, N logical attempts → N scoped `HostKeyMismatch`; mismatch always hard-blocks, never retryable, no override; stale mismatch cannot touch a replacement session.

### T2.1 — Red tests: core stale gap
```rust
#[test]
fn app_state_rejects_host_key_mismatch_from_old_session() {
    let mut state = AppState::new();
    state.tabs.open_tab(connected_tab(1, 11, 2));
    let event = AppEvent::HostKeyMismatch(HostKeyMismatch {
        scope: RemoteEventScope::new(TabId(1), SessionId(10), 1), // old epoch
        host: "example.com".into(), port: 22,
        expected_fingerprint_sha256: Some("SHA256:expected".into()),
        actual_fingerprint_sha256: "SHA256:actual".into(),
    });
    assert!(!state.should_accept_event(&event));
}
#[test]
fn app_state_accepts_host_key_mismatch_from_current_session() { /* scope epoch == 2 */ }
```
Fails to compile initially (no `scope` field).

### T2.2 — Authoritative scope on the event
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyMismatch {
    pub scope: RemoteEventScope,
    pub host: String,
    pub port: u16,
    pub expected_fingerprint_sha256: Option<String>,
    pub actual_fingerprint_sha256: String,
}
```
Expose via central guard:
```rust
Self::HostKeyMismatch(mismatch) => Some(mismatch.scope.clone()),
```
Add `remote_scope_extracts_from_host_key_mismatch`. Commit core.

### T2.3 — Physical handshake returns details, not an event
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyMismatchDetails {
    pub host: String, pub port: u16,
    pub expected_fingerprint_sha256: Option<String>,
    pub actual_fingerprint_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectFailure {
    HostKeyMismatch(HostKeyMismatchDetails),
    TrustRejected, TrustTimeout,
    AuthFailed(AuthFailure), Connection(UserFacingError),
}
```
In `check_server_key`, compute fingerprints once, record `HostKeyRejection::Mismatch(details)`, return `Ok(false)`; **remove** the `event_tx.send_async(HostKeyMismatch)` branch. Keep structural logging (no fingerprints). Add helper + test:
```rust
pub fn host_key_mismatch_event(scope: RemoteEventScope, details: HostKeyMismatchDetails) -> AppEvent {
    AppEvent::HostKeyMismatch(HostKeyMismatch {
        scope, host: details.host, port: details.port,
        expected_fingerprint_sha256: details.expected_fingerprint_sha256,
        actual_fingerprint_sha256: details.actual_fingerprint_sha256,
    })
}
```

### T2.4 — Runtime emits one per logical attempt
Replace both `ConnectFailure::HostKeyMismatch => None` branches in `runtime.rs`:
```rust
ConnectFailure::HostKeyMismatch(details) =>
    Some(host_key_mismatch_event(scope.clone(), details)),
```
Replace silent `let _ =` send with a structural `warn!` (no fingerprints). Tests: `pooled_mismatch_failure_uses_first_logical_scope`, `pooled_mismatch_failure_uses_second_logical_scope`.

### T2.5 — Pooled two-waiter test (deterministic, no barrier)
`get_or_connect` inserts `PoolEntry::Connecting(rx.resubscribe())` **synchronously** at `pool.rs:124-125` before spawning the handshake, so two back-to-back `connect_session` calls deterministically share one in-progress key. In `connect_session`, replace `ConnectFailure::HostKeyMismatch => {}` with an explicit scoped send (keep `result = Err(...)`); `get_or_connect` itself emits no logical event.
```rust
#[tokio::test(flavor = "multi_thread")]
async fn pooled_host_key_mismatch_is_emitted_for_every_logical_session() {
    // one ConnectionManager, one shared ConnectionPoolIdentity::Saved(...) so both calls share ConnectionKey;
    // call connect_session twice synchronously (scopes (TAB,SESSION,EPOCH) and (SECOND_TAB,SECOND_SESSION,EPOCH));
    // assert: both results Err(HostKeyMismatch(_)); exactly two app mismatch events;
    //         scopes == the two logical scopes; identical host/port/fingerprint; no third event; connection blocked.
}
```
Also strengthen `host_key_mismatch_blocks_connection` to assert exact scope. Run with sshd:
```bash
cargo test -p macsftp-sftp --test real_session host_key_mismatch_blocks_connection -- --exact --nocapture
cargo test -p macsftp-sftp --test real_session pooled_host_key_mismatch_is_emitted_for_every_logical_session -- --exact --nocapture
```

### T2.6 — GPUI stale rejection
Consumer uses `mismatch.scope.tab_id` after the central guard. Add `stale_host_key_mismatch_does_not_fail_replacement_session` (connect s1/e1, establish s2/e2 in same tab, deliver old mismatch → replacement stays connected, no modal) and `current_host_key_mismatch_hard_blocks_without_retry_or_trust_action`.

### T2.7 — Security gates
```bash
cargo test -p macsftp-core host_key_mismatch -- --nocapture
cargo test -p macsftp-sftp pooled_mismatch_failure_ -- --nocapture
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
bash scripts/check_sensitive_logs.sh   # no fingerprints in logs
bash scripts/check_architecture.sh
```

**PR 2 acceptance:** `HostKeyMismatch` has one authoritative `RemoteEventScope`; `ConnectFailure::HostKeyMismatch` carries details but no scope; physical code emits no first-caller-only event; two waiters each get exactly one scoped event; core accepts current / rejects stale; current mismatch remains a hard non-retryable block.

---

## PR 3 — Check live remote metadata before edit upload

**Depends on PR 1.** **Defect:** the edit watcher authorizes overwrite from cached UI listing metadata, so a concurrent external remote change can be silently overwritten.

**Files**
- `crates/core/src/core.rs` — protocol + `EditPhase::CheckingRemote` + `EditCheckId`
- `crates/app/src/edit_watcher.rs` — dispatch check instead of cached compare
- `crates/app/src/event_coordinator.rs` — sole owner of check results
- `crates/app/src/workspace/mod.rs` — `accepts_remote_scope` query + reconnect reset
- `crates/app/src/workspace/remote_edit.rs` — exhaustive phase matches
- `crates/sftp/src/runtime.rs`, `crates/sftp/src/session_actor.rs` — command routing + `symlink_metadata`
- `crates/sftp/tests/real_session.rs` — live metadata tests
- `docs/gpui-russh-plan.md` — document protocol + same-second limitation

**Deliberate limitation (resolved in review):** byte-freeze is **not** added. The mtime correlation already rejects superseded saves; freezing stale local bytes would be a data regress; the residual check-then-write (same size + same second) is the documented accepted limitation. Use `symlink_metadata`, not `metadata`.

### T3.1 — Protocol + state in core
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditCheckId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRemoteEditSnapshotCommand {
    pub tab_id: TabId, pub session_epoch: u64,
    pub edit_session_id: EditSessionId, pub check_id: EditCheckId, pub path: RemotePath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotChecked {
    pub edit_session_id: EditSessionId, pub check_id: EditCheckId,
    pub path: RemotePath, pub snapshot: RemoteSnapshot,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotCheckFailed {
    pub edit_session_id: EditSessionId, pub check_id: EditCheckId,
    pub path: RemotePath, pub error: UserFacingError,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotDispatchFailed {
    pub tab_id: TabId, pub session_epoch: u64,
    pub edit_session_id: EditSessionId, pub check_id: EditCheckId,
    pub path: RemotePath, pub error: UserFacingError,   // no SessionId
}

AppCommand::CheckRemoteEditSnapshot(CheckRemoteEditSnapshotCommand)
AppEvent::RemoteEditSnapshotChecked(RemoteScoped<RemoteEditSnapshotChecked>)
AppEvent::RemoteEditSnapshotCheckFailed(RemoteScoped<RemoteEditSnapshotCheckFailed>)
AppEvent::RemoteEditSnapshotDispatchFailed(RemoteEditSnapshotDispatchFailed)
```
Only the two actor outcomes are exposed in `remote_scope()`; all three are excluded from `is_transfer_event()`. Add `next_check_id: u64` to `EditSessionStore` (UI/core is sole allocator). Add phase and pending correlation:
```rust
pub enum EditPhase { Downloading, Editing, CheckingRemote, UploadingBack, RemoteConflict }
// on EditSession:
pub pending_check_id: Option<EditCheckId>,
pub checking_local_mtime: Option<Timestamp>,
```
Search `rg -n "EditSession \{" crates/core crates/app` and initialize both fields in every literal/fixture. Extend `update_epoch_for_tab`:
```rust
if session.phase == EditPhase::CheckingRemote {
    session.phase = EditPhase::Editing;
    session.pending_check_id = None;
    session.checking_local_mtime = None;
}
session.session_epoch = session_epoch;
// do NOT reset UploadingBack (transfer-owned) or local_mtime (pre-save baseline)
```
Core tests: `remote_edit_snapshot_events_are_remote_scoped`, `store_find_active_treats_checking_remote_as_live`, `store_reconnect_resets_checking_remote_for_retry`.

### T3.2 — Runtime routing (non-blocking)
Actor request: `RemoteSessionRequest::CheckRemoteEditSnapshot { edit_session_id, check_id, path }` (no responder; actor owns `event_tx`). Runtime handles `AppCommand::CheckRemoteEditSnapshot` before `StartTransfer`:
1. look up `sessions[command.tab_id]`; 2. require `session.session_epoch == command.session_epoch`; 3. require a real actor sender; 4. `try_send` (GPUI stays non-blocking).
On missing/stale/full/closed → emit `RemoteEditSnapshotDispatchFailed` with the command's own tab/epoch/check/path (no fabricated scope):
```rust
async fn emit_remote_edit_dispatch_failure(
    event_tx: &flume::Sender<AppEvent>,
    command: CheckRemoteEditSnapshotCommand, detail: &'static str,
) {
    let error = UserFacingError::new(ErrorCode::ChannelClosed, "Could not check remote file", detail).with_retryable(true);
    if let Err(send_error) = event_tx.send_async(
        AppEvent::RemoteEditSnapshotDispatchFailed(RemoteEditSnapshotDispatchFailed {
            tab_id: command.tab_id, session_epoch: command.session_epoch,
            edit_session_id: command.edit_session_id, check_id: command.check_id,
            path: command.path, error,
        })).await
    { warn!(error = %send_error, "remote edit dispatch failure event dropped"); }
}
```
Tests: `remote_edit_check_missing_session_emits_failure`, `…_stale_epoch_…`, `…_full_actor_queue_…`, `…_disconnected_actor_queue_…` — all retryable, `remote_scope()` is `None`.

### T3.3 — Actor metadata
```rust
fn remote_snapshot_from_metadata(metadata: &russh_sftp::client::fs::Metadata) -> RemoteSnapshot {
    RemoteSnapshot {
        size: metadata.size,
        modified_at: metadata.mtime.map(|s| Timestamp::from_secs_since_epoch(s.into())),
    }
}
```
On request: `self.sftp.symlink_metadata(path.as_str()).await`; success → scoped `RemoteEditSnapshotChecked`; **any** error (incl. `NoSuchFile`) → scoped `RemoteEditSnapshotCheckFailed` (deletion is unsafe, not "unchanged"). Real-sshd tests with unique fixture paths: `remote_edit_snapshot_check_reports_live_metadata`, `remote_edit_snapshot_check_sees_external_change_without_relisting` (second writer changes size/mtime; no UI relist), `remote_edit_snapshot_check_missing_file_returns_failure`.

### T3.4 — Watcher dispatch
Delete `current_remote_snapshot()`; keep `tab_remote_is_ready()` as readiness guard only. On changed `Editing` session:
1. capture last + current mtime; 2. `check_id = edit_sessions.next_check_id()`; 3. set `CheckingRemote`, `pending_check_id = Some(check_id)`, `checking_local_mtime = current`, leave `local_mtime = last_mtime`; 4. send `CheckRemoteEditSnapshot`; 5. on channel-full/no-window → revert to `Editing`, clear pending fields, keep `local_mtime`. Include `CheckingRemote` in watcher cleanup iteration. Tests: `poll_dispatches_remote_check_before_upload`, `poll_does_not_dispatch_duplicate_check_while_checking`, `poll_reverts_to_editing_when_check_dispatch_fails`.

### T3.5 — Apply result exactly once (`event_coordinator.rs`)
Handle all three events in `AppEventCoordinator::dispatch_event` **before** window broadcast, then return (no per-window handling — edit sessions are process-global). For scoped events:
1. find owning workspace via `accepts_remote_scope`:
```rust
pub(crate) fn accepts_remote_scope(&self, scope: &RemoteEventScope) -> bool {
    self.state.tabs.accepts_remote_event(scope)
}
```
2. find global edit session by `edit_session_id`; 3. require `phase == CheckingRemote`; 4. scope `tab_id`/`session_epoch` match; 5. `path == session.remote_path`; 6. `check_id == session.pending_check_id`; 7. `checking_local_mtime.is_some()`.
For `RemoteEditSnapshotDispatchFailed`: same except no workspace remote-scope check (command never reached an actor); exact epoch/session/check/path tuple prevents an old failure resetting a replacement.

Success transition — re-stat temp file first:
- if stat fails or mtime ≠ `checking_local_mtime` → clear pending fields, `Editing`, keep baseline (newer save re-checked).
- if mtime matches and snapshot == baseline → one `StartTransfer` upload, `UploadingBack`, `local_mtime = checking_local_mtime`, clear pending fields.
- if mtime matches and snapshot differs → `RemoteConflict`, `local_mtime = checking_local_mtime`, clear pending fields, refresh conflict modal.

Failure transition (actor stat failure or dispatch failure) → `CheckingRemote → Editing`, clear pending fields, keep temp + baseline, status "Could not verify the remote file; save will retry". If `StartTransfer` can't enter the channel, revert `UploadingBack → Editing` keeping baseline.

Nine app tests (run each by name; `remote_check` substring misses two):
`matching_remote_check_dispatches_one_upload`, `diverged_remote_check_enters_conflict_without_upload`, `failed_remote_check_returns_to_editing_without_advancing_mtime`, `stale_remote_check_after_reconnect_is_ignored_and_session_retries`, `stale_dispatch_failure_after_reconnect_does_not_reset_replacement_check`, `remote_check_event_is_applied_once_with_multiple_windows`, `duplicate_remote_check_result_does_not_dispatch_second_upload`, `late_result_from_prior_check_id_does_not_authorize_retry`, `local_save_changed_during_remote_check_requires_a_new_check`.

### T3.6 — Docs + e2e
Document protocol + same-second limitation in `docs/gpui-russh-plan.md`. Manual verify: save-no-change → 1 check + 1 upload; external change → conflict, no overwrite; disconnect mid-check → retry; remote deletion → no blind recreate. Visual-verify conflict modal/status in light+dark, narrow pane, focus state.

**PR 3 acceptance:** cached listings never authorize overwrite; at most one in-flight check per save; only exact scope+session+check+path+mtime advances the session; equal metadata → one upload, different → conflict; all failures preserve local content and retry; stale results can't mutate a reconnected session; real sshd proves external change is detected without relist; same-size/same-second limit stays documented.

---

## Cross-cutting checklist before merge
- [ ] Each PR compiles + focused tests pass (`cargo test -p <crate> <substring>`).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`.
- [ ] `bash scripts/check_architecture.sh` and `bash scripts/check_sensitive_logs.sh` pass (no secrets/fingerprints in logs).
- [ ] App/GPUI tests recorded as CI-required where local `metal` is missing.
- [ ] Real-sshd integration tests run in CI; skips are not evidence.
- [ ] No fabricated `SessionId`, no silent fallible discards, no new global timeout/watchdog.
- [ ] Architecture changes documented in `docs/gpui-russh-plan.md` (PR 3).
