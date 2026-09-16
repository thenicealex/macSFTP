# 传输终态生命周期实施计划

> **Agent 实施要求：** 必须使用 `superpowers:executing-plans`，并按任务实施本计划。

**目标：** 如果规划后续失败或取消，或者已完成的计划无法传递至 `TransferManager`，那么必须保证全局 store 中已发布的每个传输子任务均进入终态。

**架构：** 以现有事件边界划分所有权。`TransferStore` 已经记录通过 `TransferPlanProgress` 接受的每个子任务，因此 `TransferPlanFailed` 和 `TransferPlanCancelled` 会同时将计划、根任务以及这些已发布子任务转换为终态。`TransferPlanCompleted` 发生后，runtime 负责管理返回的 jobs，直至 `TransferManagerRequest::Enqueue` 成功；如果此前的传递过程失败，则为每个子任务发送一个不可重试的 `TransferFailed`。规划重试仍与根命令关联，而子任务行仅在实际存在重试路径时显示 Retry。

**技术栈：** Rust、Tokio、flume bounded channel、`macsftp-core` reducer、GPUI transfer drawer 和 Cargo tests。

---

## 范围与不变量

- PR 标题：`Guarantee transfer terminal lifecycle`
- 主要缺陷：`SFTP-TRANSFER-001`
- 预计修改以下文件：
  - `crates/core/src/core.rs`
  - `crates/sftp/src/runtime.rs`
  - 仅在需要确定性的本地部分规划 fixture 时修改 `crates/sftp/src/transfer_planner.rs` 中的测试
  - 仅在需要确定性的远程部分规划 fixture 时修改 `crates/sftp/src/session_actor.rs` 中的测试
  - `crates/app/src/workspace/transfer_render.rs`
- 必须满足以下不变量：子任务一旦出现在 `TransferPlanProgress` 中，就必须最终且仅进入一个终态，即 `Completed`、`Skipped` 或 `Failed`。
- 即使 planner 没有返回 `Vec<TransferJob>`，计划终态事件也必须将 core 已知的子任务转换为终态。
- planner 成功返回后，所有权转移至 runtime；`TransferManagerRequest::Enqueue` 被接受后，所有权转移至 manager。
- 不得仅为重复 `TransferStore` 已有的信息而新增复杂的部分规划结果类型。
- 不得注册虚假的子任务重试路径。只有 runtime/manager 能够执行重试时，重试按钮才有效。

### 任务 1：验证 core reducer 对部分规划失败和取消的处理

**文件：**
- 修改及测试：`crates/core/src/core.rs:760-843, 1018-1040`

**步骤 1：增加一个预期失败的部分规划失败测试**

创建一个计划，通过 `TransferPlanProgress` 发布两个 queued 子任务，然后应用：

```rust
AppEvent::TransferPlanFailed {
    plan_id,
    error: planning_error.clone(),
}
```

测试名称如下：

```rust
#[test]
fn transfer_plan_failure_terminalizes_every_published_child()
```

验证以下结果：
- 计划为 `TransferPlanState::Failed`，并且包含原始错误；
- 根任务为 `TransferState::Failed`，并且保留原始 retryable 标志；
- 两个子任务均为 `TransferState::Failed`，并且包含相同的错误和 retryable 标志；
- 再次应用相同事件时返回 `false`，并且状态不再变化。

**步骤 2：增加一个预期失败的部分规划取消测试**

测试名称如下：

```rust
#[test]
fn transfer_plan_cancellation_skips_every_published_child()
```

发布两个子任务并应用 `TransferPlanCancelled`，然后验证计划已取消，而且根任务和子任务均为 `Skipped`。再次应用该事件，并验证操作具有幂等性。

**步骤 3：运行测试并验证当前缺陷**

```bash
cargo test -p macsftp-core transfer_plan_failure_terminalizes_every_published_child -- --nocapture
cargo test -p macsftp-core transfer_plan_cancellation_skips_every_published_child -- --nocapture
```

实现前的预期结果为 FAIL，因为只有根任务进入终态，而两个子任务仍为 `Queued`。

**步骤 4：仅在本地保留失败测试**

除非项目明确允许包含失败测试的提交，否则不得提交这些预期失败的测试。

### 任务 2：通过计划事件将已发布子任务转换为终态

**文件：**
- 修改：`crates/core/src/core.rs:829-843, 1018-1040`
- 测试：`crates/core/src/core.rs` 的 transfer-store 测试模块

**步骤 1：扩展现有辅助函数，不增加新事件**

修改辅助函数，使其将计划终态应用于根任务以及计划中已经记录的每个子任务 ID：

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

所有已发布子任务使用现有的失败错误。取消事件将所有已发布子任务转换为 `Skipped`。由于全局 reducer 已经应用一次权威的计划终态事件，因此无需单独发送子任务终态事件。

**步骤 2：保证事件顺序安全**

延迟到达的 `TransferPlanProgress` 不得使终态计划重新进入非终态。因此，增加或扩展以下测试：

```rust
#[test]
fn transfer_plan_progress_after_terminal_event_is_ignored()
```

增加 progress 子任务之前，必须确认计划仍为 `Planning`。因此，晚于失败或取消事件到达的 progress 事件不会修改计划。

**步骤 3：运行专项测试**

```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
```

预期结果：所有 transfer-plan reducer 测试均 PASS，包括失败、取消、延迟 progress 拒绝和幂等性测试。

**步骤 4：提交**

```bash
git add crates/core/src/core.rs
git commit -m "Terminalize children when transfer planning ends"
```

### 任务 3：验证两个 planner 均可能在发布 progress 后失败

**文件：**
- 测试：`crates/sftp/src/transfer_planner.rs` 的测试模块
- 测试：`crates/sftp/src/session_actor.rs` 的测试模块或 `crates/sftp/tests/real_session.rs`

**步骤 1：增加确定性的本地部分失败测试**

测试名称如下：

```rust
#[test]
fn local_upload_failure_after_progress_emits_plan_failure()
```

使用包含测试标签和 `std::process::id()` 的唯一路径。按照以下顺序提供两个 source：
1. 有效文件，用于立即发布第一个子任务；
2. 缺失或无效的 source，用于触发后续失败。

验证事件顺序包含以下内容：
- 包含第一个子任务 ID 的 `TransferPlanProgress`；
- 同一计划的 `TransferPlanFailed`；
- planner 结果 `None`。

不要验证 planner 发出的子任务终态事件，因为 core 负责将已发布子任务转换为终态。

**步骤 2：增加确定性的取消场景测试**

测试名称如下：

```rust
#[test]
fn local_upload_cancellation_after_progress_emits_plan_cancelled()
```

仅当现有 planner seam 无法在第一次 progress 之后确定性地取消时，才增加测试专用同步点。优先控制事件 receiver 或 cancellation token，不要增加生产环境 hook。

**步骤 3：测试远程 planner**

增加以下测试：

```rust
async fn remote_download_failure_after_progress_emits_plan_failure()
```

如果没有确定性的单元测试 seam，则使用真实 sshd fixture。先发布至少一个有效的远程子任务，然后使后续目录或 source 不可读或不存在。验证 progress 先于 failure，并且结果为 `None`。如果现有 cancellation token 可以被确定性触发，那么还需增加对应的取消测试。

**步骤 4：运行 planner 测试**

```bash
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
```

预期结果为 PASS。这些测试用于验证两个 producer 路径均符合 core 契约，因此不重复 reducer 断言。

**步骤 5：提交**

```bash
git add crates/sftp/src/transfer_planner.rs crates/sftp/src/session_actor.rs crates/sftp/tests/real_session.rs
git commit -m "Test partial transfer planning termination"
```

仅将实际修改的文件加入提交。

### 任务 4：处理规划完成后的每种传递失败

**文件：**
- 修改及测试：`crates/sftp/src/runtime.rs:748-925`

**步骤 1：增加不可重试的传递错误**

manager 获得所有权之前被拒绝的子任务没有 `RetryRoute`，因此不得显示无法执行的重试操作：

```rust
fn transfer_handoff_error(detail: &'static str) -> UserFacingError {
    UserFacingError::new(
        ErrorCode::ChannelClosed,
        "Could not start transfer",
        detail,
    )
}
```

错误详情仅说明结构性原因，并且不得包含路径或凭据。

**步骤 2：增加一个私有失败处理辅助函数**

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

**步骤 3：修改 connection receiver 缺失时的返回逻辑**

将以下代码：

```rust
let Some(connection_rx) = transfer_connection_rx else {
    return;
};
```

修改为针对所有 jobs 发送失败事件。当前获取路径会将 session 缺失或过期、actor sender 缺失以及 actor request queue 已满或断开等情况统一转换为 `None`，因此该处理可以覆盖这些情况。

**步骤 4：在 connection responder 被丢弃时复用辅助函数**

使用 `fail_planned_jobs` 代替现有的内联失败循环。

**步骤 5：从 manager 发送失败结果中取得 jobs**

先构造 request，然后从 `SendError<T>` 中取得被拒绝的值：

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

处理 channel 失败时不得使用 `unwrap`、`expect` 或 `unreachable!`。

**步骤 6：增加确定性的传递测试**

仅在必要时提取一个小型私有辅助函数 `handoff_planned_jobs`。增加包含两个 job 的以下测试：
- `handoff_without_connection_receiver_fails_all_jobs`
- `handoff_with_dropped_connection_responder_fails_all_jobs`
- `handoff_with_closed_manager_fails_all_jobs`

验证准确的 ID、顺序、`ErrorCode::ChannelClosed` 以及 `retryable == false`。

**步骤 7：运行专项测试**

```bash
cargo test -p macsftp-sftp handoff_ -- --nocapture
cargo test -p macsftp-sftp completed_upload_plan_without_session_fails_every_planned_job -- --nocapture
```

预期结果为 PASS；所有测试均不得等待不会出现的子任务终态事件。

**步骤 8：提交**

```bash
git add crates/sftp/src/runtime.rs
git commit -m "Fail jobs rejected during transfer handoff"
```

### 任务 5：使 Retry 可见性与实际所有权一致

**文件：**
- 修改及测试：`crates/app/src/workspace/transfer_render.rs:358-490`

**步骤 1：增加预期失败的渲染策略测试**

如果需要，则提取以下纯函数：

```rust
fn can_retry_transfer(job: &TransferJob) -> bool {
    matches!(job.state, TransferState::Failed { retryable: true, .. })
}
```

增加以下测试：
- `retry_action_is_hidden_for_non_retryable_failure`
- `retry_action_is_shown_for_retryable_failure`

**步骤 2：根据 `retryable` 限制 callback**

使用 `can_retry_transfer(job)` 代替当前的 `matches!(job.state, TransferState::Failed { .. })` 条件。

此项修改用于保证正确性，因为规划完成后的传递失败处理没有重试路径。因此，不得增加会在无提示情况下重新执行无关任务的路径。

**步骤 3：通过根任务保留规划重试能力**

当 `TransferPlanFailed` 已将全部子任务转换为终态时，如果每个子任务的失败信息均与根任务完全相同，则保留失败根任务并隐藏其子任务。因此，`planning_retries` 中现有的根 ID 条目仍然有效；对于常规执行失败，界面继续显示各个子任务行。

增加以下测试：
- `partial_planning_failure_keeps_retryable_root_visible`
- `execution_failure_shows_terminal_child_rows`

使用小型纯分类函数，避免在渲染逻辑中增加大型布尔表达式。

**步骤 4：运行专项测试**

```bash
cargo test -p macsftp-app retry_action_ -- --nocapture
cargo test -p macsftp-app partial_planning_failure_keeps_retryable_root_visible -- --nocapture
cargo test -p macsftp-app execution_failure_shows_terminal_child_rows -- --nocapture
```

在 Xcode 工具完整时，预期结果为 PASS。如果本地 GPUI 编译因缺少 `metal` 而无法继续，则记录环境限制，并要求提供 CI 验证结果。

**步骤 5：执行视觉验证**

在窄 pane 和较低窗口中验证 transfer drawer：
- 规划失败时显示一个失败根任务，并且仅在可重试时显示 Retry；
- 传递失败时显示失败子任务行，但是不显示 Retry；
- 取消后不显示 Retry。

此修改会改变可见操作，因此需要附加截图或记录明确的视觉验证说明。

**步骤 6：提交**

```bash
git add crates/app/src/workspace/transfer_render.rs
git commit -m "Show retry only for routable transfer failures"
```

### 任务 6：执行 package 和 repository 验证

**步骤 1：运行非 GPUI 专项检查**

```bash
cargo test -p macsftp-core transfer_plan_ -- --nocapture
cargo test -p macsftp-sftp handoff_ -- --nocapture
cargo test -p macsftp-sftp local_upload_ -- --nocapture
cargo test -p macsftp-sftp remote_download_ -- --nocapture
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
cargo fmt --all --check
```

**步骤 2：在工具链允许时运行 app 测试**

```bash
cargo test -p macsftp-app retry_action_ -- --nocapture
cargo test -p macsftp-app partial_planning_failure -- --nocapture
cargo test -p macsftp-app execution_failure -- --nocapture
```

**步骤 3：运行 repository 检查**

```bash
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
bash scripts/check.sh
```

在 Xcode 安装完整时，预期结果为 PASS。如果 `xcrun` 无法定位 `metal`，则将其记录为环境限制，并附上已经通过的 core/SFTP 验证结果；但是不得声称完整检查已经通过。

## 验收清单

- 本地和远程规划失败或取消后，已发布的子任务不能继续处于 queued 状态。
- 计划进入终态后，延迟 progress 不能增加子任务，也不能使已有子任务重新进入非终态。
- session 缺失或过期、actor queue 缺失/已满/断开、connection responder 被丢弃以及 manager 停止等情况，均会使每个已返回 job 进入终态。
- 规划失败仍通过根命令执行重试；已取消任务不可重试。
- manager 获得所有权之前发生的子任务失败不可重试，并且界面不显示无效的 Retry 操作。
- manager 接受 enqueue 后的行为以及成功传输行为保持不变。
- 不增加全局 timeout、watchdog 或重复的部分规划数据模型。
