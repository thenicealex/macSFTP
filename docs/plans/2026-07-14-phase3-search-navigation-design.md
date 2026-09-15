# 阶段 3 设计 — 搜索与目录导航

**日期：** 2026-07-14 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 3；`docs/ui-ux-guidelines.md` §6.1 / §6.2。
**方法：** brainstorming；用户已经确认 5 个决策点和交付形式，具体内容参见末尾“决策记录”。
**前置条件：** 阶段 1 的文件操作和阶段 2 的反馈可观测性均已完成。

---

## 决策摘要（已确认）

1. **type-to-filter：** pane 获得焦点后可以直接输入字符；`cmd-f` 显示 filter 条并使其获得焦点（C）。
2. **导航历史：** 每个 tab 的 local 和 remote 分别维护 back/forward 双栈；入口包括按钮以及 `cmd-[` / `cmd-]`（A）。
3. **路径跳转：** 提供可选择的面包屑和 `cmd-shift-g` Go to Path（A）。
4. **隐藏文件：** 默认隐藏 dotfiles；`AppConfig.show_hidden_files` 负责持久化；`cmd-shift-.` 负责 toggle（A）。
5. **列排序：** 选择表头可以切换字段或改变方向；local 和 remote 统一使用 `tab.sort`（A）。
6. **交付：** 一份设计覆盖全部子项，但是实现可以拆分为多个 PR。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| type-to-filter | pane 获得焦点后，输入字符即可增量过滤已加载列表；`cmd-f` 可使 filter 条获得焦点；`Esc` 清除过滤条件；万级目录无卡顿 |
| back/forward | 每侧使用独立历史；主动导航更新历史；同一 path 的 refresh 不更新历史；提供 `cmd-[`/`]` 和 path bar 按钮 |
| 面包屑 / Go to Path | 路径段可以选择；`cmd-shift-g` 支持输入绝对路径并访问目标目录 |
| 隐藏文件 | 默认不显示名称以 `.` 开始的条目；toggle 写入 config；filter 在 hidden 过滤之后执行 |
| 列排序 | Name/Size/Modified 均可选择；再次选择同一列时改变方向；本地加载必须使用 `tab.sort` |

### 非目标

- 远端递归搜索、服务端 find 或索引。
- page up/down、home/end、shift 范围多选和 `cmd-a`，因为这些功能属于阶段 4。
- command palette 和 MRU tab，因为这些功能属于阶段 4。
- session 恢复，或将排序偏好持久化到 config；后者可以在阶段 5 再次评估。
- 修改 SFTP listing 协议或 `AppCommand` 的读路径结构。

---

## 2. 目录内 type-to-filter

### 2.1 状态（view，按 tab × side）

```rust
struct PaneFilter {
    /// 空串表示未过滤 / 未显示条（或条在 cmd-f 打开但尚未输入）。
    query: String,
    input: InputState,
    /// true：cmd-f 打开，键事件优先进 input；false：type-to-filter 直接改 query。
    explicit_focus: bool,
}
```

建议使用 `Workspace` 中的 `HashMap<(TabId, PaneSide), PaneFilter>` 保存该状态，也可以将其置于 tab 的 view 扩展状态中。但是该状态**不应进入** `TabState` 持久化模型，除非实现时确认持久化不可避免。

### 2.2 进入与退出

| 入口 | 行为 |
| --- | --- |
| FilePane 获得焦点 + 可打印字符 | 如果 modal、行内 rename、Go to Path 和 connect 均未优先处理键盘事件，那么将字符追加到 `query`，设置 `explicit_focus=false`，并显示 filter 条 |
| `cmd-f`（`FilterPane` action） | 显示 filter 条并设置 `explicit_focus=true`，因此用户可以粘贴和编辑文本 |
| `Esc` | 清空 `query`、reset input、隐藏 filter 条，并将焦点恢复到 FilePane |
| 导航到新 path / 切换 active tab | **清空**对应 side 的 filter，因此旧路径的过滤条件不会影响新路径 |
| Backspace（type-to-filter 模式） | 删除 `query` 的末尾字符；如果结果为空，则隐藏 filter 条 |

### 2.3 匹配与渲染顺序

根据当前 pane 的 entries 派生 **visible list**：

1. 如果 `!show_hidden_files`，则排除 `name.starts_with('.')` 的条目。
2. 如果 `query` 非空，则对 basename 执行 **case-insensitive 子串**匹配。
3. 列表已经按照 `tab.sort` 排序，因此过滤操作不改变剩余条目的相对顺序。

`uniform_list` 只使用 visible list。selection 继续以 path 为标识。如果选中项因为过滤而不可见，则保留 selection 集合，但是可见列表中可以没有高亮项；用户使用 `↑/↓` 在 visible list 中移动时，selection 更新为新的条目。

Filter 条显示 `Filter: {query}` 和 `{matched} / {total_visible_before_filter}`，其中 total 是 hidden 过滤后的数量。如果没有匹配项，则显示 empty state “No matches”。

### 2.4 键盘事件冲突

- 行内 rename、delete modal 或 connect form 可见时，type-to-filter **不得**处理键盘输入。
- filter 激活时，`enter` 仍然进入过滤后列表的选中项；`cmd-up` 等全局 pane 操作保持不变。

---

## 3. 导航历史与路径跳转

### 3.1 统一导航入口

所有访问某个 path 的操作必须调用：

```rust
fn navigate_pane(
    &mut self,
    side: PaneSide,
    path: /* LocalPath | RemotePath */,
    history: HistoryOp, // Push | Replace | Back | Forward
    window, cx,
)
```

| HistoryOp | 行为 |
| --- | --- |
| `Push` | 如果 path ≠ current，则将 current 追加到 `back`，清空 `forward`，然后加载 path |
| `Replace` | 只加载 path，不修改历史；该操作仅用于少数场景，例如路径纠错 |
| `Back` | 将 current 追加到 `forward`，移除 `back` 的末项，并将该项作为目标加载 |
| `Forward` | 执行与 `Back` 对称的操作 |

以下调用方**必须**使用该入口：双击目录、parent up、面包屑、Go to Path、back/forward 按钮和快捷键。

同一 path 的 refresh **不得**更新历史。连接后的首次 `remote_root` load 也不得更新历史；如果状态从 None 变为 Some，则可以使用 `Push`，因为此时 back 为空。

### 3.2 数据结构

每个 tab 使用以下结构：

```rust
struct PaneNavHistory {
    back: Vec<String>,    // 存 path 字符串；与 LocalPath/RemotePath 互转
    forward: Vec<String>,
}
// Tab 级：local_nav, remote_nav
```

该结构可以属于 view，由 `Workspace` 按 TabId 保存；也可以作为 `TabState` 之外的辅助字段。推荐使用 **Workspace view 状态**，因为导航历史无需扩大 core 模型；tab 关闭时即可删除对应状态。

每个栈最多保存 **50** 项。如果数量超过限制，则删除最早的项目。

### 3.3 UI 与快捷键

path bar 从左至右的顺序如下：

```text
[◀] [▶] [↑] [↻]  [breadcrumb…………]  [copy] [new folder] [delete] [upload|download]
```

- `◀`/`▶` 在不可用时显示 disabled 状态；tooltip 分别为 `Back (⌘[)` 和 `Forward (⌘])`。
- Actions 为 `NavigateBack` 和 `NavigateForward`，context 使用 `FilePane` 或 `Workspace`，并且作用于 focused side。
- 导航快捷键**不得**与 tab 切换键冲突。tab 继续使用 `cmd-shift-[/]`，因此本阶段使用 `cmd-[` / `cmd-]`。

### 3.4 面包屑

- 将绝对路径解析为 root `/` 和各个 component。
- 选择任一路径段后调用 `navigate_pane(..., prefix_path, Push)`。
- 路径过长时，中间部分显示为 `…`。MVP 中的省略号不可选择，也不显示扩展菜单。显示空间应优先分配给首段和末两段，因为规范要求优先显示末尾文件夹。

### 3.5 Go to Path（`cmd-shift-g`）

- Action：`GoToPath`。
- UI：使用轻量居中条或小型 modal，其中包含单行 `InputState`，占位文案为 `Absolute path`。
- `Enter`：
  - Local：执行 `expand_home` 后导航。如果路径不存在，则在 status 中显示错误，并且不将无效路径写入历史。
  - Remote：直接请求目标路径的 listing。如果请求失败，则使用现有 `remote.error` 和 Retry。
- `Esc`：关闭 UI，但是不执行导航。
- 导航成功时使用 `HistoryOp::Push`。

---

## 4. 隐藏文件

### 4.1 Config

```rust
// AppConfig — #[serde(default)] 已在 struct 上
pub show_hidden_files: bool,  // Default::default() => false
```

增加 `ConfigStore::set_show_hidden_files(bool) -> Result<(), ConfigError>`，其实现模式与 `set_confirm_delete` 相同。

旧 config 不包含该字段，因此 serde default 将其设为 **false**，也就是默认隐藏。

### 4.2 行为

- hidden 的定义是 `name.starts_with('.')`，并且应与现有 `FileRowModel.is_hidden` 一致。
- 该设置只影响 **view 过滤**。`TabState` 可以继续保存完整的 `entries`，或者仅在 render 阶段派生过滤结果。推荐派生 visible list，因为切换 toggle 时无需再次访问文件系统。
- Toggle 的入口包括 `cmd-shift-.`，以及 path bar 或菜单中的“Show Hidden Files”勾选项。
- hidden 和 filter 的执行顺序遵循 §2.3。

---

## 5. 列排序

### 5.1 已有能力

- core 已经实现 `FileSort`、`FileSortField`、`SortDirection` 和 `sort_entries`，并且支持 directories-first 和字段比较。
- `file_table_header` 已经显示当前列的排序标记，但是尚未处理选择事件。

### 5.2 选择行为

| 操作 | 结果 |
| --- | --- |
| 选择当前排序列 | 在 `Ascending` 和 `Descending` 之间切换 `direction` |
| 选择其他列 | 设置 `field = 该列` 和 `direction = Ascending` |
| `directories_first` | 始终为 `true`，因此不提供 UI |

Name、Size 和 Modified 三列均可选择，但是表头不显示 Kind 排序入口。

### 5.3 应用排序

- 修改 sort 后，对**当前** `tab.local.entries` 和 `tab.remote.entries` 调用 `sort_entries(&mut entries, &tab.sort)`，然后调用 `cx.notify()`。
- 修改 `load_local_directory`，使其使用 `tab.sort` 或调用方提供的 sort，而不是 `Default::default()`。
- `RemoteDirLoaded` 已经使用 `tab.sort`，因此保持现有实现。

### 5.4 持久化

sort **只在会话内有效**，也就是仅在 tab 存续期间有效，并且不写入 `AppConfig`。

---

## 6. 架构边界

```text
user 输入字符 / cmd-f
        │
        ▼
  PaneFilter (view)
        │
        ▼
  visible = filter(hide_dots(entries), query) ──► uniform_list

  up / 面包屑 / goto / back / forward / open dir
        │
        ▼
  navigate_pane(side, path, HistoryOp)
        │
        ├── update NavHistory stacks
        └── set_local_path | request_remote_directory

  header click ──► tab.sort ──► sort_entries
  cmd-shift-.  ──► AppConfig.show_hidden_files ──► re-derive visible
```

GPUI 主线程**不得**为了该功能同步递归遍历磁盘。filter 只对内存中的 entries 执行 O(n) 操作。

---

## 7. Actions 与快捷键

| Action | 默认键 | Context |
| --- | --- | --- |
| `FilterPane` | `cmd-f` | FilePane |
| `NavigateBack` | `cmd-[` | Workspace / FilePane |
| `NavigateForward` | `cmd-]` | Workspace / FilePane |
| `GoToPath` | `cmd-shift-g` | Workspace |
| `ToggleHiddenFiles` | `cmd-shift-.` | Workspace |

现有 `ParentDirectory` 的 `cmd-up`、`RefreshPane` 的 `cmd-r` 等绑定保持不变。

---

## 8. 测试计划

### Unit / app

- filter：子串匹配、大小写处理、Esc 清空，以及导航后清空 query。
- NavHistory：Push 清空 forward；Back/Forward 顺序；同一 path 的 refresh 不执行 Push。
- 面包屑：root 和中间路径的路径段组合。
- Go to Path：拒绝空路径；local 路径不存在时显示错误。
- `show_hidden_files`：默认值为 false，并且 config round-trip 后值保持一致。
- 表头：同一列切换方向、不同列重置为 Ascending；local load 遵循 tab.sort。

### 手工测试

- 在万级本地目录中输入字符并确认过滤结果。
- 在 remote 目录中使用 back/forward。
- 选择面包屑并访问祖先目录。
- 使用 `cmd-shift-g` 访问 `/tmp` 和 remote 绝对路径。
- 隐藏和显示 `.git`。
- 选择 Size 排序后，确认目录仍然优先显示。

---

## 9. 修改文件清单

| 文件 | 修改内容 |
| --- | --- |
| `crates/storage/src/config.rs` | `show_hidden_files`、setter 和测试 |
| `crates/app/src/app_actions.rs` | 新 actions 和 keybindings |
| `crates/app/src/workspace/mod.rs` | filter 和 nav 状态字段 |
| `crates/app/src/workspace/panes.rs` | `navigate_pane`、history、filter 逻辑和 local sort 修复 |
| `crates/app/src/workspace/render.rs` | path bar 按钮、面包屑、filter 条、表头 on_click 和 hidden toggle |
| `crates/ui/src/file_list.rs` | 支持选择操作的 `file_table_header` |
| `crates/app/src/workspace/modals.rs` 或 panes | Go to Path UI |
| `crates/app/src/workspace/tests.rs` | 上述行为的测试 |
| **不修改** | SFTP listing 协议、`AppCommand` 读路径枚举结构 |

---

## 10. 建议的 PR 划分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | 表头支持排序选择，并且 local 使用 `tab.sort` | 无 |
| **PR2** | `show_hidden_files` config 和 toggle | 无，因此可与 PR1 并行 |
| **PR3** | `navigate_pane`、NavHistory 和 back/forward UI/keys | 无 |
| **PR4** | 面包屑和 Go to Path | PR3 |
| **PR5** | type-to-filter 和 cmd-f | 建议先完成 PR2，因为 hidden 过滤先于 query 过滤 |

---

## 11. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| filter 是否支持模糊匹配/正则表达式 | **否**，只支持 case-insensitive 子串 |
| 历史是否跨重启 | **否**，只在当前会话有效 |
| 面包屑省略号是否提供扩展操作 | MVP 中不可选择；后续可以增加 |
| remote Go to Path 不存在 | listing 失败后显示现有 error 和 Retry，不将结果表示为空目录成功 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | type-to-filter 形式 | C — 输入字符 + `cmd-f` |
| 2 | back/forward 模型 | A — 每个 pane 使用双栈 |
| 3 | 路径跳转 | A — 面包屑 + Go to Path |
| 4 | 隐藏文件 | A — 默认隐藏 + config |
| 5 | 列排序选择 | A — 切换字段 / 改变方向 |
| 6 | 交付 | 方案 1 — 使用一份完整设计 |

---

## Key Decisions

1. **过滤只作用于已加载的 entries。** 该范围能够满足万级条目的即时过滤要求，并且不引入远端搜索。
2. **导航统一使用 `navigate_pane`。** 因此 back 历史不会与 up 或面包屑使用不同的更新逻辑。
3. **hidden 默认关闭显示。** 该默认值符合专业客户端的信息密度，同时用户可以通过显式 toggle 修改并持久化该设置。
4. **sort 只在会话内有效。** 该范围避免过早扩展 config，同时优先修正 local 和 remote 的一致性。
5. **`cmd-[/]` 用于导航。** 该绑定与 tab 使用的 `cmd-shift-[/]` 相互独立，因此两类操作不会冲突。
