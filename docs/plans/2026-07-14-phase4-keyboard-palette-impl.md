# Phase 4 Keyboard & Command Palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan one task at a time. Checkbox syntax (`- [ ]`) records implementation progress.

**Goal:** Implement the command palette (`cmd-shift-p`), complete keyboard multi-selection and paging for the file list, implement the MRU tab switcher (`ctrl-tab`), and expose shortcuts through palette keys and tooltips.

**Architecture:** An explicit `PaletteCommand` registry dispatches stable GPUI actions. List selection retains the path-based `selected_paths` and adds a view-side anchor for shift-range selection. Workspace maintains `tab_mru: Vec<TabId>`. The `cmd-shift-[/]` tab shortcuts continue to use creation order, whereas MRU only determines the ctrl-tab switcher UI order.

**Tech Stack:** Rust, GPUI (`actions!`, `KeyBinding`, `InputState`, modal-style overlays), and the existing `Workspace`, `PaneSide`, and visible indices from phase 3.

**Spec:** `docs/plans/2026-07-14-phase4-keyboard-palette-design.md`

## Global Constraints

- The palette uses only the explicit registry; therefore it must **not** reflect all `actions!` symbols.
- Palette titles use user-facing verb phrases and exclude runtime/channel/actor terminology.
- Selection remains path-based; therefore row indices cannot serve as long-term selection IDs.
- Page up/down uses `PAGE_SIZE = 10` and operates on **visible** list indices.
- `cmd-shift-[` / `]` retain **creation order**, whereas MRU applies only to the `ctrl-tab` switcher.
- Do not add a custom keybinding editor, and do not modify SFTP/core protocols.
- Recoverable paths cannot use `unwrap` according to AGENTS.md §5.
- Follow the existing workspace style, and prefer `src/foo.rs` to `mod.rs`.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/palette_commands.rs` | `PaletteCommand`, `PaletteWhen`, static registry, filter helper, and unit tests |
| **Create** `crates/app/src/workspace/command_palette.rs` | Workspace UI helpers for open/close/filter/execute |
| **Modify** `crates/app/src/app_actions.rs` | New actions and keybindings |
| **Modify** `crates/app/src/main.rs` | `mod palette_commands` |
| **Modify** `crates/app/src/workspace/mod.rs` | Palette, MRU, switcher, and anchor state; action wiring |
| **Modify** `crates/app/src/workspace/panes.rs` | selection extend, page/home/end, select all, anchor updates |
| **Modify** `crates/app/src/workspace/modals.rs` | `cancel_active_modal` palette first |
| **Modify** `crates/app/src/workspace/render.rs` | palette overlay, tab switcher overlay, tooltip key text |
| **Modify** `crates/app/src/workspace/tests.rs` | gpui tests |
| **Do not modify** | `crates/sftp` or core transfer/session protocols |

---

### Task 1: List keyboard — page / home / end / shift-range / cmd-a

**Files:**
- Modify: `crates/app/src/app_actions.rs`
- Modify: `crates/app/src/workspace/mod.rs` (fields and on_action)
- Modify: `crates/app/src/workspace/panes.rs`
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:
  - `Workspace.selection_anchor: Option<EntryPath>` (or `(PaneSide, EntryPath)`)
  - `pub const PAGE_SIZE: usize = 10;`
  - `select_index` sets a single selection **and** updates the anchor
  - `extend_selection_to(side, visible_index, cx)`
  - `select_all_visible(side, cx)`
  - Actions: `SelectNextEntryExtend`, `SelectPrevEntryExtend`, `PageDown`, `PageUp`, `SelectFirstEntry`, `SelectLastEntry`, `SelectAllEntries`

- [ ] **Step 1: Add failing tests**

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

- [ ] **Step 2: Execute the tests; expect failure because methods are missing**

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

`move_selection` must reset the anchor through `select_index`.

Implement page/home/end as follows:

```rust
pub(crate) fn page_selection(&mut self, side: PaneSide, direction: isize, cx: &mut Context<Self>) {
    let n = self.entry_count(side, cx);
    if n == 0 { return; }
    let cur = self.selected_index(side, cx).unwrap_or(0);
    let next = (cur as isize + direction * PAGE_SIZE as isize).clamp(0, n as isize - 1) as usize;
    self.select_index(side, next, cx);
}
```

- [ ] **Step 4: Add actions and keybindings**

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

Add `on_action` handlers in `mod.rs`. Each handler calls the corresponding pane method for `focused_side`.

- [ ] **Step 5: Verify that tests pass, and then commit**

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

- [ ] **Step 2: Implement the registry**, which must include at least the following commands:

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

Filtering converts `query` to lowercase. If the query is empty, return all commands that satisfy `when`; otherwise match a substring in title or keywords.

- [ ] **Step 3: Execute `cargo test -p macsftp-app --bin macsftp filter_matches filter_hides`**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(app): add explicit command palette registry"
```

---

### Task 3: Command palette UI and dispatch

**Files:**
- Create: `crates/app/src/workspace/command_palette.rs`
- Modify: `mod.rs` (state, actions, cancellation order, and rendered children)
- Modify: `modals.rs` (`cancel_active_modal` processes the palette first)
- Modify: `app_actions.rs` bind `cmd-shift-p`
- Test: `tests.rs`

**Interfaces:**
- `Workspace { palette_open, palette_input, palette_selected: usize }`
- `open_command_palette`, `close_command_palette`, `execute_palette_selected`
- `palette_context(&self) -> PaletteContext`
- `dispatch_palette_id(&mut self, id: &str, window, cx)` match on id → existing methods / `cx.dispatch_action`

- [ ] **Step 1: Test palette display, filtering, and NewTab execution**

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

- [ ] **Step 2: Implement the UI** by following the `render_go_to_path_modal` / About pattern

- Render a scrim and card with `key_context("CommandPalette")`.
- Bind the text field to `palette_input`.
- Render filtered commands with the title on the left and a muted keybinding on the right.
- Highlight `palette_selected`.
- Up/down changes the selection, Enter executes the command, and Escape invokes `cancel_active_modal`.
- Selecting a row executes its command.

Use the following structure for `dispatch_palette_id`:

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

- [ ] **Step 3: Update `cancel_active_modal`**

```rust
if self.palette_open {
    self.close_command_palette(window, cx);
    return;
}
// existing...
```

- [ ] **Step 4: Bind `cmd-shift-p` to `OpenCommandPalette`**

- [ ] **Step 5: Execute tests, and then commit**

```bash
cargo test -p macsftp-app --bin macsftp command_palette
git commit -m "feat(app): command palette UI and cmd-shift-p"
```

---

### Task 4: Tab MRU and ctrl-tab switcher

**Files:**
- Modify: `mod.rs`, `panes`/`mod` activate_tab, `render.rs`, and `app_actions.rs`
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

- [ ] **Step 2: Maintain MRU state**

```rust
fn touch_mru(&mut self, tab_id: TabId) {
    self.tab_mru.retain(|id| *id != tab_id);
    self.tab_mru.insert(0, tab_id);
}

// activate_tab / open_new_tab: touch_mru
// close_tab_by_id: retain remove
// Workspace::new after first tab: tab_mru = vec![first_id]
```

`activate_tab_in_direction` must continue to use **creation order** from the `tabs` vector; therefore it must not use MRU.

- [ ] **Step 3: Implement the tab switcher**

Define the `TabSwitcherNext` and `TabSwitcherPrev` actions, or reuse existing actions with modifiers.

Use the following bindings. The GPUI key names require confirmation against the GPUI documentation:

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

**Use Enter for confirmation**, because this behavior does not depend on key-up:

```rust
// Enter while switcher open → activate tab_mru[index], close switcher
// Esc → close without change
```

If GPUI supports modifiers-changed / key-up without substantial complexity, implement that behavior as an enhancement. However, tooltips must identify Enter as the primary confirmation method.

The UI renders an elevated list of MRU tabs, including each tab title and status color.

- [ ] **Step 4: Update `cancel_active_modal`** so that it closes the switcher before other modals, but after the palette.

- [ ] **Step 5: Execute tests, and then commit**

```bash
git commit -m "feat(app): MRU tab order and ctrl-tab switcher"
```

---

### Task 5: Tooltip discoverability and palette refinement

**Files:**
- Modify: `render.rs` (path bar and toolbar tooltips)
- Modify: `command_palette` / palette row layout if needed
- Inspect each `icon_button(` call, and ensure that labels for Refresh, Parent, Transfers, Hidden, New Folder, Delete, Back, and Forward include key chords.

- [ ] **Step 1:** Review and update strings so that they match the registry, including `⌘R`, `⌘↑`, and `⌘J`.
- [ ] **Step 2:** Ensure that palette rows display `keybinding` on the right if Task 3 has not already implemented this behavior.
- [ ] **Step 3:** Perform a smoke test with a narrow path bar, and verify that tooltips do not affect layout.
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(app): show shortcut hints in tooltips and palette rows"
```

---

### Task 6: Final verification

- [ ] **Step 1: Execute automated verification**

```bash
cargo test -p macsftp-app --bin macsftp
```

Expected result: all tests pass, including the new phase 4 tests.

- [ ] **Step 2: Complete the manual checklist**

1. Use `⌘⇧P`, enter “refresh”, and press Enter; the active pane refreshes.
2. In the file pane, verify page down, home/end, shift multi-selection, and the delete modal count after `⌘A`.
3. With three tabs, verify that `ctrl-tab` traverses MRU order, whereas `⌘⇧]` retains creation order.
4. Verify that icon tooltips display key chords.

- [ ] **Step 3: Create the spec coverage map** so that each design success row references a test or manual verification note.

---

## Self-Review (plan vs spec)

| Spec | Task |
| --- | --- |
| §2 Command palette | Tasks 2–3 |
| §3 List keyboard | Task 1 |
| §4 MRU + ctrl-tab | Task 4 |
| §5 Discoverability | Task 5 |
| cmd-shift creation order | Task 4; these shortcuts explicitly exclude MRU order |
| PAGE_SIZE=10 | Task 1 |
| Tests | Each task + Task 6 |

**Placeholder review:** The plan contains no intentional placeholders.
**Type consistency:** The plan uses `PaletteCommand`, `PaletteWhen`, `PaletteContext`, `PAGE_SIZE`, `tab_mru`, and `selection_anchor` consistently.

---

## Execution Handoff

The plan is complete and is stored at `docs/plans/2026-07-14-phase4-keyboard-palette-impl.md`.

Two execution options are available:

1. **Subagent-Driven (recommended):** Assign each task to a new subagent, and review the result.
2. **Inline Execution:** Use this session and verify the result at each checkpoint.

Select one execution option before implementation begins.
