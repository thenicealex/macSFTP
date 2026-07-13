# 遗留问题分析与修复方案（2026-07-13）

> 范围：仅分析与 `commit 37ceb63`（`feat: implement ssh multiplexing via Connection Pool (Phase 1 & 2)`）相关的两处预先存在、与多窗口无关的遗留问题。
> 验证依据：实际读取 `crates/sftp/src/{session_actor.rs, physical_connection.rs, pool.rs, runtime.rs, sftp.rs}`、`crates/sftp/tests/real_session.rs`、`crates/sftp/Cargo.toml`，并运行 `cargo clippy -p macsftp-sftp --all-targets` 与 `cargo test -p macsftp-sftp --test real_session` 取得真实报错。

---

## 0. 背景

`commit 37ceb63` 把原本 `RemoteSessionActor` 内部的"建连 + 握手 + 主机密钥信任 + 认证 + 开 SFTP 子系统"逻辑，上移到 `physical_connection.rs`（物理连接建立）与 `pool.rs`（`ConnectionManager` / SSH 多路复用）中。

重构带来了两个未收尾的残留：

1. **3 个死函数 warning**（编译器已报 `dead_code`）；
2. **`real_session.rs` 集成测试编译失败**（actor 构造函数签名变更，且 `EventReceiver` 由 flume 改为 tokio broadcast 导致 `&mut self`）。

这两处都与多窗口特性无关，是 Connection Pool WIP 的"半成品接线"遗留。

---

## 1. 问题一：SFTP 模块中 3 个死函数 warning

### 1.1 现象

`cargo clippy -p macsftp-sftp --all-targets` 报告 3 个 `dead_code`：

| # | 函数 | 位置 | 当前状态 |
|---|------|------|----------|
| 1 | `subsystem_error` | `crates/sftp/src/physical_connection.rs:193` | `fn` 私有，**0 调用** |
| 2 | `private_key_error` | `crates/sftp/src/physical_connection.rs:204` | `fn` 私有，**0 调用** |
| 3 | `lock_store` | `crates/sftp/src/session_actor.rs:62` | `fn` 私有，**0 调用** |

此外还有 **2 个"隐性死代码"**（被 `#[cfg(any())]` 编译排除，因此不报警，但同样是重复副本）：

| # | 函数 | 位置 |
|---|------|------|
| 4 | `subsystem_error` | `crates/sftp/src/session_actor.rs:2400`（位于 `#[cfg(any())]` 区块 2387–2433） |
| 5 | `private_key_error` | `crates/sftp/src/session_actor.rs:2412`（同区块） |

### 1.2 来源与用途分析

这些函数**不是"未完成的功能"，而是重复副本**。Connection Pool 重构后，等价（乃至完全相同）的错误构造逻辑已经存在于活代码路径中，并且签名、文案、错误码都一致。逐项核对：

| 死函数 | 取代它的活代码（已存在、已使用） | 结论 |
|--------|----------------------------------|------|
| `lock_store`（session_actor:62） | `physical_connection.rs:19` `pub fn lock_store`，被 `physical_connection.rs:68 / 86 / 135` 调用（新连接路径用它锁 `known_hosts`）。session_actor 现在接收已建好的 `SharedConnection`，不再直接锁 store。 | **纯冗余**，删除不影响任何活调用 |
| `subsystem_error`（physical_connection:193） | `physical_connection.rs:182` `pub fn connection_error` + `pool.rs::get_or_connect` 内部 inline 构造 `UserFacingError::new(ErrorCode::ChannelClosed, "Could not open an SSH channel." …)`（见 `runtime.rs` 428–434 段）。新路径直接构造等价错误。 | **纯冗余** |
| `private_key_error`（physical_connection:204） | `physical_connection.rs` 内 `authenticate` 自行构造 `AuthFailed` 错误；原 session_actor:746 的调用点现在处于 `#[cfg(any())]` 死区块（2387–2433）内。 | **纯冗余** |

**误判风险排查**：已用 `grep -rnE` 全仓确认三个被报函数的调用方数量均为 0（除 `physical_connection.rs:19` 的 `pub fn lock_store` 外，另两个死函数无任何调用）。删除不会破坏活代码。

**结论 —— 应"删除"而非"完成实现"**：连接建立的错误处理已由 `connection_error` + inline `UserFacingError::new` 完整覆盖，错误语义（同 `ErrorCode`、同文案）与死函数完全一致。不存在需要补全的缺失功能，因此正确处置是**移除死代码**。这也符合 `AGENTS.md` 的"无未使用代码"原则。

### 1.3 修复方案（删除）

**A. `crates/sftp/src/physical_connection.rs`**
- 删除 `subsystem_error` 整段（约 192–219 行）。
- 删除 `private_key_error` 整段（约 203–235 行）。

**B. `crates/sftp/src/session_actor.rs`**
- 删除 `lock_store` 整段（`fn lock_store { … }`，约 60–66 行）。
- 删除 `#[cfg(any())]` 死区块中的两个重复函数（2387–2433 内，含 `connection_error` / `subsystem_error` / `private_key_error` 三个重复副本）。
  - 注意：该 `#[cfg(any())]` 区块（2387–2433）是旧 connect/auth 逻辑的整块残留，建议**整块删除**更干净；若保守处理，至少删 2400 与 2412 两个函数定义。
  - 删除后若 2434 行残留一个无后继项的 `#[cfg(any())]` 属性，请一并删掉该属性行，确保 `#[cfg(test)] mod tests`（2437）正常参与编译。

**C. 验证**
```bash
cargo clippy -p macsftp-sftp --all-targets 2>&1 | grep -i "dead_code"   # 应无输出
```
预期 3 个 `dead_code` warning 全部消失。

---

## 2. 问题二：`real_session.rs` 集成测试编译失败

### 2.1 现象

`cargo test -p macsftp-sftp --test real_session` 报 2 个 error：

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

**旧签名（10 参 —— actor 自己完成建连）**，即测试当前调用的形式（`real_session.rs:68`）：

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

**新签名（7 参 —— 接收已建好的连接）**，`session_actor.rs:269`：

```rust
pub fn new(
    tab_id: TabId,
    session_id: SessionId,
    session_epoch: u64,
    shared_connection: Arc<SharedConnection>,   // 由 ConnectionManager 建好
    sftp: russh_sftp::client::SftpSession,       // 由 shared.handle 新开的 SFTP 通道
    event_tx: flume::Sender<AppEvent>,
    next_conflict_id: Arc<AtomicU64>,
)
```

**设计意图**：连接建立已上移到 Connection Pool。
- `ConnectionManager::get_or_connect(...)`（`pool.rs:41`）负责建连、握手、主机密钥信任、认证、并开一次 SFTP 子系统，返回 `broadcast::Receiver<Result<Arc<SharedConnection>, ConnectFailure>>`；`SharedConnection`（`pool.rs:17`）含 `handle: client::Handle<ClientHandler>` 与 `remote_root`。
- 调用方再从 `shared.handle` 新开一个 SFTP 通道得到 `SftpSession`，连同 `shared` 一起交给 actor（见 `runtime.rs:418–444` 的规范写法）。

**E0308 的次生原因**：`commit 37ceb63` 把 `EventReceiver` 从 flume 包装改为 tokio broadcast 包装，`recv` 变为 `&mut self`（`runtime.rs:109`：`pub async fn recv(&mut self) -> Option<AppEvent>`）。测试里 `wait_for_runtime_transfer_completion(events: &EventReceiver)`（`:128`）未同步改成 `&mut`，导致在 `:131` 传 `&events` 时类型不匹配。
- 注意：其余 runtime 集成测试（`:815` 起）都已用 `let mut events = controller.event_receiver(); … &mut events`，所以**只有** `wait_for_runtime_transfer_completion` 这一处漏改。

### 2.3 受影响调用点

| 位置 | 问题 | 修复方向 |
|------|------|----------|
| `real_session.rs:68` `RemoteSessionActor::new(...)` | 传了 10 个旧参数 | 改为 7 参 + 调用前先经 `ConnectionManager` 建连 |
| `real_session.rs:128` `wait_for_runtime_transfer_completion(events: &EventReceiver)` | 形参应为 `&mut EventReceiver` | 改签名 + 调用点 `:860`、`:887` 的 `&events` → `&mut events`（这些 `events` 已是 `let mut events = …`） |

### 2.4 修复方案（两套，二选一）

#### 方案 B（推荐：干净、不新增 dev-dep）

在 `ConnectionManager` 上新增一个**公开方法** `connect_session`，内部完成"接收 `SharedConnection` → 从 `shared.handle` 新开 SFTP 通道 → 返回 `(Arc<SharedConnection>, SftpSession)`"。其逻辑直接复用 `runtime.rs:418–444` 已有的那段。返回类型：

```rust
broadcast::Receiver<Result<(Arc<SharedConnection>, SftpSession), ConnectFailure>>
```

好处：
- 测试**无需接触 `russh`**（当前 `crates/sftp/Cargo.toml` 的 `[dev-dependencies]` 只有 `macsftp-test-support` 与 `tokio`，**没有** `russh` / `russh-sftp`），`russh` 继续留在 crate 内部。
- `runtime.rs` 也可改用它，消除重复逻辑。

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

> **关键不变量（避免死锁）**：`spawn_actor` 必须**立即返回** fixture，而不阻塞在 `rx.recv()` 上。连接建立（含主机密钥信任阻塞）发生在 `get_or_connect` 内部 spawned task 中，与测试通过 `next_event` 读取 `HostKeyUnknown` 并手动 `trust_registry.resolve(...)` 是**并发**的——现有测试（如 `known_host_unknown_prompts`）正是依赖这一"先建连、后手动信任"的流程，因此**不要在 `spawn_actor` 里自动 resolve 信任**。

#### 方案 A（最小改动：但需加 dev-dep）

不新增 API，测试直接调 `ConnectionManager::get_or_connect`，再在测试里手动 `shared.handle.channel_open_session()` 开 SFTP 通道。需要在 `crates/sftp/Cargo.toml` 的 `[dev-dependencies]` 增加：

```toml
russh = "0.62"
russh-sftp = "2.3"
```

并在测试顶部 `use russh::client;` + `use russh_sftp::client::SftpSession;`。

> 缺点：把 crate 内部依赖 `russh` 泄漏到集成测试；不如方案 B 整洁。仅在想保持 sftp 公开 API 不变时选用。

### 2.5 E0308 的精确修正

无论选 A 还是 B，都需同步修正：

```rust
// real_session.rs:128
- async fn wait_for_runtime_transfer_completion(events: &EventReceiver) -> TransferId {
+ async fn wait_for_runtime_transfer_completion(events: &mut EventReceiver) -> TransferId {

// real_session.rs:860 与 :887 的调用点
-     let _ = wait_for_runtime_transfer_completion(&events).await;
+     let _ = wait_for_runtime_transfer_completion(&mut events).await;
```
（调用处的 `events` 已是 `let mut events = controller.event_receiver();`，无需改声明。）

### 2.6 验证（实施结果）

```bash
cargo test -p macsftp-sftp --test real_session   # 编译错误消失，但初次实现后 21 项中 8 项运行时失败
```

编译错误（E0061 / E0308）确实如方案 B 预期消失。但初次跑通编译后，**仍有 8 项测试在运行时失败**，其根因超出了"actor 签名适配"本身，落在 `commit 37ceb63` 重构遗留的另外三处生产级缺陷上（详见 §4）。这三处修复后：

```bash
cargo test -p macsftp-sftp --test real_session   # 21 passed; 0 failed
cargo test -p macsftp-sftp --lib                  # 52 passed; 0 failed（含 runtime.rs 自身单测）
```

---

## 3. 综合建议

两个 issue 同源——都来自 `commit 37ceb63` 的 Connection Pool WIP 未完成收尾。

- **P0（零风险）**：删除 3(+2) 个死函数（§1.3）。纯删冗余，不改变任何行为。
- **P1**：按**方案 B** 修 `real_session.rs`（§2.4 的 `connect_session` + §2.5 的 E0308 一行修正）。
- **修完后的 gate 状态**：这两处修好后，`macsftp-sftp` 的 `dead_code` / `E0061` / `E0308` 三类错误会全部消失。但需注意——`scripts/check.sh` 当前在 `sftp` 上仍可能因**其它**预先存在的问题而红（与本任务无关，未纳入本次范围）：
  - `session_actor.rs:362` 处有一个 `unwrap`，被 `AGENTS.md` 规定的 `unwrap_used` deny lint 拦截；
  - `sftp` 还有其它 clippy warning（非 dead_code 类）。
  
  若目标是让 `scripts/check.sh` 整体转绿，需另行处理这些独立遗留，但不在本次"两处遗留"的修复范围内。

---

## 附：关键事实速查（已实测）

| 项 | 实测结果 |
|----|----------|
| 死函数调用方 | `subsystem_error`/`private_key_error`(physical_connection) / `lock_store`(session_actor) 均为 0 调用 |
| 活副本 | `physical_connection.rs:19` `pub fn lock_store`；`:182` `pub fn connection_error`；`pool.rs` `get_or_connect` inline 错误 |
| 新 actor 签名 | `session_actor.rs:269` 7 参：`(tab_id, session_id, session_epoch, Arc<SharedConnection>, SftpSession, event_tx, Arc<AtomicU64>)` |
| 规范建连写法 | `runtime.rs:400–444`（get_or_connect → 收 SharedConnection → 开 SFTP 通道 → new → run） |
| E0308 漏改点 | 仅 `wait_for_runtime_transfer_completion`（`real_session.rs:128`，调用点 `:860`/`:887`） |
| sftp dev-deps | 仅 `macsftp-test-support` + `tokio`（无 russh）→ 倾向方案 B |
| ConnectionManager / SharedConnection 是否 crate 公开 | 仅 `pub mod pool`（未 re-export 到 crate 根），测试需 `use macsftp_sftp::pool::{ConnectionManager, SharedConnection}` |

---

## 4. 实施补充发现：三项被忽略的生产级根因（§2 编译修好后才暴露）

> 仅修 `E0061`/`E0308` 不足以让 `real_session.rs` 通过。初次编译通过后 21 项里仍有 8 项运行时失败。这 8 项失败指向 `commit 37ceb63` 重构中**未收尾的事件发射逻辑**，与 actor 签名无关，属于同一 WIP 的"接线半成品"。

### 4.1 `runtime.rs` 重复发射 `TabConnecting`（影响 5 项 runtime 测试 + runtime.rs 自身单测）

**现象**：`runtime.rs` 在真实会话分支内（`get_or_connect` 之后、spawn 之前，约 411 行）与 match 之后（约 529 行，覆盖 real + mock 两种后端）**各发了一次 `TabConnecting`**。真实会话因此收到两条 `TabConnecting`。

**为什么让测试失败**：测试辅助函数连续 `next_runtime_event("TabConnecting")` → `next_runtime_event("TabConnected")` 时，第一条吃掉了 411 行的 `TabConnecting`，第二条 `let _ =` 又把 529 行的 `TabConnecting` 当 `TabConnected` 丢弃；真正的 `TabConnected` 则落到之后的 `wait_for_*_transfer_completion` 里 → panic `unexpected ... event: TabConnected`。

**影响范围**：不仅 `real_session.rs` 的 5 项（`runtime_plans_and_executes_single_file_upload_and_download`、`runtime_executes_directory_upload_with_a_bounded_session_queue`、`runtime_streams_and_executes_directory_download`、`runtime_routes_read_dir_to_real_actor`、`tabs_browse_independently_after_another_tab_disconnects`），也包括 `runtime.rs` 自身单测里断言"恰好 1 个 TabConnecting"（单连）与"恰好 2 个 TabConnecting"（双 tab）的用例——这些单测此前同样因重复事件而失败。

**修复**：删除 411 行那次（仅真实分支内的）重复发射，保留 529 行那次（覆盖 real + mock，且语义为"已 spawn actor、开始连接"）。修复后生产行为正确（UI 不再收到两次 connecting），两类测试同时转绿。

### 4.2 `authenticate()` 密钥加载失败不发 `AuthFailed` 事件（影响 2 项 encrypted_key 测试）

**现象**：`physical_connection.rs::authenticate` 只在 `AuthResult::Failure`（密码/公钥被服务端拒绝）分支 `send_async(AppEvent::AuthFailed)`；而 `PrivateKey` 分支里 `load_secret_key(...)` 失败时直接 `?` 返回 `ConnectFailure::AuthFailed`，**不发射任何事件**。

**为什么让测试失败**：`encrypted_key_with_wrong_passphrase_fails_cleanly` / `encrypted_key_without_passphrase_reports_encrypted` 期望收到 `AuthFailed` 事件，但密钥加载阶段失败时事件通道永无该事件 → 测试在 `next_event("AuthFailed")` 处等到通道关闭而 panic `event channel closed`。

**修复**：在 `load_secret_key` 失败分支补充与 `AuthResult::Failure` 分支完全一致的 `AppEvent::AuthFailed` 发射（同样的 `scope`/`AuthFailure`）。这是单一事实源式的正确修复：`establish_physical_connection` 的任一调用方（runtime 与 connect_session）都会因此收到一致的认证失败事件。

### 4.3 `connect_session` 失败时不发射 `TabDisconnected`（影响 `rejecting_unknown_host_key_disconnects`）

**现象**：`pool.rs::connect_session` 把 `get_or_connect` 的结果 `Err(failure)` 仅通过 flume 转发给调用方，**不发射任何生命周期事件**。而 `runtime.rs` 的对应 join task 在失败时（TrustRejected/TrustTimeout/Connection）会发射 `TabDisconnected`（HostKeyMismatch/AuthFailed 则不发）。

**为什么让测试失败**：`rejecting_unknown_host_key_disconnects` 走 `spawn_actor` → `connect_session`，用户拒绝未知主机密钥后连接以 `TrustRejected` 失败；因 `connect_session` 不补发 `TabDisconnected`，测试在 `next_event("TabDisconnected")` 处等到通道关闭而 panic。

**修复**：在 `connect_session` 内部、把 `result` 转发给 flume 之前，按 `runtime.rs` 的 same 失败映射补发 `TabDisconnected`（`TrustRejected`→`UserRequested`；`TrustTimeout`→对应 Error；`Connection(e)`→`Error(e)`；`HostKeyMismatch`/`AuthFailed`→不发）。这样 `connect_session` 与 runtime 产生完全一致的事件流。

### 4.4 小结

| 失败测试 | 根因 | 修复文件 |
|----------|------|----------|
| 5 项 runtime_* | `runtime.rs` 重复 `TabConnecting`（411 行） | `crates/sftp/src/runtime.rs` |
| `encrypted_key_with_wrong_passphrase_fails_cleanly` | `authenticate` 密钥加载失败不发 `AuthFailed` | `crates/sftp/src/physical_connection.rs` |
| `encrypted_key_without_passphrase_reports_encrypted` | 同上 | 同上 |
| `rejecting_unknown_host_key_disconnects` | `connect_session` 失败不发 `TabDisconnected` | `crates/sftp/src/pool.rs` |

> 上述三处均为 `commit 37ceb63` Connection Pool WIP 的接线遗漏，与多窗口无关。修复后 `real_session` 集成测试 21/21 通过，`sftp` crate 单测 52/52 通过。
