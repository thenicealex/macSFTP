# Transfer Drawer UI/UX 设计（可调整高度与布局优化）

**日期：** 2026-07-14 GMT+8

**来源：** 用户要求优化 transfer 面板 UI/UX，并允许用户通过拖拽调整垂直尺寸；相关规范参见 `docs/ui-ux-guidelines.md` §2、§4、§7、§12 和 §13。

**方法：** brainstorming；相关决策已经由用户确认，详见末尾的“决策记录”。

**工作区：** `.worktrees/transfer-drawer-ux`，分支 `transfer-drawer-ux`。

**前提：** 当前已经存在底部 drawer（`drawer_open` 与固定 `max_h(240)`）、status bar 切换功能、`TransferRow`、任务分组，以及聚合速度和 ETA。

---

## 决策摘要（已确认）

1. **范围：** 增加高度调整能力并优化现有布局，但是不修改信息架构，因此仍然使用 Active、Queued、Completed 和 Failed 四个分组。
2. **记忆：** 高度和开合状态仅在当前窗口会话内有效，并由 `Workspace` 字段保存，因此不写入 `AppConfig`。
3. **拖拽：** 用户通过顶部手柄连续调整高度，并且最终高度限制在 min–max 区间内；但是高度达到最小值时不会自动关闭。
4. **双击：** 用户双击手柄时，高度恢复为默认值。
5. **关闭：** 用户仍然通过 ⌘J 或 status bar 中的 `ShowTransferDrawer` 控制开合，因此本轮不在 header 中增加关闭按钮。
6. **架构：** 采用方案 1，即状态和逻辑均保留在 `Workspace` 中；当前只有一个使用位置，因此不抽象通用 `VerticalResizeHandle`。
7. **P0 优化：** 实现 sticky header、列表独立滚动、手柄可见状态、hover 状态、resize cursor、清晰空态，以及合理的 min/max 限制。
8. **本轮范围以外：** collapse 按钮、行虚拟化、clear completed 和进度动画。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| 垂直 resize | 用户拖动顶部手柄时，drawer 高度随之变化；用户结束操作后，高度保持不变，直至下一次拖动或双击重置 |
| 高度限制 | 在任意窗口尺寸下，高度均处于 min–max 区间内，因此 resize 不会使 pane 失去可用空间，也不会覆盖 status bar |
| 双击重置 | 用户双击手柄后，高度恢复为当前默认视觉对应的 240px |
| 会话记忆 | 用户在同一窗口内关闭并再次显示 drawer 后，界面仍使用上次的高度；但是应用重新启动或创建新窗口时使用默认高度 |
| Sticky header | 列表滚动时，“Transfers + 汇总”区域和手柄保持可见 |
| 开合语义 | ⌘J 和 status bar 仍然切换 `drawer_open`；同时，创建传输任务后自动显示 drawer 的现有行为保持不变 |
| 无障碍要求 | 手柄具有明确的视觉提示；icon-only 控件仍有 tooltip；键盘开合功能保持可用 |

### 非目标

- 不跨会话持久化高度或 open 状态。
- 不支持最小高度吸附关闭，也不增加半展开状态。
- 不提供 25%、50% 或最大化等多档预设。
- 不创建通用 splitter 组件，也不修改左右 pane 的 resize 方式。
- 不调整 transfer row 的信息架构；同时，跨应用启动的 History 不属于当前产品能力。
- 本轮不实现列表虚拟化，因为当前任务数量仍然可控；该事项属于 P2。

---

## 2. 现状与问题

| 能力 | 现状 |
| --- | --- |
| 开合 | `Workspace.drawer_open: bool` 默认值为 `true`；`ShowTransferDrawer` 与 status bar 点击均会切换该值 |
| 高度 | `render_transfer_drawer` 使用固定 `max_h(px(240.0))`，因此用户无法调整高度 |
| 滚动 | 整个 drawer（包括 header）使用 `overflow_y_scroll`，因此 header 会随列表离开可视区域 |
| 分组 | Active 和 Queued 默认展开；Completed 和 Failed 可以折叠 |
| 汇总 | Header 显示 `N active · N queued · N done · N failed`，以及聚合速度和 ETA |
| 自动显示 | 上传、下载或重试等操作会将 `drawer_open` 设为 `true` |
| 持久化 | 高度和 open 均未在 `AppConfig` 中定义字段 |
| 拖拽先例 | 文件行已经通过 `on_drag` 实现上传和下载，但是当前没有面板 resize 的实现先例 |

当任务数量较多时，固定的 240px 高度不足；但是当任务数量较少时，该高度又会占用过多空间。与此同时，header 会随列表滚动而离开可视区域。因此，用户需要能够设置适合当前工作的高度，并且在当前窗口会话中维持该高度。

---

## 3. 状态模型

以下状态全部属于每个窗口各自的 `Workspace`。这些字段表示 view/session 状态，因此不属于 `TransferStore` 的业务状态：

```text
drawer_open: bool                          // 已有
drawer_height: Pixels                      // 新增；默认 DEFAULT_DRAWER_HEIGHT
drawer_resize: Option<DrawerResizeDrag>    // 新增；仅拖拽期间为 Some

DrawerResizeDrag {
  start_y: f32,           // 指针按下时的窗口坐标 Y
  start_height: Pixels,   // 指针按下时的 drawer 高度
}
```

### 常量

实现时应在一个位置集中定义以下常量，因此测试可以使用同一组定义：

| 常量 | 建议值 | 说明 |
| --- | --- | --- |
| `DEFAULT_DRAWER_HEIGHT` | `240px` | 与当前 `max_h` 一致，并且作为双击重置的目标值 |
| `MIN_DRAWER_HEIGHT` | `≈ 36–40px` | 能够容纳手柄和一行 header；列表区域可以为空或非常短 |
| `MAX_DRAWER_HEIGHT_RATIO` | `0.5` | 相对于主内容区高度计算；主内容区位于 tab bar 下方和 status bar 上方 |
| `MAX_DRAWER_HEIGHT_ABS` | `480px` | 绝对上限；最终上限取该值与比例值中的较小值 |
| `RESIZE_HANDLE_HEIGHT` | `4–6px` | 手柄命中区高度；命中区可以略大于视觉线 |

有效高度按照以下公式计算：

```text
effective_max = min(MAX_DRAWER_HEIGHT_ABS, content_area_height * MAX_DRAWER_HEIGHT_RATIO)
drawer_height = clamp(drawer_height, MIN_DRAWER_HEIGHT, effective_max)
```

- render 或 resize move 阶段应根据当前窗口或布局高度重新计算 clamp，因此窗口缩小时不能保留超出限制的高度。
- 如果实现阶段无法获得精确的 content 高度，那么可以根据 `window.bounds()` 扣除 tab/status 的估算值；但是测试仍需覆盖 clamp 函数本身。

这些字段不应写入 `AppConfig`、`TransferStore`、`SharedTransfers` 或磁盘。

---

## 4. 布局结构

Settings surface 以外的 Workspace 主列维持以下结构：

```text
tab_bar
main_area (local | remote panes)   flex_1 min_h_0
[transfer_drawer]                  when drawer_open
status_bar
```

Drawer 内部调整为固定高度的 flex 列。根元素不再同时使用 `max_h` 和整体滚动：

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

- Drawer 根元素使用 `h(drawer_height)`、`flex_none` 和 `min_h_0`，并移除根级 `overflow_y_scroll`。
- 分组、row 渲染以及 cancel/retry 继续使用现有 `render_transfer_job` 和 section toggle。
- Status bar 仍然位于 drawer 下方，因此它不计入 drawer 高度。

---

## 5. 交互细节

### 5.1 拖拽调整高度

1. 当指针位于 resize handle 上时，界面使用 `cursor_row_resize`；同时，hover 状态通过 `border` 或 `accent` 对手柄线进行轻量高亮。
2. 主按钮触发 `MouseDown` 时，记录 `DrawerResizeDrag { start_y, start_height }`。
3. `MouseMove` 通过捕获机制或拖拽状态下的全局监听获得指针位置，具体方式以当前 GPUI API 的可行实现为准。新高度采用以下公式：

   `new_height = start_height + (start_y - current_y)`  
   因为窗口坐标 Y 向下递增，所以上移指针会增加高度，而下移指针会减小高度。
4. 每一帧都应先通过 `clamp` 计算合法高度，再写入 `drawer_height`，随后调用 `cx.notify()`。
5. `MouseUp` 或窗口失去焦点时，将 `drawer_resize` 设为 `None`。
6. 拖拽期间不得切换 open 状态；即使高度达到 min，也不得将 `drawer_open` 设为 `false`。

### 5.2 双击重置

- 手柄触发 `DoubleClick` 或等价事件后，将 `drawer_height` 设为 `DEFAULT_DRAWER_HEIGHT`，随后再次执行 clamp，并清除 drag 状态。
- Tooltip 建议使用 `"Drag to resize · Double-click to reset"`；如果长度影响布局，那么可以拆分为两个短句。

### 5.3 开合

- `ShowTransferDrawer` 和 status bar 只切换 `drawer_open`，因此它们不得修改 `drawer_height`。
- 用户关闭并再次显示 drawer 后，界面继续使用关闭前的高度。
- 上传等操作自动将 `drawer_open` 设为 `true` 时，不应强制恢复默认高度。

### 5.4 多窗口

- 每个窗口拥有独立的 `drawer_height`、`drawer_open` 和 drag 状态。
- 传输数据仍然由 `SharedTransfers` 共享，但是 UI 高度仅属于当前窗口。

### 5.5 键盘

- 当前没有明确需求，因此本轮不增加“增高/减高”快捷键。
- 开合操作仍然使用 `ShowTransferDrawer`（⌘J）和 command palette。

---

## 6. UI 优化清单

### P0（本轮必须完成）

| 项目 | 说明 |
| --- | --- |
| 可见 resize 手柄 | 顶部显示分隔线，并提供 hover 反馈和 row-resize cursor |
| Sticky header | 手柄与标题栏保持固定，因此仅 body 随滚动位置变化 |
| 独立滚动区 | body 使用 `flex_1`、`overflow_y_scroll` 和 `min_h_0` |
| 空态 | 空态继续使用居中短文案，并根据 body 高度居中，因此较大空白区域仍有明确状态信息 |
| 高度 clamp | 同时应用 min、比例 max 和绝对 max；窗口变矮时，高度自动调整到合法范围 |

### P1（本轮不实现，可以后续考虑）

- Header 中的显式 collapse 或 chevron 关闭按钮。
- Clear completed 操作入口。

### P2（不实现）

- 行列表虚拟化。
- 进度条动画。
- 使用 Toast 替代 drawer 错误信息。

---

## 7. 架构边界与文件范围

| 层 | 是否修改 | 说明 |
| --- | --- | --- |
| `core` | 否 | 传输状态机没有变化 |
| `sftp` | 否 | 无修改 |
| `storage` | 否 | 高度不持久化 |
| `ui` | 可选且极小 | 仅在 theme size token 需要 drawer 常量时修改，因此不要求创建新组件 |
| `app` | 是 | 修改 `Workspace` 字段、`render_transfer_drawer`、resize 事件和测试 |

预期修改的主要文件如下：

- `crates/app/src/workspace/mod.rs`：字段和默认值。
- `crates/app/src/workspace/render.rs`：布局、手柄和滚动区域拆分。
- `crates/app/src/workspace/tests.rs` 或相邻测试模块：高度 clamp、toggle 后保持高度，以及双击重置。
- 如果 GPUI 鼠标捕获需要辅助函数，那么该函数可以作为 `workspace` 内部私有函数，不应因此提升到 `ui` crate。

实现还必须遵守以下限制：

- GPUI 主线程不能执行网络或磁盘密集操作；但是本功能本身没有相关流程。
- 可恢复错误不能通过 `unwrap` 处理。
- fallible 结果不能被静默忽略。

---

## 8. 测试与验证

### 自动化测试（优先）

| 用例 | 断言 |
| --- | --- |
| 默认高度 | 新 Workspace 的 `drawer_height == DEFAULT` |
| clamp 下界 | 高度小于 min 时，最终生效高度不小于 min |
| clamp 上界 | 高度大于 max 时，最终生效高度不大于 max |
| toggle 保持高度 | 修改高度后依次关闭和显示 drawer，高度保持不变 |
| 双击重置 | 修改高度后执行 reset，高度恢复为经过 clamp 的 DEFAULT |
| 现有 toggle | `ShowTransferDrawer` 仍然切换 `drawer_open` |

如果 GPUI 测试无法稳定模拟拖拽像素轨迹，那么至少测试纯函数 `clamp_drawer_height(height, content_h) -> Pixels` 和 reset 逻辑。

### 手动与视觉验证

- 向上和向下调整高度时，界面更新流畅，并且 cursor 正确。
- 列表较长时，仅 body 滚动，而 header 保持可见。
- 在窄窗口、短窗口和 Retina 屏幕中，min/max 仍然合理，并且 pane 保留可用空间。
- 亮色和暗色主题中的手柄均具有足够对比度。
- 传输期间调整高度时，drawer 不会反复改变开合状态，progress 信息也不会缺失。

### 性能烟测

- 存在多个 active transfer 时，高度调整仍然流畅；实现不得因为传输 chunk 而增加 `notify`，resize 仅在指针移动时调用 `notify`。

---

## 9. 风险与待确定的实现细节

| 风险 | 处理方式 |
| --- | --- |
| GPUI 全局 mouse capture API 与项目使用的版本存在差异 | 先查看现有 `on_mouse_*` 和 window 用法；如果仅监听手柄上的 move 无法满足需求，那么使用 window 级 listener，并在实现计划中注明所选 API |
| content 高度测量误差使 max 偏大或偏小 | 对 clamp 函数进行测试，并在短窗口中进行手动验证；必要时根据窗口 bounds 采用保守估算 |
| 拖拽与文件 DnD 发生冲突 | 手柄使用独立 element，并且不绑定文件 `on_drag`；手柄命中区不得与 row 重叠 |
| 双击与短距离拖拽的事件判定发生冲突 | 仅由 `DoubleClick` 触发重置，并且依赖平台双击判定区分短距离 drag |

brainstorming 阶段已经确认全部产品决策，因此目前没有未决的产品事项。如果实现阶段发现某个 GPUI API 不可用，那么应在实现计划中记录替代方案，但是产品语义保持不变。

---

## 10. 验收标准

1. 用户可以通过顶部手柄调整 transfer drawer 高度，并且结束操作后高度保持不变。
2. 用户双击手柄后，高度恢复为默认值。
3. 高度始终位于 min 与 max 之间；因此窗口变矮时，高度会自动调整到合法范围。
4. 用户切换 drawer 开合状态后，当前窗口会话中的高度保持不变。
5. 列表滚动时，Header 和汇总信息保持可见。
6. 传输业务语义保持不变，包括 cancel、retry 和 conflict；同时，不增加跨应用启动的 History。
7. 相关单元测试通过，并且交付说明包含截图或明确的视觉验证步骤。

---

## 决策记录

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 范围重点 | A：增加高度调整能力并优化现有布局 |
| 2 | 高度与开合状态记忆 | A：仅在当前窗口会话内有效 |
| 3 | 拖拽语义 | C：连续调整高度并支持双击重置；不支持吸附关闭 |
| 4 | 实现方案 | 方案 1：在 Workspace 中定义高度状态和轻量手柄 |
| 5 | §1 状态与布局 | 已确认 |
| 6 | §2 交互 | 已确认 |
| 7 | §3 优化清单 | 已确认，仅包含 P0 |
| 8 | §4 测试与整体方案 | 已确认，并写入本设计文档 |

---

## 后续工作

用户审阅本设计文档后，使用 **writing-plans** 编写实现计划 `docs/plans/2026-07-14-transfer-drawer-ux-impl.md`，随后在 worktree 中依据计划实现。
