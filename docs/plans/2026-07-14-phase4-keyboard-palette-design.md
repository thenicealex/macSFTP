# 阶段 4 设计 — 键盘与命令面板

**Date:** 2026-07-14 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 4；`docs/ui-ux-guidelines.md` §2.2 / §5 / §6.2 / §9 / §12。
**方法：** brainstorming；用户已经确认相关决策，详情参见末尾的“决策记录”。
**前置条件：** 阶段 1–3 已经形成稳定的 action 集，包括连接、传输、Fs、过滤、导航和隐藏文件等操作。

---

## 决策摘要（已确认）

1. **Command palette 注册：** 使用显式命令注册表，因此不通过反射注册全部 `actions!`。
2. **列表多选：** 使用锚点、`shift+↑/↓` 范围扩展和 `cmd-a` 全选可见项。
3. **Tab 切换：** 使用 MRU 顺序和按住 `ctrl-tab` 时显示的切换器 UI。
4. **可发现性：** palette 行显示快捷键，而且 icon tooltip 应包含常用键位。
5. **Page 步进：** 使用约 10 行，即 `PAGE_SIZE = 10`。
6. **`cmd-shift-[/]`：** 继续按照**创建顺序**切换；MRU **仅**用于 `ctrl-tab` 切换器。
7. **交付形式：** 一份设计包含全部子项；实现阶段可以拆分 PR。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| Command palette | `cmd-shift-p` 可以打开面板；支持根据 title/keywords 模糊查询；Enter 执行命令；Esc 关闭面板；每行可以显示绑定键 |
| 列表键盘 | 支持 page up/down、home/end、shift 范围选择和 cmd-a；selection 继续以 path 为标识 |
| MRU tab | 激活 tab 时更新 MRU；ctrl-tab 切换器按照 MRU 排序；释放按键时确认选择 |
| 可发现性 | 注册表中的动作在 palette 显示快捷键；关键 tooltip 包含键位文案 |

### 非目标

- 不支持用户自定义快捷键，也不在设置页提供按键修改功能。
- 不通过 `actions!` 宏自动反射全部 action，因为这会显示不适合用户直接使用的项目。
- 不修改 SFTP/core 协议。
- 不包含阶段 5 的 session 恢复，也不包含阶段 6 的动效和完整 a11y 审计。
- 不要求修改 GPUI 系统菜单以显示 OS 风格键位。如果 GPUI 能力不足，则使用 palette 和 tooltip 提供键位信息。

---

## 2. Command palette

### 2.1 注册表（决策 A）

建议集中定义在 `crates/app/src/palette_commands.rs`，也可以定义在 `workspace/command_palette.rs`。

```rust
pub struct PaletteCommand {
    /// 稳定 id，等于 action 名（如 "NewTab"），供测试与去重。
    pub id: &'static str,
    /// 用户可见标题：动词短语，如 "New Tab"。
    pub title: &'static str,
    /// 额外搜索词（可选），如 "connection", "open"。
    pub keywords: &'static [&'static str],
    /// 展示用快捷键文案，如 "⌘T"；无则 None。
    pub keybinding: Option<&'static str>,
    /// 何时显示/可执行（粗粒度）。
    pub when: PaletteWhen,
}

pub enum PaletteWhen {
    Always,
    HasTabs,
    FilePane,           // 有 active tab
    ConnectedRemote,    // active tab connected
    // 可按需扩展；避免细到 runtime 内部
}
```

title **不得**包含 raw `Action` 类型名、runtime 或 channel 等内部信息。

**MVP 必须支持查询以下动作：** NewTab、CloseTab、Reconnect、Refresh、Focus Local/Remote、Upload/Download、Show Transfers、Open Settings、About、Delete/Rename/New Folder、Filter、Go to Path、Navigate Back/Forward、Toggle Hidden、Cancel Modal（如果适用）、Copy Path、Open Log Folder，以及阶段 1–3 的其他用户动作。内部专用或不适合用户直接执行的动作不注册。

### 2.2 UI 与交互

- 使用 `OpenCommandPalette` 和 **`cmd-shift-p`** 打开面板，并将绑定设置在 Workspace。
- UI 使用居中的 elevated 浮层，形式与 About/Go to Path 相近，而且包含遮罩、输入框和结果列表。
- 输入复用 `InputState`。结果可以使用 `uniform_list` 或普通短列表，因为命令数量远少于 10k；只有数量超过 50 时才需要虚拟化。
- 过滤不区分大小写，并匹配 `title` 和 `keywords` 的子串。可以增加简单 fuzzy，例如首字母序列；但是 MVP 使用子串匹配即可，并与阶段 3 filter 保持一致。
- 使用 `↑/↓` 或 `ctrl-n/p` 选择结果。Enter 执行高亮项并关闭面板；Esc 或选择遮罩时关闭面板。
- 使用 `cx.dispatch_action` 或 workspace 方法执行命令。命令执行后关闭 palette，并调用 `notify`。
- `when` 不满足时，可以将项目显示为禁用状态，也可以隐藏。推荐隐藏，因为这样可以减少无关信息。

### 2.3 与 modal 栈的关系

存在 host-key、conflict、delete 或 go-to-path modal 时，palette 可以继续显示，也可以拒绝显示。推荐允许显示，并将 palette 视为最上层 modal，因此 `CancelActiveModal` 应优先关闭 palette。

`cancel_active_modal` 的顺序为：palette → 其他 modal → …。

### 2.4 状态

```rust
// Workspace
palette_open: bool,
palette_input: InputState,
palette_selected: usize, // index into filtered results
```

---

## 3. 文件列表键盘能力

### 3.1 动作与按键

| Action | 默认键 | 行为 |
| --- | --- | --- |
| `SelectNextEntry` / `SelectPrevEntry` | ↓ / ↑ | 单选下一项或上一项；将新 index 设为锚点 |
| `SelectNextEntryExtend` / `SelectPrevEntryExtend` | shift-↓ / shift-↑ | 扩展选择范围 |
| `PageDown` / `PageUp` | page down / page up | 在可见列表中移动 `PAGE_SIZE = 10` |
| `SelectFirstEntry` / `SelectLastEntry` | home / end | 选择第一个或最后一个**可见**项目 |
| `SelectAllEntries` | cmd-a | 选择**当前 side 的全部可见**项目 |

所有动作均使用 `FilePane` context。MVP 不提供 Page/Home 的 extend 变体，也不提供 `shift-page`，因为当前需求不需要这些能力。如果实现成本较低，则可以增加使用相同锚点扩展规则的 `shift-page`。

### 3.2 选择模型（决策 A）

```rust
// Workspace 或随 tab 的 view 态
selection_anchor: Option<(PaneSide, EntryPath)>, // 或 visible index + path
```

- **单击或普通 ↑↓：** 设置 `selected_paths = [path]`，并设置 `anchor = path`。
- **shift+↑↓：** 如果没有 anchor，则先将当前项目设为 anchor；随后选择 anchor 与当前 visible index 之间**闭区间**内的全部 path，并使用可见列表顺序。
- **cmd-a：** 选择所有 visible entries 的 path；anchor 可以保持不变，也可以设为首项。
- **打开目录、导航或过滤条件变化：** 导航时继续使用现有 clear on navigate 行为，因此清空 selection。过滤条件变化时，如果已选项目被过滤，则保留 path，但是不显示高亮，这与阶段 3 一致。

Selection 继续**仅保存 path**，不保存 row entity。

### 3.3 Page / Home / End

- 索引范围使用经过 hidden 和 filter 处理后的 **visible indices**。
- `PAGE_SIZE = 10`，并使用 `new_index = (i ± PAGE_SIZE).clamp(0, n-1)` 计算新索引。
- 索引变化后调用现有 `UniformListScrollHandle` 的 `scroll_to_item`。

### 3.4 与 type-to-filter 的关系

当 filter 条具有 `explicit_focus` 时，字符输入由 filter 处理。但是，↑↓ 和 page 仍然可以改变 selection，因此 filter 不应通过 `stop_propagation` 忽略导航键。这项行为与阶段 3 评审一致。

---

## 4. Tab MRU 与 ctrl-tab 切换器

### 4.1 MRU 数据结构

```rust
// Workspace
tab_mru: Vec<TabId>, // front = most recent
```

- `activate_tab`、`open_new_tab` 或成功 connect 后，将对应 `TabId` 设置为 MRU 的第一项。
- `close_tab` 时，从 MRU 中移除对应项目。
- MRU 按最近使用时间排序，最近项目位于最前方。

### 4.2 `cmd-shift-[` / `]`（决策说明 A）

- 继续使用 `tabs` Vec 的**创建顺序**，因此不使用 MRU。
- 行为继续与现有 `activate_tab_in_direction` 一致。

### 4.3 ctrl-tab 切换器（决策 A）

**状态：**

```rust
tab_switcher_open: bool,
tab_switcher_index: usize, // into mru list
```

**交互：**

| 事件 | 行为 |
| --- | --- |
| `ctrl-tab`（按下） | 未显示切换器时，显示切换器，并将 index 设为次近项目 `MRU[1]`；没有次近项目时使用 0。切换器已经显示时，设置 `index = (index+1) % n` |
| `ctrl-shift-tab` | 按相反方向选择 |
| 释放 `ctrl` 或 ctrl 修饰键 | 激活 `mru[index]` 并关闭切换器 |
| `Esc` | 关闭切换器，而且不切换 tab；也可以恢复原 tab |

**UI：** 使用居中的小型浮层，按 MRU 顺序显示 tab 标题和连接状态点，并高亮当前 index。

**实现注意事项：** GPUI 需要监听 key up 或 modifiers changed。如果无法可靠监听 key-up，则采用降级交互：再次使用 ctrl-tab 时循环选择，Enter 确认，Esc 取消。实现文档必须说明采用降级方式的条件，但是应优先实现 key-up 确认。

**Actions：** 使用 `ToggleTabSwitcher`、`TabSwitcherNext` 等稳定名称。

---

## 5. 快捷键可发现性

### 5.1 Palette

每行使用以下布局：

```text
[ Title                          ⌘T ]
```

`keybinding` 使用注册表中的静态文案，而且应与 `app_actions` 的绑定一致。如果两处不一致，则以 `app_actions` 为准，并在 code review 中检查注册表的手写副本。

### 5.2 Tooltip

检查 icon-only 按钮，并为常用操作增加键位：

| 控件 | 示例 |
| --- | --- |
| Refresh | Refresh (⌘R) |
| Parent | Parent Directory (⌘↑) |
| Back/Forward | 已有 |
| New Folder / Delete | 阶段 1 已有 |
| Transfers | Toggle Transfers (⌘J) |
| Hidden | Show Hidden Files (⌘⇧.) |

### 5.3 系统菜单

如果 GPUI `MenuItem::action` 没有稳定的 checkmark 或键位 API，则不要求修改菜单文案，而是通过 palette 和 tooltip 显示快捷键。View 菜单可以继续显示动作名称。

---

## 6. 架构边界

```text
app_actions (KeyBinding)
       │
       ▼
Workspace on_action ──► 现有方法
       ▲
       │ dispatch
Command palette ──► palette_commands registry

Selection: selected_paths + selection_anchor (view/tab)
MRU: tab_mru Vec on Workspace
```

**禁止：** core 不得包含 UI 选区锚点；palette 不得执行阻塞网络操作。

---

## 7. 测试计划

### Palette

- 验证打开和 Esc 关闭。
- 验证根据 title/keywords 过滤。
- 验证 Enter 执行 NewTab，而且 tab 数量增加 1。
- 验证 `when` 过滤，即没有连接时隐藏 Download 等动作。

### 列表按键

- 验证 page down 在可见列表中移动 10 项。
- 验证 home/end。
- 验证 shift+↓ 选择区间内的 path 数量。
- 验证 cmd-a 的选择数量等于可见项目数。

### MRU

- 依次激活 B 和 A 后，验证 MRU 第一项为 A。
- 关闭 tab 后，验证 MRU 不再包含该 tab。
- 构造 3 个 tab，使创建顺序与 MRU 不同，并验证 cmd-shift-] 仍然使用创建顺序。

### 手动测试

- 仅使用键盘完成以下流程：新建 tab → 聚焦 remote → 连接（可以使用 mock）→ 多选 → 上传或下载 → 使用 palette 查询 Retry/Refresh → 关闭 tab。
- 使用 ctrl-tab 切换多个 tab。

---

## 8. 修改文件清单

| 文件 | 修改 |
| --- | --- |
| `crates/app/src/palette_commands.rs`（新） | 注册表和过滤函数 |
| `crates/app/src/workspace/command_palette.rs` 或 render/modals | palette UI |
| `crates/app/src/app_actions.rs` | 新 actions 和按键绑定 |
| `crates/app/src/workspace/mod.rs` | palette、switcher、anchor 和 mru 状态 |
| `crates/app/src/workspace/panes.rs` | 选择扩展、page/home/end 和 select all |
| `crates/app/src/workspace/render.rs` | tooltip 文案和 tab switcher 浮层 |
| `crates/app/src/workspace/tests.rs` | 上述测试 |
| `crates/ui` | 可选的 palette 通用 list row |
| **不修改** | sftp/core 协议 |

---

## 9. 建议的 PR 划分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | 列表 page/home/end、shift 范围选择和 cmd-a | 无 |
| **PR2** | Command palette、注册表和 cmd-shift-p | 无，因此可以与 PR1 并行 |
| **PR3** | MRU 数据结构和 ctrl-tab 切换器 | 无 |
| **PR4** | 完善 tooltip 键位和 palette 键位列 | PR2 |

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| palette fuzzy | MVP 使用子串匹配；后续可以增加 fuzzy |
| ctrl-tab key-up | 优先使用；如果不可用，则采用 Enter 确认的降级方式 |
| shift-click 鼠标 | 可以选择使用相同锚点；但是该能力不影响键盘 MRU 验收 |
| 传输 drawer 内键盘 | 本阶段不扩展，因为已有 cancel/retry 按钮 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | palette 注册方式 | A — 显式注册表 |
| 2 | 多选模型 | A — 锚点、shift 和 cmd-a |
| 3 | Tab 切换 | A — MRU 和 ctrl-tab 切换器 |
| 4 | 可发现性 | A — palette 键位和 tooltip |
| 5 | page 大小 | A — 10 行 |
| 6 | cmd-shift 与 MRU 的关系 | A — 保留创建顺序；MRU 仅用于 ctrl-tab |
| 7 | 交付形式 | 方案 1 — 使用一份设计完整定义所有子项 |

---

## Key Decisions

1. **使用显式 palette 注册表。** 因此，可以控制用户文案和安全边界，并符合“不暴露内部信息”的规范。
2. **使用锚点进行范围选择。** 该设计满足 §6.2，而且不增加完整 Finder cmd-click 模型的复杂度。
3. **两种 tab 导航分别使用不同顺序。** 创建顺序快捷键具有稳定结果，而 MRU 切换器用于选择最近使用的 tab。
4. **使用 `PAGE_SIZE=10`。** 该值便于测试并满足当前需求，而且不依赖 viewport 测量。
5. **主要通过 palette 提供可发现性。** 因此，在 GPUI 菜单能力有限时仍然可以显示完整快捷键信息。
