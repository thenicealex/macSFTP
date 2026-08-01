# macSFTP GPUI + russh 工程规划

本文档记录 macSFTP 从零开发 Rust 桌面产品的工程方案。目标是使用 GPUI 构建 Zed 风格的高性能自绘 macOS UI，使用 `russh + russh-sftp` 构建异步 SFTP 后端，并从第一版开始支持多标签。

## 1. 决策摘要

### 已确定

- UI 框架：GPUI。
- 产品形态：macOS 桌面应用，不上架，不做签名/公证作为第一版目标。
- UI 风格：Zed 风格，高性能自绘 UI。
- SFTP 后端：`russh + russh-sftp`，不使用 `ssh2`。
- 项目性质：从零开发 Rust 产品。
- 窗口模型：第一版只做单窗口，多窗口是后续能力。**（2026-07-13 更新：多窗口已作为 post-MVP 能力交付 —— Cmd+N / File › New Window 打开独立窗口，各窗口独立标签页、共享传输列表与设置。详见 `docs/progress-analysis-2026-07-13-multiwindow.md`。）**
- 第一版认证：密码、私钥。
- Host key policy：OpenSSH-compatible。
- known_hosts 兼容目标：第一版读写 OpenSSH-compatible 子集，完整 OpenSSH grammar 不作为 MVP 承诺。
- 多标签：第一版核心能力。
- session 模型：浏览 session 跟随 tab；传输 session 由全局 TransferManager 独立管理。
- 冲突处理：弹出 UI，让用户选择覆盖、跳过、重命名。
- 元数据：保留权限、mtime、symlink。
- 关闭 app 策略：运行中的传输随进程结束；不跨启动恢复传输目录或任务。
- 不做：目录同步、远程编辑、App Store、签名公证、S3/WebDAV/FTP。

### 主要取舍

`russh + russh-sftp` 比 `ssh2` 更适合多标签和异步并发，但需要自己承担更多应用层责任：

- host key 校验要在 `russh::client::Handler::check_server_key` 中接入；
- session lifecycle、重连、取消、超时需要应用层设计；
- GPUI 主线程和 Tokio runtime 之间必须有明确 command/event 边界；
- API 版本需要 pin，避免 GPUI 和 russh 同时变化造成大面积返工。

## 2. 成功标准

第一版 MVP 完成时，应满足以下可验证目标：

1. 应用启动后显示 GPUI 主窗口，包含多标签栏、本地文件 pane、远端文件 pane、传输队列区域。
2. 用户可以创建、关闭、切换多个远端 tab。
3. 每个 tab 可以独立使用密码或私钥连接 SFTP 服务器。
4. 未知 host key 会弹出信任确认；host key mismatch 会阻断连接。
5. 任意一个 tab 断线不影响其他 tab。
6. 用户可以浏览本地目录和远端目录。
7. 远端 10k 文件列表仍能流畅滚动、选择、排序。
8. 用户可以上传、下载、取消、重试传输任务。
9. 传输冲突时弹出覆盖、跳过、重命名选择；批量传输支持 apply-to-all。
10. 上传/下载尽力保留权限、mtime、symlink。
11. tab 关闭后，已进入全局传输队列的任务继续执行。
12. 核心状态机和 SFTP adapter 有自动化测试覆盖。

## 3. 非目标

这些能力不进入第一版，避免范围失控：

- 目录同步。
- 远程编辑。
- 内置终端。
- 多协议支持，包括 FTP、S3、WebDAV。
- App Store 发布。
- 自动更新。
- ~~多窗口。~~ **→ 已于 2026-07-13 作为 post-MVP 能力交付（见 §1 更新说明）。**
- 完整 OpenSSH config 解析。
- 完整 OpenSSH known_hosts grammar。
- jump host / ProxyCommand。
- keyboard-interactive 完整交互认证。
- ssh-agent 支持。
- uid/gid 恢复。
- 完整 VoiceOver/accessibility 达标。

注意：keyboard-interactive 在真实环境中很常见，很多服务器把密码认证包装成 keyboard-interactive。第一版不承诺完整交互流程，但认证模块需要保留扩展点。

## 4. Workspace 结构

建议使用 Cargo workspace：

```text
macsftp/
  Cargo.toml
  crates/
    app/
    ui/
    core/
    sftp/
    storage/
    platform/
    test_support/
```

### `crates/app`

GPUI 应用入口和窗口层。

职责：

- `Application` 启动；
- 主窗口创建；
- tab bar；
- file browser panes；
- transfer drawer；
- modal layer；
- command palette；
- keyboard actions；
- GPUI tests。

限制：

- 不直接调用 `russh`；
- 不直接访问 Keychain；
- 不包含 transfer planning 业务逻辑。

workspace 的渲染实现按产品 surface 分文件：`render.rs` 保留 tab、pane 和 About
等主工作区骨架，`settings_render.rs` 只负责设置与 profile 编辑，
`transfer_render.rs` 只负责传输 drawer、job row 和状态栏。三者仍是同一个
`WorkspaceView` 的实现，不引入额外 view state 或跨层抽象。

### `crates/ui`

GPUI reusable components 和主题。

职责：

- Zed 风格 theme token；
- button、icon button、input、select、tabs；
- table header；
- virtualized file list row；
- modal/dialog；
- progress row；
- empty/error/loading states。

限制：

- 只做表现和轻量 interaction；
- 不持有远端 session；
- 不知道 SFTP 协议细节。

### `crates/core`

纯业务模型和状态机。不能依赖 GPUI、russh、Tokio runtime 实现。

职责：

- `TabState`；
- `TransferState`；
- `RemoteEntry` / `LocalEntry`；
- `Command` / `Event`；
- conflict resolution；
- sorting/filtering；
- transfer planning；
- error taxonomy。

这个 crate 是稳定核心。即使换 UI 或换 SFTP 库，`core` 也应尽量不动。

### `crates/sftp`

`russh + russh-sftp` adapter。

职责：

- Tokio runtime 管理；
- `RemoteSessionActor`；
- `TransferManager`；
- password/private key auth；
- host key callback；
- SFTP session 创建；
- directory listing；
- file upload/download；
- symlink；
- metadata set/get；
- cancellation。

限制：

- 不依赖 GPUI；
- 通过 `core::Command` 和 `core::Event` 与 app 通信。

### `crates/storage`

本地持久化。

职责：

- profiles；
- app known_hosts；
- recently used paths；
- window layout；
- Keychain secret references；
- migration。

敏感信息不写入普通配置文件。

profile 持久化内部按职责拆分：`profile_file.rs` 只处理 `profiles.json` 的解析与
原子写入，`profiles.rs` 负责 `ProfileStore`、Keychain 协调和事务语义；
`storage.rs` 只组装模块并重导出现有公共 API。调用方不能绕过 `ProfileStore`
分别修改 profile 文件和 secret。

### `crates/platform`

macOS/local filesystem 边界。

职责：

- app data/config/cache/log 路径；
- local directory scan；
- chmod/mtime/symlink helpers；
- atomic local file replace；
- macOS bundle 辅助。

### `crates/test_support`

测试辅助。

职责：

- fixture directory trees；
- fake command/event sinks；
- Docker/OpenSSH integration test harness；
- test key generation；
- metadata assertion helpers。

## 5. 分层依赖

依赖方向必须保持单向：

```text
app -> ui
app -> core
app -> storage
app -> sftp

ui -> core

sftp -> core
sftp -> storage

storage -> core
platform -> core
test_support -> core
```

禁止：

- `core` 依赖 GPUI；
- `core` 依赖 russh；
- `ui` 直接发起网络请求；
- `sftp` 直接操作 GPUI Entity；
- view state 存放长期业务状态。

## 6. 运行时模型

GPUI 主线程负责 UI 和同步状态更新；Tokio runtime 负责网络和文件传输。

```text
GPUI main thread
  AppModel
  TabStore
  TransferStore
  ModalStore
        |
        | Command
        v
Tokio runtime
  RuntimeController
  RemoteSessionActor per connected tab
  TransferManager
        |
        | Event
        v
GPUI main thread
```

设计原则：

- GPUI 主线程不 await 网络；
- 网络层不直接 mutate UI；
- 所有跨线程通信走 command/event；
- event 进入 GPUI 后只更新 Entity state，再 `cx.notify()`；
- 长耗时本地文件扫描也不能阻塞 GPUI 主线程。

### ADR-002: GPUI <-> Tokio 桥接

背景：

- GPUI 有自己的 executor 和 `Task` 模型；
- `russh` 是 Tokio-native，必须运行在 Tokio runtime 上；
- Tokio task 不能直接持有 GPUI `Context`，也不能直接调用 `cx.notify()`。

决策：

- `crates/sftp` 暴露一个 `RuntimeClient`，隐藏 channel 和 Tokio 细节；
- `RuntimeController` 拥有 `tokio::runtime::Runtime`，并在 drop 时先发送 shutdown command，再做 bounded shutdown；
- GPUI -> Tokio 使用 bounded command channel；
- Tokio -> GPUI 使用 bounded event channel；
- channel 类型优先使用 `flume`，因为它在 GPUI executor 和 Tokio runtime 两侧都能工作，不要求接收端运行在 Tokio 上；
- channel 容量初始值：commands 256，events 1024；
- GPUI 侧发送 command 必须使用 `flume::Sender::try_send`，禁止在主线程调用阻塞 `send`；
- Tokio 侧发送 event 使用 `send_async`，让背压传导到 actor，但 progress event 必须先节流或合并；
- 唯一的 event drain task 由进程级 `AppEventCoordinator` 生命周期持有，在 GPUI executor 上运行；`RuntimeController::take_event_receiver()` 只能交出一次 receiver，禁止每窗口各自消费或订阅 runtime event；
- event drain task 收到 event 后进入 GPUI update closure：transfer 与 residual-temp 事件只更新一次进程级状态，tab/window 事件再分发到各 `Workspace` 并由 core stale-event guard 过滤；
- event drain task 只允许 await `flume::Receiver::recv_async()`，不能 await 网络、文件 IO 或其他可能阻塞 UI 的操作；
- Tokio task 只发送 `AppEvent`，不能持有任何 GPUI handle。

边界草图：

```text
GPUI action
 -> RuntimeClient::send(command)
 -> bounded command channel
 -> RuntimeController task on Tokio
 -> RemoteSessionActor / TransferManager
 -> bounded event channel
 -> process-wide AppEventCoordinator
 -> TransferStore / persistence update exactly once
 -> window-scoped Workspace update
 -> cx.notify() / refresh windows
```

背压策略：

- command channel 满时，用户动作类 command 返回错误并在 UI 显示轻量 warning；
- 低优先级 command，如重复 refresh、重复 progress poll，可以被合并或丢弃；
- event channel 满时由 bounded `flume` 背压生产者，不能覆盖或跳过未读事件；状态转换和终态事件绝不能丢；
- `TransferProgress` 必须节流，例如每个 transfer 最多 10 Hz 进入 UI。
- `TransferProgress` 节流必须在 TransferManager 发 event 前完成；目录 planning 的首个 child 立即发出，后续 child 在生产端按 128 个一批发送，完成事件前冲刷尾批。

shutdown 策略：

```text
App closing
 -> send AppCommand::Shutdown
 -> stop accepting new commands
 -> cancel browsing actors
 -> cancel active transfer jobs
 -> emit final events best-effort
 -> shutdown Tokio runtime with timeout
```

实现要求：

- `RuntimeController` 不能依赖 `tokio::runtime::Runtime` 的默认 `Drop` 行为；
- 显式调用 `runtime.shutdown_timeout(Duration::from_secs(3))`；
- host key、credential、transfer cancellation 等 pending request 在 shutdown 前统一 reject/cancel；
- shutdown timeout 后允许 runtime 强制释放，UI 只记录 warning，不继续等待。

不允许：

- 在 GPUI executor 上运行 `russh` future；
- 在 Tokio task 内调用 GPUI `cx.notify()`；
- 在 `Drop` 中无限等待 runtime shutdown；
- 使用 unbounded channel 传输 progress。

## 7. Command / Event 协议

### Command

```rust
pub enum AppCommand {
    OpenTab(OpenTabCommand),
    CloseTab { tab_id: TabId },
    ConnectTab(ConnectCommand),
    DisconnectTab { tab_id: TabId },
    ReadRemoteDir { tab_id: TabId, path: RemotePath },
    ReadLocalDir { tab_id: TabId, path: LocalPath },
    StartTransfer(StartTransferCommand),
    CancelTransfer { transfer_id: TransferId },
    RetryTransfer { transfer_id: TransferId },
    ResolveTransferConflict(ConflictDecisionCommand),
    AcceptHostKey(HostKeyDecisionCommand),
    RejectHostKey { request_id: TrustRequestId },
    Shutdown,
}
```

关键 payload 草案：

```rust
pub struct ConnectCommand {
    pub tab_id: TabId,
    pub session_id: SessionId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub settings: ConnectionSettings,
}

pub struct StartTransferCommand {
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub direction: TransferDirection,
    pub sources: Vec<TransferEndpoint>,
    pub destination: TransferEndpoint,
    pub metadata_policy: MetadataPolicy,
    pub conflict_policy: ConflictPolicy,
}
```

`ConnectCommand` 的 session 身份由 UI/core 侧在发起连接时分配（`AppState`
持有单调递增的 session id 计数器；`TabState::begin_connect` 递增
epoch）。runtime 不再自行分配 session_id/epoch，而是原样携带到该
session 的所有 event 中——这保证陈旧事件防护（见下文）能在 UI 侧闭环。

`settings: ConnectionSettings` 是 Keychain-backed profile 落地前的过渡
字段（决策 21）：

```rust
pub struct ConnectionSettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthCredential,
}

pub enum AuthCredential {
    Password { password: String },
    PrivateKey { key_path: String, passphrase: Option<String> },
}
```

`ConnectionSettings`/`AuthCredential` 派生 `Zeroize`/`ZeroizeOnDrop`，
`Debug` 手写实现并对 `auth` 字段整体打码；`profile_id` 字段目前恒为
占位值，等 storage 的 Keychain-backed profile 落地后才会被真正解析
使用，`settings` 随之退场。

规则：

- `tab_id` 表示发起传输的 tab；
- `session_epoch` 用于判断该 tab 的 browsing session 是否仍可作为 borrow fallback；
- `profile_id` 用于 dedicated transfer 读取 profile 和 Keychain secret；
- 批量上传/下载通过 `sources` 表达，一个 command 创建一个 `TransferPlan`；
- command 创建时要复制必要的路径和策略，不能依赖后续 UI selection 状态。

### Event

```rust
pub enum AppEvent {
    TabOpened(TabSnapshot),
    TabClosed { tab_id: TabId },
    TabConnecting { tab_id: TabId },
    HostKeyUnknown(HostKeyPrompt),
    HostKeyMismatch(HostKeyMismatch),
    TabConnected { tab_id: TabId, remote_root: RemotePath },
    TabDisconnected { tab_id: TabId, reason: DisconnectReason },
    AuthFailed { tab_id: TabId, reason: AuthFailure },
    RemoteDirLoading { tab_id: TabId, path: RemotePath },
    RemoteDirLoaded(RemoteDirSnapshot),
    RemoteOperationFailed(RemoteOperationFailure),
    LocalDirLoaded(LocalDirSnapshot),
    TransferQueued(TransferSnapshot),
    TransferPlanning { transfer_id: TransferId },
    TransferConflict(TransferConflictPrompt),
    TransferRunning(TransferSnapshot),
    TransferProgress(TransferProgress),
    TransferCompleted { transfer_id: TransferId },
    TransferSkipped { transfer_id: TransferId },
    TransferFailed(TransferFailure),
}

pub struct TransferPlanProgress {
    pub plan_id: TransferPlanId,
    pub child_jobs: Vec<TransferJob>,
    pub planned_count: usize,
    pub total_bytes: Option<u64>,
}
```

上面的 enum 是语义示意。所有来自远端 session 的 event，例如 `TabConnected`、`RemoteDirLoaded`、`RemoteOperationFailed`、`TabDisconnected`，实际类型必须包含 `RemoteEventScope`。

### Correlation

每个 command 都应携带 correlation id 或可追踪 id。至少这些对象需要稳定 id：

- `TabId`
- `SessionId`
- `TransferId`
- `TrustRequestId`
- `ConflictRequestId`

这能避免用户快速切换 tab、关闭 tab、重连时旧事件污染新状态。

### 陈旧事件防护

每个 tab 持有一个单调递增的 `session_epoch`。每次发起连接，包括 reconnect，`session_epoch` 都递增。所有来自远端 session 的 event 必须携带：

```rust
pub struct RemoteEventScope {
    pub tab_id: TabId,
    pub session_id: SessionId,
    pub session_epoch: u64,
}
```

接收 event 时必须先校验：

```text
event.tab_id exists
AND event.session_epoch == current_tab.session_epoch
AND event.session_id == current connected/connecting session
```

不满足时直接丢弃，并写 debug log。这个逻辑放在 `core`，不要散落在 view 里。

必须覆盖的测试：

- tab 关闭后，旧 `RemoteDirLoaded` 不会重新创建 tab；
- reconnect 后，旧 session 的 `TabDisconnected` 不会覆盖新 session 的 `Connected`；
- host key modal 过期后，确认按钮不会作用到新连接；
- transfer event 不依赖 tab 存活，tab 关闭后仍可更新全局 `TransferStore`。

## 8. 多标签设计

### TabState

```rust
pub struct TabState {
    pub id: TabId,
    pub session_epoch: u64,
    pub title: String,
    pub profile_id: Option<ProfileId>,
    pub local: LocalPaneState,
    pub remote: RemotePaneState,
    pub connection: ConnectionState,
    pub sort: FileSort,
    pub selection: SelectionState,
    pub pending: Vec<PendingOperation>,
}
```

### ConnectionState

```rust
pub enum ConnectionState {
    Empty,
    Connecting {
        session_id: SessionId,
        session_epoch: u64,
    },
    AwaitingHostKey {
        session_id: SessionId,
        session_epoch: u64,
        request_id: TrustRequestId,
    },
    AwaitingCredentials {
        session_id: SessionId,
        session_epoch: u64,
        request_id: CredentialRequestId,
    },
    Connected {
        session_id: SessionId,
        session_epoch: u64,
        connected_at: Timestamp,
    },
    Reconnecting {
        session_id: SessionId,
        previous_session_id: SessionId,
        session_epoch: u64,
    },
    Disconnected {
        reason: DisconnectReason,
    },
    Failed {
        error: UserFacingError,
    },
}
```

### Tab 生命周期

```text
New
 -> Connecting
 -> AwaitingHostKey
 -> Connected
 -> Disconnected
 -> Reconnecting
 -> Connected
 -> Closed
```

关闭 tab 时：

1. UI 从 `TabStore` 移除 tab；
2. 发送 `DisconnectTab` 到 runtime；
3. runtime 取消该 tab 的 browsing actor；
4. 全局 TransferManager 中已开始的 job 不因 tab 关闭取消；
5. 与已关闭 tab 相关的迟到 event 被忽略。

### 每 tab 独立 session

第一版采用每 tab 独立 browsing session。

优点：

- 隔离性好；
- 一个 tab 卡住不影响其他 tab；
- 实现简单；
- 生命周期清晰。

缺点：

- 同一 host 多 tab 会建立多条 SSH 连接；
- 服务器连接数限制可能更容易触发。

后续可以增加 host-level session pool，但不要在 MVP 引入。

### ADR-003: 浏览 session 与传输 session

MVP 明确采用两类 session：

```text
BrowsingSession
  - per tab
  - 跟随 tab 生命周期
  - 负责 read_dir、refresh、轻量 rename/mkdir/delete

TransferSession
  - owned by TransferManager
  - 跟随 transfer job 生命周期
  - tab 关闭后仍可继续
  - 负责 upload/download 和 metadata preservation
```

实现约束（2026-07-18）：`TransferSession` 的独立性指 runtime 所有权、队列、取消域和
SFTP channel 独立；它可以持有已经认证的 SSH 物理连接 `ConnectionLease`，不要求为每个
plan 重做握手。BrowsingSession 关闭只释放自己的 lease，不能取消或关闭 TransferManager
持有的 lease。物理连接断开时，连接池必须按 generation 失效该 entry，并向所有 browsing
和 transfer lease 广播；后续 acquire 必须建立新连接，禁止复用死 handle。最后一个 lease
释放后连接进入有界 idle grace period，超时从池中移除。

落地加固（2026-07-18）：TransferManager 为进程级唯一 owner；成功或跳过的 job 立即
释放 retry route，可重试失败只保留 `Weak<SharedConnection>` 与非敏感 `ConnectionKey`，
不能阻止 idle eviction；用户重连后 Retry 从连接池取得新 generation。窗口通过 `SessionCoordinator` 登记
尚未释放的 tab，窗口关闭后用一条 `CloseTabs` 批量释放 browsing actor；普通 tab 关闭在
command channel 满时使用异步补发。物理断连 token 同时驱动连接池 generation 淘汰和
browsing actor 的 `ConnectionLost` 事件。

这个取舍优先保证：

- 大文件传输不阻塞目录浏览；
- tab 关闭后，已进入全局队列的传输可以继续；
- transfer retry 不需要恢复被关闭的 tab actor。

代价：

- 同一 host 可能出现多条 SSH 连接；
- hardened server 的连接数限制可能导致传输 session 创建失败；
- host key 和认证流程会在 transfer session 上再次执行。

缓解：

- host trust 决策按 `(host, port, key_fingerprint)` 缓存，浏览 session 信任后，同 host 的 transfer session 不再重复弹窗；
- TransferManager 做 per-host 并发限制；
- 如果服务器拒绝额外连接，fallback 为使用当前 tab 的 browsing session 执行传输，此时该 transfer 绑定 tab 生命周期，关闭 tab 会提示用户传输将取消。

这是一项显式架构取舍：MVP 默认选择传输独立性，必要时降级为少连接模式。

## 9. SFTP 后端设计

### RuntimeController

职责：

- 创建 Tokio runtime；
- 接收 `AppCommand`；
- 分发到 `RemoteSessionActor` 或 `TransferManager`；
- 将 `AppEvent` 送回 GPUI。
- 管理 `RemoteSessionActor` registry；
- 管理 host key / credential pending request registry；
- 处理 runtime shutdown；
- 执行 command 背压和低优先级 command 合并。

核心结构：

```rust
pub struct RuntimeController {
    command_rx: CommandReceiver,
    event_tx: EventSender,
    sessions: HashMap<TabId, RemoteSessionHandle>,
    trust_requests: TrustRegistry,
    credential_requests: CredentialRegistry,
    transfers: TransferManager,
}
```

### RemoteSessionActor

一个已连接 tab 对应一个 actor。

职责：

- 建立 TCP 连接；
- 执行 SSH handshake；
- host key 校验；
- password/private key 认证；
- 打开 SFTP subsystem；
- 处理远端目录浏览；
- 处理轻量远端操作，如 mkdir、rename、delete；
- keepalive；
- 断线通知。

### SFTP session 创建流程

```text
TcpStream::connect(host:port)
 -> russh::client::connect(config, stream, handler)
 -> handler.check_server_key(...)
 -> authenticate_password / authenticate_publickey
 -> channel_open_session
 -> request_subsystem("sftp")
 -> SftpSession::new(channel.into_stream())
 -> emit TabConnected
```

### TransferManager

全局传输管理器。

职责：

- 队列调度；
- 并发限制；
- 传输 session 创建；
- dedicated session 引用计数；
- 进度聚合；
- 取消；
- 重试；
- 冲突暂停和恢复；
- metadata preservation；
- 临时文件清理；
- transfer session lifecycle；
- transfer session fallback。

默认并发：

```text
global active transfers: 4
per host active transfers: 2
per tab active planning operations: 1
```

第一版不做高级 bandwidth limit，但数据结构应允许后续增加。

### TransferSession

传输 session 由 TransferManager 创建和持有：

```rust
pub struct TransferSessionKey {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_fingerprint: AuthFingerprint,
}

pub enum TransferSessionMode {
    Dedicated,
    BorrowBrowsingSession { tab_id: TabId, session_epoch: u64 },
}
```

`AuthFingerprint` 是认证配置指纹，不是 secret hash，也不能由密码/passphrase 原文派生。

建议语义：

```rust
pub struct AuthFingerprint {
    pub method: AuthMethodKind,
    pub secret_ref: Option<SecretRef>,
    pub private_key_path_hash: Option<String>,
    pub profile_revision: u64,
}
```

规则：

- password auth 使用 `SecretRef + profile_revision`；
- private key auth 使用已展开并持久化的 key path 的 SHA-256、passphrase `SecretRef`、`profile_revision`；
- 未保存的手工连接使用 UI/core 分配的 `SessionId` 作为临时连接池身份，只允许同一
  session 内复用，禁止不同连接尝试共享；
- 用户修改 profile 的 password、key path、passphrase 或 auth method 时，`profile_revision` 递增；
- `AuthFingerprint` 不包含 secret value，也不对 secret value 做 hash；
- `AuthCredential` / `ConnectionSettings` 不实现 `Hash`，防止后续重新引入 secret-derived key；
- TransferManager 只在 `TransferSessionKey` 完全一致时复用 session。

持久化写入统一使用同目录临时文件、`0600`、file `fsync`、atomic rename 和 parent
directory `fsync`。如果 config/profiles/session/recents/residual 文件损坏或版本不支持，应用可用
空内存状态继续启动，但该 store 禁止写回，避免在用户恢复原文件前静默覆盖。

默认使用 `Dedicated`。如果服务端拒绝额外 SSH 连接，且用户确认接受限制，则 fallback 到 `BorrowBrowsingSession`。

Dedicated session lifecycle：

- `TransferSessionKey` 相同的 active transfer 可以复用同一个 dedicated session；
- TransferManager 维护 per-session reference count；
- transfer 开始时 acquire session，完成、失败或取消时 release；
- 最后一个引用释放后关闭 session；
- shutdown 时忽略引用计数，统一取消 transfer 并关闭 session。

fallback 影响：

- transfer 会占用该 tab 的 browsing SFTP channel 或同一 SSH connection 下的新 channel；
- tab 关闭时必须提示传输将取消；
- 大文件传输期间目录浏览可能变慢。

认证路径：

- `Dedicated`：TransferManager 从 profile 的 `SecretRef` 读取 Keychain secret，重新执行完整 SSH handshake、host key 校验和认证；
- `BorrowBrowsingSession`：复用当前 tab 已认证的 browsing session，不再读取 credential、不再执行认证；
- 如果 credential 读取失败，dedicated transfer 进入 retryable failed；
- 如果 borrow tab 已关闭或 epoch 不匹配，先回退到 dedicated；dedicated 也不可行时才进入 failed。

Borrow 模式前置验证：

- 必须在 M5 前验证 `russh-sftp` 在单 channel 或同一 SSH connection 下的并发请求能力；
- 如果单 channel 严格串行，borrow 模式只能用于用户明确接受“传输期间暂停浏览”的场景；
- 如果同一 SSH connection 可开独立 SFTP channel，则优先使用独立 channel，而不是复用同一个 SFTP channel。

## 10. Host Key 策略

### 文件策略

读取：

- `~/.ssh/known_hosts`
- app-owned known_hosts，例如 `~/Library/Application Support/macSFTP/known_hosts`

写入：

- 只写 app-owned known_hosts。

原因：

- 避免破坏用户 OpenSSH 配置；
- 仍可兼容用户已有信任；
- app 自己管理信任生命周期。

### 校验流程

```text
check_server_key(server_key)
 -> parse OpenSSH known_hosts
 -> match host + port + key
 -> Match: return true
 -> NotFound: create TrustRequest, emit HostKeyUnknown, wait user decision
 -> Mismatch: emit HostKeyMismatch and return false
 -> ParseFailure: emit failure and return false
```

### ADR-004: host key 回调与 UI 决策回传

`russh::client::Handler::check_server_key` 发生在 SSH handshake 中，必须返回是否接受 server key。因此未知 host key 的 UI 决策必须通过显式 request/response 机制回传给 runtime。

结构：

```rust
pub struct TrustRequest {
    pub id: TrustRequestId,
    pub tab_id: TabId,
    pub session_id: SessionId,
    pub session_epoch: u64,
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint_sha256: String,
    pub expires_at: Timestamp,
}
```

`TrustDecisionSender` belongs to the runtime registry entry, not to the `core` request payload. `core` only models the request identity and user-visible host key data.

流程：

```text
check_server_key
 -> known_hosts NotFound
 -> create oneshot response channel
 -> register TrustRequest in RuntimeController
 -> emit HostKeyUnknown
 -> await response with timeout
 -> Trust and Save: write app-owned known_hosts and return true
 -> Cancel / timeout / tab closed: return false
```

规则：

- `TrustRequestId` 和 `session_epoch` 必须绑定 modal；
- UI 只发送 `AcceptHostKey` 或 `RejectHostKey` command，不直接接触 oneshot sender；
- RuntimeController 收到 command 后从 `TrustRegistry` 找到 sender 并完成响应；
- request 默认 5 分钟超时，超时自动 reject 并清理 registry；
- tab 关闭或 reconnect 时，旧 trust request 立即 reject；
- transfer session 可复用已经保存的 host trust，不重复弹窗。

### Unknown host key UI

显示：

- host；
- port；
- key algorithm；
- SHA256 fingerprint；
- target known_hosts file；
- warning copy。

操作：

- Trust and Save；
- Cancel。

不建议第一版提供 Accept Once。它容易让用户误解为已保存，且会让重连行为变得不稳定。

### Mismatch policy

host key mismatch 必须阻断，不提供一键覆盖。

用户若确实需要替换，应进入后续的 known_hosts 管理界面。MVP 可以先给出错误说明和文件路径。

### known_hosts MVP 支持子集

MVP 目标是 OpenSSH-compatible 子集，而不是完整 OpenSSH grammar。

读取支持：

- app-owned known_hosts 中由 macSFTP 写入的明文 host entry；
- `host` 和 `[host]:port`；
- 常见 key type：`ssh-ed25519`、`ecdsa-sha2-nistp256`、`rsa-sha2-*`/`ssh-rsa`；
- 注释行和空行。

读取策略：

- 无法解析的行按行忽略；
- 写 `WARN` log，包含文件路径和行号，不包含整行原文；
- 单行解析失败不能导致整个 known_hosts 文件失效；
- OpenSSH `|1|` hashed host entry 使用 HMAC-SHA1 按 host pattern 匹配；不能通过解码恢复 hostname。

写入支持：

- 只写 app-owned known_hosts；
- 写明文 host 或 `[host]:port`；
- 不写 hashed host。

MVP 不承诺：

- wildcard / negated pattern；
- 一行多个 host pattern；
- `@cert-authority` CA 验证；
- OpenSSH certificate host keys；
- FIDO/U2F `sk-*` host key 完整兼容。

实现优先使用现成 crate，例如 `ssh-key` 的 `known_hosts` 能力。只有在确认 crate 不满足 MVP 子集时，才写小型解析器；禁止自己实现完整 OpenSSH parser。

## 11. 认证设计

### Profile

```rust
pub struct ConnectionProfile {
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMethod,
    pub default_remote_path: Option<RemotePath>,
}

pub enum AuthMethod {
    Password {
        secret_ref: SecretRef,
    },
    PrivateKey {
        key_path: LocalPath,
        passphrase_ref: Option<SecretRef>,
        remember_passphrase: bool,
    },
}
```

`revision` 规则：

- `revision` 从 1 开始；
- storage 层在 `save_profile` 时自动递增；
- 修改 host、port、username、auth method、private key path、secret ref、默认路径等会影响连接或传输行为的字段时必须递增；
- 只修改 UI-only 字段是否递增由 storage policy 统一决定，不能由调用方手动传入；
- `AuthFingerprint.profile_revision` 直接来自 `ConnectionProfile.revision`。

### Secret 存储

敏感字段进入 macOS Keychain：

- password；
- private key passphrase。

普通配置文件只存 `SecretRef`。

实现选型：

- macOS 第一版优先使用 `security-framework` crate 直接访问 Keychain；
- `security-framework` 只作为 macOS target dependency 编译；Linux CI 只允许显式的
  memory backend，调用 OS Keychain 必须返回 platform-unavailable，禁止静默降级；
- secret value 在内存中使用可清零容器，例如 `zeroize`；
- log 和 error 里只能出现 `SecretRef`，不能出现 secret value；
- passphrase 默认可记住到 Keychain，但 profile 里必须保存 `remember_passphrase`，用户可关闭。

**2026-07-14 边界收口：** `ProfileStore` 是 profile 文件与 Keychain 生命周期的
唯一协调者，`app` 不持有或调用 `KeychainStore`。Connect 与 Settings 分别通过
`save_connection_settings` / `save_request` 进入同一事务：先备份并写入新 secret，
`profiles.json` 原子提交成功后才替换内存并清理旧认证方式的 orphan；磁盘提交失败
时恢复原 Keychain 值。删除同样先提交 profile 文件，再 best-effort 清理无引用
secret，并把清理失败作为 warning 返回调用方。

**2026-07-14 内存与诊断收口：** password/passphrase 表单使用不可克隆的
`SecretInputState`；替换、删除、清空和 drop 都会清零旧字符串缓冲区。输入组件的
`Debug` 始终脱敏。第三方 SSH/IO 错误原文不直接进入 `UserFacingError` 或认证日志；
私钥诊断最多记录文件名，不记录完整路径。

**2026-07-15 RSA 边界策略：** `russh` 的完整 RSA feature 保持关闭，RSA 客户端私钥
会在认证前被明确拒绝；建议使用 Ed25519 或 ECDSA。原因是当前传递的 RustCrypto
`rsa` 仍受 RUSTSEC-2023-0071 timing advisory 影响。为兼容只提供 RSA host key 的
堡垒机，仓库维护最小 `russh` 补丁：只使用 AWS-LC 验证服务端 `rsa-sha2-256/512`
签名，不编译 RustCrypto RSA 私钥操作，也不启用 SHA-1 `ssh-rsa`。为兼容系统
OpenSSH 默认仍接受的旧堡垒机，RSA host key 下限为 1024 bit；低于 2048 bit 必须记录
legacy WARN。客户端 host key 协商列表必须与此边界一致；Cargo feature、算法列表和
RSA 私钥拒绝测试共同作为门禁。

**2026-07-15 macOS 本地网络隐私：** macOS 15+ 会把 GUI 应用直连私网地址纳入
Local Network Privacy。bundle 必须声明非空 `NSLocalNetworkUsageDescription`；打包脚本与
CI 对最终 `Info.plist` 做门禁。SFTP adapter 只按 `russh::Error` 的结构分类，不复制第三方
错误文本：`IO(PermissionDenied)` 映射为独立的
`ErrorCode::LocalNetworkPermissionDenied`，日志记录
`failure="local_network_permission_denied"`，UI 提供 System Settings → Privacy & Security →
Local Network 的恢复入口并允许原 tab 直接重试。当前 unsigned 开发包的系统隐私身份可能随
重建变化；稳定身份依赖后续 Developer ID 签名，不在本轮伪造 ad-hoc 签名方案。

### keyboard-interactive 扩展点

第一版不做完整 keyboard-interactive UI，但认证流程要保留 challenge/response 扩展点：

```rust
pub enum AuthFlow {
    Password,
    PrivateKey,
    KeyboardInteractiveUnsupported,
}
```

如果服务器只接受 keyboard-interactive，MVP 显示明确错误：该服务器要求 keyboard-interactive，当前版本暂不支持。不要把它伪装成普通密码失败。

### 私钥认证

流程：

```text
load private key
 -> if encrypted and passphrase missing: emit credential prompt
 -> decrypt/sign
 -> if RSA: choose compatible hash strategy
 -> authenticate_publickey
 -> handle AuthResult
```

RSA 兼容性是风险点。实现时需要覆盖：

- ed25519；
- ecdsa；
- rsa-sha2-256；
- rsa-sha2-512；
- legacy ssh-rsa server 的失败提示。

## 12. 文件模型

### RemoteEntry

```rust
pub struct RemoteEntry {
    pub name: String,
    pub path: RemotePath,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified_at: Option<Timestamp>,
    pub link_target: Option<RemotePath>,
}

pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
    Unknown,
}
```

### LocalEntry

```rust
pub struct LocalEntry {
    pub name: String,
    pub path: LocalPath,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified_at: Option<Timestamp>,
    pub link_target: Option<LocalPath>,
}
```

### Sorting

第一版排序字段：

- name；
- kind；
- size；
- modified_at。

默认排序：

1. directories first；
2. name ascending；
3. stable tie-break by full path。

## 13. 传输设计

### TransferJob

```rust
pub struct TransferJob {
    pub id: TransferId,
    pub direction: TransferDirection,
    pub source: TransferEndpoint,
    pub destination: TransferEndpoint,
    pub state: TransferState,
    pub metadata_policy: MetadataPolicy,
    pub conflict_policy: ConflictPolicy,
    pub created_at: Timestamp,
}
```

### TransferPlan

目录上传/下载不是一个单文件 job，而是一个 root plan 生成多个 child job。

```rust
pub struct TransferPlan {
    pub id: TransferPlanId,
    pub root_job_id: TransferId,
    pub source_root: TransferEndpoint,
    pub destination_root: TransferEndpoint,
    pub state: TransferPlanState,
    pub planned_count: usize,
    pub total_bytes: Option<u64>,
    pub child_jobs: Vec<TransferId>,
    pub conflict_policy: ConflictPolicy,
}

pub enum TransferPlanState {
    Queued,
    Planning,
    Running,
    Completed,
    Cancelled,
    Failed { error: UserFacingError },
}
```

规划策略：

- 单文件传输也创建一个 plan，保证 UI 和状态机统一；
- 目录递归 planning 采用流式产出，每发现一批 child job 就 emit plan progress；
- planning 阶段只遍历 source、识别 file kind、估算 size 和生成 child job；
- planning 阶段不做 destination stat，也不做冲突检测；
- destination stat 和冲突检测延迟到 child job 即将执行时；
- `apply_to_all` 作用域绑定到 `TransferPlanId`，不是全局；
- plan 级 `conflict_policy` 是默认策略；`apply_to_all` 更新 plan 级策略，后续 child job 继承；
- job 级 `conflict_policy` 只用于已开始执行的 child job 快照，不能反向覆盖 plan 默认策略；
- planning 期间 UI 显示已发现数量和已估算总字节；
- planning 失败时保留已发现 child job，但默认暂停并提示用户是否继续部分传输。
- 用户主动取消 planning 时，整个 plan 进入 `Cancelled`，已发现 child job 全部丢弃；
- planning 因错误中断时，plan 进入 `Failed`，已发现 child job 保留并暂停，UI 提示是否继续部分传输。

### TransferState

```rust
pub enum TransferState {
    Queued,
    Planning,
    WaitingForConflictDecision {
        request_id: ConflictRequestId,
    },
    Running {
        bytes_done: u64,
        bytes_total: Option<u64>,
        started_at: Timestamp,
    },
    Cancelling,
    Completed,
    Skipped,
    Failed {
        error: UserFacingError,
        retryable: bool,
    },
}
```

### ConflictPolicy

```rust
pub enum ConflictPolicy {
    Ask,
    OverwriteAll,
    SkipAll,
    RenameAll(RenameStrategy),
}
```

### ConflictDecision

```rust
pub enum ConflictDecision {
    Overwrite {
        apply_to_all: bool,
    },
    Skip {
        apply_to_all: bool,
    },
    Rename {
        new_name: String,
        apply_to_all: bool,
    },
    CancelJob,
}
```

### 冲突状态机

```text
Queued
 -> Planning
 -> conflict detected
 -> WaitingForConflictDecision
 -> decision received
 -> Running | Skipped | Failed
```

冲突不是错误，不能进入 `Failed`，除非用户取消或 resolve 后仍无法执行。

### 递归 planning 状态机

```text
Plan Queued
 -> Planning
 -> emit child jobs in batches
 -> Running
 -> child job checks destination conflict
 -> conflict policy may pause individual child job
 -> Completed | Failed | Cancelled
```

planning 不能一次性 stat 完 10k 文件后才更新 UI。否则用户会看到长时间无反馈，并且取消不及时。

### 上传流程

```text
plan source
 -> stat destination
 -> conflict if exists
 -> if symlink: create symlink
 -> if file:
      create remote temp file
      stream chunks
      fsync/flush if available
      atomically publish temp to final without replacing a concurrent target
      set permissions
      set mtime
 -> emit completed
```

### 下载流程

```text
plan source
 -> stat destination
 -> conflict if exists
 -> if symlink: create local symlink
 -> if file:
      create local temp file
      stream chunks
      flush
      atomically publish temp to final without replacing a concurrent target
      chmod
      set mtime
 -> emit completed
```

### 临时文件命名

远端：

```text
.filename.macsftp-part-<transfer-id>
```

本地：

```text
.filename.macsftp-part-<transfer-id>
```

如果目标目录不允许 dotfile，可以后续提供 fallback。第一版先保持简单。

### 取消

取消语义：

- `Queued`：直接移除或标记 skipped；
- `Planning`：取消 planning task，plan 进入 `Cancelled`，丢弃已发现 child job；
- `WaitingForConflictDecision`：关闭 modal，job 进入 cancelled；
- `Running`：停止流，关闭 handle，保留或清理 temp file；
- `Completed`：不能取消。

取消正在 running 的传输时，默认清理 `.macsftp-part` 临时文件。清理失败则记录 warning。

实现要求：

- 使用 `tokio_util::sync::CancellationToken` 传播取消；
- 不依赖 drop future 作为主要取消机制；
- 取消后 best-effort 删除 `.macsftp-part`；
- 删除失败时记录 `ResidualTempFile { host, path, transfer_id }`；
- 下次连接同 host 时，只尝试清理由 macSFTP 记录过的 residual temp file，不扫描整个远端目录。

冲突检测要求：

- `.macsftp-part` 文件不当作正常目标冲突；
- 如果最终目标不存在但存在同名 `.macsftp-part`，提示用户有残留临时文件，可清理后继续；
- retry 时优先清理旧 temp，再重新写入。

## 14. 元数据保留

### 权限

上传：

- 从本地读取 mode；
- 创建远端文件时尽量使用 mode；
- 完成后再次 `setstat`。

下载：

- 从远端 metadata 读取 permissions；
- 完成后 chmod 本地文件。

### mtime

上传：

- 从本地 metadata 获取 mtime；
- 远端写完后设置。

下载：

- 从远端 metadata 获取 mtime；
- 本地写完后设置。

### symlink

要求复制 symlink 本身，不解引用。

上传 symlink：

```text
local read_link
 -> remote symlink(link_target, destination)
```

下载 symlink：

```text
remote readlink
 -> local symlink(link_target, destination)
```

### metadata warning 展示

权限、mtime 或 symlink metadata preservation 失败时，传输本身不一定失败。UI 展示规则：

- transfer drawer 默认在对应 transfer row 上显示 warning 图标；
- row 展开或 details 面板显示完整 metadata warning；
- warning detail 包含失败字段、目标路径、底层错误摘要和是否可重试；
- warning 不弹 modal，避免批量传输时打断用户；
- completed transfer 如果带 warning，状态显示为 completed with warnings。

### 不保留 uid/gid

第一版不承诺 uid/gid。原因：

- 普通用户通常无权限 chown；
- SFTP server 行为差异大；
- 错误恢复复杂；
- 对桌面文件传输工具不是核心路径。

## 15. UI 规划

第一版只做单窗口。多窗口会改变 `AppModel`、event routing 和 modal ownership，不进入 MVP。

> **2026-07-15 更新 —— 多窗口事件边界已收敛。** 上述三点担忧均已解决：
> - **event routing**：`RuntimeController` 只暴露一个 bounded flume receiver，由进程级 `AppEventCoordinator` 唯一消费。transfer 与 residual-temp 事件只归并/持久化一次；tab/window 事件再投递给全部 workspace，并由既有的 `TabStore::accepts_remote_event` 按 `tab_id + session_id + session_epoch` 过滤。禁止 broadcast lag 覆盖未读结构事件。
> - **AppModel**：`TransferStore` 与持久化资源（config/profiles/keychain/residual_temps/recents）+ `AppPaths` + tab-id 计数器提升为进程级 GPUI global（`SharedTransfers` / `AppResources`），所有窗口共享同一实例；tab 集合、模态、焦点仍为每窗口私有。会话恢复由下述 `SessionCoordinator` 单独持有，避免通用资源容器暴露多写入口。
> - **modal ownership**：host-key 模态携带 `tab_id`，只在拥有该 tab 的窗口弹出；transfer conflict 由协调器只分配给一个活动窗口，窗口关闭后把仍 pending 的 prompt 重新分配给另一个窗口。
> - `TabId` 由每实例 `max+1` 改为共享 `Arc<AtomicU64>`，跨窗口绝不冲突（runtime 的 session 注册表按 `TabId` 索引）。
> - 关闭全部窗口后 app 常驻（原生 macOS 行为），Cmd+N / Dock 图标可重新开窗口。

> **2026-07-15 更新 —— 多窗口会话恢复采用进程级单写者。** `session.json`
> 升级为 v2：顶层保存有序的窗口快照、活动窗口索引，每个窗口快照保存稳定的
> `WindowSessionId`、活动 tab 索引和非敏感 tab/路径信息；v1 平面 tab 列表加载时
> 自动迁移为一个窗口。`SessionCoordinator` 是 `SessionStore` 的唯一所有者：app quit
> 在 GPUI 拆窗前一次收集并冻结所有 live workspace，之后的窗口关闭回调不得覆盖；
> 普通手动关窗则在窗口已不可访问后保存剩余窗口，关闭最后窗口保存空会话，Dock
> 重开得到新窗口。启动按窗口快照逐窗恢复，但不会自动发起远端连接。窗口位置和大小
> 暂不持久化。

### 主布局

```text
+------------------------------------------------------+
| Tab Bar                                              |
+----------------------+-------------------------------+
| Local Pane           | Remote Pane                   |
| path bar             | path bar                      |
| table/list           | table/list                    |
+----------------------+-------------------------------+
| Transfer Drawer                                      |
+------------------------------------------------------+
| Status Bar                                           |
+------------------------------------------------------+
```

### Tab bar

功能：

- 新建 tab；
- 关闭 tab；
- 切换 tab；
- 显示连接状态；
- 显示 tab title；
- 断线时显示 error indicator。

### File pane

功能：

- path bar；
- back/up/refresh；
- table header；
- virtualized file list；
- selection；
- context menu；
- drag selection 后续再做。

远端 pane 状态：

```text
Disconnected
Connecting
HostKeyPrompt
LoadingDirectory
Loaded
Error
```

### Transfer drawer

显示：

- active transfers；
- queued transfers；
- completed/failed collapsed section；
- progress；
- speed；
- ETA；
- warning icon for completed-with-warning transfers；
- pause/cancel/retry controls。

第一版可以不做 pause，因为 SFTP chunk stream 暂停恢复比取消/重试复杂。UI 上先提供 cancel/retry。

metadata preservation warning 默认只显示 warning 图标；点击或展开 transfer row 后显示详情。

### Modal layer

modal 类型：

- host key unknown；
- password prompt；
- private key passphrase prompt；
- transfer conflict；
- destructive operation confirm；
- error details。

注意：modal 必须绑定 request id。用户切换 tab 或旧请求过期后，旧 modal 的确认不能误作用到新 session。

### Accessibility

GPUI 的 accessibility 能力需要单独验证。MVP 不承诺完整 VoiceOver 达标，但不能主动破坏基础可用性：

- 所有 icon-only button 必须有 tooltip/label；
- modal 必须有明确标题和主操作；
- keyboard focus flow 必须可测试；
- 文件列表 selection 必须能通过键盘操作；
- 后续若 GPUI a11y API 成熟，再补 VoiceOver 语义。

### 文案与国际化边界

MVP UI 默认语言英文优先，但业务层不能硬编码最终展示文案。

要求：

- 第一版界面文案以英文为主；
- `core` 输出 error code 和参数；
- `app/ui` 负责把 error code 映射成展示文案；
- 日志使用经过分类和脱敏的诊断信息，UI 使用用户可理解信息；
- 后续国际化只替换 UI 文案映射，不改 SFTP adapter。

## 16. GPUI 实现约束

### 状态

长期状态放 GPUI Entity：

- `AppModel`
- `TabStore`
- `TransferStore`
- `ModalStore`

局部 UI 状态可以放 view：

- hover；
- focused row；
- current input draft；
- popover open state。

### 大列表

目录列表必须使用 GPUI `list` 或 uniform list 思路。

要求：

- 10k entries 不创建 10k 个长期 row entity；
- row rendering 从 snapshot + index 派生；
- sort/filter 在 core 层生成稳定 visible index；
- selection 存 id/path，不存 row index。

M1 前置 spike：

- 确认当前 pin 住的 GPUI 版本是否提供 `list` 或 uniform list API；
- 验证 10k 行滚动、hover、selection 不明显卡顿；
- 验证 row 高度固定时不会出现 layout 抖动；
- 如果 API 不存在或签名变化，先封装 `VirtualFileList`，不要让业务 view 直接依赖具体 GPUI list API。

### Actions

至少定义这些 actions：

```text
NewTab
CloseTab
ReconnectTab
RefreshPane
FocusLocalPane
FocusRemotePane
UploadSelection
DownloadSelection
CancelSelectedTransfer
OpenCommandPalette
ShowTransferDrawer
```

快捷键可以后续调整，但 action 名称应稳定。

## 17. 错误模型

错误分三层：

### TechnicalError

内部错误，保留 debug 信息：

- russh error；
- russh-sftp status；
- IO error；
- parse error；
- channel closed；
- cancellation。

### DomainError

业务语义：

- auth failed；
- host key mismatch；
- host key trust request timeout；
- user cancelled trust request；
- permission denied；
- file exists；
- not found；
- unsupported symlink；
- metadata preservation failed。

### UserFacingError

UI 展示：

- title；
- short message；
- detail；
- recovery action；
- retryable。

原则：

- 不向 UI 泄露密码、私钥路径中的敏感片段、passphrase；
- 第三方技术错误原文不能直接进入 log；应先分类并移除 secret、私钥完整路径等敏感上下文；
- 用户错误要有下一步动作。

### Logging

日志使用 `tracing` 生态：

- `tracing`；
- `tracing-subscriber`；
- `tracing-appender`。

要求：

- 所有 secret value 必须 redacted；
- 私钥路径默认只记录文件名或 hash，不记录完整路径；
- host 可以记录，username 默认可记录，password/passphrase 永不记录；
- transfer progress 不逐 chunk 记录，只记录 state transition 和节流后的 summary；
- russh/russh-sftp error 仅保留经过审核的分类信息，不直接记录第三方 `Display` / `Debug` 原文；
- UI error 只展示 `UserFacingError`。

日志分级：

```text
ERROR: unrecoverable runtime/session failure
WARN: metadata preservation failed, temp cleanup failed, retryable network failure
INFO: app start/stop, connect/disconnect, transfer completed
DEBUG: stale event dropped, command coalesced, known_hosts match details
TRACE: disabled by default
```

MVP 的 SFTP 诊断日志只覆盖一次真实连接尝试的开始到终止结果：连接开始、连接池等待或复用、SSH 握手、host key 校验、认证、SFTP subsystem 初始化，以及连接成功或分类后的失败。每条连接事件携带 `tab_id`、`session_id`、`session_epoch`，便于关联并发连接；允许记录 host、port、username 和认证方式，但不记录 credential、完整私钥路径或第三方错误原文。默认日志过滤器关闭其他 `macsftp_sftp` target，只开启经过审计的 `macsftp_sftp::connection`；因此连接成功后，目录浏览、文件操作和传输过程不会写入 SFTP 诊断日志。开发者仍可通过 `RUST_LOG` 临时覆盖过滤器。

## 18. 配置与持久化

### 配置文件

建议路径：

```text
~/Library/Application Support/macSFTP/config.json
~/Library/Application Support/macSFTP/profiles.json
~/Library/Application Support/macSFTP/known_hosts
~/Library/Logs/macSFTP/macsftp.log
```

### Profile migration

所有持久化结构带版本号：

```rust
pub struct ProfilesFile {
    pub version: u32,
    pub profiles: Vec<ConnectionProfile>,
}
```

第一版也要加版本号，避免后续迁移被迫写猜测逻辑。

会话文件恢复（2026-07-18）：`session.json` 无法解析或版本不受支持时，启动仍使用空的
内存快照，但第一次 checkpoint 必须先把原始字节原样保存为权限 `0600` 的
`session.json.corrupt[.N]`，备份成功后才允许原子写入新快照；恢复成功后正常 checkpoint
必须重新开放。禁止因一次损坏永久关闭本次进程的会话持久化，也禁止无备份覆盖原文件。

Profile 结构预留：

- `group_id: Option<ProfileGroupId>`：MVP 不实现 folder/group UI，但数据结构预留；
- `last_local_path: Option<LocalPath>`：默认本地起始目录为 home，连接成功后可保存上次路径；
- 同一个 profile 可以打开多个 tab，`TabState` 与 `ProfileId` 是多对一。

### Transfer history

**单一语义（2026-07-14）：无跨会话传输目录。**

- Drawer 只展示进程内 `TransferStore` 的 Active / Queued / Completed / Failed。
- **没有**跨启动 History 分区；退出后不恢复未完成/已完成清单。
- 不保留 `TransferHistoryStore`、history record、retry command rebuild 或仅供测试的目录模型。
- 启动时 best-effort 删除旧版本遗留的 `transfer_history.json`；该路径只用于迁移清理，
  不会读取或恢复其中的任务。

## 19. 测试计划

### core unit tests

- tab open/close/switch；
- stale event ignored；
- transfer state transitions；
- conflict apply-to-all；
- sorting；
- error mapping。

### sftp integration tests

覆盖：

- password auth；
- private key auth；
- unknown host key；
- host key mismatch；
- read remote dir；
- upload file；
- download file；
- rename conflict；
- symlink upload/download；
- permission preservation；
- mtime preservation。

测试环境：

- 使用 Docker/容器化 OpenSSH server 作为主要 fixture；
- 目标应用是 macOS，但 SFTP 协议层测试可以在 Linux CI 中跑；
- 测试容器内准备 password 用户、ed25519 key 用户、rsa key 用户；
- 容器暴露独立 known_hosts fixture，覆盖 unknown/mismatch；
- metadata 测试需要明确容器 filesystem 支持 chmod/mtime/symlink。

不假设 OpenSSH 是 in-process library。`test_support` 负责启动/等待/清理测试容器。

无 Docker 策略：

- 本地开发环境无 Docker 时，`test_support` 优先使用本机 `/usr/sbin/sshd`
  以当前用户身份在 loopback 高位端口启动真实 OpenSSH fixture（覆盖
  handshake、host key 校验、公钥认证；非 root sshd 无法校验密码，密码
  认证只能覆盖拒绝路径）；
- 本机 sshd 也不可用时，integration tests 默认 skip；
- skip 必须打印明确提示，例如 `SFTP integration tests skipped: ...`；
- skip 不导致 `cargo test --workspace` 失败；
- CI 必须提供 Docker，并运行完整 SFTP integration tests（含密码认证
  成功路径）；
- 允许开发者通过环境变量指定外部 OpenSSH test server，但这不是默认路径。

### GPUI tests

覆盖：

- tab switching；
- close active tab；
- command actions；
- keyboard focus；
- host key modal dispatch；
- conflict modal dispatch；
- transfer drawer state rendering。

### Performance smoke tests

- 10k local entries；
- 10k remote entries；
- 4 active transfers；
- 3 connected tabs；
- rapid tab switching during transfer。

### Runtime bridge tests

- command channel full 时低优先级 command 被合并或拒绝；
- progress event 被节流；
- `Shutdown` 能取消 actors 并关闭 runtime；
- Tokio task 不能直接访问 GPUI context；
- stale session event 被 core guard 丢弃。

### 实现期 spike

这些 spike 不阻塞 M0，但必须在对应里程碑前完成：

| Spike | 截止里程碑 | 验证项 |
| --- | --- | --- |
| GPUI `list` / uniform list API | M1 前 | API 存在、签名可用、10k 行不卡 |
| `flume` 在 GPUI executor 上 recv | M2a 前 | `cx.spawn` + `flume::recv()` 能编译运行 |
| `tokio::runtime::shutdown_timeout` | M2a 前 | timeout 后不会卡住 app shutdown |
| `ssh-key` known_hosts 能力 | M3 前 | 覆盖 MVP 子集，或确认需要小型 parser |
| `russh::client::Handler::check_server_key` 签名 | M3 前 | 当前版本 async/sync 形态与 oneshot 等待方式 |
| `russh-sftp` 单 channel/同连接并发 | M5 前 | 决定 borrow fallback 是否可用 |

## 20. 里程碑

### M0: Project skeleton

交付：

- Cargo workspace；
- crates scaffold；
- lint/test scripts；
- basic app entry。
- ADR-002 GPUI <-> Tokio bridge；
- ADR-003 browsing vs transfer session；
- ADR-004 host key callback decision flow。

验收：

- `cargo test --workspace` 通过；
- 空 GPUI window 启动。
- ADR 中的关键数据流已反映到 `core` 类型草案；
- M0 的类型草案只要求 struct/enum、id newtype、`new`/`Default` 骨架，不实现业务状态机。

状态：已交付。

### M1: GPUI shell + mock data

交付：

- tab bar；
- split file panes；
- transfer drawer；
- mock file entries；
- 10k entry virtualized list。
- `VirtualFileList` spike。

验收：

- 新建/关闭/切换 tab；
- mock 远端列表滚动不卡。
- GPUI 当前版本的 list/uniform-list API 已验证，或已经封装替代方案。

状态：已交付。Spike 结论：GPUI 0.2.2 `uniform_list` API 满足需求，
未额外封装 `VirtualFileList`。

### M2a: Runtime bridge channel

交付：

- Tokio runtime；
- command/event channel；
- GPUI event drain task；
- bounded channel backpressure。

验收：

- UI command 触发后台 mock event；
- progress event 可节流；
- `Shutdown` 可以停止 runtime。
- GPUI 侧 command send 使用 `try_send`，channel full 不会阻塞主线程；
- runtime shutdown 使用 `shutdown_timeout`。

状态：已交付。

### M2b: Stale event guard

交付：

- `session_epoch`；
- event scope；
- stale event drop logic；
- core tests。

验收：

- 关闭 tab 后旧 event 不污染状态。
- reconnect 后旧 session event 不覆盖新 session。

状态：已交付。

### M2c: Mock actor full loop

交付：

- mock `RemoteSessionActor`；
- mock host key prompt；
- mock transfer progress；
- modal request id routing。

验收：

- host key modal 的 accept/reject 能完整回到 mock actor；
- 旧 modal 确认不会作用到新 session。

状态：已交付。`SessionBackend::Mock` 保留供 runtime 测试使用，App
默认走 `SessionBackend::Real`（见 M3）。

### M3: russh connection

交付：

- TCP connect；
- host key prompt；
- password auth；
- private key auth；
- disconnect/reconnect。

验收：

- 能连接真实 OpenSSH server；
- unknown host key 可保存；
- mismatch 会阻断。

状态：已交付并在真实网络环境验证（真实 OpenSSH server，host key
Match/Mismatch/NotFound 三分支、密码与私钥认证、`SessionBackend::Real`
挂到 dispatch loop）。连接凭据来源是过渡态的连接表单（决策 21），
Keychain-backed profile 仍留待后续里程碑。

### M4: Remote browsing

交付：

- SFTP subsystem；
- read_dir；
- remote path navigation；
- sorting；
- refresh；
- remote operation error handling。

验收：

- 每个 tab 独立浏览远端目录；
- 一个 tab 断线不影响另一个 tab。

状态：已交付。dispatch loop 为每个真实 session 建立有界请求信箱，
`RemoteSessionActor` 用已持有的 `SftpSession` 在 `select!` 循环中处理
`ReadRemoteDir`，依次回传 `RemoteDirLoading` 与
`RemoteDirLoaded(RemoteScoped<RemoteDirSnapshot>)`；`NoSuchFile` 和
`PermissionDenied` 分别映射为可恢复的 `NotFound` / `PermissionDenied` pane
错误并提供刷新重试。App 在 `TabConnected` 后自动请求 canonical 根目录，
进目录/上级/刷新均经 command 路由，不再生成远端 mock 数据；读取期间保留
旧列表并显示 refreshing 标记。结果仍走既有的 session/epoch 陈旧事件守卫，
且只会覆盖当前请求路径。`crates/sftp/tests/real_session.rs` 覆盖真实
`read_dir`、10k entries、多 tab 独立性与 runtime → actor 路由闭环。

### M5: Transfers

交付：

- `TransferPlan`；
- upload；
- download；
- transfer queue；
- progress；
- cancel；
- retry。

验收：

- 单文件和目录传输都通过 plan 表达；
- 递归 planning 能流式更新 UI；
- planning 不做 destination conflict stat；
- `russh-sftp` borrow fallback 并发能力 spike 已完成；
- 4 个传输并发受控；
- 大文件传输不阻塞目录浏览。

状态：已交付。跨连接 residual temp 持久化与清理缺口已闭合（见 §1893 起），M5 ≈ 100%。`StartTransfer` 先建立 `TransferPlan`，本地
目录扫描在 blocking worker 中流式回传 `TransferPlanProgress`：首个 child 立即回传，
后续 child 按 128 个一批并在完成前冲刷尾批；drawer 在
planning 期间显示发现数量与总字节。进程级 TransferManager 最多并发 4 个不同 plan；
同一 plan 的 child 按规划顺序串行，避免目录创建与子文件传输竞态。目录浏览保持在独立
SFTP channel；规划、队列和运行态均可取消，失败
job 可重试。目标已存在时 job 进入 conflict state，modal 支持 overwrite、skip、
keep both、自定义 rename，以及 overwrite/skip/rename apply-to-all（作用域为
`TransferPlanId`）。自定义 rename 默认建议副本名，输入仅接受同目录文件名，并可用
Enter 提交；冲突 modal 打开后焦点直接落在该输入，并显示 source / destination /
size / mtime，操作分两行布局以适配窄窗口。上传和
下载会尽力保留 permissions / mtime；保留失败作为 warning，不把已完成的数据传输
改判失败。单文件上传/下载先写入按 `TransferId` 命名的隐藏 `.macsftp-part`，再用
hard-link no-replace 发布最终目标；这避免在冲突检查后静默覆盖并发创建的文件。远端
服务器未声明 `hardlink@openssh.com` 时 transfer 明确失败并可重试，不退回不安全的
rename 覆盖。成功、失败与取消都会 best-effort 清理 temp；清理失败作为可见 warning
保留路径详情。跨连接 residual temp 的持久化与清理已实现：temp 创建时即通过
`AppEvent::ResidualTempCreated` 记录到 `ResidualTempStore`（`residual_temp.json`，
按 `connection_key = "{host}:{port}"` 区分远端、本地用 `"local"`），app 启动对本地
temp 做无连接对账清理，tab 重连同 host 时发送 `AppCommand::RemoveRemoteTempFile` 清理
远端残留；清理成功回写 `AppEvent::ResidualTempCleared` 移除记录。崩溃/强杀后残留的
`.macsftp-part-*` 因此可在下次启动或重连时被主动清理，M5 该缺口已闭合。UI 层另新增可见的
Upload/Download 工具栏按钮（按连接+选择态启用）与跨面板拖拽传输（本地条目拖至远端面板=上传，
远端条目拖至本地面板=下载），消除此前仅菜单/快捷键导致的可发现性问题。修复一个 UI 状态 bug：当
`TransferPlanCompleted` 时 root job 被错误地设为 `Queued` 且子任务完成后没有事件再次更新它，导致
已完成的传输仍显示在 Queued 区；现子任务全部到达终态（Completed/Skipped/Failed）后 core reducer 会把
root job 与 plan 同步 finalize，空 plan 在规划完成时直接 finalize。该状态转换现集中在 core 的幂等
`TransferStore::apply_event` reducer，重复事件不会复制 plan/job/conflict。上传和下载
symlink 均复制 link 本身，不解引用，并由真实 sshd 集成测试覆盖。目录下载会在已连接 tab actor 中另开 SFTP channel
执行远端递归 listing，并按同一首条即时 + 128 条批量规则发出 `TransferPlanProgress` 后才入队；这样远端扫描不占用
浏览 channel，取消仍复用 root planning 的 cancellation token。目录 child job 会创建
本地空目录与嵌套父目录，保留目录 metadata，文件与 symlink 继续走既有执行路径。
目录上传保留每个选中目录的顶层名称，多目录不会把相对路径铺平到同一个目标；空目录也
生成 child job。已有同名目录按 merge 处理，文件级冲突仍逐项进入 plan-scoped 决策。
本地目录 listing 及递归删除/rename/mkdir 已移到 GPUI background executor；每个 tab 用
单调 local request epoch 丢弃过期结果，路径相同也不能绕过陈旧结果校验。

### M6: Conflict + metadata

交付：

- conflict modal；
- overwrite/skip/rename；
- apply-to-all；
- permission preservation；
- mtime preservation；
- symlink preservation。

验收：

- 批量冲突不会重复弹大量 modal；
- symlink 不被解引用复制。

状态：已交付（conflict modal + apply-to-all + perms/mtime/symlink 全部落地，并由真实 sshd 集成测试覆盖）。M6 ≈ 100%。

### M7: Polish and packaging

交付：

- Zed-style visual polish；
- `scripts/build_app.sh` 生成的 unsigned `build/macSFTP.app`；
- `Info.plist`、完整尺寸 `AppIcon.icns`、macOS 基础菜单和 About；
- `NSLocalNetworkUsageDescription` 与本地网络权限拒绝后的恢复 UI；
- logs；
- 单窗口 settings surface；
- version 1 `config.json`，MVP 仅持久化 `system` / `light` / `dark` 外观偏好；
- regression tests。

取舍：

- bundle 版本号从 `macsftp-app` 的 Cargo package version 派生，禁止在 About 和 plist
  中分别维护版本；
- 设置页替换主窗口工作区内容，关闭后恢复原 tab/pane 状态，不创建第二个窗口，也不
  把设置当成阻塞 modal；
- `config.json` 缺失时使用默认值且不主动创建；损坏或未来版本只回退当前会话并显示
  可见错误，直到用户明确修改设置前不覆盖原文件；
- 公证、Developer ID 签名、DMG/PKG 和自动更新仍不属于 MVP。

验收：

- 手动测试矩阵通过；
- `plutil -lint build/macSFTP.app/Contents/Info.plist` 通过；
- 最终 bundle 的 `NSLocalNetworkUsageDescription` 存在且非空；
- 不签名也可从本地构建产物运行；不承诺下载分发后的 Gatekeeper 行为。

状态：已完成。M7 全部交付物落地：unsigned `build/macSFTP.app`（`plutil -lint` 通过）、
`Info.plist` + 完整尺寸 `AppIcon.icns`、macOS 基础菜单与 About（`render_about` 含图标/名称/
`CARGO_PKG_VERSION` 版本/Copy Version Info）、tracing 日志、单窗口 settings surface
（`OpenSettings` 替换主工作区，外观 system/light/dark 持久化、关闭恢复）、`config.json` v1
（仅外观）。验收证据已补齐：`docs/m7-test-matrix.md`（手动测试矩阵 + 执行结果）、
`docs/m7-visual-polish.md`（Zed-style 视觉打磨验证标准与执行结果）、
`crates/app/src/m7_regression.rs`（M7 专属 `m7_` 前缀回归套件，守护版本单一来源与图标源资源）。
M7 ≈ 100%；MVP ≈ 100%。

## 21. 风险清单

### GPUI pre-1.0

风险：

- API 破坏性变更；
- 文档不完整；
- macOS 细节可能需要读 Zed 源码。
- `list` / uniform list API 可能不存在、移动或签名变化。

缓解：

- UI 逻辑隔离在 `app/ui`；
- 业务状态放 `core`；
- GPUI API 使用集中封装；
- 版本 pin。
- M1 之前完成 `VirtualFileList` spike。

### russh/russh-sftp API 和兼容性

风险：

- host key / private key / RSA hash 兼容性；
- SFTP server 实现差异；
- metadata setstat 支持不一致。

缓解：

- 真实 OpenSSH server integration tests；
- 错误分类清晰；
- metadata preservation 失败时传输本身仍可完成，但 UI 显示 warning；
- 版本 pin。

### 多标签资源占用

风险：

- 每 tab 独立 session 导致连接数增加；
- 受限服务器可能因 MaxSessions/MaxStartups/连接数策略直接不可用；
- keepalive 太频繁。

缓解：

- 默认连接数限制；
- 空闲 tab 后续可 suspend；
- MVP 先保持独立 browsing session，后续再设计 pool；
- transfer session 创建失败时 fallback 到借用 browsing session，并提示 tab 关闭会取消传输；
- 对同 host 传输并发默认限制为 2，且允许用户降到 1。

### 传输取消和临时文件

风险：

- 取消时远端 temp file 残留；
- 本地权限/mtime 设置失败；
- rename 原子性依赖文件系统和 server。

缓解：

- `.macsftp-part` 命名；
- best-effort cleanup；
- warnings 可见；
- transfer log 记录 temp path。
- 记录 residual temp file（`ResidualTempStore` + `AppEvent` 持久化），下次连接同 host 时主动清理（已实现）。

### 双运行时桥接

风险：

- GPUI executor 与 Tokio runtime 混用导致 event 丢失、死锁或主线程卡顿；
- unbounded progress event 淹没 UI；
- Tokio task 误持有 GPUI context。

缓解：

- ADR-002 固化桥接机制；
- bounded channel；
- progress throttling；
- runtime bridge tests；
- 禁止跨边界持有 UI context。

### host key 回调阻塞

风险：

- 未知 host key 必须在 handshake 中等待用户决策；
- 用户不响应会导致 actor 泄漏；
- 旧 modal 可能误作用到新 session。

缓解：

- ADR-004 固化 `TrustRegistry + oneshot + timeout`；
- request id + session epoch 绑定；
- tab close/reconnect 时自动 reject pending request。

## 22. Review 后补充决策

根据 `docs/archive/architecture-review.md`、`docs/architecture-review-v2.md` 和 `docs/architecture-review-v3.md`，以下问题已转为明确决策：

1. Profile folder/group：MVP 不做 UI，但 profile 数据结构预留 `group_id: Option<ProfileGroupId>`。
2. 同 profile 多 tab：允许。`TabState` 与 `ProfileId` 是多对一。
3. 关闭 app 时运行中 transfer：随进程结束；**不**跨启动展示未完成 history（会话清空）。
4. 远端删除：SFTP 无 trash 概念。MVP 直接 confirm 删除，批量删除必须显示不可撤销提示。
5. private key passphrase：默认可记住到 Keychain，但 profile 必须有 `remember_passphrase` 开关。
6. `~/.ssh/config` Host alias：不进 MVP。profile 字段保持与 ssh config 概念接近，方便后续导入。
7. 默认本地起始目录：首次为 home；之后可按 profile 记住上次路径。
8. transfer history：无跨会话目录；drawer 只显示进程内 `TransferStore` 分组。
9. known_hosts 子集外 entry：按行忽略并写 WARN log，不阻断整个文件解析。
10. 无 Docker 的本地 integration tests：默认 skip 并打印提示；CI 必须提供 Docker。
11. M0 类型草案范围：只定义核心 struct/enum/id/new/default 骨架，不实现业务状态机。
12. `ConnectionProfile.revision` 是 `AuthFingerprint.profile_revision` 的来源，由 storage 层自动递增。
13. 用户取消 planning 会丢弃 child jobs；planning 错误中断会保留已发现 child jobs 并提示是否部分继续。
14. borrow fallback 失效时先回退 dedicated，dedicated 也不可行才 failed。
15. `TransferProgress` 节流在 TransferManager 发 event 前完成；planning child 在生产端批量化，event drain 不通过丢事件来降载。
16. dedicated transfer session 使用引用计数，最后一个 active transfer 释放后关闭。
17. 三轮架构评审后不再继续做文档层架构评审；进入 M0，通过实现期 spike 验证剩余风险。
18. MVP UI 默认语言英文优先。
19. metadata preservation warning 默认在 transfer drawer 显示 warning 图标，点击或展开 transfer row 查看详情。
20. session_id/session_epoch 由 UI/core 侧在发起连接时分配并放入 `ConnectCommand`；runtime 与 actor 只是回显，不自行分配（M2 实现期决策，保证 stale event guard 端到端一致）。
21. Keychain-backed profile 落地前的过渡：连接表单收集 `ConnectionSettings`（zeroize 容器、Debug 全量 redact）并直接随 `ConnectCommand` 传给 runtime；secret 仅驻内存（per-tab 缓存供重连），不持久化。Keychain + profiles.json 接入后，command 恢复为按 `profile_id` 解析。
22. M4a `ReadRemoteDir` 路由：dispatch loop 为每个 `SessionBackend::Real` session 建立请求信箱（`flume` bounded channel），`RemoteSessionActor` 连接成功后用 `select!` 循环同时监听 cancel / connection_lost / 请求信箱，复用已持有的 `SftpSession` 执行 `read_dir`；结果经 `RemoteDirLoaded(RemoteScoped<RemoteDirSnapshot>)` 回传，走既有 session/epoch 陈旧事件守卫。App 侧在 `TabConnected` 后自动发起根目录请求；导航/上级/刷新一律改为发命令，不再本地拼接路径或复用 mock 数据。若同一 session 内有后续导航请求，只有匹配当前目标路径的 listing 可以替换列表。
23. 多窗口事件所有权：runtime event receiver 只有一个进程级所有者。transfer reducer、rate sampler 和 residual-temp persistence 只执行一次；workspace 只处理窗口/tab 状态。transfer conflict 只能属于一个 live window，owner 关闭后重新分配。

## 23. 剩余待确认问题

当前没有阻塞 M0 的待确认问题。剩余风险通过第 19 节的实现期 spike 验证。

## 24. 远程编辑的权威快照校验

远程自动回传（edit-and-upload-back）在 M0 阶段落地，但本地保存绝不能再依赖任何**缓存的 UI 目录列表**来授权对远端文件的覆盖写。本节记录权威快照校验协议及其已知边界。历史 bug 分析见 `docs/remote-editing-bug-analysis-2026-07-16.md`。

### 24.1 协议概要

1. **本地保存进入 `CheckingRemote` 阶段。** 编辑监听器（watcher）在本地临时文件落盘后，将编辑会话（edit session）推进到 `EditPhase::CheckingRemote`，并写入两个仅在该阶段为 `Some` 的字段：`pending_check_id: Option<EditCheckId>` 与 `checking_local_mtime: Option<Timestamp>`。这两个字段在每次离开该阶段时都被清空，用于阻止监听器对同一保存重复派发校验命令。
2. **UI/core 是 `EditCheckId` 的唯一分配方。** 只能通过 `EditSessionStore::next_check_id()` 分配；runtime 与 actor 只回显该 ID，绝不自行生成。
3. **runtime 将受限命令路由到 live actor。** watcher 派发一个 `AppCommand`，请求对 `remote_path` 执行一次实时的 `symlink_metadata` 读取，并携带 `EditCheckId` 与本地保存时刻的 mtime。该命令是有界的（bounded），无法路由到 live actor 时不会无限挂起。
4. **actor 使用 `symlink_metadata` 并发出带作用域的结果。** `RemoteSessionActor` 针对目标文件（而非目录列表）调用 `symlink_metadata`，回显 `EditSessionId` + `RemoteEventScope` + `EditCheckId` + path + 远端快照，经 `RemoteEditSnapshotChecked` / `RemoteEditSnapshotCheckFailed` 回传。
5. **路由失败走 epoch 关联事件。** 当受限命令无法投递到 live actor（如会话已断开）时，发出 `RemoteEditSnapshotDispatchFailed`；该事件是**按 epoch 关联**的（非 remote-scoped），携带 `tab_id` / `session_epoch` / `path`，以便只有确切的待处理元组（tab/epoch/session/check/path）可以被重置，而不会误伤重连后的替换校验。

### 24.2 结果只被应用一次（相关性守卫）

三个校验事件由**进程级** `AppEventCoordinator` 独占处理，**不广播到 workspace**（广播会竞争）。协调器通过 `TabStore::accepts_remote_event` + `workspace_windows(cx)` 迭代定位拥有该事件的 `Workspace`。

应用前必须满足**完整相关性守卫**（apply exactly once）：

- `phase == CheckingRemote`；
- `pending_check_id == Some(check_id)`；
- `checking_local_mtime.is_some()`；
- `remote_path == path`；
- `tab_id == session.tab_id`；
- `session_epoch == session.session_epoch`。

`DispatchFailed` 额外要求 `tab_id` / `session_epoch` / `path` 三者都与待处理元组一致（不仅仅是 `check_id` 匹配）。**过期的会话作用域、过期的 epoch、以及作废的 check ID 一律被忽略**，因此重连后的会话不会被旧结果篡改。

### 24.3 重新 stat 守卫（TOCTOU 残余闭环）

相关性通过后，协调器**再次 stat 临时文件**：若其当前 mtime ≠ `checking_local_mtime`，则放弃本次上传并回退到 `Editing`（保留 baseline）。这保证「校验在途期间又发生了一次本地保存」不能授权覆盖远端——必须重新发起一次校验。

### 24.4 成功 / 冲突 / 失败分支

- **快照相等**（live 远端 metadata 与校验时一致）：构造 `build_edit_upload_command` → 进入 `UploadingBack`，`local_mtime = checking_local_mtime`，清空 pending，调用 `dispatch_edit_command` 并刷新窗口。精确一次上传。
- **快照分歧**（远端 size 或整秒 mtime 不同）：进入 `RemoteConflict`，`local_mtime = checking_local_mtime`，清空 pending，刷新窗口。**不发起任何上传**，远端内容不会被覆盖。
- **校验失败 / 路由失败**：`revert_stranded_upload` 保留本地编辑与 baseline，回到可重试的 `Editing`，清空 pending。本地临时文件仍在，监听器会在下一次保存时重新派发校验，即「失败保留本地编辑并稍后重试」。

### 24.5 已知边界（未解决的问题）

比较维度仍然是 `(size, whole-second mtime)`。因此：

- **相同 size + 相同整秒 mtime 的并发远端写入无法被检测**，除非引入哈希或版本号（当前未实现，接受为已知限制，必须保留文档说明，不得谎称已修复）。
- 该限制与第 19 节的实现期 spike 一致：不追加哈希/版本协商前，无法区分「远端被另一写者以相同 size/整秒 改写」与「远端未变」。

## 25. 参考资料

- GPUI: <https://gpui.rs/>
- GPUI docs: <https://docs.rs/gpui/latest/gpui/>
- GPUI README: <https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md>
- russh docs: <https://docs.rs/russh/latest/russh/>
- russh-sftp docs: <https://docs.rs/russh-sftp/latest/russh_sftp/>
- ssh-key docs: <https://docs.rs/ssh-key/latest/ssh_key/>
- Architecture review: `docs/archive/architecture-review.md`
- Architecture review v2: `docs/architecture-review-v2.md`
- Architecture review v3: `docs/architecture-review-v3.md`
