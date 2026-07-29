# Scoped Host-Key Mismatch Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Deliver one host-key mismatch event for every logical connection attempt, scoped to that attempt, even when several sessions wait on the same pooled physical handshake.

**Architecture:** The physical SSH handler records immutable mismatch details but no longer publishes a logical app event. `ConnectFailure::HostKeyMismatch` carries those details through the connection pool broadcast. Each logical caller—the runtime or `ConnectionManager::connect_session`—translates the failure into `AppEvent::HostKeyMismatch` using its own authoritative `RemoteEventScope`. Core exposes that scope to the existing stale-event guard, so current mismatches hard-block while stale mismatches cannot affect replacement sessions.

**Tech Stack:** Rust, Tokio broadcast channels, `macsftp-core` stale-event guard, GPUI workspace tests, russh/OpenSSH integration tests.

---

## Scope and invariants

- PR title: `Scope pooled host-key mismatches to logical sessions`
- Primary defect: `CORE-SFTP-001`
- Files expected to change:
  - `crates/core/src/core.rs`
  - `crates/sftp/src/physical_connection.rs`
  - `crates/sftp/src/pool.rs`
  - `crates/sftp/src/runtime.rs`
  - `crates/app/src/workspace/event_handling.rs`
  - `crates/app/src/workspace/tests.rs`
  - `crates/sftp/tests/real_session.rs`
- Required invariants:
  1. A matching host-key mismatch always blocks the connection and remains non-retryable in UI.
  2. Every logical connection attempt receives exactly one mismatch event with its own `(tab_id, session_id, session_epoch)`.
  3. A stale mismatch never mutates a replacement session.
  4. Fingerprint details are calculated once by the physical handshake and cloned through the pooled failure; logical callers do not reread `known_hosts`.
- Do not add an override action, one-click known-host replacement, or a new session-ID allocator.
- Do not attach the first caller’s scope to a pooled failure shared by later logical callers.

### Task 1: Prove the core stale-event gap

**Files:**
- Modify/Test: `crates/core/src/core.rs:1630-1728, 1843-1849`

**Step 1: Add a failing stale mismatch test**

```rust
#[test]
fn app_state_rejects_host_key_mismatch_from_old_session() {
    let mut state = AppState::new();
    state.tabs.open_tab(connected_tab(1, 11, 2));

    let event = AppEvent::HostKeyMismatch(HostKeyMismatch {
        scope: RemoteEventScope::new(TabId(1), SessionId(10), 1),
        host: "example.com".to_string(),
        port: 22,
        expected_fingerprint_sha256: Some("SHA256:expected".to_string()),
        actual_fingerprint_sha256: "SHA256:actual".to_string(),
    });

    assert!(!state.should_accept_event(&event));
}
```

This initially fails to compile because `HostKeyMismatch` has `tab_id`, not `scope`.

**Step 2: Add the live companion test**

Add `app_state_accepts_host_key_mismatch_from_current_session` using scope `(TabId(1), SessionId(11), 2)`.

**Step 3: Run the red tests**

```bash
cargo test -p macsftp-core app_state_rejects_host_key_mismatch -- --nocapture
cargo test -p macsftp-core app_state_accepts_host_key_mismatch -- --nocapture
```

Expected before implementation: compile failure mentioning unknown field `scope`.

### Task 2: Add one authoritative scope to the core event

**Files:**
- Modify/Test: `crates/core/src/core.rs:1630-1728, 1843-1849, 2973-3015`

**Step 1: Change the payload**

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

Remove `tab_id`; do not duplicate identity values that can drift.

**Step 2: Expose it through the central guard**

Add to `AppEvent::remote_scope()`:

```rust
Self::HostKeyMismatch(mismatch) => Some(mismatch.scope.clone()),
```

Update the method comment to name unknown-host prompts and mismatch events as security-sensitive stale-filtered events.

**Step 3: Add scope extraction coverage**

Add `remote_scope_extracts_from_host_key_mismatch`, asserting exact scope, `is_remote_scoped() == true`, and `is_transfer_event() == false`.

**Step 4: Run core tests**

```bash
cargo test -p macsftp-core host_key_mismatch -- --nocapture
cargo test -p macsftp-core remote_scope_extracts_from_host_key_mismatch -- --nocapture
```

Expected: PASS.

**Step 5: Commit**

```bash
git add crates/core/src/core.rs
git commit -m "Scope host key mismatches to sessions"
```

### Task 3: Return mismatch details from the physical handshake

**Files:**
- Modify/Test: `crates/sftp/src/physical_connection.rs:25-39, 105-128, 516-580`

**Step 1: Add a scope-free physical result**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyMismatchDetails {
    pub host: String,
    pub port: u16,
    pub expected_fingerprint_sha256: Option<String>,
    pub actual_fingerprint_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyRejection {
    Mismatch(HostKeyMismatchDetails),
    UserRejected,
    PromptTimeout,
}

#[derive(Debug, Clone)]
pub enum ConnectFailure {
    HostKeyMismatch(HostKeyMismatchDetails),
    TrustRejected,
    TrustTimeout,
    AuthFailed(AuthFailure),
    Connection(UserFacingError),
}
```

`HostKeyMismatchDetails` deliberately has no `RemoteEventScope`: one physical handshake result can be observed by multiple logical callers.

**Step 2: Stop sending the app event from `check_server_key`**

On mismatch:
1. calculate expected and actual fingerprints once;
2. record `HostKeyRejection::Mismatch(details)`;
3. return `Ok(false)`.

Remove the current `event_tx.send_async(AppEvent::HostKeyMismatch(...))` from this branch. Keep structural mismatch logging, including the physical initiator’s scope for diagnostics, but never log fingerprints.

**Step 3: Carry details through handshake failure**

Map:

```rust
Some(HostKeyRejection::Mismatch(details)) => {
    ConnectFailure::HostKeyMismatch(details)
}
```

Update `log_connect_failure` pattern matching to `ConnectFailure::HostKeyMismatch(_)`.

**Step 4: Add a pure event-construction helper**

Keep logical translation consistent:

```rust
pub fn host_key_mismatch_event(
    scope: RemoteEventScope,
    details: HostKeyMismatchDetails,
) -> AppEvent {
    AppEvent::HostKeyMismatch(HostKeyMismatch {
        scope,
        host: details.host,
        port: details.port,
        expected_fingerprint_sha256: details.expected_fingerprint_sha256,
        actual_fingerprint_sha256: details.actual_fingerprint_sha256,
    })
}
```

Add `host_key_mismatch_event_uses_logical_scope` as a unit test.

**Step 5: Run physical-connection tests**

```bash
cargo test -p macsftp-sftp host_key_mismatch_event_uses_logical_scope -- --nocapture
```

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/sftp/src/physical_connection.rs
git commit -m "Return host key mismatch details from handshake"
```

### Task 4: Emit one mismatch per runtime connection attempt

**Files:**
- Modify/Test: `crates/sftp/src/runtime.rs:480-610`

**Step 1: Translate pooled failure in both runtime branches**

Both failure paths currently map `ConnectFailure::HostKeyMismatch` to `None`. Replace them with:

```rust
ConnectFailure::HostKeyMismatch(details) => Some(
    crate::physical_connection::host_key_mismatch_event(
        scope.clone(),
        details,
    ),
),
```

The branches are:
- `Ok(Ok(shared_connection))` followed by SFTP channel setup failure handling;
- `Ok(Err(failure))` from the physical connection broadcast.

The first branch should not normally receive a host-key mismatch, but exhaustive handling keeps the contract total.

**Step 2: Handle event-send failure explicitly**

Replace silent `let _ = event_tx_clone.send_async(event).await` in the touched branches with a structural warning. Do not log fingerprints or credentials.

**Step 3: Extract a private failure-to-event helper if it removes duplication**

The helper must receive the logical `scope`; it must not read a scope from `ConnectFailure`.

**Step 4: Add deterministic runtime tests**

Add:
- `pooled_mismatch_failure_uses_first_logical_scope`
- `pooled_mismatch_failure_uses_second_logical_scope`

Use the same cloned details and two different scopes. Assert exactly one event per translation and exact fingerprint preservation.

**Step 5: Run focused runtime tests**

```bash
cargo test -p macsftp-sftp pooled_mismatch_failure_ -- --nocapture
```

Expected: PASS.

**Step 6: Commit**

```bash
git add crates/sftp/src/runtime.rs
git commit -m "Emit scoped mismatch events per runtime session"
```

### Task 5: Fix the pooled convenience path and prove multiple waiters

**Files:**
- Modify: `crates/sftp/src/pool.rs:218-363`
- Test: `crates/sftp/tests/real_session.rs:1371-1410`

**Step 1: Translate mismatch in `connect_session`**

Replace:

```rust
ConnectFailure::HostKeyMismatch => {}
```

with an explicit send of `host_key_mismatch_event(scope.clone(), details.clone())`. Handle send failure with a structural warning. The returned `result` remains `Err(ConnectFailure::HostKeyMismatch(details))`; emitting the event does not convert failure into success.

`get_or_connect` itself remains a physical pooling primitive and emits no logical mismatch event.

**Step 2: Strengthen the single-session real test**

In `host_key_mismatch_blocks_connection`, assert:

```rust
assert_eq!(
    mismatch.scope,
    RemoteEventScope::new(TAB, SESSION, EPOCH),
);
```

Retain fingerprint assertions and the hard-block/no-follow-up assertion.

**Step 3: Add a pooled two-waiter regression test**

Name it:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn pooled_host_key_mismatch_is_emitted_for_every_logical_session()
```

Feasibility is confirmed deterministic, not racy: `get_or_connect` inserts
`PoolEntry::Connecting(rx.resubscribe())` synchronously at `pool.rs:124-125`
*before* it spawns the physical handshake and before it returns `rx`, and the
function takes no `.await` between that insert and its `rx` return. So calling
`connect_session` twice back-to-back (the first future is polled only up to its
first `.await`) deterministically sees the second call hit the in-progress key
and `resubscribe`. No test-only barrier or production-only hook is required;
both spawned `connect_session` tasks merely `recv().await` the same broadcast
and each maps the single `ConnectFailure::HostKeyMismatch(details)` to its own
scoped event.

Arrange:
1. start the real sshd fixture with a wrong known-host key;
2. create one `ConnectionManager`;
3. use one shared `ConnectionPoolIdentity::Saved(AuthFingerprint::private_key(...))` so both calls share the same `ConnectionKey`;
4. call `connect_session` twice synchronously before awaiting either result, with scopes `(TAB, SESSION, EPOCH)` and `(SECOND_TAB, SECOND_SESSION, EPOCH)`;
5. collect both connection results and two mismatch events.

Assert:
- both connection results are `Err(ConnectFailure::HostKeyMismatch(_))`;
- exactly two app mismatch events arrive;
- the event scopes are the two logical scopes, irrespective of event order;
- both events contain identical host/port/fingerprint details;
- no third mismatch or success event arrives;
- the connection remains blocked.

This test specifically proves `PoolEntry::Connecting(rx).resubscribe()` does not collapse logical identity to the first caller.

**Step 4: Run real integration tests**

```bash
cargo test -p macsftp-sftp --test real_session host_key_mismatch_blocks_connection -- --exact --nocapture
cargo test -p macsftp-sftp --test real_session pooled_host_key_mismatch_is_emitted_for_every_logical_session -- --exact --nocapture
```

Expected with sshd: PASS. Without sshd: explicit skip. A skip is not runtime evidence; CI must run these tests with the fixture server.

**Step 5: Commit**

```bash
git add crates/sftp/src/pool.rs crates/sftp/tests/real_session.rs
git commit -m "Scope pooled mismatches to each logical session"
```

### Task 6: Reject stale mismatches in the GPUI workspace

**Files:**
- Modify: `crates/app/src/workspace/event_handling.rs:39-84`
- Modify/Test: `crates/app/src/workspace/tests.rs:4294-4333`

**Step 1: Update the consumer**

After the central stale guard accepts the event, use:

```rust
if let Some(tab) = self.state.tabs.find_tab_mut(mismatch.scope.tab_id) {
    // existing non-retryable HostKeyMismatch error
}
```

Do not add a second scope comparison in the view.

**Step 2: Update the current mismatch test**

Construct the event with the live scope produced by the connection action. Retain assertions that:
- the current tab enters failed state;
- `ErrorCode::HostKeyMismatch` is used;
- the error is non-retryable;
- no trust modal opens.

**Step 3: Add the reconnect regression**

```rust
#[gpui::test]
fn stale_host_key_mismatch_does_not_fail_replacement_session(cx: &mut TestAppContext)
```

Connect session 1/epoch 1, establish session 2/epoch 2 in the same tab, then deliver the old mismatch. Assert the replacement remains connecting/connected and no modal appears.

**Step 4: Add the current-session companion**

```rust
#[gpui::test]
fn current_host_key_mismatch_hard_blocks_without_retry_or_trust_action(...)
```

This prevents stale filtering from accidentally weakening the live mismatch path.

**Step 5: Run app tests**

```bash
cargo test -p macsftp-app stale_host_key_mismatch -- --nocapture
cargo test -p macsftp-app current_host_key_mismatch -- --nocapture
```

Expected with complete Xcode tools: PASS. If local GPUI compilation is blocked by missing `metal`, report the environment blocker and require CI evidence.

**Step 6: Commit**

```bash
git add crates/app/src/workspace/event_handling.rs crates/app/src/workspace/tests.rs
git commit -m "Ignore stale host key mismatches"
```

### Task 7: Run security-focused verification

**Step 1: Run focused tests**

```bash
cargo test -p macsftp-core host_key_mismatch -- --nocapture
cargo test -p macsftp-sftp pooled_mismatch_failure_ -- --nocapture
cargo test -p macsftp-sftp --test real_session host_key_mismatch_blocks_connection -- --exact --nocapture
cargo test -p macsftp-sftp --test real_session pooled_host_key_mismatch_is_emitted_for_every_logical_session -- --exact --nocapture
```

**Step 2: Run lint and security gates**

```bash
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/check_sensitive_logs.sh
bash scripts/check_architecture.sh
```

Expected: PASS; fingerprints remain absent from logs.

**Step 3: Run the full quality gate**

```bash
bash scripts/check.sh
```

Expected with complete Xcode tools: PASS. If blocked by missing `metal`, document the environment failure without classifying it as a product failure.

## Acceptance checklist

- `HostKeyMismatch` contains one authoritative logical `RemoteEventScope`.
- `ConnectFailure::HostKeyMismatch` carries fingerprint details but no logical scope.
- Physical handshake code emits no first-caller-only mismatch app event.
- Two logical waiters on one in-progress pooled handshake each receive exactly one mismatch event with their own scope.
- Core accepts a current mismatch and rejects an old-session mismatch.
- A current mismatch remains a hard, non-retryable block with no trust/overwrite action.
- No fingerprint, credential, or private-key path is added to logs.
