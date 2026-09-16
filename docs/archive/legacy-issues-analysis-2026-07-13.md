# 遗留问题分析与修复方案（2026-07-13）

> 范围：本文仅分析与 `commit 37ceb63`（`feat: implement ssh multiplexing via Connection Pool (Phase 1 & 2)`）相关的两项既有遗留问题，而且这两项问题均与多窗口无关。
> 验证依据：分析过程逐一读取了 `crates/sftp/src/{session_actor.rs, physical_connection.rs, pool.rs, runtime.rs, sftp.rs}`、`crates/sftp/tests/real_session.rs` 和 `crates/sftp/Cargo.toml`，并执行 `cargo clippy -p macsftp-sftp --all-targets` 与 `cargo test -p macsftp-sftp --test real_session`，因此以下报错均来自实际结果。

---

## 0. 背景

`commit 37ceb63` 将原本位于 `RemoteSessionActor` 内部的“建立连接 + 握手 + 主机密钥信任 + 认证 + 创建 SFTP 子系统”逻辑迁移至 `physical_connection.rs`（物理连接建立）与 `pool.rs`（`ConnectionManager` / SSH 多路复用）。

但是，该重构遗留了两个未完成事项：

1. **3 个死函数 warning**（编译器已报 `dead_code`）；
2. **`real_session.rs` 集成测试编译失败**，原因是 actor 构造函数签名发生变化，而且 `EventReceiver` 从 flume 变为 tokio broadcast 后要求 `&mut self`。

这两项问题均与多窗口特性无关，而是 Connection Pool WIP 未完整实施所产生的遗留问题。

---

## 1. 问题一：SFTP 模块中 3 个死函数 warning

### 1.1 现象

`cargo clippy -p macsftp-sftp --all-targets` 报告 3 个 `dead_code`：

| # | 函数 | 位置 | 当前状态 |
|---|------|------|----------|
| 1 | `subsystem_error` | `crates/sftp/src/physical_connection.rs:193` | `fn` 私有，**0 调用** |
| 2 | `private_key_error` | `crates/sftp/src/physical_connection.rs:204` | `fn` 私有，**0 调用** |
| 3 | `lock_store` | `crates/sftp/src/session_actor.rs:62` | `fn` 私有，**0 调用** |

此外还存在 **2 个“隐性死代码”**。它们被 `#[cfg(any())]` 排除在编译范围之外，因此编译器不报告 warning，但是它们同样属于重复副本：

| # | 函数 | 位置 |
|---|------|------|
| 4 | `subsystem_error` | `crates/sftp/src/session_actor.rs:2400`（位于 `#[cfg(any())]` 区块 2387–2433） |
| 5 | `private_key_error` | `crates/sftp/src/session_actor.rs:2412`（同区块） |

### 1.2 来源与用途分析

这些函数**不是未完成功能，而是重复副本**。Connection Pool 重构后，活动代码路径中已经存在等价或完全相同的错误构造逻辑，而且签名、文案和错误码均一致。逐项核对结果如下：

| 死函数 | 取代它的活代码（已存在、已使用） | 结论 |
|--------|----------------------------------|------|
| `lock_store`（session_actor:62） | `physical_connection.rs:19` 的 `pub fn lock_store` 已被 `physical_connection.rs:68 / 86 / 135` 调用，新连接路径使用该函数锁定 `known_hosts`。session_actor 现在使用已建立的 `SharedConnection`，因此不再直接锁定 store。 | **纯冗余**，删除不影响任何活动调用 |
| `subsystem_error`（physical_connection:193） | `physical_connection.rs:182` `pub fn connection_error` + `pool.rs::get_or_connect` 内部 inline 构造 `UserFacingError::new(ErrorCode::ChannelClosed, "Could not open an SSH channel." …)`（见 `runtime.rs` 428–434 段）。新路径直接构造等价错误。 | **纯冗余** |
| `private_key_error`（physical_connection:204） | `physical_connection.rs` 内 `authenticate` 自行构造 `AuthFailed` 错误；原 session_actor:746 的调用点现在处于 `#[cfg(any())]` 死区块（2387–2433）内。 | **纯冗余** |

**误判风险检查：** 已使用 `grep -rnE` 在整个仓库中确认三个被报告函数的调用方数量均为 0。`physical_connection.rs:19` 的 `pub fn lock_store` 是同名的活动函数，而另外两个死函数没有任何调用。因此，删除这些函数不会影响活动代码。

**结论：应删除，而不应继续实现。** 连接建立的错误处理已由 `connection_error` 和 inline `UserFacingError::new` 完整覆盖，而且错误语义使用相同的 `ErrorCode` 与文案，与死函数完全一致。因此，不存在缺失功能，正确处理方式是**删除死代码**。该结论也符合 `AGENTS.md` 的“无未使用代码”原则。

### 1.3 修复方案（删除）

**A. `crates/sftp/src/physical_connection.rs`**
- 删除 `subsystem_error` 整段（约 192–219 行）。
- 删除 `private_key_error` 整段（约 203–235 行）。

**B. `crates/sftp/src/session_actor.rs`**
- 删除 `lock_store` 整段（`fn lock_store { … }`，约 60–66 行）。
- 删除 `#[cfg(any())]` 死区块中的两个重复函数。2387–2433 行包含 `connection_error`、`subsystem_error` 和 `private_key_error` 三个重复副本。
  - 该 `#[cfg(any())]` 区块（2387–2433）完整保留了旧 connect/auth 逻辑，因此建议删除整个区块。如果采用保守方案，则至少删除 2400 和 2412 行的两个函数定义。
  - 删除后，如果 2434 行存在没有后继项的 `#[cfg(any())]` 属性，则同时删除该属性行。因此，`#[cfg(test)] mod tests`（2437）可以正常参与编译。

**C. 验证**
```bash
cargo clippy -p macsftp-sftp --all-targets 2>&1 | grep -i "dead_code"   # 应无输出
```
预期结果是 3 个 `dead_code` warning 均不再出现。

---

## 2. 问题二：`real_session.rs` 集成测试编译失败

### 2.1 现象

`cargo test -p macsftp-sftp --test real_session` 报告 2 个 error：

```
error[E0061]: this function takes 7 arguments but 10 arguments were supplied
   --> crates/sftp/tests/real_session.rs:68:17
   |     let actor = RemoteSessionActor::new(
   |                 ^^^^^^^^^^^^^^^^^^^^^
   = note: expected `Arc<SharedConnection>`, found `TrustRequestId`   (arg #4)
   = note: expected `SftpSession`,        found `ConnectionSettings`  (arg #5)

error[E0308]: mismatched types
   --> crates/sftp/tests/real_session.rs:131:34
   |     match next_runtime_event(events, "runtime transfer event").await {
   |          ------------------ ^^^^^^ types differ in mutability
   |          expected mutable reference `&mut EventReceiver`
   |          found reference `&EventReceiver`
```

### 2.2 根因：commit 37ceb63 的 actor 签名变更

**旧签名包含 10 个参数，而且 actor 自行建立连接。** 测试当前在 `real_session.rs:68` 使用该形式：

```rust
RemoteSessionActor::new(
    tab_id: TabId,
    session_id: SessionId,
    session_epoch: u64,
    trust_request_id: TrustRequestId,
    settings: ConnectionSettings,        // 连接参数
    known_hosts: Arc<Mutex<KnownHostsStore>>,
    trust_config: Arc<HostTrustConfig>,
    trust_registry: Arc<TrustRegistry>,
    event_tx: flume::Sender<AppEvent>,
    next_conflict_id: Arc<AtomicU64>,
)
```

**新签名包含 7 个参数，而且使用已建立的连接。** 该签名位于 `session_actor.rs:269`：

```rust
pub fn new(
    tab_id: TabId,
    session_id: SessionId,
    session_epoch: u64,
    shared_connection: Arc<SharedConnection>,   // 由 ConnectionManager 建立
    sftp: russh_sftp::client::SftpSession,       // 由 shared.handle 创建的 SFTP 通道
    event_tx: flume::Sender<AppEvent>,
    next_conflict_id: Arc<AtomicU64>,
)
```

**设计意图：** 连接建立职责已经迁移至 Connection Pool。
- `ConnectionManager::get_or_connect(...)`（`pool.rs:41`）负责建立连接、握手、主机密钥信任、认证和创建 SFTP 子系统，并返回 `broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>>`。`SharedConnection`（`pool.rs:17`）包含 `handle: client::Handle<ClientHandler>` 与 `remote_root`。
- 随后，调用方通过 `shared.handle` 创建一个 SFTP 通道以获得 `SftpSession`，并将其与 `shared` 一同提供给 actor。规范实现位于 `runtime.rs:418–444`。

**E0308 的次生原因：** `commit 37ceb63` 将 `EventReceiver` 从 flume wrapper 改为 tokio broadcast wrapper，因此 `recv` 改为使用 `&mut self`。对应定义是 `runtime.rs:109` 的 `pub async fn recv(&mut self) -> Option<AppEvent>`。但是，测试中的 `wait_for_runtime_transfer_completion(events: &EventReceiver)`（`:128`）没有同步改为 `&mut`，因此 `:131` 传入 `&events` 时发生类型不匹配。
- 其余 runtime 集成测试从 `:815` 开始均已使用 `let mut events = controller.event_receiver(); … &mut events`。因此，**只有** `wait_for_runtime_transfer_completion` 遗漏了该修改。

### 2.3 受影响调用点

| 位置 | 问题 | 修复方向 |
|------|------|----------|
| `real_session.rs:68` `RemoteSessionActor::new(...)` | 传入 10 个旧参数 | 改为 7 个参数，并在调用前通过 `ConnectionManager` 建立连接 |
| `real_session.rs:128` `wait_for_runtime_transfer_completion(events: &EventReceiver)` | 形参应为 `&mut EventReceiver` | 改签名 + 调用点 `:860`、`:887` 的 `&events` → `&mut events`（这些 `events` 已是 `let mut events = …`） |

### 2.4 修复方案（两套，二选一）

#### 方案 B（推荐：变更明确，而且不新增 dev-dep）

在 `ConnectionManager` 上新增一个**公开方法** `connect_session`。该方法依次取得 `SharedConnection`，通过 `shared.handle` 创建 SFTP 通道，然后返回 `(Arc<SharedConnection>, SftpSession)`。其逻辑直接复用 `runtime.rs:418–444` 的现有实现。返回类型如下：

```rust
broadcast::Receiver<Result<(Arc<SharedConnection>, SftpSession), ConnectFailure>>
```

该方案具有以下优势：
- 测试**无需直接依赖 `russh`**。当前 `crates/sftp/Cargo.toml` 的 `[dev-dependencies]` 仅包含 `macsftp-test-support` 与 `tokio`，并**不包含** `russh` / `russh-sftp`，因此 `russh` 可以继续限制在 crate 内部。
- `runtime.rs` 也可以使用该方法，因此能够消除重复逻辑。

测试侧 `spawn_actor` 改为：

```rust
use macsftp_sftp::pool::{ConnectionManager, SharedConnection};
use macsftp_core::RemoteEventScope;
// （AtomicU64 已用 std::sync::atomic::AtomicU64 引入）

fn spawn_actor(server: &SshTestServer, auth: AuthCredential, prefill_host_key: Option<&str>) -> ActorFixture {
    let (event_tx, event_rx) = flume::bounded(64);
    let trust_registry = Arc::new(TrustRegistry::new());
    // …… known_hosts / trust_config / settings 保持不变 ……

    let cm = Arc::new(ConnectionManager::new());
    let scope = RemoteEventScope::new(TAB, SESSION, EPOCH);
    let mut rx = cm.connect_session(
        &settings, &scope, TRUST_REQUEST,
        known_hosts.clone(), trust_config.clone(), trust_registry.clone(),
        event_tx.clone(), CancellationToken::new(),
    );

    let cancel_for_actor = cancel.clone();
    tokio::spawn(async move {
        if let Ok(Ok((shared, sftp))) = rx.recv().await {
            let actor = RemoteSessionActor::new(
                TAB, SESSION, EPOCH, shared, sftp,
                event_tx.clone(),
                Arc::new(std::sync::atomic::AtomicU64::new(1)),
            );
            actor.run(cancel_for_actor, request_rx).await;
        }
    });

    ActorFixture { events: event_rx, requests, trust_registry, cancel, app_known_hosts_path }
}
```

> **避免死锁的关键不变量：** `spawn_actor` 必须**立即返回** fixture，不得在 `rx.recv()` 上阻塞。连接建立过程包含主机密钥信任等待，并位于 `get_or_connect` 内部的 spawned task 中。与此同时，测试通过 `next_event` 取得 `HostKeyUnknown`，然后手动调用 `trust_registry.resolve(...)`。这两个过程必须并发。现有测试，例如 `known_host_unknown_prompts`，依赖“开始建立连接，然后由测试确认信任”的顺序。因此，**不得在 `spawn_actor` 中自动 resolve 信任**。

#### 方案 A（改动较少，但是需要增加 dev-dep）

该方案不新增 API。测试直接调用 `ConnectionManager::get_or_connect`，然后在测试中调用 `shared.handle.channel_open_session()` 创建 SFTP 通道。因此，需要在 `crates/sftp/Cargo.toml` 的 `[dev-dependencies]` 中增加：

```toml
russh = "0.62"
russh-sftp = "2.3"
```

同时，在测试顶部增加 `use russh::client;` 和 `use russh_sftp::client::SftpSession;`。

> 该方案会使 crate 内部依赖 `russh` 成为集成测试的直接依赖，因此边界不如方案 B 明确。只有在必须保持 sftp 公开 API 不变时才采用该方案。

### 2.5 E0308 的精确修正

无论选择 A 还是 B，都必须同步修改以下内容：

```rust
// real_session.rs:128
- async fn wait_for_runtime_transfer_completion(events: &EventReceiver) -> TransferId {
+ async fn wait_for_runtime_transfer_completion(events: &mut EventReceiver) -> TransferId {

// real_session.rs:860 与 :887 的调用点
-     let _ = wait_for_runtime_transfer_completion(&events).await;
+     let _ = wait_for_runtime_transfer_completion(&mut events).await;
```
调用处的 `events` 已经声明为 `let mut events = controller.event_receiver();`，因此无需修改声明。

### 2.6 验证（实施结果）

```bash
cargo test -p macsftp-sftp --test real_session   # 编译错误消失，但是初次实现后 21 项中有 8 项运行时失败
```

E0061 和 E0308 编译错误按照方案 B 的预期消失。但是，首次编译成功后，**仍有 8 项测试在运行时失败**。这些失败并非由 actor 签名适配本身造成，而是源于 `commit 37ceb63` 重构遗留的另外三个生产级缺陷，详见 §4。修复这三个缺陷后，结果如下：

```bash
cargo test -p macsftp-sftp --test real_session   # 21 passed; 0 failed
cargo test -p macsftp-sftp --lib                  # 52 passed; 0 failed（含 runtime.rs 自身单测）
```

---

## 3. 综合建议

两个 issue 具有相同来源，均由 `commit 37ceb63` 的 Connection Pool WIP 未完整实施造成。

- **P0（零风险）**：删除 3(+2) 个死函数（§1.3）。这些变更仅删除冗余，因此不改变任何行为。
- **P1**：按照**方案 B** 修改 `real_session.rs`，包括 §2.4 的 `connect_session` 和 §2.5 的 E0308 单行修正。
- **修复后的 gate 状态**：完成这两项修复后，`macsftp-sftp` 中的 `dead_code`、`E0061` 和 `E0308` 三类错误均会消失。但是，`scripts/check.sh` 当前仍可能因为 `sftp` 中**其他**既有问题而失败。这些问题与本任务无关，因此不属于本次范围：
  - `session_actor.rs:362` 存在一个 `unwrap`，因此会被 `AGENTS.md` 规定的 `unwrap_used` deny lint 拒绝；
  - `sftp` 还有其它 clippy warning（非 dead_code 类）。
  
  如果目标是使 `scripts/check.sh` 整体通过，则需要另行处理这些独立遗留问题。但是，它们不属于本次“两项遗留问题”的修复范围。

---

## 附录：已验证的关键事实

| 项 | 实测结果 |
|----|----------|
| 死函数调用方 | `subsystem_error` / `private_key_error`（physical_connection）/ `lock_store`（session_actor）均为 0 调用 |
| 活副本 | `physical_connection.rs:19` `pub fn lock_store`；`:182` `pub fn connection_error`；`pool.rs` `get_or_connect` inline 错误 |
| 新 actor 签名 | `session_actor.rs:269` 7 参：`(tab_id, session_id, session_epoch, Arc<SharedConnection>, SftpSession, event_tx, Arc<AtomicU64>)` |
| 规范连接建立方式 | `runtime.rs:400–444`（get_or_connect → 获得 SharedConnection → 创建 SFTP 通道 → new → run） |
| E0308 漏改点 | 仅 `wait_for_runtime_transfer_completion`（`real_session.rs:128`，调用点 `:860`/`:887`） |
| sftp dev-deps | 仅包含 `macsftp-test-support` 和 `tokio`，不包含 russh，因此倾向方案 B |
| ConnectionManager / SharedConnection 是否 crate 公开 | 仅 `pub mod pool`（未 re-export 到 crate 根），测试需 `use macsftp_sftp::pool::{ConnectionManager, SharedConnection}` |

---

## 4. 实施期间发现的三个既有生产级根因

> 仅修复 `E0061` / `E0308` 无法使 `real_session.rs` 通过。首次编译成功后，21 项测试中仍有 8 项发生运行时失败。这 8 项失败源于 `commit 37ceb63` 重构中**未完整实现的 event 发送逻辑**，而且与 actor 签名无关。因此，它们属于同一个 WIP 的遗留问题。

### 4.1 `runtime.rs` 重复发送 `TabConnecting`

**现象：** `runtime.rs` 在真实会话分支内，即 `get_or_connect` 之后、spawn 之前的约 411 行，以及 match 之后覆盖 real 和 mock 两种后端的约 529 行，**分别发送了一次 `TabConnecting`**。因此，真实会话会获得两条 `TabConnecting`。

**测试失败原因：** 测试 helper 依次执行 `next_runtime_event("TabConnecting")` 和 `next_runtime_event("TabConnected")`。第一次调用处理了 411 行产生的 `TabConnecting`，而第二次调用中的 `let _ =` 将 529 行产生的 `TabConnecting` 误认为 `TabConnected` 并忽略。随后，真正的 `TabConnected` 由 `wait_for_*_transfer_completion` 处理，因此触发 panic `unexpected ... event: TabConnected`。

**影响范围：** 该问题影响 `real_session.rs` 中的 5 项测试：`runtime_plans_and_executes_single_file_upload_and_download`、`runtime_executes_directory_upload_with_a_bounded_session_queue`、`runtime_streams_and_executes_directory_download`、`runtime_routes_read_dir_to_real_actor` 和 `tabs_browse_independently_after_another_tab_disconnects`。此外，该问题还影响 `runtime.rs` 自身的单元测试，其中单连接用例断言“恰好 1 个 TabConnecting”，双 tab 用例断言“恰好 2 个 TabConnecting”。这些单元测试此前同样因重复 event 而失败。

**修复：** 删除约 411 行仅存在于真实会话分支中的重复 send，并保留约 529 行覆盖 real 和 mock 的 send。后者表示“已 spawn actor，并开始连接”。修复后，UI 不会获得两次 connecting event，因此两类测试均通过。

### 4.2 `authenticate()` 在密钥加载失败时不发送 `AuthFailed` event

**现象：** `physical_connection.rs::authenticate` 仅在 `AuthResult::Failure` 分支调用 `send_async(AppEvent::AuthFailed)`，该分支表示密码或公钥被服务端拒绝。但是，`PrivateKey` 分支中的 `load_secret_key(...)` 失败时会通过 `?` 直接返回 `ConnectFailure::AuthFailed`，因此**不会发送任何 event**。

**测试失败原因：** `encrypted_key_with_wrong_passphrase_fails_cleanly` 和 `encrypted_key_without_passphrase_reports_encrypted` 预期获得 `AuthFailed` event。但是，密钥加载阶段失败时，event channel 中始终不存在该 event。因此，测试在 `next_event("AuthFailed")` 处等待至 channel 关闭，并触发 panic `event channel closed`。

**修复：** 在 `load_secret_key` 失败分支中发送与 `AuthResult::Failure` 分支完全一致的 `AppEvent::AuthFailed`，并使用相同的 `scope` / `AuthFailure`。该方案维持单一事实源，因此 `establish_physical_connection` 的所有调用方，包括 runtime 和 connect_session，均会获得一致的认证失败 event。

### 4.3 `connect_session` 失败时不发送 `TabDisconnected`

**现象：** `pool.rs::connect_session` 仅通过 flume 将 `get_or_connect` 的 `Err(failure)` 结果提供给调用方，**但不发送任何生命周期 event**。相比之下，`runtime.rs` 中对应的 join task 会在 TrustRejected、TrustTimeout 或 Connection 失败时发送 `TabDisconnected`，而 HostKeyMismatch 和 AuthFailed 不发送该 event。

**测试失败原因：** `rejecting_unknown_host_key_disconnects` 使用 `spawn_actor` → `connect_session` 流程。用户拒绝未知主机密钥后，连接以 `TrustRejected` 失败。但是，`connect_session` 不发送 `TabDisconnected`，因此测试在 `next_event("TabDisconnected")` 处等待至 channel 关闭并触发 panic。

**修复：** 在 `connect_session` 内部将 `result` 提供给 flume 之前，按照 `runtime.rs` 的相同失败映射发送 `TabDisconnected`：`TrustRejected` → `UserRequested`；`TrustTimeout` → 对应 Error；`Connection(e)` → `Error(e)`；`HostKeyMismatch` / `AuthFailed` 不发送。这样，`connect_session` 与 runtime 会产生完全一致的 event sequence。

### 4.4 小结

| 失败测试 | 根因 | 修复文件 |
|----------|------|----------|
| 5 项 runtime_* | `runtime.rs` 重复发送 `TabConnecting`（411 行） | `crates/sftp/src/runtime.rs` |
| `encrypted_key_with_wrong_passphrase_fails_cleanly` | `authenticate` 密钥加载失败时不发送 `AuthFailed` | `crates/sftp/src/physical_connection.rs` |
| `encrypted_key_without_passphrase_reports_encrypted` | 同上 | 同上 |
| `rejecting_unknown_host_key_disconnects` | `connect_session` 失败时不发送 `TabDisconnected` | `crates/sftp/src/pool.rs` |

> 上述三个问题均由 `commit 37ceb63` 的 Connection Pool WIP 未完整实施造成，而且与多窗口无关。修复后，`real_session` 集成测试 21/21 通过，`sftp` crate 单元测试 52/52 通过。
