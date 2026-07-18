# Phase 4 Keyboard & Command Palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship command palette (`cmd-shift-p`), complete file-list keyboard multi-select/paging, MRU tab switcher (`ctrl-tab`), and shortcut discoverability (palette keys + tooltips).

**Architecture:** Explicit `PaletteCommand` registry dispatches stable GPUI actions. List selection keeps path-based `selected_paths` plus a view-side anchor for shift-range. Workspace maintains `tab_mru: Vec<TabId>`; creation-order tab keys stay on `cmd-shift-[/]`; MRU only drives the ctrl-tab switcher UI.

**Tech Stack:** Rust, GPUI (`actions!`, `KeyBinding`, `InputState`, modal-style overlays), existing `Workspace` / `PaneSide` / visible indices from phase 3.

**Spec:** `docs/plans/2026-07-14-phase4-keyboard-palette-design.md`

## Global Constraints

- Explicit palette registry only — do **not** reflect all `actions!` symbols.
- Palette titles: user-facing verb phrases; never runtime/channel/actor jargon.
- Selection remains path-based (no row index as long-term selection id).
- `PAGE_SIZE = 10` for page up/down on **visible** list indices.
- `cmd-shift-[` / `]` stay **creation order**; MRU only for `ctrl-tab` switcher.
- No custom keybinding editor; no SFTP/core protocol changes.
- No `unwrap` on recoverable paths (AGENTS.md §5).
- Match existing workspace style; prefer `src/foo.rs` over `mod.rs`.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/palette_commands.rs` | `PaletteCommand`, `PaletteWhen`, static registry, filter helper + unit tests |
| **Create** `crates/app/src/workspace/command_palette.rs` | open/close/filter/execute UI helpers on Workspace |
| **Modify** `crates/app/src/app_actions.rs` | new actions + keybindings |
| **Modify** `crates/app/src/main.rs` | `mod palette_commands` |
| **Modify** `crates/app/src/workspace/mod.rs` | palette / mru / switcher / anchor state; action wiring |
| **Modify** `crates/app/src/workspace/panes.rs` | selection extend, page/home/end, select all, anchor updates |
| **Modify** `crates/app/src/workspace/modals.rs` | `cancel_active_modal` palette first |
| **Modify** `crates/app/src/workspace/render.rs` | palette overlay, tab switcher overlay, tooltip key text |
| **Modify** `crates/app/src/workspace/tests.rs` | gpui tests |
| **Do not modify** | `crates/sftp`, core transfer/session protocols |

---

### Task 1: List keyboard — page / home / end / shift-range / cmd-a

**Files:**
- Modify: `crates/app/src/app_actions.rs`
- Modify: `crates/app/src/workspace/mod.rs` (fields + on_action)
- Modify: `crates/app/src/workspace/panes.rs`
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:
  - `Workspace.selection_anchor: Option<EntryPath>` (or `(PaneSide, EntryPath)`)
  - `pub const PAGE_SIZE: usize = 10;`
  - `select_index` sets single selection **and** updates anchor
  - `extend_selection_to(side, visible_index, cx)`
  - `select_all_visible(side, cx)`
  - Actions: `SelectNextEntryExtend`, `SelectPrevEntryExtend`, `PageDown`, `PageUp`, `SelectFirstEntry`, `SelectLastEntry`, `SelectAllEntries`

- [ ] **Step 1: Failing tests**

```rust
#[gpui::test]
fn page_down_moves_by_ten_on_visible_list(cx: &mut TestAppContext) {
    // fixture with 15 plain files (no dots), show_hidden whatever
    // select index 0, page_down, selected_index == 10
}

#[gpui::test]
fn shift_down_extends_selection_range(cx: &mut TestAppContext) {
    // 5 files, select index 1, extend to 3 → selected_paths len == 3
}

#[gpui::test]
fn select_all_selects_all_visible(cx: &mut TestAppContext) {
    // 3 visible + 1 hidden dotfile, cmd-a → 3 paths
}
```

- [ ] **Step 2: Run — expect FAIL / missing methods**

```bash
cargo test -p macsftp-app --bin macsftp page_down_moves shift_down_extends select_all -- --nocapture
```

- [ ] **Step 3: Implement selection helpers**

```rust
// panes.rs
pub const PAGE_SIZE: usize = 10;

// In select_index after setting selected_paths:
self.selection_anchor = Some(path.clone());

pub(crate) fn extend_selection_to(
    &mut self,
    side: PaneSide,
    visible_index: usize,
    cx: &mut Context<Self>,
) {
    let visible = self.visible_indices(side, cx);
    if visible.is_empty() { return; }
    let end = visible_index.min(visible.len() - 1);
    let anchor_path = self.selection_anchor.clone().or_else(|| {
        self.entry_path_at(side, self.selected_index(side, cx).unwrap_or(0), cx)
    });
    let Some(anchor_path) = anchor_path else { return };
    // resolve anchor to visible index
    let start = self.visible_index_of_path(side, &anchor_path, cx).unwrap_or(end);
    let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
    let mut paths = Vec::new();
    for vi in lo..=hi {
        if let Some(p) = self.entry_path_at(side, vi, cx) {
            paths.push(p);
        }
    }
    if let Some(tab) = self.active_tab_mut() {
        tab.selection.selected_paths = paths;
    }
    self.scroll_handle(side).scroll_to_item(end, ScrollStrategy::Top);
    cx.notify();
}

pub(crate) fn move_selection_extend(&mut self, side: PaneSide, offset: isize, cx: &mut Context<Self>) {
    let n = self.entry_count(side, cx);
    if n == 0 { return; }
    let current = self.selected_index(side, cx).unwrap_or(0);
    // For multi-select, "current" = edge of range away from anchor — MVP: use last selected visible index
    let edge = self.selection_edge_visible_index(side, cx).unwrap_or(current);
    let next = (edge as isize + offset).clamp(0, n as isize - 1) as usize;
    self.extend_selection_to(side, next, cx);
}

pub(crate) fn select_all_visible(&mut self, side: PaneSide, cx: &mut Context<Self>) {
    let n = self.entry_count(side, cx);
    let mut paths = Vec::new();
    for i in 0..n {
        if let Some(p) = self.entry_path_at(side, i, cx) {
            paths.push(p);
        }
    }
    if let Some(first) = paths.first() {
        self.selection_anchor = Some(first.clone());
    }
    if let Some(tab) = self.active_tab_mut() {
        tab.selection.selected_paths = paths;
    }
    cx.notify();
}
```

Wire `move_selection` to reset anchor via `select_index`.

Page/home/end:

```rust
pub(crate) fn page_selection(&mut self, side: PaneSide, direction: isize, cx: &mut Context<Self>) {
    let n = self.entry_count(side, cx);
    if n == 0 { return; }
    let cur = self.selected_index(side, cx).unwrap_or(0);
    let next = (cur as isize + direction * PAGE_SIZE as isize).clamp(0, n as isize - 1) as usize;
    self.select_index(side, next, cx);
}
```

- [ ] **Step 4: Actions + keybindings**

```rust
// app_actions.rs — add to actions! and bind_keys:
SelectNextEntryExtend, SelectPrevEntryExtend,
PageDown, PageUp, SelectFirstEntry, SelectLastEntry, SelectAllEntries,

KeyBinding::new("shift-down", SelectNextEntryExtend, Some("FilePane")),
KeyBinding::new("shift-up", SelectPrevEntryExtend, Some("FilePane")),
KeyBinding::new("pagedown", PageDown, Some("FilePane")),
KeyBinding::new("pageup", PageUp, Some("FilePane")),
KeyBinding::new("home", SelectFirstEntry, Some("FilePane")),
KeyBinding::new("end", SelectLastEntry, Some("FilePane")),
KeyBinding::new("cmd-a", SelectAllEntries, Some("FilePane")),
```

Wire in `mod.rs` `on_action` handlers calling the pane methods for `focused_side`.

- [ ] **Step 5: Tests PASS + commit**

```bash
cargo test -p macsftp-app --bin macsftp page_down shift_down select_all
git commit -m "feat(app): file list page/home/end, shift-range and select-all"
```

---

### Task 2: Palette command registry (pure)

**Files:**
- Create: `crates/app/src/palette_commands.rs`
- Modify: `crates/app/src/main.rs` (`mod palette_commands;`)

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteWhen {
    Always,
    HasTabs,
    HasActiveTab,
    ConnectedRemote,
}

#[derive(Debug, Clone, Copy)]
pub struct PaletteCommand {
    pub id: &'static str,
    pub title: &'static str,
    pub keywords: &'static [&'static str],
    pub keybinding: Option<&'static str>,
    pub when: PaletteWhen,
}

pub fn all_palette_commands() -> &'static [PaletteCommand];

pub fn filter_palette_commands(
    query: &str,
    ctx: &PaletteContext,
) -> Vec<&'static PaletteCommand>;

pub struct PaletteContext {
    pub has_tabs: bool,
    pub has_active_tab: bool,
    pub remote_connected: bool,
}
```

- [ ] **Step 1: Unit tests**

```rust
#[test]
fn filter_matches_title_case_insensitive() {
    let ctx = PaletteContext { has_tabs: true, has_active_tab: true, remote_connected: false };
    let hits = filter_palette_commands("new ta", &ctx);
    assert!(hits.iter().any(|c| c.id == "NewTab"));
}

#[test]
fn filter_hides_when_predicate_fails() {
    let ctx = PaletteContext { has_tabs: false, has_active_tab: false, remote_connected: false };
    let hits = filter_palette_commands("download", &ctx);
    assert!(!hits.iter().any(|c| c.id == "DownloadSelection"));
}
```

- [ ] **Step 2: Implement registry** — include at least:

| id | title | key | when |
| --- | --- | --- | --- |
| NewTab | New Tab | ⌘T | Always |
| CloseTab | Close Tab | ⌘W | HasTabs |
| RefreshPane | Refresh | ⌘R | HasActiveTab |
| FocusLocalPane | Focus Local Pane | ⌘1 | HasActiveTab |
| FocusRemotePane | Focus Remote Pane | ⌘2 | HasActiveTab |
| UploadSelection | Upload Selection | ⌘U | HasActiveTab |
| DownloadSelection | Download Selection | ⌘D | ConnectedRemote |
| ShowTransferDrawer | Toggle Transfers | ⌘J | Always |
| OpenSettings | Open Settings | ⌘, | Always |
| ShowAbout | About macSFTP | — | Always |
| DeleteSelection | Delete Selection | ⌘⌫ | HasActiveTab |
| RenameEntry | Rename | F2 | HasActiveTab |
| NewFolder | New Folder | ⌘⇧N | HasActiveTab |
| FilterPane | Filter Pane | ⌘F | HasActiveTab |
| GoToPath | Go to Path | ⌘⇧G | HasActiveTab |
| NavigateBack | Back | ⌘[ | HasActiveTab |
| NavigateForward | Forward | ⌘] | HasActiveTab |
| ToggleHiddenFiles | Show Hidden Files | ⌘⇧. | Always |
| CopyPath | Copy Path | ⌘⇧C | HasActiveTab |
| ReconnectTab | Reconnect | ⌘⇧R | HasActiveTab |
| OpenLogFolder | Open Log Folder | — | Always |
| OpenCommandPalette | Command Palette | ⌘⇧P | Always |

Filtering: lowercase `query` empty → all matching `when`; else title/keywords substring.

- [ ] **Step 3: `cargo test -p macsftp-app --bin macsftp filter_matches filter_hides`**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(app): add explicit command palette registry"
```

---

### Task 3: Command palette UI + dispatch

**Files:**
- Create: `crates/app/src/workspace/command_palette.rs`
- Modify: `mod.rs` (state, actions, cancel order, render children)
- Modify: `modals.rs` (`cancel_active_modal` palette first)
- Modify: `app_actions.rs` bind `cmd-shift-p`
- Test: `tests.rs`

**Interfaces:**
- `Workspace { palette_open, palette_input, palette_selected: usize }`
- `open_command_palette`, `close_command_palette`, `execute_palette_selected`
- `palette_context(&self) -> PaletteContext`
- `dispatch_palette_id(&mut self, id: &str, window, cx)` match on id → existing methods / `cx.dispatch_action`

- [ ] **Step 1: Test open + filter + execute NewTab**

```rust
#[gpui::test]
fn command_palette_filters_and_runs_new_tab(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        assert_eq!(ws.state.tabs.tabs.len(), 1);
        ws.open_command_palette(window, cx);
        assert!(ws.palette_open);
        ws.palette_input.set_value("new tab");
        // rebuild selection to first hit
        ws.palette_selected = 0;
        ws.execute_palette_selected(window, cx);
        assert!(!ws.palette_open);
        assert_eq!(ws.state.tabs.tabs.len(), 2);
    });
}
```

- [ ] **Step 2: Implement UI** (pattern from `render_go_to_path_modal` / About)

- Scrim + card `key_context("CommandPalette")`
- Text field bound to `palette_input`
- List filtered commands: title left, keybinding right (muted)
- Highlight `palette_selected`
- Keys: up/down move selection; enter execute; escape → cancel_active_modal
- Click row → execute that command

`dispatch_palette_id` example:

```rust
match id {
    "NewTab" => self.open_new_tab(window, cx),
    "CloseTab" => { if let Some(id) = self.state.tabs.active_tab_id { self.close_tab_by_id(id, window, cx); } }
    "RefreshPane" => self.refresh_focused_pane(window, cx),
    "OpenSettings" => { /* same as OpenSettings action */ }
    // ...
    _ => {}
}
```

- [ ] **Step 3: cancel_active_modal**

```rust
if self.palette_open {
    self.close_command_palette(window, cx);
    return;
}
// existing...
```

- [ ] **Step 4: Bind `cmd-shift-p`** to `OpenCommandPalette`

- [ ] **Step 5: Tests + commit**

```bash
cargo test -p macsftp-app --bin macsftp command_palette
git commit -m "feat(app): command palette UI and cmd-shift-p"
```

---

### Task 4: Tab MRU + ctrl-tab switcher

**Files:**
- Modify: `mod.rs`, `panes`/`mod` activate_tab, `render.rs`, `app_actions.rs`
- Test: `tests.rs`

**Interfaces:**

```rust
// Workspace
tab_mru: Vec<TabId>, // front = most recent
tab_switcher_open: bool,
tab_switcher_index: usize,
```

- [ ] **Step 1: Tests**

```rust
#[gpui::test]
fn activate_tab_updates_mru_front(cx: &mut TestAppContext) {
    // open 3 tabs (ids 1,2,3), activate 2 then 3
    // assert tab_mru[0] == TabId(3), tab_mru contains 2 before 1
}

#[gpui::test]
fn cmd_shift_tab_still_creation_order(cx: &mut TestAppContext) {
    // 3 tabs, activate last via MRU-irrelevant path
    // ActivateNextTab from tab1 → tab2 (creation order)
}
```

- [ ] **Step 2: Maintain MRU**

```rust
fn touch_mru(&mut self, tab_id: TabId) {
    self.tab_mru.retain(|id| *id != tab_id);
    self.tab_mru.insert(0, tab_id);
}

// activate_tab / open_new_tab: touch_mru
// close_tab_by_id: retain remove
// Workspace::new after first tab: tab_mru = vec![first_id]
```

Keep `activate_tab_in_direction` on **creation order** (`tabs` vec) — do not use MRU.

- [ ] **Step 3: Tab switcher**

Actions: `TabSwitcherNext`, `TabSwitcherPrev` (or reuse with modifiers).

Bindings (GPUI key names — verify against gpui docs; common patterns):

```rust
KeyBinding::new("ctrl-tab", TabSwitcherNext, Some("Workspace")),
KeyBinding::new("ctrl-shift-tab", TabSwitcherPrev, Some("Workspace")),
```

Logic:

```rust
fn tab_switcher_next(&mut self, cx: &mut Context<Self>) {
    if self.state.tabs.tabs.is_empty() { return; }
    if !self.tab_switcher_open {
        self.tab_switcher_open = true;
        // start at second MRU entry if exists else 0
        self.tab_switcher_index = if self.tab_mru.len() > 1 { 1 } else { 0 };
    } else {
        let n = self.tab_mru.len().max(1);
        self.tab_switcher_index = (self.tab_switcher_index + 1) % n;
    }
    cx.notify();
}
```

**Confirm on Enter** (reliable without key-up):

```rust
// Enter while switcher open → activate tab_mru[index], close switcher
// Esc → close without change
```

Also attempt modifiers-changed / key-up if easy in GPUI; document Enter as primary confirm in tooltips.

UI: elevated list of MRU tabs (title + status color).

- [ ] **Step 4: cancel_active_modal** closes switcher before other modals (after palette).

- [ ] **Step 5: Tests + commit**

```bash
git commit -m "feat(app): MRU tab order and ctrl-tab switcher"
```

---

### Task 5: Tooltip discoverability + palette polish

**Files:**
- Modify: `render.rs` (path bar / toolbar tooltips)
- Modify: `command_palette` / palette row layout if needed
- Grep for `icon_button(` and ensure key chords in labels for Refresh, Parent, Transfers, Hidden, New Folder, Delete, Back, Forward

- [ ] **Step 1:** Audit and update strings to match registry (`⌘R`, `⌘↑`, `⌘J`, etc.)
- [ ] **Step 2:** Ensure palette rows show `keybinding` on the right (if not already in Task 3)
- [ ] **Step 3:** Smoke test tooltips don't break layout (narrow path bar)
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(app): show shortcut hints in tooltips and palette rows"
```

---

### Task 6: Final verification

- [ ] **Step 1: Automated**

```bash
cargo test -p macsftp-app --bin macsftp
```

Expected: all pass (including new phase 4 tests).

- [ ] **Step 2: Manual checklist**

1. `⌘⇧P` → type "refresh" → Enter refreshes  
2. File pane: page down, home/end, shift multi-select, `⌘A` then delete modal counts  
3. Three tabs: `ctrl-tab` cycles MRU; `⌘⇧]` still creation order  
4. Icon tooltips show keys  

- [ ] **Step 3: Spec coverage map** — each design success row has test or manual note

---

## Self-Review (plan vs spec)

| Spec | Task |
| --- | --- |
| §2 Command palette | Tasks 2–3 |
| §3 List keyboard | Task 1 |
| §4 MRU + ctrl-tab | Task 4 |
| §5 Discoverability | Task 5 |
| cmd-shift creation order | Task 4 (explicit non-goal for MRU keys) |
| PAGE_SIZE=10 | Task 1 |
| Tests | Each task + Task 6 |

**Placeholder scan:** none intentional.  
**Type consistency:** `PaletteCommand`, `PaletteWhen`, `PaletteContext`, `PAGE_SIZE`, `tab_mru`, `selection_anchor` used uniformly.

---

## Execution Handoff

Plan complete and saved to `docs/plans/2026-07-14-phase4-keyboard-palette-impl.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task + review  
2. **Inline Execution** — this session with checkpoints  

Which approach?
