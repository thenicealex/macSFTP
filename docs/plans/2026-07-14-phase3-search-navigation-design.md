# 阶段 3 设计 — 搜索与目录导航

**Date:** 2026-07-14 GMT+8  
**来源：** `docs/ux-improvement-plan.md` 阶段 3；`docs/ui-ux-guidelines.md` §6.1 / §6.2。  
**方法：** brainstorming；5 个决策点 + 交付形态已与用户确认（见末尾 §决策记录）。  
**前置：** 阶段 1 文件操作、阶段 2 反馈可观测性已落地。

---

## 决策摘要（已确认）

1. **type-to-filter：** 聚焦 pane 直接键入 + `cmd-f` 显式打开/聚焦 filter 条（C）。
2. **导航历史：** 每 tab 的 local / remote 各维护 back/forward 双栈；按钮 + `cmd-[` / `cmd-]`（A）。
3. **路径跳转：** 可点击面包屑 + `cmd-shift-g` Go to Path（A）。
4. **隐藏文件：** 默认隐藏 dotfiles；`AppConfig.show_hidden_files` 持久化；`cmd-shift-.` toggle（A）。
5. **列排序：** 表头点击切换字段 / 翻转方向；local 与 remote 统一使用 `tab.sort`（A）。
6. **交付：** 一份设计覆盖全部子项；实现可 PR 切分。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| type-to-filter | 聚焦敲字增量过滤已加载列表；`cmd-f` 可聚焦条；`Esc` 清除；万级目录无卡顿 |
| back/forward | 每侧独立历史；主动导航压栈；refresh 同 path 不压栈；`cmd-[`/`]` + path bar 按钮 |
| 面包屑 / Go to Path | 路径段可点跳转；`cmd-shift-g` 输入绝对路径直达 |
| 隐藏文件 | 默认不显示 `.` 前缀名；toggle 写 config；filter 在 hidden 之后 |
| 列排序 | Name/Size/Modified 可点；同列翻方向；本地加载不再无视 `tab.sort` |

### 非目标

- 远端递归搜索 / 服务端 find / 索引。
- page up/down、home/end、shift 范围多选、`cmd-a`（阶段 4）。
- command palette、MRU tab（阶段 4）。
- session 恢复 / 排序偏好持久化到 config（阶段 5 可再议）。
- 修改 SFTP listing 协议或 `AppCommand` 读路径形状。

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

建议挂在 `Workspace` 的 `HashMap<(TabId, PaneSide), PaneFilter>`，或内嵌于 tab 的 view 扩展态（**不进** `TabState` 持久化模型，除非实现时发现必须）。

### 2.2 进入 / 退出

| 入口 | 行为 |
| --- | --- |
| FilePane 聚焦 + 可打印字符 | 无 modal / 行内 rename / Go to Path / connect 抢键时：追加到 `query`，`explicit_focus=false`，显示 filter 条 |
| `cmd-f`（`FilterPane` action） | 显示 filter 条，`explicit_focus=true`，便于粘贴与编辑 |
| `Esc` | 清空 `query`、reset input、关条，焦点回 FilePane |
| 导航到新 path / 切换 active tab | **清空**该 side 的 filter（避免陈旧过滤） |
| Backspace（type-to-filter 模式） | 删除 query 末字符；空则关条 |

### 2.3 匹配与渲染顺序

对当前 pane 的 entries 派生 **visible list**：

1. 若 `!show_hidden_files`：去掉 `name.starts_with('.')`
2. 若 `query` 非空：basename **case-insensitive 子串** 匹配
3. 列表已按 `tab.sort` 排好；过滤不重排（稳定相对序）

`uniform_list` 只绑 visible list。selection 仍按 path；若选中项被滤掉，保持 selection 集合但可见高亮可能为空——`↑/↓` 在 visible 列表上移动时写入新 selection。

Filter 条文案：`Filter: {query}` + `{matched} / {total_visible_before_filter}`（total 为 hidden 过滤后的数量）。无匹配：empty_state「No matches」。

### 2.4 键冲突

- 行内 rename / delete modal / connect form 打开时：**不**进入 type-to-filter。
- filter 激活时 `enter` 仍打开选中项（过滤后列表）；`cmd-up` 等全局 pane 动作不变。

---

## 3. 导航历史与路径跳转

### 3.1 统一导航入口

所有「到某 path」必须走：

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
| `Push` | 若 path ≠ current：把 current 压入 `back`，清空 `forward`，再加载 path |
| `Replace` | 只加载 path，不改栈（少用；例如纠错） |
| `Back` | 当前压入 `forward`，从 `back` 弹出目标并加载 |
| `Forward` | 对称 |

**必须**用此入口的调用方：双击目录、parent up、面包屑、Go to Path、back/forward 按钮与快捷键。  
**不得**压栈：同 path 的 refresh、连接后首次 `remote_root` 的首次 load（可用 `Push` 若从 None→Some，back 为空即可）。

### 3.2 数据结构

每 tab：

```rust
struct PaneNavHistory {
    back: Vec<String>,    // 存 path 字符串；与 LocalPath/RemotePath 互转
    forward: Vec<String>,
}
// Tab 级：local_nav, remote_nav
```

可放 view（`Workspace` map by TabId）或 `TabState` 旁路字段。推荐 **Workspace view 态**，避免 core 膨胀；tab 关闭时 drop。

上限：每栈最多 **50** 条，超出丢最旧。

### 3.3 UI / 快捷键

path bar 顺序（左→右）：

```text
[◀] [▶] [↑] [↻]  [breadcrumb…………]  [copy] [new folder] [delete] [upload|download]
```

- `◀`/`▶` disabled 时灰显；tooltip：`Back (⌘[)` / `Forward (⌘])`
- Actions：`NavigateBack` / `NavigateForward`，context `FilePane` 或 `Workspace`（focused side）
- **禁止**与 tab 切换键冲突：tab 保持 `cmd-shift-[/]`；本阶段用 `cmd-[` / `cmd-]`

### 3.4 面包屑

- 将绝对路径拆成段（root `/` + 各 component）。
- 每段可点 → `navigate_pane(..., prefix_path, Push)`。
- 过长：中间折叠为 `…`（不可点或展开菜单可选，MVP **不可点省略号**）；**保留首段与末两段**优先（规范：末尾文件夹优先可见）。

### 3.5 Go to Path（`cmd-shift-g`）

- Action：`GoToPath`
- UI：轻量居中条 / 小 modal：单行 `InputState`，占位 `Absolute path`
- `Enter`：  
  - Local：`expand_home` 后导航；若路径不存在，status 报错且不压无效历史  
  - Remote：直接 `navigate` 请求 listing；失败走现有 `remote.error` + Retry  
- `Esc`：关闭不导航  
- 成功导航使用 `HistoryOp::Push`

---

## 4. 隐藏文件

### 4.1 Config

```rust
// AppConfig — #[serde(default)] 已在 struct 上
pub show_hidden_files: bool,  // Default::default() => false
```

`ConfigStore::set_show_hidden_files(bool) -> Result<(), ConfigError>`，模式同 `set_confirm_delete`。

旧 config 无字段 → serde default → **false**（默认隐藏）。

### 4.2 行为

- hidden 定义：`name.starts_with('.')`（与现有 `FileRowModel.is_hidden` 一致）。
- 仅 **view 过滤**；`entries` 全量仍可保留在 `TabState`，或过滤仅在 render 派生——推荐 **派生 visible**，避免丢数据导致 toggle 时再读盘。
- Toggle：`cmd-shift-.` + path bar / 菜单「Show Hidden Files」勾选态。
- 与 filter：见 §2.3 顺序。

---

## 5. 列排序

### 5.1 已有能力

- `FileSort` / `FileSortField` / `SortDirection` / `sort_entries`（core）已实现 directories-first 与字段比较。
- `file_table_header` 已展示当前列标记，**无点击**。

### 5.2 点击行为

| 操作 | 结果 |
| --- | --- |
| 点击当前排序列 | `direction` 翻转 |
| 点击其它列 | `field = 该列`，`direction = Ascending` |
| `directories_first` | 恒为 `true`，无 UI |

Name / Size / Modified 三列可点（Kind 不暴露在表头）。

### 5.3 应用

- 改 sort 后：对 **当前** `tab.local.entries` / `tab.remote.entries` 调用 `sort_entries(&mut entries, &tab.sort)`，`cx.notify()`。
- **修复** `load_local_directory`：使用 `tab.sort`（或传入的 sort），禁止 `Default::default()`。
- `RemoteDirLoaded` 已用 `tab.sort`（保持）。

### 5.4 持久化

sort **仅会话内**（tab 存活期间）。不写 `AppConfig`。

---

## 6. 架构边界

```text
user 敲字 / cmd-f
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

**禁止：** GPUI 主线程大同步扫描磁盘以外的额外全盘 walk；filter 仅对内存 entries O(n)。

---

## 7. Actions 与快捷键

| Action | 默认键 | Context |
| --- | --- | --- |
| `FilterPane` | `cmd-f` | FilePane |
| `NavigateBack` | `cmd-[` | Workspace / FilePane |
| `NavigateForward` | `cmd-]` | Workspace / FilePane |
| `GoToPath` | `cmd-shift-g` | Workspace |
| `ToggleHiddenFiles` | `cmd-shift-.` | Workspace |

现有：`ParentDirectory` `cmd-up`、`RefreshPane` `cmd-r` 等不变。

---

## 8. 测试计划

### Unit / app

- filter：子串匹配、大小写、Esc 清空、导航清空 query  
- NavHistory：Push 清空 forward；Back/Forward 顺序；同 path refresh 不 Push  
- 面包屑：段拼接 root / 中间路径  
- Go to Path：空路径拒绝；local 缺失报错  
- `show_hidden_files` 默认 false + config round-trip  
- 表头：同列翻转、异列重置 Ascending；local load 遵循 tab.sort  

### 手测

- 万级本地目录敲字过滤  
- remote 浏览 back/forward  
- 点面包屑到祖先  
- `cmd-shift-g` 到 `/tmp` 与 remote 绝对路径  
- 隐藏/显示 `.git`  
- 点 Size 排序后目录仍优先  

---

## 9. 改动文件清单

| 文件 | 改动 |
| --- | --- |
| `crates/storage/src/config.rs` | `show_hidden_files` + setter + 测试 |
| `crates/app/src/app_actions.rs` | 新 actions + keybindings |
| `crates/app/src/workspace/mod.rs` | filter / nav 状态字段 |
| `crates/app/src/workspace/panes.rs` | `navigate_pane`、history、filter 逻辑、local sort 修复 |
| `crates/app/src/workspace/render.rs` | path bar 按钮、面包屑、filter 条、表头 on_click、hidden toggle |
| `crates/ui/src/file_list.rs` | 可点击 `file_table_header` |
| `crates/app/src/workspace/modals.rs` 或 panes | Go to Path UI |
| `crates/app/src/workspace/tests.rs` | 上述行为测试 |
| **不改** | SFTP listing 协议、`AppCommand` 读路径枚举形状 |

---

## 10. 建议 PR 切分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | 列排序可点 + local `tab.sort` 修复 | 无 |
| **PR2** | `show_hidden_files` config + toggle | 无（可与 PR1 并行） |
| **PR3** | `navigate_pane` + NavHistory + back/forward UI/keys | 无 |
| **PR4** | 面包屑 + Go to Path | PR3 |
| **PR5** | type-to-filter + cmd-f | PR2 优先（hidden 顺序） |

---

## 11. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| filter 是否模糊/正则 | **否**，仅 case-insensitive 子串 |
| 历史是否跨重启 | **否**，仅会话 |
| 面包屑省略号展开 | MVP 不可点；后续可加 |
| remote Go to Path 不存在 | listing 失败 → 现有 error + Retry，不伪造空目录成功 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | type-to-filter 形态 | C — 键入 + `cmd-f` |
| 2 | back/forward 模型 | A — 每 pane 双栈 |
| 3 | 路径跳转 | A — 面包屑 + Go to Path |
| 4 | 隐藏文件 | A — 默认隐藏 + config |
| 5 | 列排序点击 | A — 切字段 / 翻方向 |
| 6 | 交付 | 方案 1 — 一份设计做透 |

---

## Key Decisions

1. **过滤只在已加载 entries 上做** — 满足万级秒滤，不引入远端搜索。  
2. **统一 `navigate_pane`** — 防止 back 栈与 up/面包屑分叉。  
3. **hidden 默认关** — 对齐专业客户端密度；显式 toggle + 持久化。  
4. **sort 会话内** — 避免过早膨胀 config；先修 local 一致性。  
5. **`cmd-[/]` 给导航** — 与 tab 的 `cmd-shift-[/]` 分离。
