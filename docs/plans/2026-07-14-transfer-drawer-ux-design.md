# 设计 — Transfer Drawer UI/UX（可拖拽高度 + 布局打磨）

**Date:** 2026-07-14 GMT+8  
**来源：** 用户请求「优化 transfer 面板 UI/UX，添加可拖拽控制上下大小」；规范见 `docs/ui-ux-guidelines.md` §2 / §4 / §7 / §12 / §13。  
**方法：** brainstorming；决策已与用户确认（见末尾 §决策记录）。  
**工作区：** `.worktrees/transfer-drawer-ux`，分支 `transfer-drawer-ux`。  
**前置：** 现有底部 drawer（`drawer_open` + 固定 `max_h(240)`）、status bar 切换、`TransferRow`、分组与聚合速度/ETA 已就绪。

---

## 决策摘要（已确认）

1. **范围：** 可拖拽调高 + 现有布局打磨；**不**重做信息架构（仍为 Active / Queued / Completed / Failed）。
2. **记忆：** 高度与开合仅**本窗口会话内**（`Workspace` 字段）；不写 `AppConfig`。
3. **拖拽：** 顶部手柄连续调高；夹在 min–max 之间；**拖到很矮不自动关闭**。
4. **双击：** 手柄双击恢复默认高度。
5. **关闭：** 仍用 ⌘J / status bar（`ShowTransferDrawer`）；本轮 header **不**加关闭按钮。
6. **架构：** 方案 1 — 状态与逻辑留在 `Workspace`；不抽通用 `VerticalResizeHandle`（YAGNI）。
7. **打磨 P0：** sticky header、列表独立滚动、手柄可见/hover/cursor、空态略清晰、合理 min/max。
8. **本轮不做：** collapse 按钮、行虚拟化、clear completed、进度动画。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| 垂直 resize | 拖顶部手柄改变 drawer 高度，松手后高度保持到下次拖或双击重置 |
| 高度夹取 | 任意窗口尺寸下高度始终在 min–max 内；resize 不把 pane 压没、不盖住 status bar |
| 双击重置 | 双击手柄回到默认高度（与当前默认视觉相当，240px） |
| 会话记忆 | 同窗口内开关 drawer 后仍恢复上次高度；重启/新窗口用默认高度 |
| Sticky header | 列表滚动时「Transfers + 汇总」与手柄不滚走 |
| 开合不变 | ⌘J / status bar 仍 toggle `drawer_open`；开传输任务时自动打开 drawer 的既有行为保留 |
| 无障碍底线 | 手柄有可见 affordance；icon-only 控件仍有 tooltip；不破坏键盘开合 |

### 非目标

- 跨会话持久化高度或 open 状态。
- 拖到最小吸附关闭 / 半展开中间态。
- 多档预设（25% / 50% / 最大化）。
- 通用 splitter 组件或左右 pane 分栏 resize。
- 重做 transfer row 信息架构；跨启动 History 不属于产品能力。
- 列表虚拟化（本轮列表量仍可控；P2）。

---

## 2. 现状与摩擦

| 能力 | 现状 |
| --- | --- |
| 开合 | `Workspace.drawer_open: bool`，默认 `true`；`ShowTransferDrawer` 与 status bar 点击 toggle |
| 高度 | `render_transfer_drawer` 固定 `max_h(px(240.0))`，**不可调** |
| 滚动 | **整块 drawer**（含 header）`overflow_y_scroll`，header 会滚走 |
| 分组 | Active / Queued 常开；Completed / Failed 可折叠 |
| 汇总 | Header 文案：`N active · N queued · N done · N failed` + 聚合速度/ETA |
| 自动打开 | 发起上传/下载/重试等路径会 `drawer_open = true` |
| 持久化 | 高度与 open **均无** `AppConfig` 字段 |
| 拖拽先例 | 文件行有 `on_drag` 上传/下载；**无**面板 resize 先例 |

**摩擦：** 任务多时 240px 过矮；任务少时占位过大却无法压小；滚动时汇总与标题离开视口；用户无法按偏好固定工作高度。

---

## 3. 状态模型

全部为 **每窗口** `Workspace` 字段（view/session 状态，非 `TransferStore` 业务状态）：

```text
drawer_open: bool                          // 已有
drawer_height: Pixels                      // 新增；默认 DEFAULT_DRAWER_HEIGHT
drawer_resize: Option<DrawerResizeDrag>    // 新增；仅拖拽中 Some

DrawerResizeDrag {
  start_y: f32,           // 按下时指针 Y（窗口坐标）
  start_height: Pixels,   // 按下时 drawer 高度
}
```

### 常量（实现时集中一处，便于测）

| 常量 | 建议值 | 说明 |
| --- | --- | --- |
| `DEFAULT_DRAWER_HEIGHT` | `240px` | 与当前 `max_h` 一致；双击重置目标 |
| `MIN_DRAWER_HEIGHT` | `≈ 36–40px` | 手柄 + header 一行，列表可为空/极短 |
| `MAX_DRAWER_HEIGHT_RATIO` | `0.5` | 相对主内容区（tab bar 下、status bar 上）高度 |
| `MAX_DRAWER_HEIGHT_ABS` | `480px` | 绝对上限，与 ratio 取 min |
| `RESIZE_HANDLE_HEIGHT` | `4–6px` | 命中区；可略大于视觉线 |

**有效高度：**

```text
effective_max = min(MAX_DRAWER_HEIGHT_ABS, content_area_height * MAX_DRAWER_HEIGHT_RATIO)
drawer_height = clamp(drawer_height, MIN_DRAWER_HEIGHT, effective_max)
```

- 在 `render` 或 resize move 时用当前窗口/布局可得高度重算 clamp（窗口缩小后不得保持非法高度）。
- 若实现阶段拿不到精确 content 高度，允许用 `window.bounds()` 减 tab/status 估算常量；测试覆盖 clamp 函数本身。

**不进入：** `AppConfig`、`TransferStore`、`SharedTransfers`、磁盘。

---

## 4. 布局结构

Workspace 主列（Settings surface 外）保持：

```text
tab_bar
main_area (local | remote panes)   flex_1 min_h_0
[transfer_drawer]                  when drawer_open
status_bar
```

Drawer 内部改为固定高度 flex 列（**不再**整块 `max_h` + 整块滚动）：

```text
┌─────────────────────────────────────────┐
│ resize handle (4–6px, cursor row-resize)│  flex_none
├─────────────────────────────────────────┤
│ header: icon + "Transfers" + agg_label  │  flex_none ~28px
├─────────────────────────────────────────┤
│ scroll body                             │  flex_1 min_h_0 overflow_y_scroll
│   Active / Queued / Completed / Failed  │
│   empty: "No transfers"                 │
└─────────────────────────────────────────┘
height = drawer_height (clamped)
```

- Drawer 根：`h(drawer_height)` + `flex_none` + `min_h_0`；**去掉**根级 `overflow_y_scroll`。
- 分组、row 渲染、cancel/retry **复用**现有 `render_transfer_job` / section toggle。
- Status bar 仍在 drawer 下方，不参与 drawer 高度。

---

## 5. 交互细节

### 5.1 拖拽调高

1. 指针在 resize handle 上：`cursor_row_resize`；hover 时手柄线用 `border` / `accent` 轻量高亮。
2. `MouseDown`（主按钮）：记录 `DrawerResizeDrag { start_y, start_height }`。
3. `MouseMove`（捕获或在拖拽态全局监听，以实现可行 API 为准）：  
   `new_height = start_height + (start_y - current_y)`  
   （向上拖增大高度；Y 向下增大时高度减小。）
4. 每帧 `clamp` 后写 `drawer_height`，`cx.notify()`。
5. `MouseUp` / 失焦：`drawer_resize = None`。
6. 拖拽中不 toggle open；高度到 min **不**设 `drawer_open = false`。

### 5.2 双击重置

- 手柄 `DoubleClick`（或等价检测）：`drawer_height = DEFAULT_DRAWER_HEIGHT`（再 clamp），清除 drag 态。
- Tooltip 建议：`"Drag to resize · Double-click to reset"`（或拆成短句，避免过长）。

### 5.3 开合

- `ShowTransferDrawer` / status bar：仅翻转 `drawer_open`；**不**改 `drawer_height`。
- 关闭再打开：高度为关闭前值。
- 自动 `drawer_open = true` 路径（上传等）：不强制改高度。

### 5.4 多窗口

- 每窗口独立 `drawer_height` / `drawer_open` / drag 态。
- 传输数据仍共享 `SharedTransfers`；仅 UI 高度本地。

### 5.5 键盘

- 本轮**不**新增「增高/减高」快捷键（YAGNI）。
- 开合仍走 `ShowTransferDrawer`（⌘J）与 command palette。

---

## 6. UI 打磨清单

### P0（本轮必做）

| 项 | 说明 |
| --- | --- |
| 可见 resize 手柄 | 顶部分隔线 + hover 反馈 + row-resize cursor |
| Sticky header | 手柄 + 标题栏固定；仅 body 滚动 |
| 独立滚动区 | body `flex_1` + `overflow_y_scroll` + `min_h_0` |
| 空态 | 仍居中短文案；随 body 高度居中，避免巨大空白无反馈 |
| 高度 clamp | min / max(ratio, abs)；窗口变矮时自动压高度 |

### P1（本轮明确不做，可后续）

- Header 显式 collapse / chevron 关闭按钮。
- Clear completed 入口。

### P2（不做）

- 行列表虚拟化。
- 进度条动画掩盖停滞。
- Toast 替代 drawer 错误。

---

## 7. 架构边界与文件触点

| 层 | 是否改 | 说明 |
| --- | --- | --- |
| `core` | 否 | 无传输状态机变化 |
| `sftp` | 否 | |
| `storage` | 否 | 不持久化高度 |
| `ui` | 可选极小 | 仅当 theme size token 增加 drawer 常量时；**不**强制新组件 |
| `app` | 是 | `Workspace` 字段、`render_transfer_drawer`、resize 事件、测试 |

**预期主要文件：**

- `crates/app/src/workspace/mod.rs` — 字段与默认值
- `crates/app/src/workspace/render.rs` — 布局、手柄、滚动拆分
- `crates/app/src/workspace/tests.rs`（或邻近测试模块）— 高度 clamp / toggle 保持高度 / 双击重置
- 若 GPUI 鼠标捕获需 helper，可放 `workspace` 内私有 fn，不抬到 `ui` crate

**禁止：**

- 在 GPUI 主线程做网络/磁盘重活（本功能无此路径）。
- 用 `unwrap` 处理可恢复错误。
- 静默丢弃 fallible 结果。

---

## 8. 测试与验证

### 自动化（优先）

| 用例 | 断言 |
| --- | --- |
| 默认高度 | 新 Workspace `drawer_height == DEFAULT` |
| clamp 下界 | 设为小于 min → 生效高度 ≥ min |
| clamp 上界 | 设为大于 max → 生效高度 ≤ max |
| toggle 保持高度 | 改高度 → 关 → 开 → 高度不变 |
| 双击重置 | 改高度后 reset → 回到 DEFAULT（再 clamp） |
| 既有 toggle | `ShowTransferDrawer` 仍翻转 `drawer_open`（回归） |

拖拽像素路径若 GPUI 测试难驱动，至少测 **纯函数** `clamp_drawer_height(height, content_h) -> Pixels` 与 reset 逻辑。

### 手动 / 视觉

- 拖高/拖矮流畅，cursor 正确。
- 列表长时只 body 滚，header 固定。
- 窄窗、短窗、Retina 下 min/max 仍合理，pane 不被压没。
- 亮/暗主题下手柄对比足够。
- 传输中拖拽不导致开关抖动或丢 progress 显示。

### 性能烟测

- 多 active transfer 时拖拽仍跟手；不引入逐 chunk 额外 notify（resize 仅指针移动时 notify）。

---

## 9. 风险与未决实现细节

| 风险 | 缓解 |
| --- | --- |
| GPUI 全局 mouse capture API 与项目版本差异 | 先查现有 `on_mouse_*` / window 用法；手柄上 move 不够时用 window 级 listener；实现计划里写明选用 API |
| content 高度测量不准导致 max 过松/过紧 | clamp 函数可测；手动短窗验证；必要时用窗口 bounds 保守估算 |
| 拖拽与文件 DnD 冲突 | 手柄独立 element，不挂文件 `on_drag`；命中区不与 row 重叠 |
| 双击与单击拖起手冲突 | 仅 DoubleClick 重置；短距离 drag 不算双击（依赖平台双击判定） |

**无产品未决项**（brainstorming 已关闭）。实现时若某 GPUI API 不可用，在 impl plan 中记录替代方案，不回退改产品语义。

---

## 10. 成功标准（验收）

1. 用户可拖顶部手柄改变 transfer drawer 高度，松手后高度保持。
2. 双击手柄恢复默认高度。
3. 高度始终夹在 min 与 max 之间；窗口变矮时自动合法化。
4. 开关 drawer 不丢会话内高度。
5. Header + 汇总在列表滚动时保持可见。
6. 不改变传输业务语义（cancel/retry/conflict；无跨启动 History）。
7. 相关单元测试通过；交付说明含视觉验证方式（截图或步骤）。

---

## 决策记录

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 范围重心 | A：可拖拽调高 + 现有布局打磨 |
| 2 | 高度/开合记忆 | A：本窗口会话内 only |
| 3 | 拖拽语义 | C：连续调高 + 双击重置；不吸附关闭 |
| 4 | 实现路径 | 方案 1：Workspace 内联高度 + 轻量手柄 |
| 5 | §1 状态与布局 | 通过 |
| 6 | §2 交互 | 通过 |
| 7 | §3 打磨清单 | 通过（P0 only） |
| 8 | §4 测试与整体 | 通过，写入本 spec |

---

## 下一步

用户审阅本 spec 后，使用 **writing-plans** 产出实现计划（`docs/plans/2026-07-14-transfer-drawer-ux-impl.md`），再在 worktree 中按计划实现。
