# macSFTP 高严重度缺陷详细代码实施计划

**范围：** 本文档规定 2026-07-28 审计发现的三个高严重度缺陷的代码级修复方案。三个 PR 相互独立，并遵循 TDD 顺序：失败测试 → 最小修复 → 定向测试 → 提交 → 质量检查。本文档不修改产品源代码，而是作为实施约定。

**合并顺序与依赖关系**
- **PR 1**：`Guarantee transfer terminal lifecycle`（`SFTP-TRANSFER-001`）。该 PR 没有依赖，因此首先合并。
- **PR 2**：`Scope pooled host-key mismatches`（`CORE-SFTP-001`）。该 PR 在产品逻辑上不依赖 PR 1，但是两者都会修改 `core.rs` 中相同的 `AppEvent` / `remote_scope()` 位置。因此，该 PR 第二个合并。
- **PR 3**：`Check live remote metadata before edit upload`（`APP-EDIT-001`）。该 PR **依赖 PR 1**，因为 handoff 失败必须产生 transfer terminal event。该 PR 在逻辑上独立于 PR 2，但是 `core.rs` 存在文本冲突。因此，该 PR 最后合并。

**全局规则（源自 `AGENTS.md`）**
- 可恢复错误不得使用 `unwrap`、`expect` 或 panic，而且 fallible send 不得使用 `let _ =` 忽略结果。
- 不得构造虚假的 `SessionId`；stale/remote scope validation 位于 `core`。
- Symlink 必须按 link 处理，因此使用 `symlink_metadata`，不得使用 `metadata`。
- 本地环境缺少 Xcode `metal`，因此无法执行 App/GPUI 测试，相关测试必须在 CI 中执行。Real-sshd integration test 需要 CI 中的 fixture server，因此显式 skip 不能作为验证证据。
- 每次提交只包含一个逻辑变更，并执行 `scripts/check.sh` 中对应的检查子集。

---

## PR 1：保证 transfer terminal lifecycle

**缺陷：** transfer planning 发布 job 后，可能因以下两种情况而持续处于非 terminal 状态：（a）以流式方式发布 child 后，planning 发生失败或取消；（b）已完成的 plan 无法传递至 `TransferManager`。

**文件**
- `crates/core/src/core.rs`：reducer terminalization
- `crates/sftp/src/runtime.rs`：planning 后的 handoff 失败处理
- `crates/sftp/src/transfer_planner.rs`、`crates/sftp/src/session_actor.rs`：producer 测试
- `crates/app/src/workspace/transfer_render.rs`：retry 可见性

**所有权划分**
- Core 已记录通过 `TransferPlanProgress` 发布的每个 child。因此，`TransferPlanFailed` / `TransferPlanCancelled` 必须以原子方式将这些 child 与 plan/root 一并转换为 terminal 状态。
- `TransferPlanCompleted` 之后，runtime 在 `TransferManagerRequest::Enqueue` 成功之前拥有返回的 job。因此，此阶段的任何 handoff 失败都必须为每个 child 发送一个不可重试的 `TransferFailed`。
- 不得新增内容复杂的 partial-planner result，也不得构造虚假的 child retry route。

### T1.1：partial-plan 失败或取消的失败测试
在 `crates/core/src/core.rs` 的 transfer-store 测试模块中增加以下测试：

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
执行：
```bash
cargo test -p macsftp-core transfer_plan_failure_terminalizes_every_published_child -- --nocapture
cargo test -p macsftp-core transfer_plan_cancellation_skips_every_published_child -- --nocapture
```
修复前的预期结果为 **FAIL**，因为 child 仍处于 `Queued` 状态。

### T1.2：将已发布的 child 转换为 terminal 状态
扩展 `set_plan_terminal_state`，使其将 plan terminal state 同时应用于 root 和**每个已记录的 child**：

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
增加 guard，防止迟到的 `TransferPlanProgress` 使 terminal plan 恢复为活动状态。插入 child 之前，必须确认 plan 仍处于 `Planning`，并增加 `transfer_plan_progress_after_terminal_event_is_ignored` 测试。

```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
```
提交命令：`git commit -m "Terminalize children when transfer planning ends"`。

### T1.3：两个 planner 在 progress 之后失败的 producer 测试
- `local_upload_failure_after_progress_emits_plan_failure`：使用两个 source。第一个有效，并立即发布 child；第二个缺失或无效。断言 event 顺序为 `TransferPlanProgress(child)` → `TransferPlanFailed(plan)`，而且 planner result 为 `None`。
- `local_upload_cancellation_after_progress_emits_plan_cancelled`。
- `remote_download_failure_after_progress_emits_plan_failure`；如果不存在 unit seam，则使用 real sshd。
planner 测试不得断言 child terminal event，因为该状态由 core 负责。

```bash
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
```

### T1.4：planning 后的 handoff 失败处理（`runtime.rs`）
manager 获得所有权之前被拒绝的 child 没有 `RetryRoute`，因此必须标记为不可重试：

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
- 使用 `fail_planned_jobs(...)` 替代 missing-connection-receiver 分支中的 `return`。
- dropped connection responder 分支复用该 helper。
- manager send 失败时从错误中恢复 job：

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
不得使用 `unwrap` 或 `unreachable!`。增加 `handoff_without_connection_receiver_fails_all_jobs`、`handoff_with_dropped_connection_responder_fails_all_jobs` 和 `handoff_with_closed_manager_fails_all_jobs` 测试，并断言准确的 ID、`ErrorCode::ChannelClosed` 和 `retryable == false`。
```bash
cargo test -p macsftp-sftp handoff_ -- --nocapture
```

### T1.5：Retry 可见性（`transfer_render.rs`）
提取纯 helper，并根据实际所有权决定是否显示 action：

```rust
fn can_retry_transfer(job: &TransferJob) -> bool {
    matches!(job.state, TransferState::Failed { retryable: true, .. })
}
```
使用 `can_retry_transfer(job)` 替代现有的 `matches!(job.state, TransferState::Failed { .. })` 条件。planning-failure retry 继续由 root 的 `planning_retries` entry 提供。如果每个 child 与 root 包含完全相同的 failure，则隐藏 child，并保留 failed root。增加 `retry_action_is_hidden_for_non_retryable_failure`、`retry_action_is_shown_for_retryable_failure`、`partial_planning_failure_keeps_retryable_root_visible` 和 `execution_failure_shows_terminal_child_rows` 测试。此外，在窄 pane 中对 drawer 进行视觉验证。

### T1.6：质量检查
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

**PR 1 验收标准：** 已发布的 child 均不得持续处于 `Queued`；迟到的 progress 不得使 terminal plan 恢复为活动状态；所有 handoff 失败模式都必须使每个返回的 job 转换为 terminal 状态；planning retry 保留在 root；manager 获得所有权之前的失败不可重试，而且 UI 不显示无效的 Retry。

---

## PR 2：将 pooled host-key mismatch 关联到 logical session

**缺陷：** `HostKeyMismatch` 仅包含 `tab_id`。因此，pooled physical handshake 产生的 mismatch 可能关联到错误的第一个 logical session，而其他 waiter 无法获得 event。

**文件**
- `crates/core/src/core.rs`：event payload 和 `remote_scope()`
- `crates/sftp/src/physical_connection.rs`：返回不含 scope 的 details
- `crates/sftp/src/pool.rs`：为每个 logical waiter 发送一个 scoped event
- `crates/sftp/src/runtime.rs`：按照 logical scope 转换 pooled failure
- `crates/app/src/workspace/event_handling.rs`、`crates/app/src/workspace/tests.rs`：GPUI stale guard
- `crates/sftp/tests/real_session.rs`：单 waiter 和双 waiter sshd 测试

**不变量：** 一次 physical handshake 和 N 次 logical attempt 必须产生 N 个 scoped `HostKeyMismatch`。mismatch 始终阻止连接，不可重试，也不允许 override；stale mismatch 不得修改 replacement session。

### T2.1：验证 core stale 缺口的失败测试
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
初始状态下编译失败，因为不存在 `scope` 字段。

### T2.2：event 使用权威 scope
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
通过 central guard 提供 scope：
```rust
Self::HostKeyMismatch(mismatch) => Some(mismatch.scope.clone()),
```
增加 `remote_scope_extracts_from_host_key_mismatch` 测试，然后提交 core 变更。

### T2.3：Physical handshake 返回 details
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
在 `check_server_key` 中只计算一次 fingerprint，记录 `HostKeyRejection::Mismatch(details)`，然后返回 `Ok(false)`。同时，**删除** `event_tx.send_async(HostKeyMismatch)` 分支。保留结构化日志，但是日志不得包含 fingerprint。增加以下 helper 和测试：
```rust
pub fn host_key_mismatch_event(scope: RemoteEventScope, details: HostKeyMismatchDetails) -> AppEvent {
    AppEvent::HostKeyMismatch(HostKeyMismatch {
        scope, host: details.host, port: details.port,
        expected_fingerprint_sha256: details.expected_fingerprint_sha256,
        actual_fingerprint_sha256: details.actual_fingerprint_sha256,
    })
}
```

### T2.4：Runtime 为每次 logical attempt 发送一个 event
替换 `runtime.rs` 中的两个 `ConnectFailure::HostKeyMismatch => None` 分支：
```rust
ConnectFailure::HostKeyMismatch(details) =>
    Some(host_key_mismatch_event(scope.clone(), details)),
```
使用结构化 `warn!` 替代静默的 `let _ =` send，而且日志不得包含 fingerprint。增加 `pooled_mismatch_failure_uses_first_logical_scope` 和 `pooled_mismatch_failure_uses_second_logical_scope` 测试。

### T2.5：确定性的 pooled 双 waiter 测试
`get_or_connect` 在创建 handshake 之前，会在 `pool.rs:124-125` **同步**插入 `PoolEntry::Connecting(rx.resubscribe())`。因此，连续两次调用 `connect_session` 必然共享一个 in-progress key。在 `connect_session` 中，使用显式 scoped send 替代 `ConnectFailure::HostKeyMismatch => {}`，同时保留 `result = Err(...)`；`get_or_connect` 本身不发送 logical event。
```rust
#[tokio::test(flavor = "multi_thread")]
async fn pooled_host_key_mismatch_is_emitted_for_every_logical_session() {
    // 使用一个 ConnectionManager 和一个共享的 ConnectionPoolIdentity::Saved(...)，因此两次调用共享 ConnectionKey；
    // 同步调用两次 connect_session，scope 分别为 (TAB,SESSION,EPOCH) 和 (SECOND_TAB,SECOND_SESSION,EPOCH)；
    // 断言：两个 result 均为 Err(HostKeyMismatch(_))，而且恰好存在两个 app mismatch event；
    //       scope 等于两个 logical scope，host/port/fingerprint 相同，不存在第三个 event，并且连接被阻止。
}
```
同时增强 `host_key_mismatch_blocks_connection`，使其断言准确的 scope。使用 sshd 执行：
```bash
cargo test -p macsftp-sftp --test real_session host_key_mismatch_blocks_connection -- --exact --nocapture
cargo test -p macsftp-sftp --test real_session pooled_host_key_mismatch_is_emitted_for_every_logical_session -- --exact --nocapture
```

### T2.6：GPUI 拒绝 stale event
central guard 校验完成后，consumer 使用 `mismatch.scope.tab_id`。增加 `stale_host_key_mismatch_does_not_fail_replacement_session`：连接 s1/e1，在同一个 tab 中建立 s2/e2，然后发送旧 mismatch；replacement 保持 connected，而且不显示 modal。另增加 `current_host_key_mismatch_hard_blocks_without_retry_or_trust_action`。

### T2.7：安全检查
```bash
cargo test -p macsftp-core host_key_mismatch -- --nocapture
cargo test -p macsftp-sftp pooled_mismatch_failure_ -- --nocapture
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
bash scripts/check_sensitive_logs.sh   # 日志中不得包含 fingerprint
bash scripts/check_architecture.sh
```

**PR 2 验收标准：** `HostKeyMismatch` 具有唯一权威的 `RemoteEventScope`；`ConnectFailure::HostKeyMismatch` 包含 details，但不包含 scope；physical code 不发送仅属于第一个 caller 的 event；两个 waiter 分别获得且仅获得一个 scoped event；core 接受 current event 并拒绝 stale event；current mismatch 仍然阻止连接，而且不可重试。

---

## PR 3：edit upload 之前检查实时 remote metadata

**依赖 PR 1。** **缺陷：** edit watcher 根据 UI 列表缓存的 metadata 决定是否允许覆盖。因此，并发的外部 remote change 可能被静默覆盖。

**文件**
- `crates/core/src/core.rs`：protocol、`EditPhase::CheckingRemote` 和 `EditCheckId`
- `crates/app/src/edit_watcher.rs`：发送 check，替代 cached compare
- `crates/app/src/event_coordinator.rs`：check result 的唯一处理方
- `crates/app/src/workspace/mod.rs`：`accepts_remote_scope` query 和 reconnect reset
- `crates/app/src/workspace/remote_edit.rs`：完整匹配所有 phase
- `crates/sftp/src/runtime.rs`、`crates/sftp/src/session_actor.rs`：command routing 和 `symlink_metadata`
- `crates/sftp/tests/real_session.rs`：live metadata 测试
- `docs/gpui-russh-plan.md`：记录 protocol 和 same-second limitation

**评审确认的限制：** 不增加 byte-freeze。mtime correlation 已经能够拒绝被后续保存取代的结果，而冻结 stale local byte 会造成数据回退。剩余的 check-then-write 限制，即相同 size 且位于同一秒，属于已记录并接受的限制。必须使用 `symlink_metadata`，不得使用 `metadata`。

### T3.1：Core 中的 protocol 和 state
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
`remote_scope()` 仅包含两个 actor outcome，而 `is_transfer_event()` 排除全部三个 event。在 `EditSessionStore` 中增加 `next_check_id: u64`，并由 UI/core 作为唯一 allocator。增加 phase 和 pending correlation：
```rust
pub enum EditPhase { Downloading, Editing, CheckingRemote, UploadingBack, RemoteConflict }
// EditSession 字段：
pub pending_check_id: Option<EditCheckId>,
pub checking_local_mtime: Option<Timestamp>,
```
逐一检查 `crates/core` 和 `crates/app` 中的 `EditSession` literal/fixture，并在每处初始化这两个字段。可使用 `rg -n "EditSession \{" crates/core crates/app` 确定位置。扩展 `update_epoch_for_tab`：
```rust
if session.phase == EditPhase::CheckingRemote {
    session.phase = EditPhase::Editing;
    session.pending_check_id = None;
    session.checking_local_mtime = None;
}
session.session_epoch = session_epoch;
// 不得重置 UploadingBack（由 transfer 持有）或 local_mtime（保存前 baseline）
```
增加以下 core 测试：`remote_edit_snapshot_events_are_remote_scoped`、`store_find_active_treats_checking_remote_as_live` 和 `store_reconnect_resets_checking_remote_for_retry`。

### T3.2：Runtime 的非阻塞 routing
Actor request 为 `RemoteSessionRequest::CheckRemoteEditSnapshot { edit_session_id, check_id, path }`。该 request 不含 responder，因为 actor 持有 `event_tx`。Runtime 在 `StartTransfer` 之前处理 `AppCommand::CheckRemoteEditSnapshot`：
1. 查询 `sessions[command.tab_id]`；2. 要求 `session.session_epoch == command.session_epoch`；3. 要求存在真实的 actor sender；4. 使用 `try_send`，因此 GPUI 保持非阻塞。
如果 session 缺失、stale，或者 queue 为 full/closed，则发送 `RemoteEditSnapshotDispatchFailed`。该 event 使用 command 自身的 tab/epoch/check/path，因此不得构造虚假 scope：
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
增加 `remote_edit_check_missing_session_emits_failure`、`…_stale_epoch_…`、`…_full_actor_queue_…` 和 `…_disconnected_actor_queue_…` 测试。所有错误均可重试，而且 `remote_scope()` 为 `None`。

### T3.3：Actor metadata
```rust
fn remote_snapshot_from_metadata(metadata: &russh_sftp::client::fs::Metadata) -> RemoteSnapshot {
    RemoteSnapshot {
        size: metadata.size,
        modified_at: metadata.mtime.map(|s| Timestamp::from_secs_since_epoch(s.into())),
    }
}
```
处理 request 时，调用 `self.sftp.symlink_metadata(path.as_str()).await`。成功时发送 scoped `RemoteEditSnapshotChecked`；发生**任何**错误时，包括 `NoSuchFile`，发送 scoped `RemoteEditSnapshotCheckFailed`。删除属于不安全状态，因此不能视为“unchanged”。使用唯一 fixture path 增加 real-sshd 测试：`remote_edit_snapshot_check_reports_live_metadata`、`remote_edit_snapshot_check_sees_external_change_without_relisting`（第二个 writer 修改 size/mtime，且 UI 不重新查询列表）和 `remote_edit_snapshot_check_missing_file_returns_failure`。

### T3.4：Watcher dispatch
删除 `current_remote_snapshot()`，并仅将 `tab_remote_is_ready()` 作为 readiness guard。对于发生变化且处于 `Editing` 的 session：
1. 记录 last mtime 和 current mtime；2. 执行 `check_id = edit_sessions.next_check_id()`；3. 将 phase 设为 `CheckingRemote`，将 `pending_check_id` 设为 `Some(check_id)`，将 `checking_local_mtime` 设为 current，并保留 `local_mtime = last_mtime`；4. 发送 `CheckRemoteEditSnapshot`；5. channel-full 或 no-window 时恢复为 `Editing`，清除 pending 字段，并保留 `local_mtime`。watcher cleanup iteration 必须包含 `CheckingRemote`。增加 `poll_dispatches_remote_check_before_upload`、`poll_does_not_dispatch_duplicate_check_while_checking` 和 `poll_reverts_to_editing_when_check_dispatch_fails` 测试。

### T3.5：仅应用一次结果（`event_coordinator.rs`）
在 window broadcast **之前**，由 `AppEventCoordinator::dispatch_event` 处理全部三个 event，然后直接返回。edit session 属于 process-global state，因此不得进行 per-window handling。对于 scoped event：
1. 通过 `accepts_remote_scope` 确定所属 workspace：
```rust
pub(crate) fn accepts_remote_scope(&self, scope: &RemoteEventScope) -> bool {
    self.state.tabs.accepts_remote_event(scope)
}
```
2. 根据 `edit_session_id` 查询 global edit session；3. 要求 `phase == CheckingRemote`；4. 要求 scope 中的 `tab_id` / `session_epoch` 匹配；5. 要求 `path == session.remote_path`；6. 要求 `check_id == session.pending_check_id`；7. 要求 `checking_local_mtime.is_some()`。
`RemoteEditSnapshotDispatchFailed` 使用相同规则，但是不执行 workspace remote-scope check，因为 command 从未传递至 actor。准确的 epoch/session/check/path tuple 可以防止旧 failure 重置 replacement。

Success transition 必须首先重新 stat temp file：
- 如果 stat 失败或 mtime ≠ `checking_local_mtime`，则清除 pending 字段，恢复为 `Editing`，并保留 baseline。因此，更新的保存结果会再次接受检查。
- 如果 mtime 匹配且 snapshot == baseline，则发送一次 `StartTransfer` upload，将 phase 设为 `UploadingBack`，将 `local_mtime` 设为 `checking_local_mtime`，并清除 pending 字段。
- 如果 mtime 匹配但 snapshot 不同，则将 phase 设为 `RemoteConflict`，将 `local_mtime` 设为 `checking_local_mtime`，清除 pending 字段，并刷新 conflict modal。

actor stat failure 或 dispatch failure 时，将 phase 从 `CheckingRemote` 恢复为 `Editing`，清除 pending 字段，保留 temp 和 baseline，并显示状态文本“Could not verify the remote file; save will retry”。如果 `StartTransfer` 无法进入 channel，则将 phase 从 `UploadingBack` 恢复为 `Editing`，并保留 baseline。

共有九个 app 测试。必须按名称逐一执行，因为 `remote_check` 子字符串会遗漏其中两个：
`matching_remote_check_dispatches_one_upload`, `diverged_remote_check_enters_conflict_without_upload`, `failed_remote_check_returns_to_editing_without_advancing_mtime`, `stale_remote_check_after_reconnect_is_ignored_and_session_retries`, `stale_dispatch_failure_after_reconnect_does_not_reset_replacement_check`, `remote_check_event_is_applied_once_with_multiple_windows`, `duplicate_remote_check_result_does_not_dispatch_second_upload`, `late_result_from_prior_check_id_does_not_authorize_retry`, `local_save_changed_during_remote_check_requires_a_new_check`.

### T3.6：文档和端到端验证
在 `docs/gpui-russh-plan.md` 中记录 protocol 和 same-second limitation。执行以下手动验证：保存且 remote 无变化时产生一次 check 和一次 upload；发生 external change 时显示 conflict 且不覆盖；check 期间断开连接时允许重试；remote file 被删除时不得直接重新创建。此外，在 light、dark、窄 pane 和 focus state 下对 conflict modal/status 进行视觉验证。

**PR 3 验收标准：** cached listing 不得作为允许覆盖的依据；每次保存最多存在一个 in-flight check；只有 scope、session、check、path 和 mtime 均准确匹配时才能更新 session；metadata 相同时执行一次 upload，不同时进入 conflict；所有失败都必须保留 local content 并允许重试；stale result 不得修改 reconnected session；real sshd 必须证明无需重新查询列表即可检测 external change；文档必须继续记录 same-size/same-second limit。

---

## 合并前的通用检查清单
- [ ] 每个 PR 均可编译，而且定向测试通过（`cargo test -p <crate> <substring>`）。
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo fmt --all --check` 均通过。
- [ ] `bash scripts/check_architecture.sh` 和 `bash scripts/check_sensitive_logs.sh` 均通过，而且日志中不包含 secret/fingerprint。
- [ ] 本地缺少 `metal` 时，将 App/GPUI 测试明确记录为 CI 必须执行的测试。
- [ ] Real-sshd integration test 在 CI 中执行；skip 不能作为验证证据。
- [ ] 不得构造虚假的 `SessionId`，不得静默忽略 fallible operation，而且不得增加 global timeout/watchdog。
- [ ] PR 3 在 `docs/gpui-russh-plan.md` 中记录 architecture change。
