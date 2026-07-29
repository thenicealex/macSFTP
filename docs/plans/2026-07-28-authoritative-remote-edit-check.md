# Authoritative Remote-Edit Snapshot Check Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Check live remote metadata immediately before an automatic edit upload so a stale UI directory listing cannot authorize overwriting a concurrent remote change.

**Architecture:** Introduce an explicit command/request/event round trip for `symlink_metadata(remote_path)` on the connected browsing actor. UI/core allocates a stable `EditCheckId`; the edit session enters `CheckingRemote` and stores that ID with the exact local-save mtime before dispatch. Actor outcomes carry `EditSessionId + EditCheckId + RemoteEventScope`; runtime routing failures use a separate process-global tab/epoch/edit-session/check event because no legitimate `SessionId` exists when a session is missing. The process-wide coordinator is the sole result owner and only the exact pending check may advance to upload or conflict.

**Tech Stack:** Rust, GPUI app globals, Tokio/flume runtime bridge, russh-sftp metadata, real OpenSSH integration tests.

---

## Scope, ordering, and deliberate limitation

- PR title: `Check live remote metadata before edit upload`
- Primary defect: `APP-EDIT-001`
- Implement after `2026-07-28-transfer-terminal-events.md`; this PR relies on upload handoff failures producing transfer terminal events.
- It is logically independent of `2026-07-28-host-key-mismatch-scope.md`, but both PRs edit central `AppEvent`/`remote_scope()` definitions in `crates/core/src/core.rs`. If the mismatch PR merges first, rebase this PR and resolve that textual overlap without combining behavior.
- Expected files:
  - `crates/core/src/core.rs`
  - `crates/app/src/edit_watcher.rs`
  - `crates/app/src/event_coordinator.rs`
  - `crates/app/src/workspace/mod.rs` for a narrow `accepts_remote_scope` query and reconnect reset
  - `crates/app/src/workspace/remote_edit.rs` for exhaustive `EditPhase` matches/status text
  - `crates/app/src/workspace/tests.rs` and/or `crates/app/src/edit_watcher.rs` tests
  - `crates/sftp/src/runtime.rs`
  - `crates/sftp/src/session_actor.rs`
  - `crates/sftp/tests/real_session.rs`
  - `docs/gpui-russh-plan.md`
- Do not hash file contents in this PR. SFTP metadata is whole-second `(size, mtime)`, so same-size writes within the same second remain an accepted limitation documented in `docs/remote-editing-bug-analysis-2026-07-16.md`.
- Use `symlink_metadata`, not metadata that follows links; remote editing must continue treating a symlink as a link rather than silently changing file semantics.

## Rejected alternatives

1. **Actor `oneshot` response only:** avoids a core event, but bypasses the established app event pump/stale-event guard and leaves reconnect correlation ad hoc.
2. **Pending boolean while phase remains `Editing`:** fewer enum edits, but the watcher can redispatch checks unless every path inspects the marker; phase and pending state can disagree.
3. **Recommended explicit scoped event + `CheckingRemote`:** makes the in-flight operation visible in the state machine, prevents duplicate dispatch by construction, and reuses central stale filtering.

### Reviewer concern: byte-freeze is intentionally not a separate fix

During review, the question was raised whether the authorization-time mtime check actually "freezes" the bytes the upload worker later reads from `local_temp_path` (the live editor file, via `build_edit_upload_command` → `TransferEndpoint::Local(temp)`). It intentionally does not, and freezing them would be the worse choice. The real gap is already closed:

1. **Superseded-save result rejection.** When the check result arrives we re-stat the temp file and require `current_mtime == checking_local_mtime` (Task 5 Step 2). If the user saved again while the check was in flight, the current mtime no longer matches, so the result is rejected, the session reverts to `Editing`, and the watcher issues a fresh authoritative check for the newer save. This is exactly the stale-result case the `EditCheckId` correlation also guards; it does not require copying bytes.
2. **Latest local content wins (correct).** Once authorized and in `UploadingBack`, a *later* local save during the network handoff is correctly uploaded as the user's latest edit. Freezing a snapshot of the T1 bytes would instead push stale content if the user saved T2 — a data regress, not a fix.
3. **Residual check-then-write window is the accepted limitation.** The only remaining race is an external remote change that lands during the actual upload I/O, after the metadata check passed. That is the same fundamental check-then-write TOCTOU already documented as the same-size/same-second limitation; it cannot be closed without server-side conditional writes or content hashing, both explicitly out of scope for this PR (see Scope).
4. **Architecture.** Freezing would require the sftp transfer worker to be aware of edit-session mtime, violating the `sftp -> gpui` boundary in `AGENTS.md`. The authoritative check belongs in the runtime/actor, not in the worker's byte reader.

Conclusion: no new snapshot/freeze mechanism. Keep the mtime correlation, keep the residual limitation documented, and do not push byte-freeze into the transfer worker.

### Task 1: Define the remote-edit check protocol and state

**Files:**
- Modify: `crates/core/src/core.rs:1516-1667`
- Modify: `crates/core/src/core.rs:1972-2045`
- Test: `crates/core/src/core.rs:2669-2755`, `2949-2995`, `3614-3725`

**Step 1: Add the command payload**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditCheckId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRemoteEditSnapshotCommand {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
}
```

Add:

```rust
AppCommand::CheckRemoteEditSnapshot(CheckRemoteEditSnapshotCommand)
```

Add `next_check_id: u64` plus `next_check_id()` to `EditSessionStore`, following its existing `next_id()` allocation pattern. UI/core is the sole check-ID allocator; runtime and actor only echo it. The runtime obtains the authoritative `session_id` from its live `RemoteSessionHandle`; UI/core remains authoritative for `tab_id` and epoch.

**Step 2: Add result and failure payloads**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotChecked {
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub snapshot: RemoteSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotCheckFailed {
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub error: UserFacingError,
}
```

Add actor-outcome events plus a separate runtime-routing failure:

```rust
AppEvent::RemoteEditSnapshotChecked(RemoteScoped<RemoteEditSnapshotChecked>)
AppEvent::RemoteEditSnapshotCheckFailed(RemoteScoped<RemoteEditSnapshotCheckFailed>)
AppEvent::RemoteEditSnapshotDispatchFailed(RemoteEditSnapshotDispatchFailed)
```

Define the dispatch-failure payload without a `SessionId`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEditSnapshotDispatchFailed {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub edit_session_id: EditSessionId,
    pub check_id: EditCheckId,
    pub path: RemotePath,
    pub error: UserFacingError,
}
```

Expose only the two actor outcomes in `AppEvent::remote_scope()`. Keep all three out of `is_transfer_event()`. `RemoteEditSnapshotDispatchFailed` is not a remote-session event: the process-wide coordinator accepts it only when its tab, epoch, edit-session ID, path, phase, and local-save generation still match the stored edit session.

**Step 3: Add the explicit phase**

```rust
pub enum EditPhase {
    Downloading,
    Editing,
    CheckingRemote,
    UploadingBack,
    RemoteConflict,
}
```

Add pending-check correlation to the session:

```rust
pub pending_check_id: Option<EditCheckId>,
pub checking_local_mtime: Option<Timestamp>,
```

Both fields are `Some(...)` only in `CheckingRemote`; clear both on every transition out of that phase. Update comments, every `EditSession` struct literal/fixture across `crates/core` and `crates/app`, and exhaustive tests so `CheckingRemote` is treated as a live phase that blocks duplicate editing. Search before implementation with `rg -n "EditSession \{" crates/core crates/app` (or the repository search tool) and initialize both fields explicitly; do not rely on an unrelated `Default` implementation to hide missing fixture intent. Both fields are required: mtime binds the result to local bytes, while `EditCheckId` rejects a delayed result from an earlier retry of the same save.

**Step 4: Make reconnect recovery explicit**

Change `EditSessionStore::update_epoch_for_tab` so a session in `CheckingRemote` is reset to `Editing` while its epoch is updated. Preserve `local_mtime`: it must remain the pre-save baseline, causing the unchanged local save to be detected again after reconnect.

```rust
if session.phase == EditPhase::CheckingRemote {
    session.phase = EditPhase::Editing;
    session.pending_check_id = None;
    session.checking_local_mtime = None;
}
session.session_epoch = session_epoch;
```

Preserve `local_mtime`, which remains the pre-save watcher baseline. Do not reset `UploadingBack`; that phase is owned by the transfer lifecycle.

**Step 5: Write core tests first**

Add:
- `remote_edit_snapshot_events_are_remote_scoped`
- `store_find_active_treats_checking_remote_as_live`
- `store_reconnect_resets_checking_remote_for_retry`

**Step 6: Run core tests**

```bash
cargo test -p macsftp-core remote_edit_snapshot -- --nocapture
cargo test -p macsftp-core store_reconnect_resets_checking_remote_for_retry -- --nocapture
```

Expected before implementation: compile failures for missing types/variants. Expected after implementation: PASS.

**Step 7: Commit**

```bash
git add crates/core/src/core.rs
git commit -m "Model remote edit snapshot checks"
```

### Task 2: Route the command through the runtime without blocking GPUI

**Files:**
- Modify: `crates/sftp/src/runtime.rs:687-746`
- Modify: `crates/sftp/src/session_actor.rs:80-128`
- Test: `crates/sftp/src/runtime.rs` test module

**Step 1: Add the actor request**

```rust
RemoteSessionRequest::CheckRemoteEditSnapshot {
    edit_session_id: EditSessionId,
    check_id: EditCheckId,
    path: RemotePath,
}
```

No responder is needed; the actor owns `event_tx` and attaches its live scope. The actor's request branch awaits `symlink_metadata` and emits one success/failure before returning to its receive loop. If actor cancellation wins before a queued request is consumed, reconnect calls `update_epoch_for_tab`, which resets the pending `CheckingRemote` state for retry; do not add a speculative timeout/watchdog in this PR.

**Step 2: Add runtime routing**

Handle `AppCommand::CheckRemoteEditSnapshot(command)` before `StartTransfer`:

1. Look up `sessions[command.tab_id]`.
2. Require `session.session_epoch == command.session_epoch`.
3. Require a real actor request sender.
4. Call non-blocking `try_send`; GPUI remains non-blocking because it only sent the command to the bounded runtime bridge.

On missing/stale session, missing actor sender, or full/disconnected actor queue, emit `RemoteEditSnapshotDispatchFailed` immediately with the command's `tab_id`, `session_epoch`, `edit_session_id`, `check_id`, and path. Do not use the `RemoteSessionHandle.session_id` and do not fabricate a scope: this event reports that routing never reached a live actor, so it is intentionally epoch-correlated rather than remote-session-scoped.

Use one helper so all rejection branches have identical semantics:

```rust
async fn emit_remote_edit_dispatch_failure(
    event_tx: &flume::Sender<AppEvent>,
    command: CheckRemoteEditSnapshotCommand,
    detail: &'static str,
) {
    let error = UserFacingError::new(
        ErrorCode::ChannelClosed,
        "Could not check remote file",
        detail,
    )
    .with_retryable(true);
    if let Err(send_error) = event_tx
        .send_async(AppEvent::RemoteEditSnapshotDispatchFailed(
            RemoteEditSnapshotDispatchFailed {
                tab_id: command.tab_id,
                session_epoch: command.session_epoch,
                edit_session_id: command.edit_session_id,
                check_id: command.check_id,
                path: command.path,
                error,
            },
        ))
        .await
    {
        warn!(error = %send_error, "remote edit dispatch failure event dropped");
    }
}
```

Keep details structural and path-free. Never log `command.path`. If the app event receiver is closed there is no state consumer left; otherwise the implementation must satisfy this rule: no command-dispatch failure may leave the session in `CheckingRemote` once the event coordinator receives the failure.

**Step 3: Add runtime unit tests**

Add deterministic tests for:
- `remote_edit_check_missing_session_emits_failure`
- `remote_edit_check_stale_epoch_emits_failure`
- `remote_edit_check_full_actor_queue_emits_failure`
- `remote_edit_check_disconnected_actor_queue_emits_failure`

For every failure test, assert the event is `RemoteEditSnapshotDispatchFailed`, its tab/epoch/edit-session/check/path exactly match the command, `error.retryable` is true, and `event.remote_scope()` is `None`. This is the chosen protocol, not a fallback. Do not create a fake `SessionId`.

**Step 4: Run runtime tests**

```bash
cargo test -p macsftp-sftp remote_edit_check_ -- --nocapture
```

Expected: all PASS and each failure is retryable.

**Step 5: Commit**

```bash
git add crates/sftp/src/runtime.rs crates/sftp/src/session_actor.rs
git commit -m "Route remote edit snapshot checks"
```

### Task 3: Implement authoritative actor metadata events

**Files:**
- Modify: `crates/sftp/src/session_actor.rs:499-645`
- Test: `crates/sftp/tests/real_session.rs`

**Step 1: Add metadata conversion helper**

```rust
fn remote_snapshot_from_metadata(
    metadata: &russh_sftp::client::fs::Metadata,
) -> RemoteSnapshot {
    RemoteSnapshot {
        size: metadata.size,
        modified_at: metadata
            .mtime
            .map(|seconds| Timestamp::from_secs_since_epoch(seconds.into())),
    }
}
```

Reuse this helper where practical, but do not perform unrelated refactoring.

**Step 2: Handle the actor request**

Call:

```rust
self.sftp.symlink_metadata(path.as_str()).await
```

On success emit:

```rust
AppEvent::RemoteEditSnapshotChecked(RemoteScoped::new(
    scope.clone(),
    RemoteEditSnapshotChecked {
        edit_session_id,
        check_id,
        path,
        snapshot,
    },
))
```

On `NoSuchFile` and all other errors emit `RemoteEditSnapshotCheckFailed`; deletion is indeterminate/unsafe for overwrite, not “unchanged.” Use a user-facing retryable error such as “Could not check remote file” and retain the local edit.

**Step 3: Write real sshd tests**

Add unique fixture paths and these tests:

1. `remote_edit_snapshot_check_reports_live_metadata`
   - create file;
   - consume `TabConnected`;
   - send actor request with `EditSessionId(7)` and `EditCheckId(9)`;
   - assert scope, both IDs, path, size, and mtime.

2. `remote_edit_snapshot_check_sees_external_change_without_relisting`
   - create baseline file and record initial `(size, mtime)`;
   - start actor, but do not request a directory relist;
   - modify the server-side fixture directly (the test’s second writer), ensuring size differs or mtime advances to avoid the known same-second/same-size limitation;
   - send the check request;
   - assert returned snapshot differs from baseline.

3. `remote_edit_snapshot_check_missing_file_returns_failure`
   - remove the file before check;
   - assert scoped failure with matching edit-session ID, check ID, and path.

**Step 4: Run real integration tests**

```bash
cargo test -p macsftp-sftp --test real_session remote_edit_snapshot_check -- --nocapture
```

Expected with sshd: PASS. Without sshd: explicit skips; CI must execute these tests with the real server.

**Step 5: Commit**

```bash
git add crates/sftp/src/session_actor.rs crates/sftp/tests/real_session.rs
git commit -m "Read live metadata for remote edits"
```

### Task 4: Change the watcher from cached comparison to check dispatch

**Files:**
- Modify: `crates/app/src/edit_watcher.rs:54-264`
- Test: `crates/app/src/edit_watcher.rs` test module

**Step 1: Replace the old watcher expectation with failing tests**

Update/add tests proving:

1. `poll_dispatches_remote_check_before_upload`
   - changed local file + ready connected tab;
   - after one poll, phase is `CheckingRemote`;
   - command is `CheckRemoteEditSnapshot`, not `StartTransfer`.

2. `poll_does_not_dispatch_duplicate_check_while_checking`
   - poll twice without a result;
   - only one check command exists.

3. `poll_reverts_to_editing_when_check_dispatch_fails`
   - full command channel or no owning window;
   - phase returns to `Editing` and old `local_mtime` remains.

**Step 2: Remove cached listing authorization**

Delete `current_remote_snapshot()` from `edit_watcher.rs`. Keep `tab_remote_is_ready()` as a readiness guard to avoid dispatching while disconnected/reconnecting/refreshing; it is no longer evidence that the file is unchanged.

**Step 3: Dispatch the check**

For a changed `Editing` session:

1. Capture `last_mtime` and current mtime.
2. Allocate `check_id = edit_sessions.next_check_id()`.
3. Set phase to `CheckingRemote`, set `pending_check_id = Some(check_id)` and `checking_local_mtime = current`, and leave `local_mtime = last_mtime`.
4. Send `AppCommand::CheckRemoteEditSnapshot` with that `check_id` through the owning window’s runtime client.
5. On channel full/closed/no owning window, revert the phase to `Editing`, clear both pending-check fields, and do not advance `local_mtime`.

Generalize `dispatch_edit_command`/rollback naming if needed, but keep it private and avoid a cross-layer abstraction.

**Step 4: Ensure lifecycle cleanup includes `CheckingRemote`**

The candidate iterator currently chains `editing_sessions()` and `conflict_sessions()`. Add a store iterator for watcher-cleanup phases or include `CheckingRemote` so missing temp files are still reaped. After successful metadata reads, only `Editing` initiates new checks.

**Step 5: Run watcher tests**

```bash
cargo test -p macsftp-app poll_dispatches_remote_check_before_upload -- --nocapture
cargo test -p macsftp-app poll_does_not_dispatch_duplicate_check_while_checking -- --nocapture
cargo test -p macsftp-app poll_reverts_to_editing_when_check_dispatch_fails -- --nocapture
```

Expected with full Xcode tools: PASS. If local GPUI compilation is blocked by missing `metal`, record and defer these exact tests to CI.

**Step 6: Commit**

```bash
git add crates/app/src/edit_watcher.rs crates/core/src/core.rs
git commit -m "Check remote state before edit upload"
```

### Task 5: Apply check results exactly once

**Files:**
- Modify: `crates/app/src/event_coordinator.rs:79-114`
- Modify: `crates/app/src/workspace/mod.rs:688-709` for an owner-scoped stale-check query
- Modify: `crates/app/src/edit_watcher.rs` for upload helper reuse
- Test: `crates/app/src/event_coordinator.rs` test module

**Step 1: Make the process-wide coordinator the sole owner**

Handle all three check events in `AppEventCoordinator::dispatch_event` before ordinary window broadcasting, then return. Do not add them to `Workspace::handle_app_event`; edit sessions are process-global and broadcasting would let multiple windows race to apply one result.

For `RemoteEditSnapshotChecked` and scoped `RemoteEditSnapshotCheckFailed`:

1. Find the workspace that owns `scope.tab_id`.
2. Add and call a narrow workspace query:

```rust
pub(crate) fn accepts_remote_scope(&self, scope: &RemoteEventScope) -> bool {
    self.state.tabs.accepts_remote_event(scope)
}
```

If no owning workspace accepts the scope, drop the event as stale. Stop after the first owning workspace because `TabId` is process-allocated and authoritative.
3. Find the global edit session by `payload.edit_session_id`.
4. Require `phase == CheckingRemote`.
5. Require scope `tab_id` and `session_epoch` match the edit session.
6. Require payload path equals `session.remote_path`.
7. Require payload `check_id == session.pending_check_id`.
8. Require `checking_local_mtime.is_some()`.

For `RemoteEditSnapshotDispatchFailed`:

1. Find the global edit session by `edit_session_id`.
2. Require `phase == CheckingRemote`.
3. Require payload `tab_id`, `session_epoch`, and path exactly match the session.
4. Require payload `check_id == session.pending_check_id`.
5. Require `checking_local_mtime.is_some()`.
6. No workspace remote-scope check is required because the command never reached an actor; the exact epoch/edit-session/check/path tuple prevents an old dispatch failure from resetting a replacement check.

Extract a private `apply_remote_edit_check_event` helper if needed so tests can call this process-global path directly. Every accepted branch refreshes or updates only the owning window for status/modal presentation, then `dispatch_event` returns without broadcasting.

**Step 2: Define success transitions**

Before comparing remote snapshots, re-stat the temp file and compare its current mtime with `checking_local_mtime`:

- If stat fails or the mtime differs, clear both pending-check fields, return to `Editing`, keep the old `local_mtime` baseline, and dispatch no upload/conflict. The watcher will check the newer save again.
- If the mtime still matches, apply the remote result:
  - Returned snapshot equals baseline:
    1. build the edit `StartTransfer` upload command;
    2. set phase `UploadingBack`;
    3. set `local_mtime = checking_local_mtime`;
    4. clear both pending-check fields;
    5. dispatch exactly one upload.
  - Returned snapshot differs:
    1. set phase `RemoteConflict`;
    2. set `local_mtime = checking_local_mtime` so the same save does not re-flag;
    3. clear both pending-check fields;
    4. refresh the owning window to show the existing conflict modal.

Never authorize an upload using a mtime read only after the remote response; it would bind the result to whatever happens to be on disk then, not to the save that initiated the check.

**Step 3: Define failure transitions**

For actor stat failure or runtime dispatch failure:
- return `CheckingRemote -> Editing`;
- clear both `pending_check_id` and `checking_local_mtime`;
- keep the temp file;
- keep the pre-save `local_mtime` baseline;
- surface a status message such as “Could not verify the remote file; save will retry” in the owning window; do not claim a conflict and do not upload.

**Step 4: Prevent upload dispatch stranding**

If `StartTransfer` cannot enter the app-to-runtime channel, revert `UploadingBack -> Editing` and preserve the old local baseline, using the same rollback behavior already used by `dispatch_edit_command`. If it enters runtime, PR 1 guarantees a terminal event for later handoff failures.

**Step 5: Add app state-machine tests**

Add:
- `matching_remote_check_dispatches_one_upload`
- `diverged_remote_check_enters_conflict_without_upload`
- `failed_remote_check_returns_to_editing_without_advancing_mtime`
- `stale_remote_check_after_reconnect_is_ignored_and_session_retries`
- `stale_dispatch_failure_after_reconnect_does_not_reset_replacement_check`
- `remote_check_event_is_applied_once_with_multiple_windows`
- `duplicate_remote_check_result_does_not_dispatch_second_upload`
- `late_result_from_prior_check_id_does_not_authorize_retry`
- `local_save_changed_during_remote_check_requires_a_new_check`

Test the exact command payload, phase, mtime behavior, and zero/one upload count.

**Step 6: Run every acceptance-critical app test**

Run all nine named tests from Step 5 explicitly. The `remote_check` substring only matches seven of them; `stale_dispatch_failure_after_reconnect_*` and `late_result_from_prior_check_id_*` are not selected by it, so list every name. Do not use bare `--exact` for module-qualified unit tests.

```bash
cargo test -p macsftp-app matching_remote_check_dispatches_one_upload -- --nocapture
cargo test -p macsftp-app diverged_remote_check_enters_conflict_without_upload -- --nocapture
cargo test -p macsftp-app failed_remote_check_returns_to_editing_without_advancing_mtime -- --nocapture
cargo test -p macsftp-app stale_remote_check_after_reconnect_is_ignored_and_session_retries -- --nocapture
cargo test -p macsftp-app stale_dispatch_failure_after_reconnect_does_not_reset_replacement_check -- --nocapture
cargo test -p macsftp-app remote_check_event_is_applied_once_with_multiple_windows -- --nocapture
cargo test -p macsftp-app duplicate_remote_check_result_does_not_dispatch_second_upload -- --nocapture
cargo test -p macsftp-app late_result_from_prior_check_id_does_not_authorize_retry -- --nocapture
cargo test -p macsftp-app local_save_changed_during_remote_check_requires_a_new_check -- --nocapture
```

Before committing, inspect Cargo's `running N tests` output and confirm all nine ran. If any name is not selected, run that exact name separately.

Expected with full Xcode tools: PASS; otherwise record missing `metal` and require CI evidence.

**Step 7: Commit**

```bash
git add crates/app/src/event_coordinator.rs crates/app/src/workspace/mod.rs crates/app/src/edit_watcher.rs crates/core/src/core.rs
git commit -m "Apply authoritative edit conflict checks"
```

### Task 6: Update architecture documentation and run end-to-end verification

**Files:**
- Modify: `docs/gpui-russh-plan.md`
- Verify: `docs/remote-editing-bug-analysis-2026-07-16.md`

**Step 1: Document the protocol and limitation**

In the remote-edit/runtime sections, record:
- local save enters `CheckingRemote`;
- runtime routes a bounded command to the live actor;
- UI/core allocates `EditCheckId`; actor echoes it with `EditSessionId` and scope;
- actor uses `symlink_metadata` and emits a scoped result;
- stale session scopes, stale epochs, and obsolete check IDs are ignored;
- failure preserves local edits and retries later;
- comparison remains `(size, whole-second mtime)`, so same-size/same-second writes are not detectable without hashing/versioning.

Do not rewrite the historical bug report; link to it if helpful.

**Step 2: Run focused non-GPUI checks**

```bash
cargo test -p macsftp-core remote_edit -- --nocapture
cargo test -p macsftp-sftp remote_edit_check_ -- --nocapture
cargo test -p macsftp-sftp --test real_session remote_edit_snapshot_check -- --nocapture
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
```

Expected: PASS or explicit real-sshd skip.

**Step 3: Run app checks where toolchain permits**

```bash
cargo test -p macsftp-app remote_check -- --nocapture
```

Expected: PASS with Xcode Metal tools. Current known blocker is `xcrun: unable to find utility "metal"`; do not misclassify it as a test assertion failure.

**Step 4: Run repository gates**

```bash
cargo fmt --all --check
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
bash scripts/check.sh
```

Expected with complete local dependencies: PASS.

**Step 5: Manual behavior verification**

1. Open a remote file for editing.
2. Save locally with no remote change; observe one check followed by one upload and return to `Editing`.
3. Open the same remote file through a second SFTP client, modify it with different size or a later mtime, then save locally in macSFTP; observe `RemoteConflict` and verify remote content was not overwritten.
4. Disconnect while a check is in flight, reconnect, and save again; verify the session retries rather than becoming stuck.
5. Delete the remote file before local save; verify the app preserves the local temp file and presents conflict/failure instead of recreating blindly.

This changes state flow but does not require new visible controls. Verify light and dark themes, narrow pane, short window, focus state, and keyboard access for the existing conflict modal/status path. Provide a visual verification note; because the planned failure status text is user-visible, attach a screenshot unless implementation reuses an unchanged existing message.

**Step 6: Commit documentation**

```bash
git add docs/gpui-russh-plan.md
git commit -m "Document remote edit snapshot validation"
```

## Acceptance checklist

- Cached UI listings are never used to authorize automatic overwrite.
- Every local save dispatches at most one in-flight remote check.
- Only an exact live actor scope + edit-session ID + edit-check ID + path + local-save mtime can advance the session; runtime routing failures may only reset the exact pending tab/epoch/session/check/path tuple.
- Equal live metadata dispatches exactly one upload; different metadata dispatches none and shows conflict.
- Dispatch/stat/reconnect failures preserve local content and return to a retryable state.
- A stale result cannot mutate a reconnected session.
- Real sshd tests prove actor metadata reflects a second writer without a UI relist.
- The same-size/same-second limitation remains clearly documented rather than falsely claimed fixed.
