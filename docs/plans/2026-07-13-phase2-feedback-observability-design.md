# 阶段 2 设计 — 反馈与可观测性

**Date:** 2026-07-13 GMT+8  
**来源：** `docs/ux-improvement-plan.md` 阶段 2；`docs/ui-ux-guidelines.md` §2.3 / §4 / §6.3 / §7 / §10。  
**方法：** brainstorming 流程；5 个决策点 + 交付形态已与用户确认（见末尾 §决策记录）。  
**前置：** 阶段 1 核心文件操作已落地（`FsCommand`、删除确认、上下文菜单等）。

---

## 决策摘要（已确认）

1. **速度/ETA 落点：** 纯 view 侧 `RateSampler`（滑动窗口），不改 `core`/`sftp` 进度协议。
2. **首次加载形态：** 居中 spinner + 短文案「Loading…」；不做 skeleton。
3. **Retry 语义：** 只重试当前 path 的目录读取（`ReadRemoteDir` / 本地 `load_local_directory`）；不重放 Fs 增删改。
4. **连接取消：** 复用 `AppCommand::DisconnectTab`（+ 本地状态清理）；不新造 `CancelConnect`。
5. **Status bar 传输提示：** 摘要可点击展开/收起 transfer drawer（现有 toggle 行为保留并补齐选中计数等）。
6. **交付形态：** 一份设计覆盖 2a–2d；实现可按 PR 切分，规格一次写清。

---

## 1. 目标与非目标

### 目标

让所有「进行中 / 加载中 / 出错」状态在原位透明表达，消灭占位符与静默：

| 子项 | 成功标准 |
| --- | --- |
| **2a** 真实速度 + ETA | transfer row 不再出现字面量 `— MB/s · ETA —`；有平滑速度；停滞显示 **Stalled**；drawer 顶部有聚合总速度/总 ETA |
| **2b** 加载与连接 | 首载 spinner；重载保留列表 + Refreshing…；Connecting 显示目标 host + **Cancel** |
| **2c** 就地错误 | 读目录失败时 pane 内 title + message + **Retry**；status bar 只留摘要 |
| **2d** status bar | drawer 收起时仍有 active/failed 提示且可点开；显示 focused pane 选中计数（>0） |

### 非目标

- 不修改 `TransferProgress` / `TransferState::Running` 的 wire 形状以携带 rate。
- 不做 skeleton rows。
- 不重放失败的 `FsCommand`。
- 不做 command palette、导航历史、MRU tab（阶段 3/4）。
- 不做 status bar popover 详情列表。
- 不在 status bar 强制显示 MB/s（窄窗优先；速率主战场在 drawer）。

---

## 2. 传输速度 + ETA（2a）

### 2.1 数据模型（view-only）

挂在与 `SharedTransfers` 同生命周期的旁路 map（推荐：`SharedTransfers` 内字段或 `Workspace` 持有并在 event drain 时更新——**优先放在共享传输层旁**，多窗口各自订阅 progress 时各自更新，或共享同一 `SharedTransfers` 时共享同一 sampler map）。

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

**不**写入 `TransferJob` / 不序列化 / 不进 history JSON。

### 2.2 更新时机

| 事件 | 行为 |
| --- | --- |
| `TransferProgress` | `sampler.push(now, bytes_done)`，丢弃窗口外旧样本 |
| `TransferRunning` / 首次进入 `Running` | 初始化 sampler，记录 `started_at` 侧的第一个点（若 progress 已带 bytes） |
| `TransferCompleted` / `Failed` / `Skipped` | `transfer_rates.remove(id)` |
| `Cancelling` | 停止用于 ETA 的展示（可显示 Cancelling…，不再更新 speed） |

进度事件已在 runtime 源头节流（ADR-002）；UI 侧采样基于**已收到的** progress，不再二次节流发送，仅做窗口聚合。

### 2.3 算法（纯函数，可单测）

常量（实现可微调，测试钉死默认值）：

| 常量 | 默认 | 含义 |
| --- | --- | --- |
| `WINDOW_SECS` | 4.0 | 滑动窗口长度 |
| `WARMUP_SECS` | 0.5 | 窗口 elapsed 不足时不报速度 |
| `STALL_SECS` | 3.0 | 无字节增长超过此时长 → Stalled |

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

**展示规则（规范 §7：不得用动画掩盖真实停滞）：**

- 正常：`{done} / {total} · {speed} · ETA {eta}`  
  例：`12.4 MB / 100 MB · 3.2 MB/s · ETA 27s`
- 预热：`{done} / {total} · — MB/s · ETA —`（仅预热短窗，不是永久占位）
- 无 total：`{done} · {speed}`（无 ETA）
- **Stalled：** `{done} / {total} · Stalled`（进度条仍反映真实 `bytes_done/total`，不假前进）

速度格式化：≥1 MB/s 用 `X.Y MB/s`，否则 `X.Y KB/s`；ETA：`<1m` 用秒，否则 `Xm Ys` 或 `Xh Ym`。

### 2.4 Drawer 顶部聚合

在 transfer drawer 顶部（section header 旁或独立一行）显示：

- `N active · {aggregate_speed}`  
- 若可估：`· ETA {aggregate_eta}`

**聚合规则：**

- `aggregate_speed` = 所有 `Running` job 的 `speed_bps` 之和（None 当 0 参与加总）。
- `remaining_bytes` = Σ max(0, total − done)（仅有 `bytes_total` 的 job）。
- `aggregate_eta` = `remaining_bytes / aggregate_speed`（当 aggregate_speed > 0 且 remaining 有定义）。
- 全部 stalled 或无 running：不显示假 ETA。

### 2.5 渲染挂载点

`render_transfer_job` 中 `TransferState::Running` 的 `detail` 字符串改为调用 rate 查询 API，删除硬编码：

```text
"{} / {} · — MB/s · ETA —"
```

---

## 3. 加载与连接中（2b）

### 3.1 远程目录加载三态

| 场景 | 条件 | UI |
| --- | --- | --- |
| 首次加载 | `remote.is_refreshing && remote.entries.is_empty()` | 保留 path bar；列表区居中 **spinner + “Loading…”** |
| 重载 | `is_refreshing && !entries.is_empty()` | 保留旧列表；path bar「Refreshing…」（已有，保持） |
| 就绪 | `!is_refreshing` | 正常列表 / “Empty directory” |

本地 pane 同步 `read_local_directory`，本阶段不引入本地异步 spinner。

**Spinner 实现：** 优先复用/扩展 `empty_state` 或 `crates/ui` 极小 `loading_spinner()`（无第三方依赖）；固定区域高度，避免布局抖动。

### 3.2 连接中 / 等待 host key

远程 pane 在下列状态时 empty_state：

| `ConnectionState` | 标题/文案 | 动作 |
| --- | --- | --- |
| `Connecting` / `Reconnecting` | host（`tab.title`）+ “Connecting…” | **Cancel** |
| `AwaitingHostKey` | host + “Waiting for host key…” | **Cancel**（与 modal Reject 语义一致：拒绝 trust + 断开） |
| 其它失败/断开 | 沿用现有文案与 Reconnect / Edit Connection | 不变 |

### 3.3 Cancel 行为（决策 4）

```text
Cancel connect:
  1. send AppCommand::DisconnectTab { tab_id }
  2. if AwaitingHostKey with live request_id → 同时 RejectHostKey（或依赖 disconnect 清理 registry；实现时二选一并测清楚，避免重复 resolve）
  3. UI: tab.disconnect(UserRequested) 或等价；清 remote path/entries/error
  4. drain_expired_modals；焦点回 pane
```

**约束：**

- Reconnect 时 epoch 已 +1；旧 actor 的迟到事件靠 stale guard 丢弃（已有）。
- Cancel 不得 `unwrap`；channel full 时 status bar 提示（现有 `send_command` 模式）。

---

## 4. 就地错误恢复（2c）

### 4.1 读目录失败

当 `tab.remote.error = Some(UserFacingError)` 且 tab 已 Connected（或路径已请求过）：

- pane 内展示：`title` + `message`（`detail` 默认不展示给用户，避免内部术语；需要时可折叠，MVP 不展示）。
- **Retry** 按钮（决策 3）：  
  `request_remote_directory(tab_id, path)`，`path` 为 `tab.remote.path`（若 None 则 Retry 隐藏或 disabled）。
- 点击后：清 error 或置 `is_refreshing`（与正常 refresh 一致），避免错误文案与 Loading 叠两层。

`RemoteDirLoaded` 成功路径必须 `remote.error = None`（已有逻辑保持）。

### 4.2 Fs 失败

`FsOperationFailed`：**仅 status bar 摘要**（阶段 1 约定）。  
不挡列表、不提供一键重放。用户通过菜单/快捷键再次发起操作。

### 4.3 status bar 与 pane 分工

| 位置 | 内容 |
| --- | --- |
| pane | 目录错误详情 + Retry |
| status bar | 一行截断摘要（含最近 `status_message`）；不替代 pane |

---

## 5. Status bar 增强（2d）

现状已具备：连接色点 + host、可选 `status_message`、右侧 active/failed 可点击 toggle drawer。

本阶段增量：

| 项 | 行为 |
| --- | --- |
| 传输摘要 | drawer **收起时仍显示**；`failed > 0` 时 failed 段用 `theme.colors.error` |
| 点击 | 保持 toggle `drawer_open`（决策 5）；tooltip 保留 ⌘J |
| 选中计数 | focused pane：`selection.selected_paths` 中属于该 side 的数量；**仅当 count > 0** 显示 `N selected` |
| 不展示 | status bar 内的 MB/s 强制字段（速率见 drawer） |

布局建议（左 → 右）：

```text
[● Connected · host] [— status_message?] [N selected?]     [2 active · 1 failed]
```

窄窗：`truncate` 左侧与中间；右侧摘要 `flex_none`。

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

- `core` 增加 rate 字段（决策 1）。
- GPUI 线程 await 网络。
- 用假进度动画填充 stalled 传输。

---

## 7. 测试计划

### 纯函数（`rate_sampler`）

- 窗口内两点 → 正确 `speed_bps`
- elapsed < warmup → speed `None`
- bytes 长时间不变 → `stalled == true`
- total/done/speed → ETA；speed 0 → ETA None
- 终态清理后 map 不含 id

### App / Workspace

- `TransferProgress` 序列后 detail **不含**永久占位 `— MB/s · ETA —`（预热除外可测 stalled/正常路径）
- `RemoteOperationFailed` → pane 可 Retry；dispatch `ReadRemoteDir` path 匹配
- `RemoteDirLoaded` → error 清除
- Connecting empty_state 含 Cancel；Cancel 后 `DisconnectTab` 入队且 connection 非 Connecting
- status bar：selection > 0 时出现 “N selected”；transfer 区 click 切换 `drawer_open`

### 手测

- 大文件上传/下载观察 MB/s 与 ETA 收敛
- 限速或暂停后端 → Stalled
- 首载 spinner；刷新保留列表
- 连接中 Cancel 再连
- drawer 收起时 status 仍提示传输并可点开

---

## 8. 改动文件清单

| 文件 | 改动 |
| --- | --- |
| `crates/app/src/workspace/rate_sampler.rs`（新）或 `transfers.rs` 内模块 | `RateSampler`、聚合、格式化、单测 |
| `crates/app/src/workspace/event_handling.rs` | progress/终态维护 sampler |
| `crates/app/src/workspace/render.rs` | transfer detail、drawer 聚合、首载 spinner、连接 Cancel、status 选中计数/失败色 |
| `crates/app/src/workspace/mod.rs` / `modals.rs` / `panes.rs` | Cancel connect 辅助（若需） |
| `crates/app/src/workspace/tests.rs` | 上述 UI 行为测试 |
| `crates/ui/src/*` | 可选 spinner 组件 |
| **不改** | `crates/core` 进度类型、`session_actor` progress payload、阶段 1 Fs 协议 |

---

## 9. 建议 PR 切分（实现期）

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | RateSampler + row 真实速度/ETA + drawer 聚合 + 单测 | 无 |
| **PR2** | 首载 spinner + Connecting Cancel | 无（可与 PR1 并行） |
| **PR3** | 就地 Retry + status 选中计数 + failed 色强调 | 无 |

每 PR 独立可验；合并顺序任意，建议 1 → 2 → 3 便于 review 叙事。

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| Host-key 时 Cancel 是否显式 `RejectHostKey` | 实现时选「disconnect 已 reject_all_for_tab」或双发；**以集成测试：modal 消失且无悬挂 trust** 为准 |
| 多窗口 sampler | 共享 `SharedTransfers` 则共享 map；各窗口独立 store 则各算——与现多窗口架构一致即可 |
| 本地 pane error Retry | 若本地读失败仅 status 文案，本阶段可不加 Retry；远程优先 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 速度/ETA 计算落点 | A — 纯 view `RateSampler` |
| 2 | 首次加载 UI | A — 居中 spinner |
| 3 | Retry 语义 | A — 仅重试当前 ReadDir |
| 4 | 连接可取消 | A — `DisconnectTab` 复用 |
| 5 | status 传输提示交互 | A — 可点击展开 drawer |
| 6 | 交付形态 | 方案 1 — 2a–2d 一份设计做透 |

---

## Key Decisions

1. **速率是 UI 派生态，不是协议字段** — 保持 core 纯净，符合 AGENTS 分层与进度节流职责划分。  
2. **Stalled 明文展示** — 满足规范「不得用动画掩盖停滞」。  
3. **Retry = 刷新当前目录** — 覆盖最大失败面，避免陈旧 Fs 重放。  
4. **Cancel connect = 断会话** — 不引入新命令，降低实现与测试面。  
5. **Fs 错误不升格为挡列表 empty_state** — 与阶段 1 一致，阶段 2 只把**读目录**错误做透。
