# 阶段 4 设计 — 键盘与命令面板

**Date:** 2026-07-14 GMT+8  
**来源：** `docs/ux-improvement-plan.md` 阶段 4；`docs/ui-ux-guidelines.md` §2.2 / §5 / §6.2 / §9 / §12。  
**方法：** brainstorming；决策点已与用户确认（见末尾 §决策记录）。  
**前置：** 阶段 1–3 已沉淀稳定 action 集（连接、传输、Fs、过滤、导航、隐藏文件等）。

---

## 决策摘要（已确认）

1. **Command palette 注册：** 显式命令注册表（非反射全部 `actions!`）。
2. **列表多选：** 锚点 + `shift+↑/↓` 范围扩展 + `cmd-a` 全选可见项。
3. **Tab 切换：** MRU 序 + `ctrl-tab` 按住切换器 UI。
4. **可发现性：** palette 行显示快捷键 + icon tooltip 补全常见绑定。
5. **Page 步进：** 约 10 行（`PAGE_SIZE = 10`）。
6. **`cmd-shift-[/]`：** 仍按 **创建顺序**；MRU **仅** 用于 `ctrl-tab` 切换器。
7. **交付：** 一份设计覆盖全部子项；实现可 PR 切分。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| Command palette | `cmd-shift-p` 打开；模糊搜 title/keywords；Enter 执行；Esc 关闭；每行可显示绑定键 |
| 列表键盘 | page up/down、home/end、shift 范围选、cmd-a；selection 仍 path-based |
| MRU tab | 激活 tab 时更新 MRU；ctrl-tab 切换器按 MRU；松手确认 |
| 可发现性 | 注册表内动作的快捷键出现在 palette；关键 tooltip 含键位文案 |

### 非目标

- 用户自定义快捷键 / 设置页改键。
- 从 `actions!` 宏自动反射全部 action（会暴露不适合用户的项）。
- 改 SFTP/core 协议。
- 阶段 5 session 恢复、阶段 6 动效与全量 a11y 审计。
- 强制改造 GPUI 系统菜单以显示 OS 风格键位（能力不足则依赖 palette + tooltip）。

---

## 2. Command palette

### 2.1 注册表（决策 A）

集中文件建议：`crates/app/src/palette_commands.rs`（或 `workspace/command_palette.rs`）。

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

**不**把 raw `Action` 类型名、runtime、channel 写进 title。

**覆盖范围（MVP 必须可搜）：**  
NewTab、CloseTab、Reconnect、Refresh、Focus Local/Remote、Upload/Download、Show Transfers、Open Settings、About、Delete/Rename/New Folder、Filter、Go to Path、Navigate Back/Forward、Toggle Hidden、Cancel Modal（若适用）、Copy Path、Open Log Folder 等阶段 1–3 用户动作。  
内部-only 或不适合用户的不进表。

### 2.2 UI 与交互

- 打开：`OpenCommandPalette` + **`cmd-shift-p`**（绑定到 Workspace）。
- 形态：居中 elevated 浮层（类似 About/Go to Path），遮罩 + 输入框 + 结果列表。
- 输入：复用 `InputState`；结果用 `uniform_list` 或短列表（命令数 ≪ 10k，普通 `div` 列表也可；若 >50 条再 virtualize）。
- 过滤：**case-insensitive**，匹配 `title` 与 `keywords` 子串；可选简单 fuzzy（首字母序列）——MVP **子串即可**，与阶段 3 filter 一致。
- 导航：结果列表 `↑/↓` 或 `ctrl-n/p`；`Enter` 执行高亮项并关闭；`Esc` / 点遮罩关闭。
- 执行：`cx.dispatch_action` 或 workspace 方法调用；执行后关闭 palette 并 `notify`。
- `when` 不满足：项灰显且不可 Enter，或直接从列表隐藏——**推荐隐藏**以降低噪音。

### 2.3 与 modal 栈

- 打开 palette 时：若已有 host-key / conflict / delete / go-to-path，**仍可打开**或 **先不抢**——推荐：**允许打开**，`CancelActiveModal` 优先关 palette（palette 视为顶层 modal）。
- `cancel_active_modal` 顺序：palette → 其它 modal → …

### 2.4 状态

```rust
// Workspace
palette_open: bool,
palette_input: InputState,
palette_selected: usize, // index into filtered results
```

---

## 3. 文件列表键盘补齐

### 3.1 动作与键

| Action | 默认键 | 行为 |
| --- | --- | --- |
| `SelectNextEntry` / `SelectPrevEntry` | ↓ / ↑ | 单选移动；重置锚点为新 index |
| `SelectNextEntryExtend` / `SelectPrevEntryExtend` | shift-↓ / shift-↑ | 范围扩展 |
| `PageDown` / `PageUp` | page down / page up | 可见步进 `PAGE_SIZE = 10` |
| `SelectFirstEntry` / `SelectLastEntry` | home / end | 首/末 **可见** 项 |
| `SelectAllEntries` | cmd-a | 全选 **当前 side 可见** 项 |

均 context `FilePane`。Page/Home 在 extend 变体上：MVP **不做 shift-page**（YAGNI）；若实现成本低可加 `shift-page` 同锚点扩展。

### 3.2 选择模型（决策 A）

```rust
// Workspace 或随 tab 的 view 态
selection_anchor: Option<(PaneSide, EntryPath)>, // 或 visible index + path
```

- **单击 / 普通 ↑↓：** `selected_paths = [path]`，`anchor = path`。
- **shift+↑↓：** 若无 anchor，先设当前为 anchor；再选中 anchor 与当前 visible index **闭区间**内全部 path（按可见列表序）。
- **cmd-a：** 所有 visible entries 的 path；anchor 保持或设为首项。
- **打开目录 / 导航 / 过滤变更：** 清空 selection（与现网行为一致）或尽量保留 path——**保持现有 clear on navigate**；filter 变更时若选中被滤掉则保留 path 但无高亮（同阶段 3）。

Selection **只存 path**（已有），不存 row entity。

### 3.3 Page / Home / End

- 索引空间 = **visible** indices（hidden + filter 之后）。
- `PAGE_SIZE = 10`；`new_index = (i ± PAGE_SIZE).clamp(0, n-1)`。
- 移动后 `scroll_to_item`（已有 `UniformListScrollHandle`）。

### 3.4 与 type-to-filter

- filter 条 `explicit_focus` 时：字符进 filter；↑↓/page 仍可改变 selection（不 `stop_propagation` 吞掉导航键）——与阶段 3 评审一致。

---

## 4. Tab MRU 与 ctrl-tab 切换器

### 4.1 MRU 数据结构

```rust
// Workspace
tab_mru: Vec<TabId>, // front = most recent
```

- `activate_tab` / `open_new_tab` / 成功 connect 后：将该 `TabId` 移到 MRU 前端。
- `close_tab`：从 MRU 移除。
- 顺序：最近在前。

### 4.2 `cmd-shift-[` / `]`（决策补充 A）

- **保持创建顺序**（`tabs` Vec 序），**不**改走 MRU。
- 行为与现网 `activate_tab_in_direction` 一致。

### 4.3 ctrl-tab 切换器（决策 A）

**状态：**

```rust
tab_switcher_open: bool,
tab_switcher_index: usize, // into mru list
```

**交互：**

| 事件 | 行为 |
| --- | --- |
| `ctrl-tab`（按下） | 若未打开：打开切换器，index = 次近（MRU[1]）或 0；若已打开：index = (index+1) % n |
| `ctrl-shift-tab` | 反向 |
| 松开 `ctrl`（或 `ctrl` 修饰键释放） | 确认激活 `mru[index]`，关闭切换器 |
| `Esc` | 关闭不切换（或回原 tab） |

**UI：** 居中小浮层，列出 MRU 中 tab 标题 + 连接状态点；高亮当前 index。  
**实现注意：** GPUI 需监听 key up / modifiers changed——若无可靠 key-up，降级为：**第二次 ctrl-tab 循环，Enter 确认，Esc 取消**（文档写清降级条件）。优先实现 key-up 确认。

**Actions：** `ToggleTabSwitcher` / `TabSwitcherNext` 等稳定名。

---

## 5. 快捷键可发现性

### 5.1 Palette

每行布局：

```text
[ Title                          ⌘T ]
```

`keybinding` 来自注册表静态文案（与 `app_actions` 绑定保持同步；两处不一致时以 `app_actions` 为准，注册表手写副本需 code review）。

### 5.2 Tooltip

审计 icon-only 按钮，补常见键：

| 控件 | 示例 |
| --- | --- |
| Refresh | Refresh (⌘R) |
| Parent | Parent Directory (⌘↑) |
| Back/Forward | 已有 |
| New Folder / Delete | 已有阶段 1 |
| Transfers | Toggle Transfers (⌘J) |
| Hidden | Show Hidden Files (⌘⇧.) |

### 5.3 系统菜单

GPUI `MenuItem::action` 无稳定 checkmark/键位 API 时：**不强制**改菜单文案；依赖 palette + tooltip。View 菜单可继续列动作名。

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

**禁止：** core 引入 UI 选区锚点；palette 执行阻塞网络。

---

## 7. 测试计划

### Palette

- 打开/Esc 关闭  
- 过滤 title/keywords  
- Enter 触发 NewTab（tab 数 +1）  
- `when` 过滤：无连接时隐藏 Download 等  

### 列表键

- page down 步进 10（可见列表）  
- home/end  
- shift+↓ 选中区间 path 数  
- cmd-a 等于可见条目数  

### MRU

- 激活 B 再 A：MRU front = A  
- 关闭 tab 从 MRU 移除  
- cmd-shift-] 仍按创建序（构造 3 tab，验证顺序与 MRU 不同场景）  

### 手测

- 纯键盘：开 tab → 聚焦 remote → 连接（若可 mock）→ 多选 → 上传/下载 → 开 palette 找 Retry/Refresh → 关 tab  
- ctrl-tab 切换多个 tab  

---

## 8. 改动文件清单

| 文件 | 改动 |
| --- | --- |
| `crates/app/src/palette_commands.rs`（新） | 注册表 + 过滤函数 |
| `crates/app/src/workspace/command_palette.rs` 或 render/modals | palette UI |
| `crates/app/src/app_actions.rs` | 新 actions + 键绑定 |
| `crates/app/src/workspace/mod.rs` | palette / switcher / anchor / mru 状态 |
| `crates/app/src/workspace/panes.rs` | 选择扩展、page/home/end、select all |
| `crates/app/src/workspace/render.rs` | tooltip 文案；tab switcher 浮层 |
| `crates/app/src/workspace/tests.rs` | 上述测试 |
| `crates/ui` | 可选：通用 list row for palette |
| **不改** | sftp/core 协议 |

---

## 9. 建议 PR 切分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | 列表 page/home/end + shift 范围 + cmd-a | 无 |
| **PR2** | Command palette + 注册表 + cmd-shift-p | 无（可与 PR1 并行） |
| **PR3** | MRU 数据结构 + ctrl-tab 切换器 | 无 |
| **PR4** | tooltip 键位补全 + palette 键位列 polish | PR2 |

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| palette fuzzy | MVP 子串；后续可加 |
| ctrl-tab key-up | 优先；无则 Enter 确认降级 |
| shift-click 鼠标 | 可选同锚点；**不阻塞**键盘 MRU 验收 |
| 传输 drawer 内键盘 | 本阶段不扩展（已有 cancel/retry 按钮） |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | palette 注册方式 | A — 显式表 |
| 2 | 多选模型 | A — 锚点 + shift + cmd-a |
| 3 | Tab 切换 | A — MRU + ctrl-tab 切换器 |
| 4 | 可发现性 | A — palette 键 + tooltip |
| 5 | page 大小 | A — 10 行 |
| 6 | cmd-shift vs MRU | A — 创建序保留；MRU 仅 ctrl-tab |
| 7 | 交付 | 方案 1 — 一份设计做透 |

---

## Key Decisions

1. **显式 palette 表** — 控制用户文案与安全边界，符合规范「不暴露内部」。  
2. **锚点范围选** — 满足 §6.2，不引入完整 Finder cmd-click 复杂度。  
3. **双轨 tab 导航** — 创建序快捷键可预测；MRU 切换器服务「刚用过的 tab」。  
4. **PAGE_SIZE=10** — 可测、够用，避免依赖 viewport 测量。  
5. **可发现性以 palette 为主** — 在 GPUI 菜单限制下性价比最高。
