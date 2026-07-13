# Transfer Drawer UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the transfer drawer vertically resizable (drag handle + double-click reset) with sticky header / independent list scroll and session-local height, without changing transfer business semantics.

**Architecture:** Session-only `drawer_height` on `Workspace`. Pure `clamp_drawer_height` helper for min/max. GPUI `on_drag` + `on_drag_move` on a top handle (payload carries start height/Y). Layout: fixed `h(drawer_height)` column with handle + header (flex_none) and job list body (`flex_1` + `overflow_y_scroll`). No `AppConfig` persistence, no new `ui` crate component.

**Tech Stack:** Rust, GPUI 0.2.2 (`Pixels`, `on_drag` / `on_drag_move`, `MouseButton`, `cursor_row_resize`, `ClickEvent::click_count`), existing `Workspace` / `render_transfer_drawer`.

**Spec:** `docs/plans/2026-07-14-transfer-drawer-ux-design.md`

## Global Constraints

- **Scope:** resize + P0 polish only; do **not** rework Active/Queued/Completed/Failed/History IA.
- **Memory:** height and open state are **window-session only** (`Workspace` fields); no `AppConfig` / disk.
- **Drag:** continuous height change; **never** auto-close when height hits min.
- **Close path:** still `ShowTransferDrawer` / status bar only (no header close button this round).
- **Double-click handle** → default height (`240px`), then clamp.
- **No** `unwrap` / `expect` on recoverable paths; no `let _ =` on fallible ops.
- **No** changes to `core` transfer state machine, `sftp`, or Keychain.
- Surgical diffs; match existing theme tokens and drawer chrome style.
- Work in worktree branch `transfer-drawer-ux` under `.worktrees/transfer-drawer-ux`.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/workspace/drawer_height.rs` | Constants, `clamp_drawer_height`, pure unit tests |
| **Modify** `crates/app/src/workspace/mod.rs` | `drawer_height` field, default, `mod drawer_height`, optional resize helpers |
| **Modify** `crates/app/src/workspace/render.rs` | Fixed-height layout, sticky header, handle drag/double-click |
| **Modify** `crates/app/src/workspace/tests.rs` | Session height / toggle / reset gpui tests |
| **Do not modify** | `crates/core/**`, `crates/sftp/**`, `crates/storage/**` (unless a tiny re-export is forced — prefer not) |

## Suggested PR mapping

| PR | Tasks |
| --- | --- |
| PR1 | Task 1–2 (height math + Workspace field) |
| PR2 | Task 3–4 (layout + drag/double-click) |

---

### Task 1: Clamp helper + constants (pure)

**Files:**
- Create: `crates/app/src/workspace/drawer_height.rs`
- Modify: `crates/app/src/workspace/mod.rs` — add `mod drawer_height;` and `pub(crate) use drawer_height::{...}` if needed

**Interfaces:**
- Produces:

```rust
use gpui::{Pixels, px};

/// Default open height (matches former `max_h(240)`).
pub(crate) const DEFAULT_DRAWER_HEIGHT: Pixels = px(240.0);
/// Smallest allowed height: handle + one header row (list may be empty).
pub(crate) const MIN_DRAWER_HEIGHT: Pixels = px(40.0);
/// Absolute ceiling regardless of window size.
pub(crate) const MAX_DRAWER_HEIGHT_ABS: Pixels = px(480.0);
/// Fraction of the main content area (tab bar bottom → status bar top).
pub(crate) const MAX_DRAWER_HEIGHT_RATIO: f32 = 0.5;
/// Hit target for the resize grip.
pub(crate) const RESIZE_HANDLE_HEIGHT: Pixels = px(5.0);

/// Approximate chrome outside the content area when measuring max height.
/// tab_bar (~36) + status_bar (theme ~22–28) — keep conservative so max is not too tall.
pub(crate) const APPROX_CHROME_HEIGHT: Pixels = px(64.0);

/// Clamp a requested drawer height into [min, max(content)].
///
/// `content_area_height` is the vertical space available for panes+drawer
/// (viewport minus approximate chrome). When unknown, callers may pass
/// `viewport_height - APPROX_CHROME_HEIGHT` (floored at min).
pub(crate) fn clamp_drawer_height(height: Pixels, content_area_height: Pixels) -> Pixels {
    let max_from_ratio = content_area_height * MAX_DRAWER_HEIGHT_RATIO;
    let mut max_height = if max_from_ratio < MAX_DRAWER_HEIGHT_ABS {
        max_from_ratio
    } else {
        MAX_DRAWER_HEIGHT_ABS
    };
    if max_height < MIN_DRAWER_HEIGHT {
        max_height = MIN_DRAWER_HEIGHT;
    }
    if height < MIN_DRAWER_HEIGHT {
        MIN_DRAWER_HEIGHT
    } else if height > max_height {
        max_height
    } else {
        height
    }
}

/// Content area height from window viewport (best-effort for render-time clamp).
pub(crate) fn content_area_height_from_viewport(viewport_height: Pixels) -> Pixels {
    let raw = viewport_height - APPROX_CHROME_HEIGHT;
    if raw < MIN_DRAWER_HEIGHT {
        MIN_DRAWER_HEIGHT
    } else {
        raw
    }
}
```

- [ ] **Step 1: Write the failing tests** (in `drawer_height.rs` under `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    #[test]
    fn clamp_enforces_minimum() {
        let content = px(800.0);
        assert_eq!(clamp_drawer_height(px(10.0), content), MIN_DRAWER_HEIGHT);
    }

    #[test]
    fn clamp_enforces_absolute_maximum() {
        let content = px(2000.0); // 50% = 1000 > 480 abs
        assert_eq!(
            clamp_drawer_height(px(900.0), content),
            MAX_DRAWER_HEIGHT_ABS
        );
    }

    #[test]
    fn clamp_enforces_ratio_maximum() {
        let content = px(400.0); // 50% = 200 < 480
        assert_eq!(clamp_drawer_height(px(300.0), content), px(200.0));
    }

    #[test]
    fn clamp_passes_through_in_range() {
        let content = px(800.0);
        assert_eq!(clamp_drawer_height(px(240.0), content), px(240.0));
    }

    #[test]
    fn content_area_from_viewport_subtracts_chrome() {
        assert_eq!(
            content_area_height_from_viewport(px(864.0)),
            px(800.0)
        );
    }
}
```

- [ ] **Step 2: Run tests — expect FAIL** (module missing)

```bash
cd /Users/alex/Projects/macSFTP/.worktrees/transfer-drawer-ux
cargo test -p macsftp-app clamp_enforces -- --nocapture
```

Expected: compile error / test binary cannot find tests.

- [ ] **Step 3: Implement `drawer_height.rs` + wire `mod drawer_height` in `mod.rs`**

Place `mod drawer_height;` next to other workspace mods at the bottom of `mod.rs`. Do not re-export publicly outside the crate.

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo test -p macsftp-app clamp_enforces -- --nocapture
cargo test -p macsftp-app content_area_from_viewport -- --nocapture
```

Expected: all five tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/drawer_height.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): add transfer drawer height clamp helper"
```

---

### Task 2: Workspace session height field + apply/reset API

**Files:**
- Modify: `crates/app/src/workspace/mod.rs`
- Modify: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Consumes: `DEFAULT_DRAWER_HEIGHT`, `clamp_drawer_height`, `content_area_height_from_viewport` from Task 1
- Produces (on `Workspace`):

```rust
// field
drawer_height: Pixels, // init DEFAULT_DRAWER_HEIGHT

// methods (pub(crate) on impl Workspace)
fn set_drawer_height(&mut self, height: Pixels, viewport_height: Pixels) {
    let content = content_area_height_from_viewport(viewport_height);
    self.drawer_height = clamp_drawer_height(height, content);
}

fn reset_drawer_height(&mut self, viewport_height: Pixels) {
    self.set_drawer_height(DEFAULT_DRAWER_HEIGHT, viewport_height);
}

fn reclamp_drawer_height(&mut self, viewport_height: Pixels) {
    self.set_drawer_height(self.drawer_height, viewport_height);
}
```

Drag payload type (also in `drawer_height.rs` or `mod.rs` — prefer `drawer_height.rs`):

```rust
/// GPUI drag value for the transfer drawer resize handle.
#[derive(Clone, Debug)]
pub(crate) struct TransferDrawerResize {
    pub start_height: Pixels,
    pub start_y: Pixels,
}
```

Empty drag preview (no visual ghost required for resize; minimal transparent view):

```rust
// In drawer_height.rs or render.rs
pub(crate) struct ResizeDragGhost;

impl gpui::Render for ResizeDragGhost {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div().w(px(1.0)).h(px(1.0))
    }
}
```

- [ ] **Step 1: Failing gpui tests** in `tests.rs`

```rust
#[gpui::test]
fn transfer_drawer_default_height(cx: &mut TestAppContext) {
    let (workspace, _cx, _channels) = init_workspace(cx);
    workspace.read_with(&_cx, |workspace, _| {
        assert_eq!(
            workspace.drawer_height,
            crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
        );
    });
}

#[gpui::test]
fn transfer_drawer_height_survives_toggle(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        workspace.set_drawer_height(gpui::px(180.0), gpui::px(900.0));
    });
    cx.dispatch_action(ShowTransferDrawer);
    cx.dispatch_action(ShowTransferDrawer);
    workspace.read_with(&cx, |workspace, _| {
        assert!(workspace.drawer_open);
        assert_eq!(workspace.drawer_height, gpui::px(180.0));
    });
}

#[gpui::test]
fn transfer_drawer_reset_height(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update(&mut cx, |workspace, _cx| {
        workspace.set_drawer_height(gpui::px(360.0), gpui::px(900.0));
        workspace.reset_drawer_height(gpui::px(900.0));
    });
    workspace.read_with(&cx, |workspace, _| {
        assert_eq!(
            workspace.drawer_height,
            crate::workspace::drawer_height::DEFAULT_DRAWER_HEIGHT
        );
    });
}
```

Import `ShowTransferDrawer` is already used in this file. Use `workspace.update` / `update_in` consistently with neighboring tests (`read_with`, `dispatch_action`).

If `drawer_height` is private, tests live in the same parent module via `mod tests` inside `workspace` — existing tests already access `workspace.drawer_open`, so `drawer_height` can stay private field with `pub(crate)` methods, and tests can read the field if tests are a child module of `workspace` (they are: `mod tests` in `mod.rs`). Field access from `tests` works for private fields of `Workspace` only if tests are nested in the same module — check: `tests.rs` is `mod tests` under workspace, so it **can** access private fields of sibling items only through `super::Workspace` — in Rust, child modules cannot access private fields of parent structs... Actually private fields are visible to the parent module and its children? **No** — fields private to the parent module are accessible from child modules of that parent. Yes: `pub struct` fields with no `pub` are visible to the entire parent module tree including `mod tests`. Confirmed by existing `workspace.drawer_open` asserts.

- [ ] **Step 2: Run — expect FAIL**

```bash
cargo test -p macsftp-app transfer_drawer_default_height -- --nocapture
```

Expected: missing field / method.

- [ ] **Step 3: Minimal implementation**

In `Workspace` struct after `drawer_open`:

```rust
drawer_height: Pixels,
```

In `Workspace::new` init:

```rust
drawer_open: true,
drawer_height: DEFAULT_DRAWER_HEIGHT,
```

Add imports for `Pixels` / `drawer_height::*`. Implement `set_drawer_height`, `reset_drawer_height`, `reclamp_drawer_height` on `impl Workspace`.

- [ ] **Step 4: Run — expect PASS**

```bash
cargo test -p macsftp-app transfer_drawer_ -- --nocapture
```

Expected: new tests + existing `transfer_drawer_toggles_via_action` PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/drawer_height.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): session-local transfer drawer height on Workspace"
```

---

### Task 3: Layout — fixed height, sticky header, scroll body

**Files:**
- Modify: `crates/app/src/workspace/render.rs` — `render_transfer_drawer`

**Interfaces:**
- Consumes: `self.drawer_height`, `clamp_drawer_height` / `content_area_height_from_viewport`, `RESIZE_HANDLE_HEIGHT`
- Produces: drawer root with structure:

```text
div#transfer-drawer  h(clamped) flex_col flex_none
  handle (Task 4 wires events; Task 3 can render static bar)
  header (existing chrome)
  body flex_1 min_h_0 overflow_y_scroll
    sections / rows / empty
```

- [ ] **Step 1: Replace root max_h + whole-drawer scroll**

Current (remove):

```rust
let mut drawer = div()
    .id("transfer-drawer")
    .flex()
    .flex_col()
    .flex_none()
    .max_h(px(240.0))
    .overflow_y_scroll()
    ...
```

New pattern:

```rust
let viewport_h = /* if render gains `window: &mut Window`, use window.viewport_size().height;
                   else pass window from Render::render — see Step 1b */;
let content_h = content_area_height_from_viewport(viewport_h);
let height = clamp_drawer_height(self.drawer_height, content_h);

let mut drawer = div()
    .id("transfer-drawer")
    .flex()
    .flex_col()
    .flex_none()
    .h(height)
    .min_h(MIN_DRAWER_HEIGHT)
    .bg(theme.colors.surface)
    .border_t_1()
    .border_color(theme.colors.border);
```

**Step 1b — window access:** Today `render_transfer_drawer(&self, cx: &mut Context<Self>)` has no `Window`. Options (pick first that compiles cleanly):

1. Change signature to `render_transfer_drawer(&self, window: &mut Window, cx: &mut Context<Self>)` and pass `window` from `Render::render` (preferred — call site already has `window`).
2. Or clamp only with a large fallback content height when window unavailable (worse).

Also reclamp on render if stored height exceeds new max after window shrink:

```rust
// Cannot mutate self in &self render. Either:
// (A) reclamp only in drag/reset paths + window resize observer, or
// (B) change to &mut self (Render::render has &mut self already — method can take &mut self).
```

**Preferred:** change method to `&mut self` and at start of render:

```rust
self.reclamp_drawer_height(window.viewport_size().height);
let height = self.drawer_height;
```

Call site in `mod.rs` `Render::render` already uses `&mut self` → `self.render_transfer_drawer(window, cx)`.

- [ ] **Step 2: Split handle + sticky header + scroll body**

```rust
// Handle (static chrome in this task; events in Task 4)
drawer = drawer.child(
    div()
        .id("transfer-drawer-resize-handle")
        .flex_none()
        .w_full()
        .h(RESIZE_HANDLE_HEIGHT)
        .cursor_row_resize()
        .bg(theme.colors.border) // thin line affordance; hover polish in Task 4
);

// Existing header child (Transfers + agg_label) — keep as flex_none, do not put in scroll
// ...

// Body
let mut body = div()
    .id("transfer-drawer-body")
    .flex()
    .flex_col()
    .flex_1()
    .min_h_0()
    .overflow_y_scroll();

// Move section loops / empty state into `body`, then:
drawer = drawer.child(body);
```

Empty state: keep centered short message inside body (min height of body handles centering via flex if desired):

```rust
body = body.child(
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(theme.colors.text_muted)
        .child("No transfers"),
);
```

- [ ] **Step 3: Build**

```bash
cargo check -p macsftp-app
```

Expected: success. Visual: drawer height = default; header fixed; long lists scroll inside body.

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): sticky transfer drawer header and fixed session height layout"
```

---

### Task 4: Drag resize + double-click reset

**Files:**
- Modify: `crates/app/src/workspace/render.rs`
- Modify: `crates/app/src/workspace/drawer_height.rs` (if ghost/payload not already there)
- Modify: `crates/app/src/workspace/tests.rs` (logic-level coverage already in Task 2; optional drag test skip if hard)

**Interfaces:**
- Consumes: `TransferDrawerResize`, `ResizeDragGhost`, `set_drawer_height`, `reset_drawer_height`
- GPUI APIs (0.2.2):

```rust
// StatefulInteractiveElement — element must be stateful (.id present)
.on_drag(
    TransferDrawerResize {
        start_height: self.drawer_height,
        start_y: px(0.0), // overwritten at drag start via constructor position if needed
    },
    |value, _offset, _window, cx| {
        // Capture start_y from window.mouse_position() at drag construction:
        cx.new(|_cx| ResizeDragGhost)
    },
)
.on_drag_move(cx.listener(
    |workspace, event: &DragMoveEvent<TransferDrawerResize>, window, cx| {
        let Some(drag) = event.drag::<TransferDrawerResize>(cx) else {
            // If API differs, use event.dragged_item downcast — check DragMoveEvent methods:
            // event.dragged_item is Arc<dyn Any>; prefer:
            // let drag = event.drag(cx) documented in div.rs
            return;
        };
        // Preferred: store start_y/start_height in payload at on_drag constructor time.
        let start = drag; // TransferDrawerResize
        let current_y = event.event.position.y;
        let delta = start.start_y - current_y; // drag up → taller
        let new_height = start.start_height + delta;
        workspace.set_drawer_height(new_height, window.viewport_size().height);
        cx.notify();
    },
))
```

**Critical implementation note — capture start position correctly:**

`on_drag` value is fixed when the element is built each frame. For correct deltas, either:

**Option A (recommended):** On drag constructor, store start in the ghost entity or update workspace:

```rust
.on_drag((), move |_unit, _offset, window, cx| {
    // Problem: constructor is not a Workspace listener.
})
```

**Option B (works with GPUI file-row pattern):** Put `start_height` + `start_y` into the drag value at **mouse-down** by using `on_mouse_down` to set `workspace.drawer_resize = Some(...)`, and on move read from workspace — but then you need global move.

**Option C (use on_drag value updated each frame):** Each render rebuilds:

```rust
TransferDrawerResize {
    start_height: self.drawer_height,
    start_y: /* cannot know until mouse down */,
}
```

Looking at GPUI source: drag starts after threshold; constructor receives `cursor_offset` and can read `window.mouse_position()`. The **value** `T` is cloned at element build time, not at drag start. So `start_height`/`start_y` in `T` are stale unless we set them in the constructor via a side channel.

**Practical pattern for this codebase:**

```rust
// Workspace field (session only):
drawer_resize: Option<TransferDrawerResize>, // set on drag begin

// Handle:
.on_mouse_down(MouseButton::Left, cx.listener(|ws, event, window, cx| {
    if event.click_count >= 2 {
        ws.reset_drawer_height(window.viewport_size().height);
        ws.drawer_resize = None;
        cx.notify();
        return;
    }
    ws.drawer_resize = Some(TransferDrawerResize {
        start_height: ws.drawer_height,
        start_y: event.position.y,
    });
    cx.notify();
}))
// Attach move/up on workspace root OR use on_drag + on_drag_move:

// Preferred GPUI-native resize path:
.on_drag(
    TransferDrawerResize {
        start_height: self.drawer_height,
        start_y: px(0.0),
    },
    cx.listener(|ws, value, offset, window, cx| {
        // NOTE: on_drag constructor signature is Fn(&T, Point, &mut Window, &mut App) -> Entity
        // NOT cx.listener — cannot access Workspace easily unless using Entity handle.
    }),
)
```

**Chosen approach (implement this):**

1. Capture `Entity<Workspace>` before building handle (already common: `let workspace = cx.entity()`).
2. `on_drag` payload type `TransferDrawerResize` built empty; constructor:

```rust
let workspace_entity = cx.entity();
.on_drag(
    TransferDrawerResize {
        start_height: self.drawer_height,
        start_y: px(0.0),
    },
    move |value, _cursor_offset, window, cx| {
        let start_y = window.mouse_position().y;
        let start_height = value.start_height;
        workspace_entity.update(cx, |ws, _cx| {
            ws.drawer_resize = Some(TransferDrawerResize {
                start_height,
                start_y,
            });
        });
        cx.new(|_| ResizeDragGhost)
    },
)
.on_drag_move(cx.listener(|ws, event: &DragMoveEvent<TransferDrawerResize>, window, cx| {
    let Some(start) = ws.drawer_resize.clone() else {
        return;
    };
    let current_y = event.event.position.y;
    let new_height = start.start_height + (start.start_y - current_y);
    ws.set_drawer_height(new_height, window.viewport_size().height);
    window.set_window_cursor_style(CursorStyle::ResizeRow); // if available; else cursor on handle is enough
    cx.notify();
}))
```

3. Clear `drawer_resize` on mouse up — attach on workspace root when `drawer_resize.is_some()`:

```rust
// In Render::render workspace root, when drawer_resize is Some:
.on_mouse_up(MouseButton::Left, cx.listener(|ws, _e, _w, cx| {
    if ws.drawer_resize.take().is_some() {
        cx.notify();
    }
}))
```

Also clear when drag ends via GPUI dropping active_drag (mouse up anywhere). The root `on_mouse_up` is sufficient.

4. **Double-click:** on handle

```rust
.on_click(cx.listener(|ws, event: &ClickEvent, window, cx| {
    if event.click_count() >= 2 {
        ws.reset_drawer_height(window.viewport_size().height);
        ws.drawer_resize = None;
        cx.notify();
    }
}))
```

Or `on_mouse_down` with `event.click_count >= 2` **before** starting drag (prevents drag on double-click). Prefer mouse_down double-click path so drag does not start.

5. Tooltip:

```rust
.tooltip(text_tooltip("Drag to resize · Double-click to reset"))
```

6. Hover: `.hover(|s| s.bg(theme.colors.accent))` with low-key color, or slightly thicker line.

7. Set drag cursor style if API allows: `cx.set_active_drag_cursor_style(CursorStyle::ResizeRow, window)` during move.

- [ ] **Step 1: Implement handle interactions as above**

Add `drawer_resize: Option<TransferDrawerResize>` to `Workspace`, default `None` in `new`.

Ensure imports in `render.rs`: `MouseButton`, `DragMoveEvent`, `ClickEvent`, `CursorStyle` as needed.

- [ ] **Step 2: Build + unit tests**

```bash
cargo test -p macsftp-app transfer_drawer_ -- --nocapture
cargo check -p macsftp-app
```

Expected: PASS / success.

- [ ] **Step 3: Manual smoke (document in commit body if no screenshot)**

1. Open app, open Transfers (⌘J).
2. Drag handle up → drawer taller; drag down → shorter; stops at min/max.
3. Double-click handle → height returns ~240.
4. Toggle close/open → height preserved.
5. Scroll long job list → header stays put.
6. Resize window shorter → drawer does not overflow past max.

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/render.rs crates/app/src/workspace/drawer_height.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): drag-resize transfer drawer with double-click reset"
```

---

### Task 5: Regression gate + design cross-check

**Files:**
- None required unless Task 4 left polish gaps
- Optional: one-line note in `docs/ui-ux-guidelines.md` §7 that drawer height is user-resizable (session) — only if you already touch docs; **skip** if pure code PR preferred

- [ ] **Step 1: Full app test slice**

```bash
cargo test -p macsftp-app --lib
```

Expected: all pass (or pre-existing failures only — do not land new failures).

- [ ] **Step 2: Spec coverage checklist** (implementer marks mentally)

| Spec requirement | Task |
| --- | --- |
| Drag vertical resize | 4 |
| min/max clamp | 1 + 2 + render reclamp |
| Double-click reset | 4 |
| Session-only memory | 2 |
| Toggle preserves height | 2 |
| Sticky header + body scroll | 3 |
| No auto-close at min | 4 (no code sets drawer_open=false on height) |
| No AppConfig | — (no storage edits) |
| P0 empty state | 3 |
| Handle cursor/tooltip | 4 |

- [ ] **Step 3: Final commit only if docs updated; else done**

---

## Self-Review (plan vs spec)

| Spec section | Covered by |
| --- | --- |
| §1 Goals (resize, clamp, double-click, session, sticky) | Tasks 1–4 |
| §1 Non-goals (persist, snap-close, virtualization, clear completed) | Global Constraints — no tasks |
| §3 State model | Task 2 (`drawer_height`, `drawer_resize`) |
| §4 Layout | Task 3 |
| §5 Interaction | Task 4 |
| §6 P0 polish | Tasks 3–4 |
| §6 P1/P2 | Explicitly out of scope |
| §8 Tests | Tasks 1–2 automated; Task 4 manual smoke |
| GPUI drag risk | Task 4 documents Option + concrete `on_drag`/`drawer_resize` hybrid |

**Placeholder scan:** None intentional; if `DragMoveEvent::drag` method name differs in 0.2.2, use `dragged_item` + downcast or rely solely on `workspace.drawer_resize` (already set in constructor) so move handler does not need payload from event.

**Verified GPUI 0.2.2 APIs:** `on_drag`, `on_drag_move`, `on_mouse_down`, `on_mouse_up`, `on_click` + `click_count()`, `cursor_row_resize()`, `window.viewport_size()`, `Pixels` `PartialOrd` + `Mul<f32>`.

---

## Execution Handoff

Plan complete and saved to `docs/plans/2026-07-14-transfer-drawer-ux-impl.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session implements tasks sequentially with checkpoints  

Which approach?
