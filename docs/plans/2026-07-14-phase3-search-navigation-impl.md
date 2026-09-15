# Phase 3 Search & Directory Navigation Implementation Plan

> **Agent 执行要求：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans`，并且按任务实施本计划。各步骤使用复选框（`- [ ]`）记录状态。

**目标：** 增加 type-to-filter、每个 pane 独立的前进/后退历史、可单击的面包屑、Go to Path、隐藏文件切换和可单击的列排序，因此大型目录也能保持可用。

**架构：** View 层 filter 和 nav history 存储在 `Workspace` 中，并且所有路径变更都通过 `navigate_pane(..., HistoryOp)` 完成。隐藏文件设置和 filter 从已有条目派生**可见**列表，但是不修改已存储的条目。排序继续使用现有的 `tab.sort` 和 `sort_entries`，同时 header 增加交互能力。

**Tech Stack:** Rust, GPUI, `macsftp_core::{FileSort, FileSortField, sort_entries, LocalPath, RemotePath}`, `macsftp_storage::AppConfig`, existing `InputState` / `uniform_list`.

**Spec:** `docs/plans/2026-07-14-phase3-search-navigation-design.md`

## Global Constraints

- Filter only already-loaded entries (no remote recursive search).
- Case-insensitive **substring** match on basename only (no regex).
- Hidden = `name.starts_with('.')`; default `show_hidden_files = false` in config.
- Nav history 仅在当前 session 有效（每个 stack 最多 50 条），因此不持久化。
- Sort 仅在当前 session 的 `tab.sort` 中有效，因此不能将排序状态写入 config。
- `cmd-[` / `cmd-]` = NavigateBack/Forward; tab switch stays `cmd-shift-[` / `]`.
- 刷新相同路径时不能向 history 增加记录。
- No `unwrap`/`expect` on recoverable paths (AGENTS.md §5).
- 优先使用 `src/foo.rs`，而不使用 `mod.rs`，并且遵循现有 workspace 风格。
- Do not change SFTP listing protocol or `AppCommand` read shapes.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/workspace/nav.rs` | `HistoryOp`, `PaneNavHistory`, pure history ops + unit tests |
| **Create** `crates/app/src/workspace/visible_entries.rs` (or helpers in panes) | `apply_hidden_and_filter` pure helpers + tests |
| **Modify** `crates/storage/src/config.rs` | `show_hidden_files` + setter + tests |
| **Modify** `crates/ui/src/file_list.rs` + `ui.rs` | Clickable `file_table_header` |
| **Modify** `crates/app/src/app_actions.rs` | New actions + keybindings |
| **Modify** `crates/app/src/workspace/mod.rs` | Fields: nav map, filter map, go_to_path state；注册 actions |
| **Modify** `crates/app/src/workspace/panes.rs` | `navigate_pane`, sort apply, visible list helpers, open_entry/up use navigate |
| **Modify** `crates/app/src/workspace/render.rs` | Path bar back/forward, breadcrumb, filter bar, sort clicks, hidden toggle |
| **Modify** `crates/app/src/workspace/modals.rs` | Go to Path UI + cancel_active_modal |
| **Modify** `crates/app/src/workspace/tests.rs` | App-level tests |
| **Do not modify** | `session_actor` listing, core `AppCommand` read enums |

---

### Task 1：可单击的列排序和 local `tab.sort` 修复

**文件：**
- Modify: `crates/ui/src/file_list.rs`
- Modify: `crates/ui/src/ui.rs` (re-export if signature changes)
- Modify: `crates/app/src/workspace/panes.rs` (`load_local_directory`)
- Modify: `crates/app/src/workspace/render.rs` (header callback + `cycle_sort`)
- Test: `crates/app/src/workspace/tests.rs`

**接口：**
- 产出以下接口：
  - `file_table_header(sort, cx, on_click: impl Fn(FileSortField, &mut Window, &mut App))`；也可以保留纯 header，并在 app 的 render 中重新构建 header columns 以增加单击行为。但是，优先为 `file_table_header` 增加可选 click handlers。
  - `Workspace::apply_sort_field(&mut self, field: FileSortField, cx)`
  - `load_local_directory` sorts with `&tab.sort`

- [ ] **Step 1：编写失败测试，验证 local load 使用 tab sort**

In `tests.rs`:

```rust
#[gpui::test]
fn local_directory_respects_tab_sort_by_size(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    let (fixture, base) = {
        // unique temp dir with two files of different sizes
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "macsftp-sort-{}-{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("small.bin"), vec![0u8; 1]).unwrap();
        (dir.clone(), LocalPath::new(dir.to_string_lossy().into_owned()))
    };
    workspace.update_in(&mut cx, |workspace, window, cx| {
        if let Some(tab) = workspace.active_tab_mut() {
            tab.sort.field = FileSortField::Size;
            tab.sort.direction = SortDirection::Ascending;
        }
        workspace.set_local_path(base, window, cx);
        let names: Vec<_> = workspace
            .active_tab()
            .unwrap()
            .local
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        // directories_first: only files here — small before big when ascending size
        let small_i = names.iter().position(|n| *n == "small.bin").unwrap();
        let big_i = names.iter().position(|n| *n == "big.bin").unwrap();
        assert!(small_i < big_i, "expected size ascending, got {names:?}");
    });
    let _ = std::fs::remove_dir_all(&fixture);
}
```

Import `FileSortField`, `SortDirection` from `macsftp_core`.

- [ ] **Step 2：执行测试，预期结果为 FAIL**

```bash
cargo test -p macsftp-app --bin macsftp local_directory_respects_tab_sort -- --nocapture
```

预期结果：FAIL，因为两个文件默认按名称排序，所以 `big` 在字母顺序上位于 `small` 之前。

- [ ] **Step 3：修复 `load_local_directory`**

```rust
// panes.rs
macsftp_core::sort_entries(&mut entries, &tab.sort);
```

使用该调用替代 `&Default::default()`。

- [ ] **Step 4：让 header 支持单击操作**

修改 `file_table_header` 以接收单击回调。建议采用以下方式：

```rust
pub fn file_table_header(
    sort: &FileSort,
    cx: &App,
    mut on_field: impl FnMut(FileSortField, &mut Window, &mut App) + 'static,
) -> impl IntoElement
```

为每个 column label 的 `div` 增加 `.id(...).on_click`，并且调用 `on_field(FileSortField::Name|Size|ModifiedAt, ...)`。

更新所有调用点，包括 `render.rs` 以及相关 test/helper；通常只有 `render.rs`：

```rust
.child(file_table_header(&sort, cx, {
    let entity = cx.entity();
    move |field, window, cx| {
        entity.update(cx, |workspace, cx| {
            workspace.apply_sort_field(field, cx);
        });
    }
}))
```

Implement:

```rust
pub(crate) fn apply_sort_field(&mut self, field: FileSortField, cx: &mut Context<Self>) {
    let Some(tab) = self.active_tab_mut() else { return };
    if tab.sort.field == field {
        tab.sort.direction = match tab.sort.direction {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        };
    } else {
        tab.sort.field = field;
        tab.sort.direction = SortDirection::Ascending;
    }
    let sort = tab.sort.clone();
    macsftp_core::sort_entries(&mut tab.local.entries, &sort);
    macsftp_core::sort_entries(&mut tab.remote.entries, &sort);
    cx.notify();
}
```

- [ ] **Step 5：测试 apply_sort_field toggle**

```rust
#[gpui::test]
fn apply_sort_field_toggles_direction_on_same_column(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, cx| {
        workspace.apply_sort_field(FileSortField::Name, cx);
        // default was already Name Ascending → becomes Descending
        assert_eq!(
            workspace.active_tab().unwrap().sort.direction,
            SortDirection::Descending
        );
        workspace.apply_sort_field(FileSortField::Size, cx);
        let sort = &workspace.active_tab().unwrap().sort;
        assert_eq!(sort.field, FileSortField::Size);
        assert_eq!(sort.direction, SortDirection::Ascending);
    });
}
```

注意：如果 `apply_sort_field` 不需要 window，则只接收 `cx: &mut Context`，因此与只调用 `cx.notify()` 的现有方法保持一致。

- [ ] **Step 6：执行测试**

```bash
cargo test -p macsftp-app --bin macsftp local_directory_respects_tab_sort apply_sort_field -- --nocapture
```

预期结果：PASS。

- [ ] **Step 7：提交变更**

```bash
git add crates/ui/src/file_list.rs crates/ui/src/ui.rs \
  crates/app/src/workspace/panes.rs crates/app/src/workspace/render.rs \
  crates/app/src/workspace/tests.rs
git commit -m "feat(app): clickable column sort and fix local tab.sort"
```

---

### Task 2：`show_hidden_files` config 和 visible-list filtering

**Files:**
- Modify: `crates/storage/src/config.rs`
- Modify: `crates/app/src/app_actions.rs`
- Modify: `crates/app/src/workspace/mod.rs` (action handler)
- Modify: `crates/app/src/workspace/panes.rs` or new helper module for `visible_entries`
- Modify: `crates/app/src/workspace/render.rs` (use visible list; toggle button)
- Test: config unit test + workspace test

**Interfaces:**
- 产出以下接口：
  - `AppConfig.show_hidden_files: bool` default **false**
  - `ConfigStore::set_show_hidden_files(bool) -> Result<(), ConfigError>`
  - `fn entry_is_hidden(name: &str) -> bool { name.starts_with('.') }`
  - `fn filter_hidden<E: HasName>(entries: &[E], show_hidden: bool) -> impl Iterator`
  - Action `ToggleHiddenFiles` + `cmd-shift-.`

- [ ] **Step 1：编写 config 测试（TDD）**

```rust
#[test]
fn show_hidden_files_defaults_false_and_round_trips() {
    let path = temp_config_path("hidden");
    cleanup(&path);
    let store = ConfigStore::open(path.clone()).unwrap();
    assert!(!store.config().show_hidden_files);
    std::fs::write(path.as_str(), r#"{"version":1,"appearance":"system"}"#).unwrap();
    let mut store = ConfigStore::open(path.clone()).unwrap();
    assert!(!store.config().show_hidden_files);
    store.set_show_hidden_files(true).unwrap();
    let restored = ConfigStore::open(path.clone()).unwrap();
    assert!(restored.config().show_hidden_files);
    cleanup(&path);
}
```

- [ ] **Step 2：实现 config 字段**

```rust
pub struct AppConfig {
    pub version: u32,
    pub appearance: AppearancePreference,
    pub confirm_delete: bool,
    pub show_hidden_files: bool,
}
// Default: show_hidden_files: false
// set_show_hidden_files like set_confirm_delete
```

- [ ] **Step 3：执行 storage 测试**

```bash
cargo test -p macsftp-storage show_hidden -- --nocapture
```

预期结果：PASS。

- [ ] **Step 4：实现 visible list helper 和 workspace toggle**

```rust
// panes.rs or visible.rs
pub(crate) fn is_dotfile(name: &str) -> bool {
    name.starts_with('.')
}

pub(crate) fn visible_local_indices(entries: &[LocalEntry], show_hidden: bool, query: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| show_hidden || !is_dotfile(&e.name))
        .filter(|(_, e)| query.is_empty() || e.name.to_lowercase().contains(&query.to_lowercase()))
        .map(|(i, _)| i)
        .collect()
}
// same for RemoteEntry
```

Task 2 始终传入 `query: ""`，因为 filter 属于 Task 5。但是，仍需实现包含两个过滤条件的函数签名，因此 Task 5 只需要提供 query。

注册 `ToggleHiddenFiles`：

```rust
pub(crate) fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
    let next = !cx.resources().config.config().show_hidden_files;
    match cx.resources_mut().config.set_show_hidden_files(next) {
        Ok(()) => self.config_error = None,
        Err(e) => {
            warn!(error = %e, "could not save show_hidden_files");
            self.config_error = Some("Could not write config.json…".into());
        }
    }
    cx.notify();
}
```

在 `mod.rs` 和 `app_actions.rs` 中注册 action。

- [ ] **Step 5：让 render 使用 visible indices**

file list 中的 `uniform_list`、`entry_count`、`entry_path_at`、`selected_index`、`move_selection` 和 `open_entry_at` 必须基于**可见**索引空间执行。

关键要求：`entry_count(side)` 必须返回可见条目数，而 `entry_path_at` 必须将可见索引映射到实际条目。

在 path bar 中增加 toggle（文本或图标），tooltip 为 `Show Hidden Files (⌘⇧.)`，并且显示当前选中状态。

- [ ] **Step 6：编写 Workspace 测试**

```rust
#[gpui::test]
fn hidden_files_filtered_by_default(cx: &mut TestAppContext) {
    // create dir with ".secret" and "visible.txt"
    // set_local_path
    // assert entry_count Local == 1 (only visible)
    // toggle_hidden_files
    // assert entry_count == 2
}
```

- [ ] **Step 7：执行完整 app 测试并提交**

```bash
cargo test -p macsftp-app --bin macsftp
git add crates/storage/src/config.rs crates/app/src/app_actions.rs \
  crates/app/src/workspace/*.rs
git commit -m "feat(app): hide dotfiles by default with config toggle"
```

---

### Task 3：`navigate_pane`、NavHistory 和 back/forward

**Files:**
- Create: `crates/app/src/workspace/nav.rs`
- Modify: `crates/app/src/workspace/mod.rs` (`mod nav;`, `tab_nav: HashMap<TabId, TabNavState>`)
- Modify: `crates/app/src/workspace/panes.rs`
- Modify: `crates/app/src/workspace/render.rs` (◀ ▶ buttons)
- Modify: `crates/app/src/app_actions.rs`
- Test: unit tests in `nav.rs` + gpui test

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOp {
    Push,
    Replace,
    Back,
    Forward,
}

#[derive(Debug, Clone, Default)]
pub struct PaneNavHistory {
    pub back: Vec<String>,
    pub forward: Vec<String>,
}

impl PaneNavHistory {
    pub const MAX: usize = 50;
    pub fn push_navigating_from(&mut self, from: Option<&str>, to: &str) { /* ... */ }
    pub fn go_back(&mut self, current: &str) -> Option<String> { /* ... */ }
    pub fn go_forward(&mut self, current: &str) -> Option<String> { /* ... */ }
    pub fn can_back(&self) -> bool { !self.back.is_empty() }
    pub fn can_forward(&self) -> bool { !self.forward.is_empty() }
}

#[derive(Debug, Clone, Default)]
pub struct TabNavState {
    pub local: PaneNavHistory,
    pub remote: PaneNavHistory,
}
```

`push_navigating_from`：如果 `from` 是 `Some(f)` 且 `f != to`，则将 `f` 加入 back，清空 forward，并将长度限制为 MAX。

`go_back`：将 `current` 加入 forward，然后从 back 中移除并返回最后一项。

如果路径相同或 from 为空，则不修改 history。

- [ ] **Step 1：在 `nav.rs` 中编写 unit tests（TDD）**

```rust
#[test]
fn push_clears_forward() {
    let mut h = PaneNavHistory::default();
    h.push_navigating_from(Some("/a"), "/b");
    h.forward.push("/x".into()); // simulate
    h.push_navigating_from(Some("/b"), "/c");
    assert!(h.forward.is_empty());
    assert_eq!(h.back, vec!["/a".to_string(), "/b".to_string()]);
}

#[test]
fn back_and_forward_round_trip() {
    let mut h = PaneNavHistory::default();
    h.push_navigating_from(Some("/a"), "/b");
    h.push_navigating_from(Some("/b"), "/c");
    let to = h.go_back("/c").unwrap();
    assert_eq!(to, "/b");
    let to = h.go_forward("/b").unwrap();
    assert_eq!(to, "/c");
}

#[test]
fn push_same_path_is_noop() {
    let mut h = PaneNavHistory::default();
    h.push_navigating_from(Some("/a"), "/a");
    assert!(h.back.is_empty());
}
```

- [ ] **Step 2：实现 `nav.rs` 和 `mod nav`**

- [ ] **Step 3：实现 `navigate_pane`**

```rust
pub(crate) fn navigate_pane_local(
    &mut self,
    path: LocalPath,
    op: HistoryOp,
    window: &mut Window,
    cx: &mut Context<Self>,
) {
    let tab_id = match self.active_tab() {
        Some(t) => t.id,
        None => return,
    };
    let current = self.active_tab().and_then(|t| t.local.path.clone());
    let nav = self.tab_nav.entry(tab_id).or_default();
    match op {
        HistoryOp::Push => {
            nav.local.push_navigating_from(
                current.as_ref().map(|p| p.as_str()),
                path.as_str(),
            );
        }
        HistoryOp::Replace => {}
        HistoryOp::Back => {
            let Some(cur) = current.as_ref() else { return };
            let Some(target) = nav.local.go_back(cur.as_str()) else { return };
            self.set_local_path(LocalPath::new(target), window, cx);
            self.clear_filter(PaneSide::Local);
            return;
        }
        HistoryOp::Forward => { /* symmetric */ }
    }
    self.set_local_path(path, window, cx);
    self.clear_filter(PaneSide::Local);
}

// navigate_pane_remote similar → request_remote_directory
```

调整现有调用：
- `open_entry_at` directory → `navigate_* (Push)`
- `go_to_parent_directory` → `navigate_* (Push)` with parent
- `refresh_focused_pane` → **keep** `set_local_path` / `request_remote_directory` **without** Push (same path)

On `close_tab_by_id`: `self.tab_nav.remove(&tab_id)`.

- [ ] **Step 4：实现 UI 和 actions**

```rust
// app_actions
NavigateBack, NavigateForward,
// keys: cmd-[ , cmd-] on Workspace
```

Path bar 提供 back/forward icon buttons；当 `!can_back/forward` 时禁用相应按钮。可以使用文本 `◀`/`▶`，如果已有合适图标则使用现有图标。

- [ ] **Step 5：编写 integration test**

```rust
#[gpui::test]
fn navigate_back_restores_previous_local_path(cx: &mut TestAppContext) {
    // fixture with parent/child dirs
    // navigate_pane_local(child, Push)
    // navigate_pane_local(parent via Back)
    // assert local.path == parent
}
```

- [ ] **Step 6: Commit**

```bash
git commit -m "feat(app): per-pane navigation history with back and forward"
```

---

### Task 4：Breadcrumbs 和 Go to Path

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (breadcrumb segments)
- Modify: `crates/app/src/workspace/modals.rs` or panes (Go to Path state + modal)
- Modify: `crates/app/src/app_actions.rs` (`GoToPath`)
- Modify: `crates/app/src/workspace/mod.rs`
- 可选：在 `nav.rs` 或 `helpers.rs` 中实现纯函数 `split_path_segments(path: &str) -> Vec<(label, absolute)>`，并增加 unit tests

**Interfaces:**
- `fn breadcrumb_segments(path: &str) -> Vec<(String /*label*/, String /*absolute*/)>`
- `go_to_path_open: bool` + `go_to_path_input: InputState` on Workspace
- `submit_go_to_path` → `navigate_* (Push)` after validation

- [ ] **Step 1：编写 segments 的 unit tests**

```rust
#[test]
fn breadcrumb_segments_root_and_nested() {
    assert_eq!(
        breadcrumb_segments("/"),
        vec![("/".into(), "/".into())]
    );
    let segs = breadcrumb_segments("/Users/alex/Projects");
    assert_eq!(segs.last().unwrap().1, "/Users/alex/Projects");
    assert_eq!(segs[0].0, "/");
}
```

Implementation sketch:

```rust
pub fn breadcrumb_segments(path: &str) -> Vec<(String, String)> {
    if path.is_empty() || path == "/" {
        return vec![("/".into(), "/".into())];
    }
    let mut out = vec![("/".into(), "/".into())];
    let mut acc = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        acc.push('/');
        acc.push_str(part);
        out.push((part.to_string(), acc.clone()));
    }
    out
}
```

- [ ] **Step 2：渲染 breadcrumbs**

使用水平 segment buttons 替换单个截断的 path label，并且在单击后以 Push 方式导航。MVP 的折叠规则为：如果 `segments.len() > 5`，则显示第一个 segment、不可单击的 `…` 和最后两个 segment。

- [ ] **Step 3：实现 Go to Path modal**

```rust
// open
self.go_to_path_open = true;
self.go_to_path_input.clear();
window.focus(&self.modal_focus);

// submit
let raw = self.go_to_path_input.value().trim();
if raw.is_empty() { error; return; }
match self.focused_side {
  Local => {
    let path = LocalPath::new(expand_home(raw));
    if !std::path::Path::new(path.as_str()).exists() {
      self.status_message = Some("Path not found".into());
      return;
    }
    self.navigate_pane_local(path, HistoryOp::Push, window, cx);
  }
  Remote => {
    self.navigate_pane_remote(RemotePath::new(raw), HistoryOp::Push, window, cx);
  }
}
self.go_to_path_open = false;
```

`cancel_active_modal` 优先关闭 go_to_path。

快捷键：`cmd-shift-g` → `GoToPath`。

Escape 绑定：如果使用独立 context，则增加 `GoToPath` key context。

- [ ] **Step 4：执行测试并提交**

```bash
cargo test -p macsftp-app --bin macsftp breadcrumb go_to_path
git commit -m "feat(app): path breadcrumbs and Go to Path"
```

---

### Task 5：type-to-filter 和 `cmd-f`

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` (filter map fields)
- Modify: `crates/app/src/workspace/panes.rs` (key handling, clear_filter on navigate — already stubbed in Task 3)
- Modify: `crates/app/src/workspace/render.rs` (filter bar UI)
- Modify: `crates/app/src/app_actions.rs` (`FilterPane` = `cmd-f`)
- Test: workspace tests

**Interfaces:**

```rust
#[derive(Debug, Clone, Default)]
pub struct PaneFilter {
    pub query: String,
    pub input: InputState,
    pub explicit_focus: bool,
}
// Workspace: pane_filters: HashMap<(TabId, PaneSide), PaneFilter>
// or active-tab only: local_filter + remote_filter fields (simpler for MVP)
```

MVP 建议：在 Workspace 上设置 `local_filter` 和 `remote_filter` 两个字段，并在 tab 切换时通过 `activate_tab` 或 `open_new_tab` 清空。无需使用完整 HashMap，而且该方案仍符合设计意图。

- [ ] **Step 1：编写纯 match helper 测试**

```rust
#[test]
fn filter_query_case_insensitive_substring() {
    assert!(name_matches("ReadMe.TXT", "me.t"));
    assert!(!name_matches("ReadMe.TXT", "xyz"));
}

fn name_matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}
```

- [ ] **Step 2：将 query 应用于 visible indices**（Task 2 helper 已经接收 query）

- [ ] **Step 3：处理 FilePane 键盘事件**

在 `render_pane` 的 `.on_key_down` 中处理以下逻辑，并且与 inline_edit handler 组合：

```rust
// if go_to_path / delete_confirm / connect / inline_edit: return
// if FilterPane explicit_focus: route to input
// else if printable char and no modifiers (except shift): append to query
// Backspace: pop char
// Escape: clear filter (also via CancelActiveModal if preferred)
```

`cmd-f`：设置 `explicit_focus = true`，显示 filter bar，并将焦点置于 pane。

当 `!query.is_empty() || explicit_focus` 时，显示以下 filter bar UI：

```text
Filter: {query} · {matched}/{total_after_hidden}
```

- [ ] **Step 4：在导航或 tab 切换时清空 filter**

`navigate_pane_*` 已经调用清空逻辑。此外，`activate_tab` 必须清空两个 filter。

- [ ] **Step 5：编写测试**

```rust
#[gpui::test]
fn type_to_filter_reduces_visible_local_entries(cx: &mut TestAppContext) {
    // fixtures a.txt b.txt
    // set filter query "a"
    // assert entry_count == 1
    // clear
    // assert entry_count == 2
}
```

- [ ] **Step 6：提交变更**

```bash
git commit -m "feat(app): type-to-filter and cmd-f for file panes"
```

---

### Task 6：最终验证

- [ ] **Step 1：执行自动化验证**

```bash
cargo test -p macsftp-storage --lib
cargo test -p macsftp-app --bin macsftp
rg "Default::default\(\)" crates/app/src/workspace/panes.rs  # load_local_directory must use tab.sort
```

- [ ] **Step 2：执行手动 smoke checklist**（在 PR 或报告中记录）

1. 在包含万级条目的 local dir 中，type-to-filter 响应迅速
2. 在 3 个目录之间执行 Back/forward；刷新不会错误修改 stack
3. 使用 Breadcrumb 导航到祖先目录
4. 分别在 local 和 remote pane 中验证 `cmd-shift-g`
5. Hidden 默认关闭；`cmd-shift-.` 可以显示 `.git`
6. 单击 Size header 后，directories 仍位于最前

- [ ] **Step 3：检查 Spec 覆盖情况**

将 design §2–§5 的每项成功标准映射到测试或手动检查项。如果存在缺失，则增加相应验证。

---

## Self-Review (plan vs spec)

| Spec | Task |
| --- | --- |
| §2 type-to-filter + cmd-f | Task 5 |
| §3 navigate + history + keys | Task 3 |
| §3 breadcrumb + Go to Path | Task 4 |
| §4 hidden files config | Task 2 |
| §5 column sort + local sort fix | Task 1 |
| Visible list order: hidden then filter | Task 2 + 5 |
| Refresh no push | Task 3 |
| Tests | Each task + Task 6 |

**Placeholder scan:** none intentional.  
**Type consistency:** `HistoryOp`, `PaneNavHistory`, `navigate_pane_local/remote`, `show_hidden_files`, `apply_sort_field`, `PaneFilter` used uniformly.

---

## Execution Handoff

计划已完成，并且保存在 `docs/plans/2026-07-14-phase3-search-navigation-impl.md`。

**执行方式：**

1. **Subagent-Driven（推荐）**：每个任务使用新的 subagent，并且在任务之间进行 review
2. **Inline Execution**：在当前 session 中执行，并设置 checkpoints

请选择执行方式。
