# 大文件并发传输调优与远程编辑深度联动实施方案

## Steps

- [x] Phase 0: 文档与可见变化前置同步
  - [x] 0.1 修订 `docs/architecture.md` §9：允许本地保存感知，但仅作视觉提示
  - [x] 0.2 同步 `README.md` 与 `CHANGELOG.md` 的用户可见变化
- [x] Phase 1: 传输通道调优与 Plan 内并发放开 (P0)
  - [x] 1.1 `copy_file_chunks` 缓冲 64 KiB → 256 KiB（真正的包长杠杆）
  - [x] 1.2 `open_transfer_sftp` 改用 `new_with_config`，只调 `max_concurrent_writes` 8→16 与 `request_timeout_secs` 10→30
  - [x] 1.3 `physical_connection.rs` 通道窗口 2 MiB → 4 MiB
  - [x] 1.4 `TransferManager` 改为 `running_per_plan`，单 Plan 4 并发、全局 6 并发，并加入 Plan 间公平调度
  - [x] 1.5 同步三处递减计数与 `cleanup_plan_if_idle`，补调度单元测试
- [x] Phase 2: 远程编辑按扩展名智能路由外部编辑器 (P0)
  - [x] 2.1 `EditorRule` 定义在 `macsftp-core`，`storage::AppConfig` 引用它；**不升配置版本号**
  - [x] 2.2 规则匹配实现大小写不敏感、First-Match-Wins 且最长后缀优先
  - [x] 2.3 未命中严格兜底 `/usr/bin/open -t`；命中才用 `open -a <app>`
  - [x] 2.4 Settings 增加“编辑器关联”列表，`ConfigStore` 增加持久化入口
  - [x] 2.5 编写匹配、序列化与防提权安全测试
- [x] Phase 3: 基于进程级 FSEvents 的本地保存感知与 UI 脏标记 (P1)
  - [x] 3.1 `macsftp-platform` 显式声明 `notify`（8.x）依赖，启动时创建 `<edits_dir>/<edit_run_id>/` 并递归监听
  - [x] 3.2 防抖、临时副产物过滤、**自身写盘抑制**（基线 size+mtime 比对）
  - [x] 3.3 `EditSession.local_modified` + `AppEvent::LocalEditModified`，进入 `Editing` 时记录基线
  - [x] 3.4 UI：`●` 标记、状态栏 Accent 高亮；`⌘⇧U` 需新增 Action 并注册到 keymap / menu / palette
  - [x] 3.5 严格恪守 §9：绝不自动静默上传
  - [x] 3.6 原子保存、自身写盘抑制、Tab 销毁清理的生命周期测试
- [x] Phase 4: SFTP 下载通道滑动窗口流水线 (P1)
  - [x] 4.1 自行构造 `RawSftpSession`：`new_with_config` → `init` → `limits` 协商 → `set_limits`
  - [x] 4.2 实现 `PipelinedDownloadReader`（16 个在途 `SSH_FXP_READ`），保留取消、进度节流与 `shutdown` 契约
  - [x] 4.3 编写短读重取、乱序完成、未知长度重试、取消与端到端集成测试；1 GiB 吞吐对比与断链场景未纳入自动化门禁
- [x] Phase 5: 远端冲突时的外部 Diff 工具联动 (P2)
  - [x] 5.1 点击 Compare 时按需懒拉取远端快照，禁止冲突即下载
  - [x] 5.2 `platform` 用绝对路径探测 Diff 工具（`/usr/bin/opendiff`、`/usr/local/bin/code`、`/opt/homebrew/bin/code`），只 `spawn`
  - [x] 5.3 `RemoteConflict` 模态框增加 “Compare…” 与可测试的探测逻辑

---

## 1. 背景与设计原则

### 1.1 核心问题定位

1. **单大文件下载瓶颈**：`russh-sftp` 的 `File::poll_read` 没有在途预读（`russh-sftp-2.3.0/src/client/fs/file.rs:257-290`），每发一个读请求都要等 1 个 RTT，50 ms 链路上吞吐被封在约 1.3 MB/s。
2. **单大文件上传粒度小**：`copy_file_chunks` 硬编码 64 KiB 缓冲（`crates/sftp/src/session_actor.rs:2193`），而 `poll_write` 每次只取 buffer 中 `max_write_len` 这么多数据，所以**帧大小由我们的 buffer 决定，不由 SFTP 配置决定**。
3. **目录传输串行化（弱点 T2）**：`TransferManager::start_queued` 对同一 `TransferPlanId` 强制串行（`crates/sftp/src/transfer_manager.rs:246`）。
4. **远程编辑配置单薄**：只有全局单个 `external_editor`（`crates/storage/src/config.rs:27`）。
5. **本地编辑感知断层**：外部编辑器保存后没有任何 UI 反馈。
6. **远端冲突盲选**：只给覆盖 / 放弃 / 稍后，没有内容对比。

### 1.2 架构硬约束（`AGENTS.md` 与 `docs/architecture.md`）

- **分层**：`core` 纯模型（已启用 serde）；`platform` 只依赖 `core`，不得依赖 `storage`；`sftp` 隔离在 Tokio，与 GPUI 只走有界 channel。
- **安全**：未命中编辑器规则必须走 `/usr/bin/open -t`，绝不裸 `open <file>`（`.command` / `.workflow` 等会被 LaunchServices 直接执行）。
- **显式确认**：本地保存只产生视觉提示，上传始终由用户触发（`docs/architecture.md` §9）。
- **ID 权威**：`TransferId` / `EditSessionId` 由 UI/core 分配，actor 与平台层只回显。

---

## 2. Phase 1: 传输通道调优与 Plan 内并发放开

### 2.1 目标

把单文件吞吐从“每包等一个 RTT”提升到“在途 8~16 个包”，并把目录传输从“单文件串行”放开到 Plan 内 4 并发。

### 2.2 代码改动

#### 1. 块复制缓冲扩容（真正的吞吐杠杆）

`crates/sftp/src/session_actor.rs`，`copy_file_chunks` 约 2193 行：

```rust
// 64 KiB -> 256 KiB。帧大小由这里决定：poll_write 只消费 buffer 中
// max_write_len 的数据，buffer 小则包一定小。
const BUFFER_SIZE: usize = 256 * 1024;
```

#### 2. 会话配置：只改并发写与超时

`open_transfer_sftp`（约 686 行）当前调用 `SftpSession::new(...)`，即全部默认值。默认值中 **`max_packet_len` 已经是 262144**（`russh-sftp-2.3.0/src/client/mod.rs:44-54`），把它再写一遍没有任何效果。真正的改动是：

```rust
let sftp_config = russh_sftp::client::Config {
    // 默认 262144，保持不变；不要在实现里声称这是优化项。
    max_packet_len: 262144,
    // 8 -> 16：仅在“8 x 256 KiB = 2 MiB 在途量 < 链路 BDP”时有收益，
    // 即高 RTT、高带宽链路。低延迟链路提升有限。
    max_concurrent_writes: 16,
    // 10 -> 30：每个请求的超时，给慢盘/慢链路留余量。
    request_timeout_secs: 30,
};
SftpSession::new_with_config(channel.into_stream(), sftp_config)
    .await
    .map_err(/* 保持现有错误映射 */)
```

#### 3. SSH 通道窗口

`crates/sftp/src/physical_connection.rs:472` 的 `client_config`：

```rust
let mut config = client::Config {
    keepalive_interval: Some(Duration::from_secs(15)),
    keepalive_max: 3,
    // 2 MiB -> 4 MiB：避免高 BDP 链路把窗口打满后停等。
    // 代价是每通道最多多占 2 MiB 缓冲，6 并发通道仍然可接受。
    window_size: 4 * 1024 * 1024,
    ..client::Config::default()
};
```

> `maximum_packet_size` 默认 32768，russh 明确拒绝 >65535（`client/mod.rs:1021`）。本阶段不动它；如后续要减少每包封装开销，最多可到 65535，但需先验证中间设备兼容性。

#### 4. Plan 内并发与公平调度

`crates/sftp/src/transfer_manager.rs`：

```rust
// 全局 6：1 个浏览通道 + 最多 6 个传输通道仍在 OpenSSH 默认
// MaxSessions 10 之内，也给加固服务器（常见 4~5）留出失败空间。
pub(crate) const MAX_CONCURRENT_TRANSFERS: usize = 6;
pub(crate) const MAX_CONCURRENT_PER_PLAN: usize = 4;

// 字段：HashSet<TransferPlanId> -> HashMap<TransferPlanId, usize>
running_per_plan: HashMap<TransferPlanId, usize>,
```

`start_queued` 的选取要分两轮，否则大目录 Plan 会凭 FIFO 顺序长期占满 4 个槽位，饿死排在后面的小 Plan：

```rust
// 第一轮：优先给当前 0 并发的 Plan，保证多 Plan 场景都能推进；
// 第二轮：再给已有并发的 Plan 补位到 per-plan 上限。
let position = self
    .pending
    .iter()
    .position(|queued| !self.running_per_plan.contains_key(&queued.plan_id))
    .or_else(|| {
        self.pending.iter().position(|queued| {
            self.running_per_plan
                .get(&queued.plan_id)
                .copied()
                .unwrap_or(0)
                < MAX_CONCURRENT_PER_PLAN
        })
    });
```

递增只在 `open_transfer_sftp` **成功之后**、`self.running.insert(...)` 同时进行。

> 下载目录计划的第一个 job 是目录本身：它先 `remove_dir_all` 目标目录再重建。若子 job 并发启动，刚写入的文件会被这次删除抹掉。因此对 `direction == Download` 的 Plan 追加一道排他门：Plan 的首个 job 必须独自运行并结束后，其余 job 才能启动；非首个目录 job 只 `create_dir_all`，不重复删除目标（整棵树已由首个 job 清空）。上传目录 job 只创建远端目录、从不删除，因此不设这道门，保留 per-plan 并发。`plan_root_jobs` 随 Plan 空闲一并清理，避免长期累积。
>
> 排他门只管“删除顺序”，不传播成功：根 job 失败或取消后，子 job 仍会启动（与串行队列的既有语义一致）。但删除权限是**一次性**的：只要该 Plan 有任一非根 job 启动过，后续重试的根 job 就不再删除目标，只做合并，否则会删掉已完成的子任务输出。重试通过单独请求重新入队，必须显式标记为非根，不能重新推断“首个 job 即根”。
>
> 已知取舍：若用户在根 job 运行之后才把冲突策略升级为 `OverwriteAll`，根 job 已经合并过目标目录，此时只会覆盖冲突文件，不删除本地独有文件。根 job 启动时若已是 `OverwriteAll`，整棵树仍会被清空。
>
> 删除权限同样约束文件/软链分支：无权限时若目标路径是目录，直接以 “Destination Changed Type” 失败而不是 `remove_dir_all`。这样即使远端条目在两次尝试之间从目录变成文件/软链，重试也不会抹掉已完成子任务的输出；普通文件覆盖不受影响。比较下载被用户取消（`TransferSkipped`）也必须清掉 “Comparing…” 状态。

#### 5. 递减计数必须同步三处

漏任何一处都会让 `running_per_plan` 只增不减，最终全局限流死锁：

| 位置 | 场景 | 处理 |
| --- | --- | --- |
| `transfer_manager.rs:108-124` | `complete_rx` 分支，任务正常/失败结束 | 主路径，必须递减 |
| `transfer_manager.rs:146-156` | `Cancel` 命中 `pending` 中尚未启动的任务 | 未递增则无需递减，但要确认没有重复计数 |
| `transfer_manager.rs:288` | `open_transfer_sftp` 失败后直接 `cleanup_plan_if_idle` | 未递增，不得递减；确认计数未提前增加 |

`cleanup_plan_if_idle`（约 227 行）同步改为查新结构，用 `contains_key`：

```rust
let has_running = self.running_per_plan.contains_key(&plan_id);
```

---

## 3. Phase 2: 远程编辑按扩展名智能路由

### 3.1 目标

支持按模式（`*.py`、`*.md`、`Dockerfile`）关联编辑器；未命中严格使用系统文本编辑器。

### 3.2 类型归属：定义在 `core`

`macsftp-platform` 只依赖 `macsftp-core`，拿不到 `storage` 的类型；`core` 已经启用 serde derive，因此类型放 `core`，`storage::AppConfig` 直接引用：

`crates/core/src/core.rs`

```rust
/// 外部编辑器路由规则。匹配语义由 `EditorRule::matches` 统一实现，
/// 避免 storage / platform / app 各写一套。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditorRule {
    pub pattern: String,
    pub app_name: String,
}

impl EditorRule {
    /// 大小写不敏感；只支持 `*` 与 `?`，不做完整 glob 语法。
    pub fn matches(&self, file_name: &str) -> bool { /* 纯函数，core 单测覆盖 */ }
}

/// 规则表求值：First-Match-Wins，调用方保证规则已按“最长后缀优先”排序。
pub fn resolve_editor_app<'a>(
    file_name: &str,
    rules: &'a [EditorRule],
    fallback: Option<&'a str>,
) -> Option<&'a str> {
    rules
        .iter()
        .find(|rule| rule.matches(file_name))
        .map(|rule| rule.app_name.as_str())
        .or(fallback)
}
```

`crates/storage/src/config.rs`：

```rust
pub struct AppConfig {
    pub version: u32,
    pub appearance: AppearancePreference,
    pub confirm_delete: bool,
    pub show_hidden_files: bool,
    pub external_editor: Option<String>,   // 兜底编辑器，None = open -t
    #[serde(default)]
    pub editor_rules: Vec<macsftp_core::EditorRule>,
}
```

**不升 `CURRENT_VERSION`**。新增字段有 `#[serde(default)]` 就足够向后兼容；升版本不仅没有收益，还会直接打破 `crates/storage/src/config.rs:357-369` 里“version 2 必须被拒绝且 `supported: 1`”的既有断言，而 `load_from_disk` 本身也没有写回新版本的迁移步骤。

实现时补一个排序入口（新增/保存规则时按后缀长度降序），并保证同长度时保持用户顺序。

### 3.3 匹配语义（必须写进测试）

- **大小写不敏感**：macOS 文件系统默认不区分大小写，`*.png` 必须命中 `shot.PNG`，否则会退化成 `open -t` 把图片当文本打开。
- **First-Match-Wins + 最长后缀优先**：`*.tar.gz` 与 `*.gz` 并存时，排序后 `*.tar.gz` 先命中。
- **只支持 `*` / `?`**：自己写 20 行匹配器，不为一个配置项引入 `glob` crate（当前 `glob` 只是传递依赖，没有任何 crate 声明它）。`*` 不匹配 `/`，避免模式跨越目录语义。

### 3.4 平台层保持极薄

`macsftp-platform::build_open_command(temp, editor)` 保持现有签名与语义（`crates/platform/src/platform.rs:285-296`）：`Some(app)` → `open -a <app>`，`None` → `open -t`。规则求值在 `core` 完成，`app` 层把解析结果传下来，`platform` 不感知规则表。

### 3.5 Settings 与持久化

- `ConfigStore` 增加 `set_editor_rules(Vec<EditorRule>)`，与现有 `set_confirm_delete` / `set_show_hidden_files` 同构（锁文件 + 原子写）。
- Settings 通用页新增“编辑器关联”表：模式 + 应用名两列，支持增删改与顺序调整；空状态要给出“未配置时使用系统默认文本编辑器”的说明。
- 应用名使用 `open -a` 接受的名称（用户可在 Finder 中复制的应用名），不做 shell 拼接。

---

## 4. Phase 3: 进程级 FSEvents 本地保存感知

### 4.1 为什么必须用 FSEvents，以及为什么只用一个 watcher

VS Code / JetBrains / Vim 的“安全保存”是写临时文件再 `rename` 覆盖，原 inode 立刻变成孤儿，`kqueue` 挂在原 fd 上从此收不到事件；FSEvents 是按路径的，天然免疫。

同时**不要按会话建 watcher**：打开 10 个文件就会有 10 个 FSEventStream，窗口/Tab 销毁时漏注销就是线程泄漏。改为进程级单例，递归监听 `<edits_dir>/<edit_run_id>/`：

```text
<edits_dir>/<edit_run_id>/<session_id>/<file_name>
```

路径结构由 `start_edit_download` 固定生成（`crates/app/src/workspace/remote_edit.rs:256-262`），所以事件路径切分第三段即可解析出 `session_id`；会话已销毁就丢弃事件。

实现要点：

- **依赖**：`notify` 当前是 **8.2.0 且只是传递依赖**（无 crate 声明）。需在 `crates/platform/Cargo.toml` 显式加 `notify`（lock 里已有，不产生新下载）。
- **目录必须先存在**：FSEvents 递归监听要求根目录存在。启动时创建 `<edits_dir>/<edit_run_id>/`（与现有 run namespace 生命周期一致），不要等第一次编辑才建。
- **latency**：使用默认 FSEvents 延迟即可；上层再做 600~800 ms 防抖合并连续保存。
- **过滤**：丢弃 `.*.swp`、`*~`、`*.tmp`、`4913` 等副产物；只接受文件名与会话记录的远端文件名一致的事件。

### 4.2 自身写盘抑制（否则刚打开就显示“已修改”）

编辑下载本身就会往被监听的目录里写文件，watcher 一定会收到我们自己的写入。仅靠文件名匹配区分不了，会在下载完成的瞬间把文件标脏。

规则：

1. 会话进入 `EditPhase::Editing` 时记录基线 `local_baseline: { size, mtime }`（下载完成处，`crates/app/src/event_coordinator.rs` 的 transfer 完成分支）。
2. 收到事件后 `stat` 目标文件：与基线完全一致 → 视为自身写盘或编辑器只改了元数据，**丢弃**。
3. 只有 `phase == Editing` 时才接受事件；`Downloading` / `CheckingRemote` / `UploadingBack` / `RemoteConflict` 期间一律忽略。
4. 复位时机：上传成功后重新记录基线并清除标记；`DiscardLocal` 重新下载后同样重置；`Later` 不改动标记。

### 4.3 事件与状态

- `crates/core/src/core.rs`：`EditSession` 增加 `local_modified: bool`；新增 `AppEvent::LocalEditModified { session_id, temp_path }`。事件是进程级（与 transfer 事件同类），进入状态机后仍必须校验 `session_id` 存在且 `local_temp_path` 匹配，避免旧事件串台。
- `crates/app/src/event_coordinator.rs`：分发到各窗口 Workspace，命中 session 才置位并 `cx.notify()`。
- **绝不发起上传**。

### 4.4 UI 与快捷键（需要新增 Action）

现状：`upload_selected_edit` 只由状态栏按钮直接调用（`crates/app/src/workspace/transfer_render.rs:692`），没有 Action。加 `⌘⇧U` 必须同时改四处，否则菜单/调色板/键盘行为会不一致（`crates/app/src/app_menus.rs:1-14` 的边界测试会直接失败）：

1. `crates/app/src/app_actions.rs`：新增 `UploadEditedFile`，`KeyBinding::new("cmd-shift-u", ..., Some("Workspace"))`（`⌘U` 已被 `UploadSelection` 占用）。
2. `crates/app/src/app_menus.rs`：File（或 Edit）菜单项。
3. `crates/app/src/palette_commands.rs`：命令面板项。
4. enabled 判定：无选中、选中非远端文件、或该文件不在 `Editing` 阶段时禁用，并在状态栏给出一句原因。

视觉：文件名前 `●`（`warning`/`accent`），状态栏按钮在脏状态下用 Accent；上传成功或 `DiscardLocal` 后圆点消失。窄 pane（约 360 px）下标记不得挤压文件名截断逻辑。

---

## 5. Phase 4: SFTP 下载滑动窗口流水线

### 5.1 为什么不能复用 `SftpSession`

`SftpSession.session` 是私有字段且没有访问器（`russh-sftp-2.3.0/src/client/session.rs:28-31`），无法从中取出 `Arc<RawSftpSession>`。必须自己构造 raw 会话，并**手工复刻 limits 协商**，否则 `RawSftpSession::read` 的 `self.limits.read_len` 为 `None`（`rawsession.rs:330`），不会按服务端声明的 `max_read_len` 截断，对声明了限额的服务器会发出超大读请求。

```rust
use std::sync::Arc;
use russh_sftp::client::RawSftpSession;

let mut raw = RawSftpSession::new_with_config(channel.into_stream(), sftp_config);
let version = raw.init().await.map_err(/* ... */)?;

// 复刻 SftpSession::new_with_config 的协商（session.rs:63-70）。
// 缺这一步，read 不会被服务端的 max_read_len 约束。
if version
    .extensions
    .get("limits@openssh.com")
    .is_some_and(|value| value == "1")
{
    let extension = raw.limits().await.map_err(/* ... */)?;
    raw.set_limits(/* Limits::from(extension) */);
}

let raw = Arc::new(raw);
```

同一会话里的 open / close 也必须走 raw：`raw.open(path, OpenFlags::READ, FileAttributes::empty())` 拿 handle 字符串，结束后 `raw.close(handle)`。不要让 `SftpSession` 与 raw 混用同一个通道。

### 5.2 读取器结构

`crates/sftp/src/pipelined_reader.rs`（新文件）：

```rust
pub struct PipelinedDownloadReader {
    raw_session: Arc<RawSftpSession>,
    file_handle: String,
    chunk_size: u32,       // 受 max_read_len 与 max_packet_len 约束
    window_capacity: usize, // 16
    next_request_offset: u64,
    in_flight: VecDeque<(u64, oneshot::Receiver<Result<Vec<u8>, UserFacingError>>)>,
    eof_reached: bool,
}
```

- 发射端：`in_flight.len() < window_capacity && !eof_reached` 时持续发 `read(handle, offset, chunk_size)`，`next_request_offset += chunk_size`。
- 落盘端：按 offset 直接 `seek` + 写（`pwrite` 语义），不等待队头，乱序到达也不阻塞。
- 终止：`StatusCode::Eof` 或空数据 → `eof_reached`；文件总长已知时以 `size` 为准校验最终写入长度。

### 5.3 必须保留的既有契约

`copy_file_chunks`（`session_actor.rs:2181-2227`）现在负责的不只是搬运字节，新的读取器必须逐条对齐，否则会默默退化：

| 契约 | 现状 | 要求 |
| --- | --- | --- |
| 取消 | 每次 read/write 都 `tokio::select!` 在 `cancel.cancelled()` 上 | 发射与落盘两侧都要检查，取消后不留 `.macsftp-part` |
| 进度 | `ProgressThrottle::new(10)` + 结束补一次 | 保持同等节流与最终 100% 事件 |
| 落盘收尾 | `destination.shutdown()` | 本地文件必须 flush/shutdown |
| 错误映射 | `local_transfer_error("Transfer Read Failed", ...)` | 保持同样的 `UserFacingError` 与 retryable 语义 |

---

## 6. Phase 5: 远端冲突的外部 Diff 联动

### 6.1 按需懒拉取

冲突判定时**只弹窗**，不下载任何远端内容——否则用户直接点“Discard My Changes”就白拉了可能几百 MB 的数据。仅当用户点击 “Compare Changes…” 时才发起只读下载到 `<edits_dir>/<run_id>/<session_id>/conflict-remote.<ext>`，按钮期间显示 spinner，失败要落回可重试状态而不是卡死。

### 6.2 工具探测必须用绝对路径

从 Finder 启动的 `.app` 继承极简 PATH（`/usr/bin:/bin:/usr/sbin:/sbin`），`Command::new("code")` 在打包后**一定找不到**，只在终端 `cargo run` 时“看起来正常”。探测按绝对路径枚举，且逻辑要可单测：

```rust
/// 候选路径注入，便于单测不依赖本机安装情况。
pub fn detect_diff_tool(candidates: &[std::path::PathBuf]) -> Option<DiffTool> { /* ... */ }

pub const DIFF_CANDIDATES: &[&str] = &[
    "/usr/bin/opendiff",        // Xcode Command Line Tools (FileMerge)
    "/usr/local/bin/code",
    "/opt/homebrew/bin/code",
];
```

- `opendiff` 与 `code --diff` 都只用 `spawn()`；`opendiff` 是 GUI 启动器，用 `output()` 会一直阻塞到 GUI 关闭。
- `code --version` 之类探测同样走绝对路径。
- 两者都不存在时，`Compare…` 按钮禁用并提示“未找到可用的对比工具”，不要弹错误框。

### 6.3 交互

`resolve_edit_conflict`（`crates/app/src/workspace/remote_edit.rs:346`）的三选项保持不动，新增 Compare 只读操作，不改变 `EditPhase`。对比期间模态框继续显示，用户回来看完再决定；临时快照随会话清理一起删除。

---

## 7. 测试与验收门禁

| 层级 | 覆盖点 | 命令 |
| --- | --- | --- |
| core 单测 | `EditorRule::matches` 大小写不敏感、`*`/`?` 语义、最长后缀优先、`None` 兜底 | `cargo test -p macsftp-core` |
| storage 单测 | `editor_rules` 缺省为空、往返序列化、**version 1 仍可读且 future-version 断言不变** | `cargo test -p macsftp-storage` |
| platform 单测 | `build_open_command` 两条分支不变；`detect_diff_tool` 用候选路径注入覆盖“只有 opendiff / 只有 code / 都没有” | `cargo test -p macsftp-platform` |
| sftp 单测 | `start_queued` 的 per-plan 上限、两 Plan 并行推进不互相饿死、三个递减点后计数归零、`cleanup_plan_if_idle` 不提前清策略；下载 Plan 根 job 排他、重试根不得删除已完成的子任务输出；短读重取、乱序完成、未知长度重试 | `cargo test -p macsftp-sftp` |
| sftp 集成 | 目录并发上传完整性与冲突策略；目录下载后不再被兄弟目录 job 删除；取消后无残留；EOF 与已知长度校验。**未覆盖**：1 GiB 吞吐对比、断链 | `cargo test -p macsftp-sftp --test integration`（无 Docker 时明确 skip） |
| app 单测 | 自身写盘不标脏、编辑器保存标脏、`DiscardLocal` 与上传成功复位、跨 Tab 迟到事件被丢弃、`⌘⇧U` 在不可编辑选区时禁用 | `cargo test -p macsftp-app` |
| 手动 | 720×480 窄窗口下 `●` 不挤压文件名；浅色/深色对比度；`⌘⇧U` 可达性 | 记录到 `docs/visual-evidence/` |

---

## 8. 文档与交付

- **Phase 3 开工前**必须先改 `docs/architecture.md` §9 现在那句“应用不监视外部编辑器的保存动作”，改为“允许基于 FSEvents 的本地保存感知，但只用于视觉提示，上传仍需显式触发”。不改文档就改行为，违反 AGENTS §10。
- Phase 2 涉及用户可见设置项，同步 `README.md` 与 `CHANGELOG.md`。
- 交付后把仍有效的约束（per-plan 并发上限、编辑器路由的兜底规则、FSEvents 单例与自身写盘抑制）并回 `docs/architecture.md`，并按 `docs/README.md` 的维护规则删除本实施计划文件。
