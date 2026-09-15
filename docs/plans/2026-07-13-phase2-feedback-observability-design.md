# 阶段 2 设计 — 反馈与可观测性

**Date:** 2026-07-13 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 2；`docs/ui-ux-guidelines.md` §2.3 / §4 / §6.3 / §7 / §10。
**方法：** brainstorming 流程；用户已经确认 5 个决策点和交付形式，详情参见末尾的“决策记录”。
**前置条件：** 阶段 1 已经实现核心文件操作，包括 `FsCommand`、删除确认和上下文菜单等功能。

---

## 决策摘要（已确认）

1. **速度/ETA 的计算位置：** 在纯 view 侧使用滑动窗口 `RateSampler`，因此不修改 `core` 或 `sftp` 的进度协议。
2. **首次加载形式：** 使用居中的 spinner 和短文案“Loading…”，不使用 skeleton。
3. **Retry 语义：** 仅重新读取当前 path 的目录，即使用 `ReadRemoteDir` 或本地 `load_local_directory`；不再次执行 Fs 增删改操作。
4. **连接取消：** 复用 `AppCommand::DisconnectTab` 并清理本地状态，因此不新增 `CancelConnect`。
5. **Status bar 传输提示：** 摘要支持切换 transfer drawer 的展开状态，并保留现有 toggle 行为，同时增加选中计数等信息。
6. **交付形式：** 一份设计包含 2a–2d；实现阶段可以拆分 PR，但是规格在本文中完整定义。

---

## 1. 目标与非目标

### 目标

所有“进行中、加载中、出错”状态均在对应界面区域明确显示，因此界面不再使用长期占位内容，也不再忽略这些状态：

| 子项 | 成功标准 |
| --- | --- |
| **2a** 真实速度 + ETA | transfer row 不再长期显示字面量 `— MB/s · ETA —`；速度经过平滑处理；停滞时显示 **Stalled**；drawer 顶部显示总速度和总 ETA |
| **2b** 加载与连接 | 首次加载显示 spinner；重新加载时保留列表并显示 Refreshing…；Connecting 显示目标 host 和 **Cancel** |
| **2c** 就地错误 | 目录读取失败时，pane 内显示 title、message 和 **Retry**；status bar 仅显示摘要 |
| **2d** status bar | drawer 关闭时仍显示 active/failed 提示，而且可以通过该提示展开 drawer；focused pane 的选中数量大于 0 时显示计数 |

### 非目标

- 不修改 `TransferProgress` 或 `TransferState::Running` 的 wire 结构以传递 rate。
- 不实现 skeleton rows。
- 不再次执行失败的 `FsCommand`。
- 不实现 command palette、导航历史和 MRU tab，这些内容属于阶段 3/4。
- 不实现 status bar popover 详情列表。
- 不要求 status bar 显示 MB/s，因为窄窗口需要优先保证主要信息，而速率信息由 drawer 显示。

---

## 2. 传输速度 + ETA（2a）

### 2.1 数据模型（view-only）

在与 `SharedTransfers` 生命周期相同的辅助 map 中保存采样数据。推荐将其定义为 `SharedTransfers` 的字段，或者由 `Workspace` 持有并在 event drain 时更新。采样数据应位于共享传输层：如果多个窗口分别订阅 progress，那么各窗口分别更新；如果多个窗口共享同一个 `SharedTransfers`，那么它们也共享同一个 sampler map。

```rust
struct RateSample {
    at: Instant,
    bytes_done: u64,
}

struct RateSampler {
    /// 保留约 WINDOW_SECS 内的样本（默认 4s）。
    samples: VecDeque<RateSample>,
}

// transfer_id → sampler；job 进入终态时 remove
transfer_rates: HashMap<TransferId, RateSampler>
```

采样数据**不**写入 `TransferJob`，不序列化，也不写入 history JSON。

### 2.2 更新时机

| 事件 | 行为 |
| --- | --- |
| `TransferProgress` | 执行 `sampler.push(now, bytes_done)`，并丢弃窗口以外的旧样本 |
| `TransferRunning` / 首次进入 `Running` | 初始化 sampler；如果 progress 已经包含 bytes，则记录 `started_at` 对应的第一个点 |
| `TransferCompleted` / `Failed` / `Skipped` | 执行 `transfer_rates.remove(id)` |
| `Cancelling` | 不再显示 ETA；可以显示 Cancelling…，而且不再更新 speed |

runtime 已经根据 ADR-002 对进度事件执行源头节流。因此，UI 侧采样仅依据**已经收到的** progress 进行窗口聚合，不对事件发送执行第二次节流。

### 2.3 算法（纯函数，可以进行单元测试）

以下为默认常量。实现阶段可以调整这些值，但是测试应固定所采用的默认值：

| 常量 | 默认 | 含义 |
| --- | --- | --- |
| `WINDOW_SECS` | 4.0 | 滑动窗口长度 |
| `WARMUP_SECS` | 0.5 | elapsed 小于该值时不显示速度 |
| `STALL_SECS` | 3.0 | 字节数超过该时长没有增加时显示 Stalled |

```text
speed_bps =
  if samples < 2 or elapsed < WARMUP_SECS: None
  else: (bytes_last - bytes_first) / elapsed_secs

stalled =
  speed is 0 (or None after warmup) AND
  (now - last_bytes_change_at) >= STALL_SECS
  // last_bytes_change_at：最近一次 bytes_done 严格增大的时刻

eta_secs =
  if stalled or speed is None or bytes_total is None or speed == 0: None
  else: (bytes_total - bytes_done) / speed_bps
```

**显示规则遵守规范 §7，因此动画不得掩盖实际停滞：**

- 正常：`{done} / {total} · {speed} · ETA {eta}`，例如 `12.4 MB / 100 MB · 3.2 MB/s · ETA 27s`。
- 预热：`{done} / {total} · — MB/s · ETA —`。该内容仅在短暂预热期显示，不是长期占位内容。
- 无 total：`{done} · {speed}`，因此不显示 ETA。
- **Stalled：** `{done} / {total} · Stalled`。进度条仍然根据实际 `bytes_done/total` 显示，不模拟进度增加。

速度大于或等于 1 MB/s 时使用 `X.Y MB/s`，否则使用 `X.Y KB/s`。ETA 小于 1 分钟时使用秒，否则使用 `Xm Ys` 或 `Xh Ym`。

### 2.4 Drawer 顶部聚合

在 transfer drawer 顶部的 section header 旁边或单独一行显示：

- `N active · {aggregate_speed}`
- 可以估算时，再显示 `· ETA {aggregate_eta}`

**聚合规则：**

- `aggregate_speed` 等于所有 `Running` job 的 `speed_bps` 之和，其中 None 按 0 计算。
- `remaining_bytes` 等于 Σ max(0, total − done)，而且只计算存在 `bytes_total` 的 job。
- 当 `aggregate_speed > 0` 且 remaining 有定义时，`aggregate_eta = remaining_bytes / aggregate_speed`。
- 所有 job 均为 stalled 或没有 running job 时，不显示无依据的 ETA。

### 2.5 渲染位置

`render_transfer_job` 中 `TransferState::Running` 的 `detail` 字符串应调用 rate 查询 API，因此需要删除以下硬编码：

```text
"{} / {} · — MB/s · ETA —"
```

---

## 3. 加载与连接中（2b）

### 3.1 远程目录加载三态

| 场景 | 条件 | UI |
| --- | --- | --- |
| 首次加载 | `remote.is_refreshing && remote.entries.is_empty()` | 保留 path bar；列表区域居中显示 **spinner + “Loading…”** |
| 重新加载 | `is_refreshing && !entries.is_empty()` | 保留原列表；path bar 显示已有的“Refreshing…” |
| 就绪 | `!is_refreshing` | 显示正常列表或“Empty directory” |

本地 pane 继续同步执行 `read_local_directory`，因此本阶段不增加本地异步 spinner。

**Spinner 实现：** 优先复用或扩展 `empty_state`，也可以在 `crates/ui` 中增加最小的 `loading_spinner()`，但是不增加第三方依赖。spinner 区域应使用固定高度，因此加载状态变化不会导致布局位移。

### 3.2 连接中 / 等待 host key

远程 pane 在以下状态中显示 empty_state：

| `ConnectionState` | 标题/文案 | 动作 |
| --- | --- | --- |
| `Connecting` / `Reconnecting` | host（`tab.title`）+ “Connecting…” | **Cancel** |
| `AwaitingHostKey` | host + “Waiting for host key…” | **Cancel**，其语义与 modal Reject 一致，即拒绝 trust 并断开连接 |
| 其他失败或断开状态 | 继续使用现有文案与 Reconnect / Edit Connection | 不变 |

### 3.3 Cancel 行为（决策 4）

```text
Cancel connect:
  1. send AppCommand::DisconnectTab { tab_id }
  2. if AwaitingHostKey with live request_id → 同时 RejectHostKey（或依赖 disconnect 清理 registry；实现时二选一并测清楚，避免重复 resolve）
  3. UI: tab.disconnect(UserRequested) 或等价；清 remote path/entries/error
  4. drain_expired_modals；焦点回 pane
```

**约束：**

- Reconnect 时 epoch 已经加 1，因此 stale guard 会忽略旧 actor 的延迟事件，这是现有行为。
- Cancel 不得使用 `unwrap`；channel full 时，按照现有 `send_command` 模式在 status bar 显示提示。

---

## 4. 就地错误恢复（2c）

### 4.1 目录读取失败

当 `tab.remote.error = Some(UserFacingError)`，而且 tab 已 Connected 或已经请求过路径时：

- pane 内显示 `title` 和 `message`。`detail` 默认不向用户显示，以避免内部术语；MVP 不提供折叠详情。
- 显示 **Retry** 按钮（决策 3），并执行 `request_remote_directory(tab_id, path)`。`path` 使用 `tab.remote.path`；如果其值为 None，则隐藏或禁用 Retry。
- 用户选择 Retry 后，清除 error 或将 `is_refreshing` 设为 true，并与正常 refresh 使用相同逻辑。因此，错误文案和 Loading 状态不会同时显示。

`RemoteDirLoaded` 的成功路径必须执行 `remote.error = None`，并保留现有逻辑。

### 4.2 Fs 失败

`FsOperationFailed` **仅在 status bar 显示摘要**，这与阶段 1 的约定一致。列表仍然可用，而且不提供自动再次执行操作的功能。用户可以通过菜单或快捷键再次发起操作。

### 4.3 status bar 与 pane 的职责

| 位置 | 内容 |
| --- | --- |
| pane | 目录错误详情和 Retry |
| status bar | 一行截断摘要，其中包含最近的 `status_message`；该摘要不能替代 pane 中的信息 |

---

## 5. Status bar 增强（2d）

当前实现已经包含连接状态色点与 host、可选的 `status_message`，以及右侧支持切换 drawer 状态的 active/failed 摘要。

本阶段增加以下行为：

| 项 | 行为 |
| --- | --- |
| 传输摘要 | drawer **关闭时仍然显示**；`failed > 0` 时，failed 部分使用 `theme.colors.error` |
| 点击 | 继续切换 `drawer_open`（决策 5）；保留 ⌘J tooltip |
| 选中计数 | 统计 focused pane 的 `selection.selected_paths` 中属于该 side 的数量；**仅当 count > 0** 时显示 `N selected` |
| 不显示 | status bar 不增加固定的 MB/s 字段，因为速率由 drawer 显示 |

布局建议按照从左至右的顺序：

```text
[● Connected · host] [— status_message?] [N selected?]     [2 active · 1 failed]
```

窄窗口中，左侧和中间内容使用 `truncate`，右侧摘要使用 `flex_none`。

---

## 6. 架构边界

```text
runtime (已有)          app view
TransferProgress  ──►  handle_app_event
                       ├─ update TransferJob.state
                       └─ RateSampler.push

render_transfer_job ──► query RateSampler ──► detail string
drawer header       ──► aggregate rates

RemoteOperationFailed ──► tab.remote.error ──► empty_state + Retry
Retry click ──► ReadRemoteDir (existing)

Cancel connect ──► DisconnectTab (+ optional RejectHostKey)
```

**禁止：**

- 因决策 1 而禁止向 `core` 增加 rate 字段。
- GPUI 线程不得 await 网络操作。
- 不得使用虚假进度动画表示 stalled 传输。

---

## 7. 测试计划

### 纯函数（`rate_sampler`）

- 窗口内存在两个点时，`speed_bps` 计算正确。
- `elapsed < warmup` 时，speed 为 `None`。
- bytes 长时间不变时，`stalled == true`。
- 根据 total、done 和 speed 正确计算 ETA；speed 为 0 时 ETA 为 None。
- 终态清理后，map 不包含对应 id。

### App / Workspace

- 收到 `TransferProgress` 序列后，detail **不包含**长期占位内容 `— MB/s · ETA —`；预热情况除外，可以测试 stalled 和正常路径。
- `RemoteOperationFailed` 发生后，pane 显示 Retry；dispatch 的 `ReadRemoteDir` path 正确。
- `RemoteDirLoaded` 发生后清除 error。
- Connecting empty_state 包含 Cancel；执行 Cancel 后，`DisconnectTab` 已进入队列，而且 connection 不再是 Connecting。
- status bar 在 selection > 0 时显示“N selected”；点击 transfer 区域时切换 `drawer_open`。

### 手动测试

- 使用大文件上传和下载，确认 MB/s 与 ETA 逐渐稳定。
- 对后端限速或暂停后，确认界面显示 Stalled。
- 确认首次加载显示 spinner，而且刷新时保留列表。
- 在连接期间执行 Cancel，然后重新连接。
- drawer 关闭时，确认 status 仍提示传输，而且可以通过提示展开 drawer。

---

## 8. 修改文件清单

| 文件 | 修改 |
| --- | --- |
| `crates/app/src/workspace/rate_sampler.rs`（新）或 `transfers.rs` 内模块 | `RateSampler`、聚合、格式化和单元测试 |
| `crates/app/src/workspace/event_handling.rs` | 根据 progress 和终态维护 sampler |
| `crates/app/src/workspace/render.rs` | transfer detail、drawer 聚合、首次加载 spinner、连接 Cancel、status 选中计数和失败颜色 |
| `crates/app/src/workspace/mod.rs` / `modals.rs` / `panes.rs` | 在需要时增加 Cancel connect 辅助逻辑 |
| `crates/app/src/workspace/tests.rs` | 上述 UI 行为测试 |
| `crates/ui/src/*` | 可选的 spinner 组件 |
| **不修改** | `crates/core` 进度类型、`session_actor` progress payload 和阶段 1 Fs 协议 |

---

## 9. 建议的 PR 划分（实现阶段）

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | RateSampler、row 真实速度/ETA、drawer 聚合和单元测试 | 无 |
| **PR2** | 首次加载 spinner 和 Connecting Cancel | 无，因此可以与 PR1 并行 |
| **PR3** | 就地 Retry、status 选中计数和 failed 颜色强调 | 无 |

每个 PR 均可独立验收，合并顺序可以任意选择。建议按照 1、2、3 的顺序合并，因为该顺序便于评审。

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| Host-key 状态下 Cancel 是否显式执行 `RejectHostKey` | 实现时在“disconnect 已执行 reject_all_for_tab”和同时发送两个命令之间选择；**集成测试必须确认 modal 消失且不存在未处理的 trust request** |
| 多窗口 sampler | 如果共享 `SharedTransfers`，那么共享 map；如果各窗口使用独立 store，那么分别计算。具体方式应与现有多窗口架构一致 |
| 本地 pane error Retry | 如果本地读取失败仅显示 status 文案，那么本阶段可以不增加 Retry，因此优先实现远程 Retry |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 速度/ETA 的计算位置 | A — 纯 view `RateSampler` |
| 2 | 首次加载 UI | A — 居中 spinner |
| 3 | Retry 语义 | A — 仅重新执行当前 ReadDir |
| 4 | 连接可取消 | A — 复用 `DisconnectTab` |
| 5 | status 传输提示交互 | A — 可以通过点击展开 drawer |
| 6 | 交付形式 | 方案 1 — 使用一份设计完整定义 2a–2d |

---

## Key Decisions

1. **速率是 UI 派生状态，不是协议字段。** 因此，该设计保持 core 的职责边界，并符合 AGENTS 对分层和进度节流职责的规定。
2. **明确显示 Stalled。** 因此，该设计符合“不得用动画掩盖停滞”的规范。
3. **Retry 表示刷新当前目录。** 该行为可以处理最常见的失败情况，同时避免再次执行可能已经失效的 Fs 操作。
4. **Cancel connect 表示断开会话。** 因此，不需要增加新命令，并且可以减少实现和测试范围。
5. **Fs 错误不会使列表变为 empty_state。** 该行为与阶段 1 一致，而且阶段 2 仅完整处理**目录读取**错误。
