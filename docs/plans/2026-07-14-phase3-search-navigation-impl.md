# Phase 3 Search & Directory Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add type-to-filter, per-pane back/forward history, clickable breadcrumbs, Go to Path, hidden-files toggle, and clickable column sort so large directories are usable.

**Architecture:** View-layer filter + nav history on `Workspace`; all path changes go through `navigate_pane(..., HistoryOp)`. Hidden files and filter derive a **visible** list without mutating stored entries. Sort uses existing `tab.sort` + `sort_entries`; header becomes interactive.

**Tech Stack:** Rust, GPUI, `macsftp_core::{FileSort, FileSortField, sort_entries, LocalPath, RemotePath}`, `macsftp_storage::AppConfig`, existing `InputState` / `uniform_list`.

**Spec:** `docs/plans/2026-07-14-phase3-search-navigation-design.md`

## Global Constraints

- Filter only already-loaded entries (no remote recursive search).
- Case-insensitive **substring** match on basename only (no regex).
- Hidden = `name.starts_with('.')`; default `show_hidden_files = false` in config.
- Nav history is **session-only** (max 50 per stack); not persisted.
- Sort is session-only on `tab.sort`; do not write sort to config.
- `cmd-[` / `cmd-]` = NavigateBack/Forward; tab switch stays `cmd-shift-[` / `]`.
- Refresh same path must **not** push history.
- No `unwrap`/`expect` on recoverable paths (AGENTS.md §5).
- Prefer `src/foo.rs` over `mod.rs`; match existing workspace style.
- Do not change SFTP listing protocol or `AppCommand` read shapes.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/workspace/nav.rs` | `HistoryOp`, `PaneNavHistory`, pure history ops + unit tests |
| **Create** `crates/app/src/workspace/visible_entries.rs` (or helpers in panes) | `apply_hidden_and_filter` pure helpers + tests |
| **Modify** `crates/storage/src/config.rs` | `show_hidden_files` + setter + tests |
| **Modify** `crates/ui/src/file_list.rs` + `ui.rs` | Clickable `file_table_header` |
| **Modify** `crates/app/src/app_actions.rs` | New actions + keybindings |
| **Modify** `crates/app/src/workspace/mod.rs` | Fields: nav map, filter map, go_to_path state; wire actions |
| **Modify** `crates/app/src/workspace/panes.rs` | `navigate_pane`, sort apply, visible list helpers, open_entry/up use navigate |
| **Modify** `crates/app/src/workspace/render.rs` | Path bar back/forward, breadcrumb, filter bar, sort clicks, hidden toggle |
| **Modify** `crates/app/src/workspace/modals.rs` | Go to Path UI + cancel_active_modal |
| **Modify** `crates/app/src/workspace/tests.rs` | App-level tests |
| **Do not modify** | `session_actor` listing, core `AppCommand` read enums |

---

### Task 1: Clickable column sort + local `tab.sort` fix

**Files:**
- Modify: `crates/ui/src/file_list.rs`
- Modify: `crates/ui/src/ui.rs` (re-export if signature changes)
- Modify: `crates/app/src/workspace/panes.rs` (`load_local_directory`)
- Modify: `crates/app/src/workspace/render.rs` (header callback + `cycle_sort`)
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:
  - `file_table_header(sort, cx, on_click: impl Fn(FileSortField, &mut Window, &mut App))` **or** keep pure header and attach clicks in app by rebuilding header columns in render — prefer extending `file_table_header` with optional click handlers.
  - `Workspace::apply_sort_field(&mut self, field: FileSortField, cx)`
  - `load_local_directory` sorts with `&tab.sort`

- [ ] **Step 1: Failing test — local load uses tab sort**

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

- [ ] **Step 2: Run test — expect FAIL**

```bash
cargo test -p macsftp-app --bin macsftp local_directory_respects_tab_sort -- --nocapture
```

Expected: FAIL (both files sorted by name default → alphabetical `big` before `small`).

- [ ] **Step 3: Fix `load_local_directory`**

```rust
// panes.rs
macsftp_core::sort_entries(&mut entries, &tab.sort);
```

instead of `&Default::default()`.

- [ ] **Step 4: Make header clickable**

Change `file_table_header` to accept clicks. Practical approach:

```rust
pub fn file_table_header(
    sort: &FileSort,
    cx: &App,
    mut on_field: impl FnMut(FileSortField, &mut Window, &mut App) + 'static,
) -> impl IntoElement
```

Wrap each column label `div` with `.id(...).on_click` calling `on_field(FileSortField::Name|Size|ModifiedAt, ...)`.

Update all call sites (`render.rs` and any test/helpers) — typically only `render.rs`:

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

- [ ] **Step 5: Test apply_sort_field toggle**

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

Note: `apply_sort_field` takes `cx: &mut Context` only if no window needed — match existing methods (`cx.notify()` only).

- [ ] **Step 6: Run tests**

```bash
cargo test -p macsftp-app --bin macsftp local_directory_respects_tab_sort apply_sort_field -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/ui/src/file_list.rs crates/ui/src/ui.rs \
  crates/app/src/workspace/panes.rs crates/app/src/workspace/render.rs \
  crates/app/src/workspace/tests.rs
git commit -m "feat(app): clickable column sort and fix local tab.sort"
```

---

### Task 2: `show_hidden_files` config + visible-list filtering

**Files:**
- Modify: `crates/storage/src/config.rs`
- Modify: `crates/app/src/app_actions.rs`
- Modify: `crates/app/src/workspace/mod.rs` (action handler)
- Modify: `crates/app/src/workspace/panes.rs` or new helper module for `visible_entries`
- Modify: `crates/app/src/workspace/render.rs` (use visible list; toggle button)
- Test: config unit test + workspace test

**Interfaces:**
- Produces:
  - `AppConfig.show_hidden_files: bool` default **false**
  - `ConfigStore::set_show_hidden_files(bool) -> Result<(), ConfigError>`
  - `fn entry_is_hidden(name: &str) -> bool { name.starts_with('.') }`
  - `fn filter_hidden<E: HasName>(entries: &[E], show_hidden: bool) -> impl Iterator`
  - Action `ToggleHiddenFiles` + `cmd-shift-.`

- [ ] **Step 1: Config test (TDD)**

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

- [ ] **Step 2: Implement config field**

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

- [ ] **Step 3: Run storage tests**

```bash
cargo test -p macsftp-storage show_hidden -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Visible list helper + workspace toggle**

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

For Task 2 only, pass `query: ""` always (filter is Task 5). Still implement the dual filter signature so Task 5 only fills query.

Wire `ToggleHiddenFiles`:

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

Register action in `mod.rs` + `app_actions.rs`.

- [ ] **Step 5: Render uses visible indices**

In file list `uniform_list` / `entry_count` / `entry_path_at` / `selected_index` / `move_selection` / `open_entry_at`: operate on **visible** index space.

Critical: `entry_count(side)` must return visible count; `entry_path_at` maps visible index → real entry.

Path bar: add toggle (text or icon) with tooltip `Show Hidden Files (⌘⇧.)` reflecting checked state.

- [ ] **Step 6: Workspace test**

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

- [ ] **Step 7: Full app tests + commit**

```bash
cargo test -p macsftp-app --bin macsftp
git add crates/storage/src/config.rs crates/app/src/app_actions.rs \
  crates/app/src/workspace/*.rs
git commit -m "feat(app): hide dotfiles by default with config toggle"
```

---

### Task 3: `navigate_pane` + NavHistory + back/forward

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

`push_navigating_from`: if `from` is `Some(f)` and `f != to`, push `f` to back, clear forward, trim MAX.  
`go_back`: push `current` to forward, pop back.  
Same path / empty from: no-op push.

- [ ] **Step 1: Unit tests in `nav.rs` (TDD)**

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

- [ ] **Step 2: Implement `nav.rs` + `mod nav`**

- [ ] **Step 3: `navigate_pane`**

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

Refactor:
- `open_entry_at` directory → `navigate_* (Push)`
- `go_to_parent_directory` → `navigate_* (Push)` with parent
- `refresh_focused_pane` → **keep** `set_local_path` / `request_remote_directory` **without** Push (same path)

On `close_tab_by_id`: `self.tab_nav.remove(&tab_id)`.

- [ ] **Step 4: UI + actions**

```rust
// app_actions
NavigateBack, NavigateForward,
// keys: cmd-[ , cmd-] on Workspace
```

Path bar: back/forward icon buttons (disabled when `!can_back/forward`). Use text `◀`/`▶` or existing icons if any.

- [ ] **Step 5: Integration test**

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

### Task 4: Breadcrumbs + Go to Path

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (breadcrumb segments)
- Modify: `crates/app/src/workspace/modals.rs` or panes (Go to Path state + modal)
- Modify: `crates/app/src/app_actions.rs` (`GoToPath`)
- Modify: `crates/app/src/workspace/mod.rs`
- Optional: pure `split_path_segments(path: &str) -> Vec<(label, absolute)>` in `nav.rs` or `helpers.rs` with unit tests

**Interfaces:**
- `fn breadcrumb_segments(path: &str) -> Vec<(String /*label*/, String /*absolute*/)>`
- `go_to_path_open: bool` + `go_to_path_input: InputState` on Workspace
- `submit_go_to_path` → `navigate_* (Push)` after validation

- [ ] **Step 1: Unit tests for segments**

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

- [ ] **Step 2: Render breadcrumbs**

Replace single truncated path label with horizontal segment buttons (click → navigate Push). MVP collapse: if `segments.len() > 5`, show first + `…` (not clickable) + last two.

- [ ] **Step 3: Go to Path modal**

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

`cancel_active_modal` closes go_to_path first.  
Key: `cmd-shift-g` → `GoToPath`.  
Escape binding: add `GoToPath` key context if separate.

- [ ] **Step 4: Tests + commit**

```bash
cargo test -p macsftp-app --bin macsftp breadcrumb go_to_path
git commit -m "feat(app): path breadcrumbs and Go to Path"
```

---

### Task 5: type-to-filter + `cmd-f`

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

MVP recommendation: **two fields on Workspace** `local_filter` / `remote_filter` cleared on tab switch (`activate_tab` / `open_new_tab`), instead of full HashMap — still matches design intent.

- [ ] **Step 1: Pure match helper tests**

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

- [ ] **Step 2: Wire visible indices with query** (Task 2 helper already takes query)

- [ ] **Step 3: Key handling on FilePane**

In `render_pane` `.on_key_down` (compose with inline_edit handler):

```rust
// if go_to_path / delete_confirm / connect / inline_edit: return
// if FilterPane explicit_focus: route to input
// else if printable char and no modifiers (except shift): append to query
// Backspace: pop char
// Escape: clear filter (also via CancelActiveModal if preferred)
```

`cmd-f`: set `explicit_focus = true`, show bar, focus pane.

Filter bar UI when `!query.is_empty() || explicit_focus`:

```text
Filter: {query} · {matched}/{total_after_hidden}
```

- [ ] **Step 4: Clear filter on navigate / tab change**

Already called from `navigate_pane_*`. On `activate_tab`, clear both filters.

- [ ] **Step 5: Tests**

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

- [ ] **Step 6: Commit**

```bash
git commit -m "feat(app): type-to-filter and cmd-f for file panes"
```

---

### Task 6: Final verification

- [ ] **Step 1: Automated**

```bash
cargo test -p macsftp-storage --lib
cargo test -p macsftp-app --bin macsftp
rg "Default::default\(\)" crates/app/src/workspace/panes.rs  # load_local_directory must use tab.sort
```

- [ ] **Step 2: Manual smoke checklist** (document in PR/report)

1. Local 万级 dir: type-to-filter snappy  
2. Back/forward across 3 folders; refresh does not break stack wrongly  
3. Breadcrumb jump to ancestor  
4. `cmd-shift-g` local + remote  
5. Hidden off by default; `cmd-shift-.` shows `.git`  
6. Click Size header; directories stay first  

- [ ] **Step 3: Spec coverage self-check**

Map each design §2–§5 success criterion to a test or manual item; fix gaps.

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

Plan complete and saved to `docs/plans/2026-07-14-phase3-search-navigation-impl.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session with checkpoints  

Which approach?
