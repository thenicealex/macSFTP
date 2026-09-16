# Host-Key Mismatch 作用域实施计划

> **Agent 执行要求：** 必须使用 `superpowers:executing-plans`，并且按任务实施本计划。

**目标：** 每次逻辑连接尝试都必须收到一个 host-key mismatch event，并且该 event 仅属于对应的连接尝试。即使多个 session 等待同一个池化物理握手，该要求也必须成立。

**架构：** 物理 SSH handler 记录不可变的 mismatch details，但是不再发布逻辑 app event。`ConnectFailure::HostKeyMismatch` 通过连接池 broadcast 传递这些 details。每个逻辑调用方，即 runtime 或 `ConnectionManager::connect_session`，使用自身的权威 `RemoteEventScope` 将 failure 转换为 `AppEvent::HostKeyMismatch`。Core 将该 scope 提供给现有 stale-event guard。因此，当前 mismatch 会强制阻断连接，但是过期 mismatch 不会影响替代 session。

**技术栈：** Rust、Tokio broadcast channel、`macsftp-core` stale-event guard、GPUI workspace tests、russh/OpenSSH integration tests。

---

## 范围与不变量

- PR 标题：`Scope pooled host-key mismatches to logical sessions`
- 主要缺陷：`CORE-SFTP-001`
- 预计修改以下文件：
  - `crates/core/src/core.rs`
  - `crates/sftp/src/physical_connection.rs`
  - `crates/sftp/src/pool.rs`
  - `crates/sftp/src/runtime.rs`
  - `crates/app/src/workspace/event_handling.rs`
  - `crates/app/src/workspace/tests.rs`
  - `crates/sftp/tests/real_session.rs`
- 必须保持以下不变量：
  1. 匹配的 host-key mismatch 始终阻断连接，并且在 UI 中保持不可重试。
  2. 每次逻辑连接尝试恰好收到一个 mismatch event，并且 event 包含该尝试自身的 `(tab_id, session_id, session_epoch)`。
  3. 过期 mismatch 绝不能修改替代 session。
  4. 物理握手只计算一次 fingerprint details，并通过 pooled failure 复制该数据。因此，逻辑调用方不能再次读取 `known_hosts`。
- 不能增加 override action、一键替换 known-host 的功能或新的 session-ID allocator。
- 不能将第一个调用方的 scope 写入由后续逻辑调用方共享的 pooled failure。

### Task 1：验证 core 的 stale-event 缺口

**文件：**
- 修改/测试：`crates/core/src/core.rs:1630-1728, 1843-1849`

**Step 1：增加失败的 stale mismatch 测试**

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

该测试在实现前无法编译，因为 `HostKeyMismatch` 包含 `tab_id`，但是不包含 `scope`。

**Step 2：增加当前 session 的对应测试**

增加 `app_state_accepts_host_key_mismatch_from_current_session`，并使用 scope `(TabId(1), SessionId(11), 2)`。

**Step 3：执行预期失败的测试**

```bash
cargo test -p macsftp-core app_state_rejects_host_key_mismatch -- --nocapture
cargo test -p macsftp-core app_state_accepts_host_key_mismatch -- --nocapture
```

实现前的预期结果：编译失败，并且错误信息指出未知字段 `scope`。

### Task 2：为 core event 增加唯一的权威 scope

**文件：**
- 修改/测试：`crates/core/src/core.rs:1630-1728, 1843-1849, 2973-3015`

**Step 1：修改 payload**

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

移除 `tab_id`，因为重复存储 identity values 可能导致数据不一致。

**Step 2：通过 central guard 提供 scope**

在 `AppEvent::remote_scope()` 中增加：

```rust
Self::HostKeyMismatch(mismatch) => Some(mismatch.scope.clone()),
```

更新方法注释，并明确说明 unknown-host prompt 和 mismatch event 是需要 stale filtering 的安全敏感事件。

**Step 3：增加 scope extraction 覆盖**

增加 `remote_scope_extracts_from_host_key_mismatch`，并断言 scope 完全一致、`is_remote_scoped() == true` 和 `is_transfer_event() == false`。

**Step 4：执行 core 测试**

```bash
cargo test -p macsftp-core host_key_mismatch -- --nocapture
cargo test -p macsftp-core remote_scope_extracts_from_host_key_mismatch -- --nocapture
```

预期结果：PASS。

**Step 5：提交变更**

```bash
git add crates/core/src/core.rs
git commit -m "Scope host key mismatches to sessions"
```

### Task 3：让物理握手返回 mismatch details

**文件：**
- 修改/测试：`crates/sftp/src/physical_connection.rs:25-39, 105-128, 516-580`

**Step 1：增加不含 scope 的物理结果**

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

`HostKeyMismatchDetails` 明确不包含 `RemoteEventScope`，因为多个逻辑调用方可以观察同一个物理握手结果。

**Step 2：停止从 `check_server_key` 发送 app event**

发生 mismatch 时：

1. 计算一次 expected 和 actual fingerprints；
2. 记录 `HostKeyRejection::Mismatch(details)`；
3. 返回 `Ok(false)`。

从该分支移除当前的 `event_tx.send_async(AppEvent::HostKeyMismatch(...))`。保留结构化 mismatch 日志，并且为诊断信息保留物理发起方的 scope，但是绝不能记录 fingerprint。

**Step 3：通过 handshake failure 传递 details**

使用以下映射：

```rust
Some(HostKeyRejection::Mismatch(details)) => {
    ConnectFailure::HostKeyMismatch(details)
}
```

将 `log_connect_failure` 的模式匹配更新为 `ConnectFailure::HostKeyMismatch(_)`。

**Step 4：增加纯 event-construction helper**

使用以下辅助函数。因此，各处的逻辑转换保持一致：

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

增加单元测试 `host_key_mismatch_event_uses_logical_scope`。

**Step 5：执行 physical-connection 测试**

```bash
cargo test -p macsftp-sftp host_key_mismatch_event_uses_logical_scope -- --nocapture
```

预期结果：PASS。

**Step 6：提交变更**

```bash
git add crates/sftp/src/physical_connection.rs
git commit -m "Return host key mismatch details from handshake"
```

### Task 4：为每次 runtime 连接尝试发送一个 mismatch event

**文件：**
- 修改/测试：`crates/sftp/src/runtime.rs:480-610`

**Step 1：在两个 runtime 分支中转换 pooled failure**

两个 failure path 目前都将 `ConnectFailure::HostKeyMismatch` 映射为 `None`。因此，将该映射替换为：

```rust
ConnectFailure::HostKeyMismatch(details) => Some(
    crate::physical_connection::host_key_mismatch_event(
        scope.clone(),
        details,
    ),
),
```

需要修改以下分支：
- `Ok(Ok(shared_connection))` 之后的 SFTP channel setup failure 处理；
- 来自物理连接 broadcast 的 `Ok(Err(failure))`。

第一个分支通常不会收到 host-key mismatch，但是处理全部枚举情况可以保证 contract 完备。

**Step 2：明确处理 event-send failure**

将所修改分支中的静默 `let _ = event_tx_clone.send_async(event).await` 替换为结构化 warning。但是，不能记录 fingerprint 或 credential。

**Step 3：如果能够消除重复，则提取 private failure-to-event helper**

该辅助函数必须接收逻辑 `scope`，并且不能从 `ConnectFailure` 读取 scope。

**Step 4：增加确定性的 runtime 测试**

增加以下测试：
- `pooled_mismatch_failure_uses_first_logical_scope`
- `pooled_mismatch_failure_uses_second_logical_scope`

两个测试使用相同的 details 副本和不同的 scope。每次转换必须恰好产生一个 event，并且 fingerprint 必须保持完全一致。

**Step 5：执行聚焦的 runtime 测试**

```bash
cargo test -p macsftp-sftp pooled_mismatch_failure_ -- --nocapture
```

预期结果：PASS。

**Step 6：提交变更**

```bash
git add crates/sftp/src/runtime.rs
git commit -m "Emit scoped mismatch events per runtime session"
```

### Task 5：修复 pooled convenience path，并验证多个 waiter

**文件：**
- 修改：`crates/sftp/src/pool.rs:218-363`
- 测试：`crates/sftp/tests/real_session.rs:1371-1410`

**Step 1：在 `connect_session` 中转换 mismatch**

替换以下代码：

```rust
ConnectFailure::HostKeyMismatch => {}
```

使用 `host_key_mismatch_event(scope.clone(), details.clone())` 明确发送 event。如果发送失败，则记录结构化 warning。发送 event 不会将 failure 转换为 success，因此返回的 `result` 仍然是 `Err(ConnectFailure::HostKeyMismatch(details))`。

`get_or_connect` 本身仍然是物理连接池 primitive，因此不发送逻辑 mismatch event。

**Step 2：强化 single-session 集成测试**

在 `host_key_mismatch_blocks_connection` 中增加以下断言：

```rust
assert_eq!(
    mismatch.scope,
    RemoteEventScope::new(TAB, SESSION, EPOCH),
);
```

保留 fingerprint 断言，以及 hard-block/no-follow-up 断言。

**Step 3：增加 pooled two-waiter 回归测试**

测试名称如下：

```rust
#[tokio::test(flavor = "multi_thread")]
async fn pooled_host_key_mismatch_is_emitted_for_every_logical_session()
```

该测试具有确定性，因此不依赖竞态条件。`get_or_connect` 在 `pool.rs:124-125`
同步插入 `PoolEntry::Connecting(rx.resubscribe())`，然后创建物理握手任务并
返回 `rx`。在插入 entry 与返回 `rx` 之间，该函数没有 `.await`。因此，连续
调用两次 `connect_session` 时，第一个 future 只执行到首个 `.await`，而第二次
调用必然查询到进行中的 key 并执行 `resubscribe`。因此，测试无需专用 barrier，
也无需仅用于 production 的 hook。两个 `connect_session` task 都对同一个 broadcast
执行 `recv().await`，并且分别将同一个 `ConnectFailure::HostKeyMismatch(details)`
映射为包含自身 scope 的 event。

准备测试环境：

1. 使用错误的 known-host key 启动真实 sshd fixture；
2. 创建一个 `ConnectionManager`；
3. 使用一个共享的 `ConnectionPoolIdentity::Saved(AuthFingerprint::private_key(...))`，因此两次调用共享同一个 `ConnectionKey`；
4. 在等待任何结果之前，同步调用两次 `connect_session`，并分别使用 scopes `(TAB, SESSION, EPOCH)` 和 `(SECOND_TAB, SECOND_SESSION, EPOCH)`；
5. 获取两个 connection result 和两个 mismatch event。

验证以下结果：
- 两个 connection result 都是 `Err(ConnectFailure::HostKeyMismatch(_))`；
- app 恰好收到两个 mismatch event；
- event scope 等于两个逻辑 scope，并且不依赖 event 顺序；
- 两个 event 包含完全相同的 host、port 和 fingerprint details；
- 不会出现第三个 mismatch event 或 success event；
- 连接保持阻断状态。

该测试专门验证 `PoolEntry::Connecting(rx).resubscribe()` 不会将逻辑 identity 限制为第一个调用方的 identity。

**Step 4：执行真实 integration tests**

```bash
cargo test -p macsftp-sftp --test real_session host_key_mismatch_blocks_connection -- --exact --nocapture
cargo test -p macsftp-sftp --test real_session pooled_host_key_mismatch_is_emitted_for_every_logical_session -- --exact --nocapture
```

存在 sshd 时，预期结果为 PASS。不存在 sshd 时，测试必须明确标记为 skip。但是，skip 不能作为 runtime 证据，因此 CI 必须通过 fixture server 执行这些测试。

**Step 5：提交变更**

```bash
git add crates/sftp/src/pool.rs crates/sftp/tests/real_session.rs
git commit -m "Scope pooled mismatches to each logical session"
```

### Task 6：在 GPUI workspace 中拒绝过期 mismatch

**文件：**
- 修改：`crates/app/src/workspace/event_handling.rs:39-84`
- 修改/测试：`crates/app/src/workspace/tests.rs:4294-4333`

**Step 1：更新 consumer**

central stale guard 接受 event 后，使用以下代码：

```rust
if let Some(tab) = self.state.tabs.find_tab_mut(mismatch.scope.tab_id) {
    // existing non-retryable HostKeyMismatch error
}
```

不要在 view 中增加第二次 scope 比较。

**Step 2：更新当前 mismatch 测试**

使用 connection action 产生的 live scope 构造 event。保留以下断言：
- 当前 tab 进入 failed state；
- 使用 `ErrorCode::HostKeyMismatch`；
- 该错误不可重试；
- 不会显示 trust modal。

**Step 3：增加 reconnect 回归测试**

```rust
#[gpui::test]
fn stale_host_key_mismatch_does_not_fail_replacement_session(cx: &mut TestAppContext)
```

先在同一个 tab 中连接 session 1/epoch 1，再建立 session 2/epoch 2，然后传递旧 mismatch。确认替代 session 保持 connecting 或 connected 状态，并且不会显示 modal。

**Step 4：增加当前 session 的对应测试**

```rust
#[gpui::test]
fn current_host_key_mismatch_hard_blocks_without_retry_or_trust_action(...)
```

该测试可以防止 stale filtering 意外削弱 live mismatch path。

**Step 5：执行 app 测试**

```bash
cargo test -p macsftp-app stale_host_key_mismatch -- --nocapture
cargo test -p macsftp-app current_host_key_mismatch -- --nocapture
```

Xcode 工具完整时，预期结果为 PASS。如果本地 GPUI 编译因为缺少 `metal` 而无法继续，那么需要报告环境限制，并要求提供 CI 证据。

**Step 6：提交变更**

```bash
git add crates/app/src/workspace/event_handling.rs crates/app/src/workspace/tests.rs
git commit -m "Ignore stale host key mismatches"
```

### Task 7：执行安全专项验证

**Step 1：执行聚焦测试**

```bash
cargo test -p macsftp-core host_key_mismatch -- --nocapture
cargo test -p macsftp-sftp pooled_mismatch_failure_ -- --nocapture
cargo test -p macsftp-sftp --test real_session host_key_mismatch_blocks_connection -- --exact --nocapture
cargo test -p macsftp-sftp --test real_session pooled_host_key_mismatch_is_emitted_for_every_logical_session -- --exact --nocapture
```

**Step 2：执行 lint 与安全检查**

```bash
cargo clippy -p macsftp-core -p macsftp-sftp --all-targets -- -D warnings
cargo fmt --all --check
bash scripts/check_sensitive_logs.sh
bash scripts/check_architecture.sh
```

预期结果为 PASS，并且日志中仍然没有 fingerprint。

**Step 3：执行完整质量检查**

```bash
bash scripts/check.sh
```

Xcode 工具完整时，预期结果为 PASS。如果缺少 `metal` 导致检查无法继续，那么需要记录该环境问题，但是不能将其归类为产品缺陷。

## 验收清单

- `HostKeyMismatch` 包含唯一且权威的逻辑 `RemoteEventScope`。
- `ConnectFailure::HostKeyMismatch` 包含 fingerprint details，但是不包含逻辑 scope。
- 物理握手代码不会产生仅属于第一个调用方的 mismatch app event。
- 两个逻辑 waiter 等待同一个进行中的 pooled handshake 时，各自恰好收到一个包含自身 scope 的 mismatch event。
- Core 接受当前 mismatch，并且拒绝旧 session 的 mismatch。
- 当前 mismatch 始终构成强制且不可重试的阻断，并且不提供 trust/overwrite action。
- 日志中没有新增 fingerprint、credential 或 private-key path。
