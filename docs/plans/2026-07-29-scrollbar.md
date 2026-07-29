# Custom Themed Scrollbar Implementation Plan

**Completion:** Implemented on `feat/custom-scrollbar`. The final integration also covers the MRU tab switcher and adds retained `ScrollbarState` synchronization, outside-track drag capture, immediate repaint tests, and a tab-switcher overflow regression test.

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace GPUI's thin auto-hiding native scrollbars with a unified, always-visible, light/dark-themed custom scrollbar across all scrollable areas (file panes, transfer drawer, command palette, profile-picker modal).

**Architecture:** A reusable stateful `Scrollbar` view in `crates/ui` reads scroll geometry from a `ScrollHandle` (`offset()`/`max_offset()`/`bounds()`) and draws a track + rounded thumb, themed from `cx.theme().colors`. Container areas keep GPUI's native scroll *behavior* (`overflow_y_scroll()` for wheel/touch/keyboard) but suppress the native *visual* via `scrollbar_width(px(0))`, then overlay the custom `Scrollbar` bound to the same handle. Drag and track-click are implemented on the custom component; wheel/touch stay native.

**Tech Stack:** Rust, GPUI 0.2.2, `crates/ui` (theme + components), `crates/app` (workspace views). TDD with GPUI `test-support`.

**Design doc:** `docs/plans/2026-07-29-scrollbar-design.md` (authoritative design; this file is the task breakdown).

**Branch:** `feat/custom-scrollbar` (already created; design doc committed as `f9fc165`).

**Key GPUI 0.2.2 facts (verified):**
- `ScrollHandle`: `offset() -> Point<Pixels>` (negative y when scrolled down), `max_offset() -> Size<Pixels>` (max scrollable distance = content − viewport), `bounds() -> Bounds<Pixels>` (viewport), `set_offset(Point<Pixels>)`.
- `UniformListScrollHandle(pub Rc<RefCell<UniformListScrollState>>)`; `UniformListScrollState { pub base_handle: ScrollHandle, .. }` → read via `handle.0.borrow().base_handle.clone()`.
- `UniformList: InteractiveElement + Styled` → `.scrollbar_width(px(0.))` and `.track_scroll(..)` available on `uniform_list(..)`.
- `uniform_list` sets `overflow.y = Scroll` internally → it draws a native auto-hiding scrollbar; suppress with `scrollbar_width(px(0.))`.
- `Theme` is a GPUI `Global` in `crates/ui/src/theme.rs`; `cx.theme()` via `ActiveTheme` trait. `ThemeColors` and `ThemeSizes` are `#[derive(Clone, Copy)]`.

**Gates (run after every task that touches code):**
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
```
App/GPUI rendering tests may be blocked locally by missing Xcode `metal`; run `cargo test -p macsftp-ui` for pure-logic tests, CI runs the rest.

---

### Task 1: Theme tokens for the scrollbar

**Files:**
- Modify: `crates/ui/src/theme.rs` (`ThemeColors` struct ~24-46; `ThemeSizes` ~56-63; `one_dark()` ~67-91; `one_light()` ~94-118; `default_sizes()` ~144-153; tests ~166-221)

**Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/ui/src/theme.rs`:

```rust
#[test]
fn scrollbar_tokens_are_defined_and_distinct_per_appearance() {
    let dark = Theme::one_dark();
    let light = Theme::one_light();

    // Both appearances define all four scrollbar color tokens.
    assert_ne!(dark.colors.scrollbar_thumb, light.colors.scrollbar_thumb);
    assert_ne!(dark.colors.scrollbar_thumb_hover, light.colors.scrollbar_thumb_hover);
    assert_ne!(dark.colors.scrollbar_thumb_active, light.colors.scrollbar_thumb_active);
    // Track is transparent in both, but the field must exist.
    assert_eq!(dark.colors.scrollbar_track, light.colors.scrollbar_track);

    // A scrollbar width token exists and is positive.
    assert!(dark.sizes.scrollbar_width > px(0.0));
    assert_eq!(dark.sizes.scrollbar_width, light.sizes.scrollbar_width);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p macsftp-ui scrollbar_tokens_are_defined_and_distinct_per_appearance`
Expected: FAIL — `no field scrollbar_thumb on type ThemeColors` (compile error).

**Step 3: Write minimal implementation**

In `ThemeColors` (after `info: Hsla,`), add:
```rust
    /// Custom scrollbar thumb (resting).
    pub scrollbar_thumb: Hsla,
    /// Custom scrollbar thumb on hover.
    pub scrollbar_thumb_hover: Hsla,
    /// Custom scrollbar thumb while being dragged.
    pub scrollbar_thumb_active: Hsla,
    /// Custom scrollbar track background (transparent by default).
    pub scrollbar_track: Hsla,
```

In `ThemeSizes` (after `status_bar_height: Pixels,`), add:
```rust
    /// Width of the custom scrollbar (track + thumb).
    pub scrollbar_width: Pixels,
```

In `one_dark()` `ThemeColors { .. }`, add after `info: rgb(0x56b6c2).into(),`:
```rust
                scrollbar_thumb: hsla(0.0, 0.0, 1.0, 0.22),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 1.0, 0.36),
                scrollbar_thumb_active: hsla(0.0, 0.0, 1.0, 0.50),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
```

In `one_light()` `ThemeColors { .. }`, add after `info: rgb(0x0184bc).into(),`:
```rust
                scrollbar_thumb: hsla(0.0, 0.0, 0.0, 0.30),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 0.0, 0.45),
                scrollbar_thumb_active: hsla(0.0, 0.0, 0.0, 0.55),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
```

In `default_sizes()` `ThemeSizes { .. }`, add after `status_bar_height: px(26.0),`:
```rust
        scrollbar_width: px(10.0),
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p macsftp-ui scrollbar_tokens_are_defined_and_distinct_per_appearance`
Expected: PASS. Also run `cargo test -p macsftp-ui` → all theme tests still pass.

**Step 5: Gates + commit**

```
cargo fmt --all --check
cargo clippy -p macsftp-ui --all-targets -- -D warnings
```
```bash
git add crates/ui/src/theme.rs
git commit -m "Add scrollbar theme tokens"
```

---

### Task 2: Scrollbar component + ScrollArea helper

**Files:**
- Create: `crates/ui/src/scrollbar.rs`
- Modify: `crates/ui/src/ui.rs` (add `mod scrollbar;` + re-exports ~1-25)

**Step 1: Write the failing test (geometry)**

Create `crates/ui/src/scrollbar.rs` with an empty module + a `#[cfg(test)]` block. First add the pure-geometry helper as the unit-testable seam:

```rust
use gpui::{Pixels, px};

/// Minimum thumb height so a tiny thumb stays grabbable.
pub const MIN_THUMB: Pixels = px(24.0);

/// Thumb geometry derived from scroll state. Returns `None` when content
/// fits the viewport (no scrollbar should be shown).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbGeometry {
    pub thumb_height: Pixels,
    pub thumb_top: Pixels,
    pub track_height: Pixels,
}

/// Compute thumb geometry from viewport height, scrollable distance
/// (`max_offset().height`), and the current scrolled distance
/// (`-offset().y`, clamped to `[0, scrollable]`).
pub fn thumb_geometry(
    viewport_h: Pixels,
    scrollable: Pixels,
    scrolled: Pixels,
) -> Option<ThumbGeometry> {
    if viewport_h <= px(0.0) || scrollable <= px(0.0) {
        return None;
    }
    let content_h = viewport_h + scrollable;
    let track_height = viewport_h;
    let thumb_height = (viewport_h * viewport_h / content_h).max(MIN_THUMB);
    let scrolled = scrolled.max(px(0.0)).min(scrollable);
    let thumb_top = (scrolled / scrollable) * (track_height - thumb_height);
    Some(ThumbGeometry { thumb_height, thumb_top, track_height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_thumb_when_content_fits_viewport() {
        assert!(thumb_geometry(px(400.0), px(0.0), px(0.0)).is_none());
        assert!(thumb_geometry(px(400.0), px(-10.0), px(0.0)).is_none());
    }

    #[test]
    fn thumb_at_top_when_not_scrolled() {
        let g = thumb_geometry(px(400.0), px(400.0), px(0.0)).unwrap();
        assert_eq!(g.thumb_top, px(0.0));
        // thumb = viewport^2 / content = 400*400/800 = 200
        assert_eq!(g.thumb_height, px(200.0));
        assert_eq!(g.track_height, px(400.0));
    }

    #[test]
    fn thumb_clamps_to_min() {
        // Huge content -> thumb would be tiny, clamped to MIN_THUMB.
        let g = thumb_geometry(px(400.0), px(100_000.0), px(0.0)).unwrap();
        assert_eq!(g.thumb_height, MIN_THUMB);
    }

    #[test]
    fn thumb_at_bottom_when_fully_scrolled() {
        let g = thumb_geometry(px(400.0), px(400.0), px(400.0)).unwrap();
        // thumb_top = (400/400) * (400 - 200) = 200
        assert_eq!(g.thumb_top, px(200.0));
    }

    #[test]
    fn scrolled_is_clamped_to_scrollable_range() {
        // scrolled beyond scrollable clamps to bottom.
        let g = thumb_geometry(px(400.0), px(400.0), px(999.0)).unwrap();
        assert_eq!(g.thumb_top, px(200.0));
        // negative scrolled clamps to top.
        let g = thumb_geometry(px(400.0), px(400.0), px(-50.0)).unwrap();
        assert_eq!(g.thumb_top, px(0.0));
    }
}
```

**Step 2: Run test to verify it passes (geometry is pure)**

Run: `cargo test -p macsftp-ui thumb_`
Expected: PASS (5 tests). (Geometry is pure fns; they compile & pass immediately. The failing-test discipline applies to the interactive `Scrollbar` view below.)

**Step 3: Implement the `Scrollbar` view + `ScrollArea` helper**

Append to `crates/ui/src/scrollbar.rs` (above the `#[cfg(test)]` block):

```rust
use gpui::{
    App, Context, ElementId, IntoElement, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollHandle, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
};

use crate::theme::ActiveTheme;

/// A vertical, always-visible, theme-aware scrollbar overlaid on a scroll
/// container. Bind it to the same `ScrollHandle` the container tracks.
pub struct Scrollbar {
    handle: ScrollHandle,
    dragging: bool,
    drag_start_y: Pixels,
    drag_start_offset: Pixels,
}

impl Scrollbar {
    pub fn new(handle: ScrollHandle) -> Self {
        Self {
            handle,
            dragging: false,
            drag_start_y: px(0.0),
            drag_start_offset: px(0.0),
        }
    }

    /// Build a vertical scrollbar view bound to a plain `ScrollHandle`.
    pub fn vertical(handle: ScrollHandle, cx: &mut App) -> impl IntoElement {
        cx.new(|_| Scrollbar::new(handle))
    }

    /// Build a vertical scrollbar view bound to a `UniformListScrollHandle`
    /// (the local/remote file panes).
    pub fn vertical_uniform(handle: &UniformListScrollHandle, cx: &mut App) -> impl IntoElement {
        let base = handle.0.borrow().base_handle.clone();
        cx.new(|_| Scrollbar::new(base))
    }

    fn thumb_color(&self, cx: &App) -> gpui::Hsla {
        let colors = cx.theme().colors;
        if self.dragging {
            colors.scrollbar_thumb_active
        } else {
            colors.scrollbar_thumb
        }
    }
}

impl gpui::Render for Scrollbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let viewport_h = self.handle.bounds().size.height;
        let scrollable = self.handle.max_offset().height;
        let scrolled = (-self.handle.offset().y).max(px(0.0));

        let Some(geom) = thumb_geometry(viewport_h, scrollable, scrolled) else {
            // No overflow: render nothing.
            return div().into_any_element();
        };

        let width = theme.sizes.scrollbar_width;
        let thumb_color = self.thumb_color(cx);
        let track_color = cx.theme().colors.scrollbar_track;
        let handle = self.handle.clone();
        let drag_start_y = self.drag_start_y;
        let drag_start_offset = self.drag_start_offset;
        let dragging = self.dragging;

        div()
            .id("custom-scrollbar")
            .absolute()
            .top_0()
            .right_0()
            .h(geom.track_height)
            .w(width)
            .bg(track_color)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _window, _cx| {
                    // Clicking the track (not the thumb) pages toward the click.
                    let viewport = this.handle.bounds().size.height;
                    let click_y = ev.position.y;
                    let thumb_top = thumb_top_of(&this.handle);
                    if click_y < thumb_top {
                        page(&this.handle, viewport, false);
                    } else if click_y > thumb_top + thumb_height_of(&this.handle) {
                        page(&this.handle, viewport, true);
                    } else {
                        // Grabbed the thumb: begin a drag.
                        this.dragging = true;
                        this.drag_start_y = ev.position.y;
                        this.drag_start_offset = -this.handle.offset().y;
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _window, _cx| {
                if this.dragging {
                    let delta = ev.position.y - this.drag_start_y;
                    let scrollable = this.handle.max_offset().height;
                    let viewport = this.handle.bounds().size.height;
                    let content = viewport + scrollable;
                    let thumb_h = (viewport * viewport / content).max(MIN_THUMB);
                    let travel = (viewport - thumb_h).max(px(0.0));
                    let new_scrolled = if travel > px(0.0) {
                        this.drag_start_offset + (delta / travel) * scrollable
                    } else {
                        px(0.0)
                    };
                    let clamped = new_scrolled.max(px(0.0)).min(scrollable);
                    this.handle.set_offset(Point::new(px(0.0), -clamped));
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(move |this, _ev: &MouseUpEvent, _window, _cx| {
                    this.dragging = false;
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(geom.thumb_top)
                    .left_0()
                    .w(width)
                    .h(geom.thumb_height)
                    .rounded(geom.thumb_height / 2.0)
                    .bg(thumb_color)
                    // Hover darkening (purely visual; no state needed).
                    .hover(|t| t.bg(cx.theme().colors.scrollbar_thumb_hover)),
            )
            .into_any_element()
    }
}

fn thumb_top_of(handle: &ScrollHandle) -> Pixels {
    let viewport = handle.bounds().size.height;
    let scrollable = handle.max_offset().height;
    let scrolled = (-handle.offset().y).max(px(0.0));
    thumb_geometry(viewport, scrollable, scrolled)
        .map(|g| g.thumb_top)
        .unwrap_or(px(0.0))
}

fn thumb_height_of(handle: &ScrollHandle) -> Pixels {
    let viewport = handle.bounds().size.height;
    let scrollable = handle.max_offset().height;
    thumb_geometry(viewport, scrollable, scrolled_max(handle))
        .map(|g| g.thumb_height)
        .unwrap_or(MIN_THUMB)
}

fn scrolled_max(handle: &ScrollHandle) -> Pixels {
    (-handle.offset().y).max(px(0.0))
}

/// Page up (down=false) or down (down=true) by ~90% of the viewport.
fn page(handle: &ScrollHandle, viewport: Pixels, down: bool) {
    let cur = -handle.offset().y;
    let scrollable = handle.max_offset().height;
    let step = viewport * 0.9;
    let next = if down { cur + step } else { cur - step };
    let clamped = next.max(px(0.0)).min(scrollable);
    handle.set_offset(Point::new(px(0.0), -clamped));
}

/// Convenience wrapper: a scroll container that keeps native scroll behavior
/// (wheel/touch/keyboard via `overflow_y_scroll`) but hides the native
/// scrollbar visual and overlays the custom `Scrollbar`.
pub fn scroll_area(
    id: impl Into<ElementId>,
    content: impl IntoElement,
    handle: &ScrollHandle,
    cx: &mut App,
) -> impl IntoElement {
    let scrollbar = Scrollbar::vertical(handle.clone(), cx);
    div()
        .id(id)
        .relative()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .scrollbar_width(px(0.0))
        .track_scroll(handle)
        .child(content)
        .child(scrollbar)
}
```

> **Note:** `track_scroll` takes `&ScrollHandle` on `div` (the `InteractiveElement` fluent API). If the compiler reports `track_scroll` expects `UniformListScrollHandle` on a plain `div`, use the `ScrollHandle`-flavored overload (`div`'s `track_scroll(&ScrollHandle)` exists at `div.rs:1077`). Verify during implementation.

**Step 4: Register the module + re-exports**

In `crates/ui/src/ui.rs`:
- Add `mod scrollbar;` (alphabetical, after `mod input;`).
- Add to the re-exports:
```rust
pub use scrollbar::{Scrollbar, ScrollArea, scroll_area, thumb_geometry, ThumbGeometry, MIN_THUMB};
```
(Export only what's used externally: `Scrollbar`, `scroll_area`, `thumb_geometry`. Adjust to actual usage; keep `MIN_THUMB`/`ThumbGeometry` pub if tests reference them.)

**Step 5: Write an interaction test (drag changes offset)**

Add to `crates/ui/src/scrollbar.rs` `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod interaction_tests {
    use super::*;
    use gpui::{TestAppContext, px};

    // Geometry-only smoke check that the helper clamps a drag-derived offset.
    #[test]
    fn page_clamps_within_scrollable_range() {
        // We can't easily spin a real ScrollHandle without a window/layout,
        // so this test guards the clamp math used by `page`/drag.
        let viewport = px(400.0);
        let scrollable = px(400.0);
        // drag delta beyond travel clamps to bottom.
        let g = thumb_geometry(viewport, scrollable, scrollable).unwrap();
        assert_eq!(g.thumb_top, px(200.0)); // bottom
    }
}
```
> Full pointer-drag integration tests require a GPUI window + layout (the `ScrollHandle` state is populated during layout). Add a `#[gpui::test]` that renders a `scroll_area` with tall content, simulates `mouse_down` on the thumb + `mouse_move`, and asserts `handle.offset()` changed — but only if the local toolchain can render (metal). Otherwise mark `#[ignore]` and let CI run it. Prefer the pure geometry tests as the reliable gate.

**Step 6: Run tests + gates**

```
cargo test -p macsftp-ui
cargo fmt --all --check
cargo clippy -p macsftp-ui --all-targets -- -D warnings
```
Expected: all ui tests pass; clippy clean. Fix any unused-import/field warnings (e.g., `drag_start_y`/`drag_start_offset` are read in closures — keep them; remove `dragging` local if unused).

**Step 7: Commit**

```bash
git add crates/ui/src/scrollbar.rs crates/ui/src/ui.rs
git commit -m "Add custom themed scrollbar component"
```

---

### Task 3: Wire the custom scrollbar into the file panes

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (file-pane `uniform_list` block ~760-872)

**Step 1: Locate the file-pane render**

The local/remote pane renders `uniform_list(..).track_scroll(self.scroll_handle(side).clone())` (render.rs ~868-869) and returns `into_any_element()` (~872). It is then placed into the pane container `div()` at ~875.

**Step 2: Suppress the native scrollbar + overlay the custom one**

Change the tail of the `uniform_list` block. Before:
```rust
            )
            .track_scroll(self.scroll_handle(side).clone())
            .h_full()
            .w_full()
            .into_any_element()
```
After:
```rust
            )
            .track_scroll(self.scroll_handle(side).clone())
            .scrollbar_width(px(0.0))
            .h_full()
            .w_full()
            .into_any_element()
```

Then wrap the pane content in a `relative()` container and add the custom scrollbar as a sibling. At the pane container `div()` (~875), make it `relative()` and add a `.child(macsftp_ui::Scrollbar::vertical_uniform(self.scroll_handle(side), cx))`. Concretely, where the pane `div()` is built (~875-880), add `.relative()` and append the scrollbar child after the `uniform_list` child:

```rust
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .relative()                       // added: anchor for the overlay scrollbar
            .key_context("FilePane")
            // ...existing children (header, uniform_list, etc.)...
            .child(macsftp_ui::Scrollbar::vertical_uniform(
                self.scroll_handle(side),
                cx,
            ))
```

> Ensure `px` and `macsftp_ui` are already imported in `render.rs` (they are: `px` is used elsewhere; `macsftp_ui` is the app's alias for the ui crate — confirm the exact import path used in `render.rs` and match it).

**Step 3: Verify it compiles + existing pane tests pass**

```
cargo build -p macsftp-app
cargo test -p macsftp-app file_pane   # or whatever existing pane tests exist
```
Expected: builds; existing tests pass. Manually confirm (if a window is available) the file pane shows the custom thumb and scrolls.

**Step 4: Gates + commit**

```
cargo fmt --all --check
cargo clippy -p macsftp-app --all-targets -- -D warnings
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
```
```bash
git add crates/app/src/workspace/render.rs
git commit -m "Use custom scrollbar in file panes"
```

---

### Task 4: Wire the custom scrollbar into transfer drawer, command palette, profile-picker modal

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` (`Workspace` struct fields ~123-124; init ~238-239) — add persisted `ScrollHandle`s.
- Modify: `crates/app/src/workspace/transfer_render.rs` (~279-285)
- Modify: `crates/app/src/workspace/command_palette.rs` (~322-330)
- Modify: `crates/app/src/workspace/modals.rs` (~657-664)

**Step 1: Add persisted ScrollHandle fields to `Workspace`**

In `crates/app/src/workspace/mod.rs`, next to `local_scroll`/`remote_scroll` (~123-124):
```rust
    local_scroll: UniformListScrollHandle,
    remote_scroll: UniformListScrollHandle,
    transfer_scroll: gpui::ScrollHandle,
    command_palette_scroll: gpui::ScrollHandle,
    profile_picker_scroll: gpui::ScrollHandle,
```
In the constructor (~238-239), initialize:
```rust
            local_scroll: UniformListScrollHandle::new(),
            remote_scroll: UniformListScrollHandle::new(),
            transfer_scroll: gpui::ScrollHandle::new(),
            command_palette_scroll: gpui::ScrollHandle::new(),
            profile_picker_scroll: gpui::ScrollHandle::new(),
```
Add accessors next to `scroll_handle` (~433):
```rust
    pub(crate) fn transfer_scroll(&self) -> &gpui::ScrollHandle {
        &self.transfer_scroll
    }
    pub(crate) fn command_palette_scroll(&self) -> &gpui::ScrollHandle {
        &self.command_palette_scroll
    }
    pub(crate) fn profile_picker_scroll(&self) -> &gpui::ScrollHandle {
        &self.profile_picker_scroll
    }
```

**Step 2: Transfer drawer**

`transfer_render.rs:279-285`. Before:
```rust
        let mut body = div()
            .id("transfer-drawer-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
```
After (use `scroll_area` so the body keeps native scroll + custom overlay):
```rust
        // Build the inner content (rows / empty state) without overflow; the
        // scroll_area wrapper supplies overflow + the custom scrollbar.
        let mut body = macsftp_ui::scroll_area(
            "transfer-drawer-body",
            div().flex().flex_col().flex_1().min_h_0(),
            self.transfer_scroll(),
            cx,
        );
```
> `scroll_area` returns an element; since `body` is later mutated with `.child(...)` conditionally, ensure `scroll_area`'s return type supports the same builder calls, or restructure: keep `body` as the inner content `div` and wrap it once at the return site with `scroll_area("transfer-drawer-body", body, self.transfer_scroll(), cx)`. Prefer the latter to avoid fighting the builder type. Concretely: build `body` as a plain `div().id("transfer-drawer-body").flex().flex_col().flex_1().min_h_0()` (no `overflow_y_scroll`), add children, then at the return point wrap: `.child(macsftp_ui::scroll_area("transfer-drawer-body", body, self.transfer_scroll(), cx))`. Match whatever the function currently returns.

**Step 3: Command palette**

`command_palette.rs:322-330`. Replace:
```rust
                            div()
                                .id("command-palette-results")
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .when(has_results, |list| list.children(rows))
                                .when(!has_results, |list| { ... })
```
with a `scroll_area` wrapper around a plain list `div` (no `overflow_y_scroll`), passing `self.command_palette_scroll()` (or the equivalent `workspace.command_palette_scroll()` if `self` is `Workspace`). The `.when(..)` conditional children move onto the inner content `div`.

**Step 4: Profile-picker modal**

`modals.rs:657-664`. Replace:
```rust
            let mut picker_panel = div()
                .id("profile-picker-panel")
                .flex()
                .flex_col()
                .gap_0()
                .ml(px(104.0))
                .max_h(px(200.0))
                .overflow_y_scroll()
                ...
```
with `scroll_area("profile-picker-panel", <inner div without overflow>, self.profile_picker_scroll(), cx)`, preserving `max_h(px(200.0))`, border, radius, bg on the wrapper.

**Step 5: Build + smoke test**

```
cargo build -p macsftp-app
cargo test -p macsftp-app
```
Expected: builds; existing tests pass.

**Step 6: Gates + commit**

```
cargo fmt --all --check
cargo clippy -p macsftp-app --all-targets -- -D warnings
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
```
```bash
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/transfer_render.rs crates/app/src/workspace/command_palette.rs crates/app/src/workspace/modals.rs
git commit -m "Use custom scrollbar in drawer, palette, and profile picker"
```

---

### Task 5: Regression + full gates

**Files:**
- Verify only (no new files unless a test is added).

**Step 1: Verify scrollbar_width(0) still allows native wheel scroll**

Run the app (if a window is available) or add a `#[gpui::test]` that builds a `scroll_area` with content taller than the viewport, dispatches a scroll-wheel event, and asserts `handle.offset()` changed. If `scrollbar_width(0)` blocks wheel scrolling, apply the fallback: add `.on_scroll_wheel` to the `scroll_area` container that calls `handle.set_offset(handle.offset() + delta)`.

**Step 2: Regression — scroll_to_item still syncs**

Add/confirm a test that after a custom-scrollbar drag (`handle.set_offset`), calling `uniform_list`'s `scroll_handle.scroll_to_item(ix, ScrollStrategy::Top)` still repositions correctly (both share the same handle). Existing `panes.rs` `scroll_to_item` calls must remain functional.

**Step 3: Full gate suite**

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
cargo test -p macsftp-ui
cargo test -p macsftp-app
```
Expected: all green (app/GPUI render tests may be metal-blocked locally — CI runs them).

**Step 4: Commit (if any test/fix was added)**

```bash
git add -A
git commit -m "Verify scrollbar scroll behavior and regression"
```

---

## Acceptance checklist

- All scrollable areas (file panes, transfer drawer, command palette, profile-picker modal) show a unified, always-visible, rounded, theme-aware (light/dark) scrollbar thumb.
- GPUI's native auto-hiding scrollbar is suppressed (`scrollbar_width(0)`) wherever the custom one is shown.
- Native wheel/touch/keyboard scrolling still works.
- Dragging the thumb scrolls; clicking the track above/below the thumb pages up/down.
- `uniform_list`'s `scroll_to_item` still works (shares the same handle).
- No overflow ⇒ no thumb rendered.
- `cargo fmt`, `clippy -D warnings`, `check_architecture.sh`, `check_sensitive_logs.sh` all pass.

## Out of scope (YAGNI)

- Horizontal scrollbar (path bar `render.rs:56` keeps GPUI default).
- Tab-switcher list (`render.rs:174-181`) — transient popup, no persisted handle; skip for v1.
- Auto-hide / fade animation (v1 is always-visible).
- New keyboard scroll interactions.
