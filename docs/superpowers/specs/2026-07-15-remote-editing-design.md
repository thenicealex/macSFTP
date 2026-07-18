# 远程编辑（Remote Editing）设计

- **日期**: 2026-07-15
- **状态**: 已批准，待实现
- **范围**: 为 macSFTP 增加"下载 → 外部编辑 → 自动回传"的远程文件编辑能力

## 1. 目标与动机

当前 macSFTP 双击/回车远程**文件**时没有任何行为（`open_entry_at` at
`crates/app/src/workspace/panes.rs:321` 只对目录导航进入）。远程编辑填补这个空白：
让用户像 Transmit / Cyberduck / FileZilla 一样，在本地熟悉的编辑器里改远程文件，
保存后自动同步回服务器。

底层能力已具备并将被复用：下载/上传传输（带进度、冲突、取消、重试）、
`RemoteSessionActor` 会话、`TransferStore` 事件流、`AppResources` 进程级共享状态、
`AppEventCoordinator` 的 `cx.spawn` 长驻异步循环模式。

## 2. 已定决策（澄清结论）

| 维度 | 决策 |
|---|---|
| 交互模型 | 下载 → 外部编辑 → 自动回传（经典 Edit 模式），复用现有传输机制 |
| 打开方式 | 系统默认关联应用；支持在设置里配置自定义编辑器 |
| 回传触发 | 自动监视——保存即回传 |
| 监视机制 | **轮询 mtime**（复用 GPUI `background_executor().timer()`），不引入 `notify` 依赖 |
| 会话生命周期 | **应用级**——只要 macSFTP 开着就监视；退出时清理临时文件 |
| 会话注册表位置 | `AppResources`（GPUI Global，跨窗口共享），与现有 `residual_temps` 并列 |
| 远程冲突 | 回传前比对 `(size, mtime)` 快照；远程已改动则弹警告 |
| 变更检测 | 全程只用 `(size, mtime)`，**不加内容哈希**（本地或远程都不加） |
| 文件类型限制 | 不限类型 |
| 大文件阈值 | **100 MB** 以上弹确认后再下载 |
| 断线处理 | **保留会话+临时文件**，并给明确提示；重连后可继续保存 |
| 重复 Edit | 同一 `(profile_id, remote_path)` **复用会话**，不重复下载 |

### 监视机制选型理由（轮询 vs FSEvents）

项目依赖纪律很强（vendored russh、`deny.toml`、因 RUSTSEC-2023-0071 禁用 RSA）。
为一个"保存后约 1 秒内回传"完全够用的功能引入 FSEvents（`notify`）依赖不划算。
活跃编辑文件通常个位数，`std::fs::metadata` 轮询成本可忽略，且与现有
`residual_temps` 清理、事件泵架构风格一致。监视器逻辑通过接口隔离，
若日后需要即时性可无痛替换为 `notify`。

### 不加哈希理由

SFTP 无"远程哈希"原语（russh-sftp 只有文件读写，无 `check-file` 扩展；
本项目是纯 SFTP 子系统，不跑 exec/shell）。远程哈希需重新下载整个文件，
对大文件是双倍流量，明显过重。`(size, mtime)` 是行业标准做法，
真正漏检窗口极窄（他人在同一秒内改成完全相同大小），可接受。

## 3. 架构总览与数据流

一次远程编辑的完整生命周期：

```
用户在远程面板对文件点 "Edit"（或双击/回车远程文件）
  │
  ▼
① 大文件检查 (>100 MB → 弹确认)
  │
  ▼
② 下载到 ~/Library/Application Support/macSFTP/edits/<edit-session-id>/<filename>
   复用现有 StartTransfer(Download) 机制；记录 transfer_id
  │
  ▼
③ 下载完成 (TransferCompleted) → 在注册表登记 EditSession：
     { remote_path, tab_id, session_epoch, profile_id, local_temp_path,
       remote_snapshot(size+mtime), local_mtime(下载后), active_transfer }
   然后用 `open` 打开（系统默认应用 / 用户配置的编辑器）
  │
  ▼
④ 轮询循环 (每 ~1s) stat 所有 Editing 阶段的临时文件
     本地 mtime 变化 → 触发回传
  │
  ▼
⑤ 回传前：重新 stat 远程文件，比对 remote_snapshot
     ├─ 一致 → 上传覆盖 (StartTransfer Upload)，完成后刷新 remote_snapshot
     └─ 变化 → 弹「远程已改动」警告 (覆盖 / 取消)
  │
  ▼
⑥ 应用退出 → 清理 edits/ 整个目录；启动时兜底清理残留
```

**关键复用点**：下载/上传完全走现有 `StartTransfer` command + `TransferStore`
事件流，不新造传输代码。编辑相关的传输默认不弹出传输抽屉（`drawer_open` 不强制打开），
但仍可在抽屉看到。

### 组件划分（遵循现有 crate 边界）

| Crate | 新增/改动 | 职责 |
|---|---|---|
| `macsftp-core` | `EditSession`、`EditSessionId`、`RemoteSnapshot`、`EditPhase` 模型 + `EditSessionStore` 纯集合逻辑 + 纯状态机（`local_changed`、`remote_diverged`） | 纯逻辑，可单测 |
| `macsftp-platform` | `AppPaths.edits_dir`、`open_in_editor()`（封装 `open`）、edits 目录创建/清理 | macOS 边界 |
| `macsftp-storage` | `AppConfig.external_editor: Option<String>` + `set_external_editor()` | 持久化配置 |
| `macsftp-app` | `EditSessionStore` 挂到 `AppResources`、`EditWatcher`（`cx.spawn` 轮询循环，仿 `AppEventCoordinator`）、Edit 动作接线、UI（右键菜单项、大文件确认弹窗、远程冲突弹窗、设置项） | 编排与 UI |

## 4. 组件细节与状态机

### 4.1 `macsftp-core` 模型

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditSessionId(pub u64);

/// 一次远程编辑的完整状态。纯数据，无 I/O。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSession {
    pub id: EditSessionId,
    pub remote_path: RemotePath,
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub local_temp_path: LocalPath,
    pub phase: EditPhase,
    /// 下载时远程文件的 (size, mtime) 快照，用于回传前冲突检测
    pub remote_snapshot: RemoteSnapshot,
    /// 上一次已知的本地临时文件 mtime；轮询用它判定"是否被保存"
    pub local_mtime: Option<Timestamp>,
    /// 关联当前进行中的传输（下载或回传），用于把 TransferCompleted 映射回会话
    pub active_transfer: Option<TransferId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSnapshot {
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPhase {
    Downloading,           // 初次下载中
    Editing,               // 已在编辑器打开，轮询监视中
    UploadingBack,         // 检测到保存，回传中
    RemoteConflict,        // 回传前发现远程已改动，等用户决策
    Failed { error: UserFacingError },
}
```

**纯状态机函数（在 core 里，完全可单测，无 I/O）**：

```rust
impl EditSession {
    /// 轮询判定：本地临时文件 mtime 是否比上次记录的新 → 需要回传。
    /// 仅在 Editing 阶段返回 true（下载/回传中不重复触发）。
    pub fn local_changed(&self, current_mtime: Option<Timestamp>) -> bool;

    /// 回传前冲突判定：当前远程快照 vs 下载时快照。
    pub fn remote_diverged(&self, current: RemoteSnapshot) -> bool;
}
```

`EditSessionStore`（也放 core，纯集合逻辑）：
`register / get / find_by_transfer / find_by_temp_path / update_phase / remove / all_active`。
"哪个 transfer 属于哪个编辑会话"的映射是纯逻辑，app 层只管 I/O 和 UI。

### 4.2 状态机流转

```
        register(下载发起)
            │
            ▼
      ┌─────────────┐  TransferCompleted(download)
      │ Downloading │─────────────┐
      └─────────────┘             │  记录 local_mtime, 调 open()
            │ TransferFailed      ▼
            ▼               ┌───────────┐
        ┌────────┐         │  Editing  │◀────────────┐
        │ Failed │         └───────────┘             │ 上传完成
        └────────┘          │ 轮询: local_changed     │ 刷新 remote_snapshot
                            ▼                         │
                    remote_diverged?                  │
                     ├─ 否 ─▶ UploadingBack ──────────┘
                     └─ 是 ─▶ RemoteConflict
                                 │ 用户选"覆盖" ─▶ UploadingBack
                                 │ 用户选"取消" ─▶ Editing(回到监视，不回传)
```

`UploadingBack` 上传**失败**（连接断开除外，见边界表）：回到 `Editing` 并保留
临时文件，状态栏提示回传失败可重试——不进 `Failed`（`Failed` 仅用于初次下载
失败这种"从未成功打开编辑"的终态）。

**去抖策略**：检测到 `local_changed` 后进入 `UploadingBack`，该阶段轮询不再触发
新回传；回传完成刷新 `local_mtime` + `remote_snapshot` 再回到 `Editing`。
避免编辑器多次写盘（如临时 `.swp`、原子替换）造成重复上传。

### 4.3 `EditWatcher`（app 层，仿 `AppEventCoordinator`）

```rust
pub struct EditWatcher { _task: Task<()> }

impl EditWatcher {
    pub fn start(cx: &mut App) -> Self {
        let task = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await; // ~1s
                if cx.update(|cx| poll_edit_sessions(cx)).is_err() {
                    break; // app 关闭
                }
            }
        });
        Self { _task: task }
    }
}
```

`poll_edit_sessions`：遍历 `Editing` 阶段的会话 → `std::fs::metadata` 取 mtime →
`session.local_changed()` → 命中则发起回传（先查远程快照）。
空注册表时轮询几乎零成本（直接返回）。

## 5. UI 接线

### 5.1 入口

**远程面板右键菜单**（`crates/app/src/workspace/file_ops.rs:690` 附近，
现有 `has_entry && !is_local && connected` 分支里，在 Download 之前插入）：

```
Edit                    ← 新增，仅对文件（非目录）显示
Download
Copy Path
```

**双击/回车远程文件**默认走 Edit（目前 `open_entry_at` at `panes.rs:321`
对文件无操作）——最自然的入口。目录仍是导航进入。

### 5.2 弹窗

**大文件确认弹窗**：点 Edit 时若 `size > EDIT_SIZE_WARN_THRESHOLD`（**100 MB**），
弹确认「该文件较大（X MB），仍要下载编辑吗?」→ 继续 / 取消。
复用现有 delete-confirm 弹窗的样式模式。

**远程冲突弹窗**（回传前 `remote_diverged`）：「远程文件自你打开后已被改动。
覆盖将丢失远程的改动。」→ 覆盖 / 取消。取消则回到 `Editing`（继续监视，
下次保存再问）。复用现有 transfer-conflict 弹窗的呈现/窗口归属机制
（`event_coordinator.rs` 的 `present_orphaned_transfer_conflicts` 模式），
保证多窗口下只有一个窗口弹。

### 5.3 设置项

`settings_render.rs` + config 新增「外部编辑器」文本输入：
空 = 系统默认（`open <file>`），非空 = `open -a <editor> <file>`。默认空。

### 5.4 打开编辑器（platform 层）

```rust
pub fn open_in_editor(temp: &LocalPath, editor: Option<&str>) -> std::io::Result<()> {
    match editor {
        None => Command::new("/usr/bin/open").arg(temp.as_str()).spawn(),
        Some(app) => Command::new("/usr/bin/open").args(["-a", app, temp.as_str()]).spawn(),
    }.map(|_| ())
}
```

用 `open`（而非直接 exec）天然处理 `.app` 关联、无阻塞、无需管理子进程生命周期。
这也天然满足"应用级监视"——`open` 立即返回，不绑定进程树。
`open_in_editor` 通过可注入的 trait/函数指针暴露，测试里替换为 mock。

## 6. 边界情况

| 情况 | 处理 |
|---|---|
| 下载失败 | 会话进 `Failed`，状态栏提示；不打开编辑器；临时文件按现有传输残留机制清理 |
| 回传时连接已断/session_epoch 过期 | 用现有 stale-event guard 判断；会话**保留**在 `Editing`，状态栏明确提示「已断线，重连后可继续保存」——临时文件和会话不丢 |
| 同一远程文件被 Edit 两次 | 注册表按 `(profile_id, remote_path)` 查重：`Editing` / `UploadingBack` 会话再次调用 `open_in_editor` 打开已有临时文件，不重复下载；`Downloading` 等待完成；`RemoteConflict` 先处理冲突 |
| 编辑器原子替换文件（inode 变了） | 轮询用路径 `stat` 而非 fd，mtime 变化照样捕获；mtime 变即回传 |
| 临时文件被用户手动删除 | 轮询 `stat` 报 NotFound → 会话标记结束并从注册表移除，状态栏提示 |
| 回传中用户又保存一次 | `UploadingBack` 阶段不触发新回传（去抖）；完成刷新基准后回 `Editing`，后续保存正常触发 |
| 应用退出 | Drop `EditSessionStore` 或退出钩子 → 删除整个 `edits/` 目录 |
| 启动残留（上次崩溃） | 仿 `reconcile_local_residual_temps`：启动时清空 `edits/` 目录（编辑会话不跨重启持久化） |

### 临时文件布局

```
~/Library/Application Support/macSFTP/edits/
    <app-run-id>/
        <edit-session-id>/
            <原始文件名>      ← 保留原名，编辑器标题栏显示正确，后缀关联正确
```

每次进程运行使用唯一目录，避免外部编辑器在 macSFTP 重启后把新文件误认为上次运行中已打开、后被删除的同一路径文档。每会话独立子目录，避免不同远程路径的同名文件冲突（如两个 `config.json`）。

## 7. 测试策略

遵循项目现有分层测试风格（core 纯逻辑单测、app 用 `gpui::test`、
sftp 用真实 sshd fixture）。

### 7.1 `macsftp-core` — 纯状态机单测（主战场，无 I/O）

| 测试 | 验证 |
|---|---|
| `local_changed_only_in_editing_phase` | Downloading/UploadingBack 阶段 mtime 变化不触发；Editing 阶段才触发 |
| `local_changed_detects_newer_mtime` | mtime 前进 → true；相同/为 None → false |
| `remote_diverged_on_size_change` | size 不同 → true |
| `remote_diverged_on_mtime_change` | mtime 不同 → true |
| `remote_diverged_false_when_identical` | (size,mtime) 都相同 → false |
| `store_register_and_find_by_transfer` | 下载/回传 transfer_id 能映射回会话 |
| `store_dedup_by_profile_and_remote_path` | 同 (profile_id, remote_path) 重复注册 → 返回已有会话，不新建 |
| `store_remove_clears_lookups` | 移除后按 id / transfer / path 都查不到 |
| `phase_transitions_are_valid` | 非法流转（如 Failed→Editing）被拒或不发生 |

### 7.2 `macsftp-app` — 集成/交互测试（`gpui::test`）

| 测试 | 验证 |
|---|---|
| `edit_action_starts_download_to_edits_dir` | 点 Edit → 发出 StartTransfer(Download)，目标在 edits/ 子目录 |
| `download_complete_registers_session_and_opens` | TransferCompleted → 会话进 Editing、记录 local_mtime（open 调用注入 mock 验证被调） |
| `poll_triggers_upload_on_local_change` | 注入变新的 mtime → 发出 StartTransfer(Upload) 回原远程路径 |
| `poll_ignores_unchanged_files` | mtime 不变 → 不发传输 |
| `remote_divergence_presents_conflict_modal` | 回传前远程快照变 → 弹冲突弹窗，不直接上传 |
| `conflict_overwrite_proceeds_upload` / `conflict_cancel_returns_to_editing` | 两个决策分支正确 |
| `duplicate_edit_focuses_existing_session` | 第二次 Edit 同文件 → 不重复下载 |
| `large_file_prompts_confirmation` | size>100MB → 弹确认；确认后才下载 |
| `disconnect_preserves_session_with_notice` | session_epoch 过期 → 会话保留 + 状态提示，不丢临时文件 |
| `external_editor_config_round_trips` | 设置外部编辑器 → 持久化 + 读回 |

**可测性设计**：`open_in_editor` 通过可注入的 trait/函数指针，测试里替换为 mock
（不真的 spawn `open`）；轮询循环的时钟/mtime 读取也注入，避免真实等待和真实
文件系统依赖。

### 7.3 `macsftp-platform` — 轻量单测

- `edits_dir` 路径正确、`ensure_directories` 覆盖它
- `open_in_editor` 命令构造正确（参数拼接，不实际 spawn）

### 7.4 手动验证（UI 无法完全自动化的部分）

- 真实连 SFTP → Edit 一个文件 → 改保存 → 看到自动回传 → 远程确实更新
- 配置 VS Code 作为外部编辑器再验一次
- 断线场景验证提示出现

## 8. 不做的事（YAGNI）

- 不做内置文本编辑器（仅外部编辑器往返）
- 不引入 `notify`/FSEvents 依赖（轮询足够）
- 不加内容哈希（本地或远程）
- 不做编辑会话跨重启持久化（启动清空 edits/）
- 不做文件类型白名单/过滤
