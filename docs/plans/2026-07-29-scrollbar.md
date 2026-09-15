# 自定义主题滚动条实施计划

**完成状态：** 已在 `feat/custom-scrollbar` 分支实现。最终集成还涵盖 MRU tab switcher，并且增加了持久化 `ScrollbarState` 同步、轨道外拖动捕获、即时重绘测试和 tab-switcher overflow 回归测试。

> **Agent 执行要求：** 必须使用 `superpowers:executing-plans`，并且按任务实施本计划。

**目标：** 在全部可滚动区域中，以统一、始终可见并且支持浅色和深色主题的自定义滚动条替代 GPUI 的窄型自动隐藏原生滚动条。这些区域包括文件 pane、传输 drawer、command palette 和 profile-picker modal。

**架构：** `crates/ui` 中可复用且有状态的 `Scrollbar` view 从 `ScrollHandle` 的 `offset()`、`max_offset()` 和 `bounds()` 读取滚动几何信息，并根据 `cx.theme().colors` 绘制轨道与圆角 thumb。容器区域保留 GPUI 的原生滚动行为，即通过 `overflow_y_scroll()` 支持滚轮、触控和键盘；但是，容器通过 `scrollbar_width(px(0))` 隐藏原生视觉元素，并在其上叠加绑定同一 handle 的自定义 `Scrollbar`。自定义组件负责拖动和轨道单击，而滚轮与触控仍由原生机制处理。

**技术栈：** Rust、GPUI 0.2.2、负责 theme 与 component 的 `crates/ui`、负责 workspace view 的 `crates/app`，以及采用 GPUI `test-support` 的 TDD 流程。

**设计文档：** `docs/plans/2026-07-29-scrollbar-design.md` 是权威设计说明，而本文档负责拆分实施任务。

**分支：** `feat/custom-scrollbar` 已经创建，并且设计文档对应的提交为 `f9fc165`。

**已经验证的 GPUI 0.2.2 事实：**
- `ScrollHandle` 提供 `offset() -> Point<Pixels>`、`max_offset() -> Size<Pixels>`、`bounds() -> Bounds<Pixels>` 和 `set_offset(Point<Pixels>)`。向下滚动时 y 为负值，而最大可滚动距离等于 content 减去 viewport。
- `UniformListScrollHandle(pub Rc<RefCell<UniformListScrollState>>)` 包含 `UniformListScrollState { pub base_handle: ScrollHandle, .. }`。因此，通过 `handle.0.borrow().base_handle.clone()` 读取基础 handle。
- `UniformList` 实现 `InteractiveElement + Styled`。因此，`uniform_list(..)` 可以使用 `.scrollbar_width(px(0.))` 和 `.track_scroll(..)`。
- `uniform_list` 在内部设置 `overflow.y = Scroll`，因此会绘制自动隐藏的原生滚动条。使用 `scrollbar_width(px(0.))` 可以隐藏该滚动条。
- `crates/ui/src/theme.rs` 中的 `Theme` 是 GPUI `Global`，并且 `ActiveTheme` trait 提供 `cx.theme()`。`ThemeColors` 和 `ThemeSizes` 均使用 `#[derive(Clone, Copy)]`。

**质量检查：** 每个修改代码的任务完成后，执行以下命令：
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
```
如果本地环境缺少 Xcode `metal`，App/GPUI rendering tests 可能无法执行。因此，本地环境应执行 `cargo test -p macsftp-ui` 以验证纯逻辑测试，其余测试由 CI 执行。

---

### Task 1：定义滚动条 theme token

**文件：**
- 修改：`crates/ui/src/theme.rs`，涉及 `ThemeColors` struct 约 24-46 行、`ThemeSizes` 约 56-63 行、`one_dark()` 约 67-91 行、`one_light()` 约 94-118 行、`default_sizes()` 约 144-153 行以及 tests 约 166-221 行。

**Step 1：编写预期失败的测试**

在 `crates/ui/src/theme.rs` 的 `#[cfg(test)] mod tests` block 中增加以下代码：

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

**Step 2：执行测试并确认其失败**

执行：`cargo test -p macsftp-ui scrollbar_tokens_are_defined_and_distinct_per_appearance`

预期结果为 FAIL，并且编译错误为 `no field scrollbar_thumb on type ThemeColors`。

**Step 3：编写最小实现**

在 `ThemeColors` 的 `info: Hsla,` 之后增加：
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

在 `ThemeSizes` 的 `status_bar_height: Pixels,` 之后增加：
```rust
    /// Width of the custom scrollbar (track + thumb).
    pub scrollbar_width: Pixels,
```

在 `one_dark()` 的 `ThemeColors { .. }` 中，于 `info: rgb(0x56b6c2).into(),` 之后增加：
```rust
                scrollbar_thumb: hsla(0.0, 0.0, 1.0, 0.22),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 1.0, 0.36),
                scrollbar_thumb_active: hsla(0.0, 0.0, 1.0, 0.50),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
```

在 `one_light()` 的 `ThemeColors { .. }` 中，于 `info: rgb(0x0184bc).into(),` 之后增加：
```rust
                scrollbar_thumb: hsla(0.0, 0.0, 0.0, 0.30),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 0.0, 0.45),
                scrollbar_thumb_active: hsla(0.0, 0.0, 0.0, 0.55),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
```

在 `default_sizes()` 的 `ThemeSizes { .. }` 中，于 `status_bar_height: px(26.0),` 之后增加：
```rust
        scrollbar_width: px(10.0),
```

**Step 4：执行测试并确认其通过**

执行：`cargo test -p macsftp-ui scrollbar_tokens_are_defined_and_distinct_per_appearance`

预期结果为 PASS。然后执行 `cargo test -p macsftp-ui`，并确认全部 theme test 仍然通过。

**Step 5：执行质量检查并提交变更**

```
cargo fmt --all --check
cargo clippy -p macsftp-ui --all-targets -- -D warnings
```
```bash
git add crates/ui/src/theme.rs
git commit -m "Add scrollbar theme tokens"
```

---

### Task 2：实现 Scrollbar component 与 ScrollArea helper

**文件：**
- 创建：`crates/ui/src/scrollbar.rs`
- 修改：`crates/ui/src/ui.rs`，增加 `mod scrollbar;`，并在约 1-25 行增加 re-export。

**Step 1：编写 geometry 测试**

创建包含空 module 和 `#[cfg(test)]` block 的 `crates/ui/src/scrollbar.rs`。首先增加纯 geometry helper，以形成可执行单元测试的边界：

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

**Step 2：执行测试并确认其通过**

执行：`cargo test -p macsftp-ui thumb_`

预期结果为 PASS，共 5 个测试。Geometry helper 是纯函数，因此可以立即编译并通过；预期失败测试的规则适用于后续 interactive `Scrollbar` view。

**Step 3：实现 `Scrollbar` view 与 `ScrollArea` helper**

在 `crates/ui/src/scrollbar.rs` 的 `#[cfg(test)]` block 之前增加以下代码：

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

> **说明：** `div` 的 `track_scroll` 通过 `InteractiveElement` fluent API 接受 `&ScrollHandle`。如果编译器报告普通 `div` 的 `track_scroll` 需要 `UniformListScrollHandle`，那么使用接受 `ScrollHandle` 的 overload；`div` 的 `track_scroll(&ScrollHandle)` 位于 `div.rs:1077`。实施时需要验证该行为。

**Step 4：注册 module 并增加 re-export**

在 `crates/ui/src/ui.rs` 中执行以下修改：
- 按字母顺序在 `mod input;` 之后增加 `mod scrollbar;`。
- 在 re-export 中增加：
```rust
pub use scrollbar::{Scrollbar, ScrollArea, scroll_area, thumb_geometry, ThumbGeometry, MIN_THUMB};
```
只导出外部实际使用的 `Scrollbar`、`scroll_area` 和 `thumb_geometry`。根据实际使用情况调整该列表；但是，如果测试引用 `MIN_THUMB` 或 `ThumbGeometry`，则保留其 `pub` 可见性。

**Step 5：编写拖动改变 offset 的交互测试**

在 `crates/ui/src/scrollbar.rs` 的 `#[cfg(test)] mod tests` 中增加以下代码：

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
> 完整的 pointer-drag integration test 需要 GPUI window 与 layout，因为 `ScrollHandle` state 会在 layout 期间生成。如果本地 toolchain 能够通过 `metal` 渲染，则增加一个 `#[gpui::test]`：渲染包含高 content 的 `scroll_area`，模拟 thumb 上的 `mouse_down` 和后续 `mouse_move`，然后断言 `handle.offset()` 已经变化。否则，将该测试标记为 `#[ignore]` 并由 CI 执行。纯 geometry test 是稳定的质量检查依据，因此应优先使用。

**Step 6：执行测试与质量检查**

```
cargo test -p macsftp-ui
cargo fmt --all --check
cargo clippy -p macsftp-ui --all-targets -- -D warnings
```
预期全部 UI 测试通过，并且 clippy 没有警告。处理全部 unused import 或 field warning。例如，closure 会读取 `drag_start_y` 和 `drag_start_offset`，因此需要保留这两个字段；如果局部变量 `dragging` 未使用，则移除该变量。

**Step 7：提交变更**

```bash
git add crates/ui/src/scrollbar.rs crates/ui/src/ui.rs
git commit -m "Add custom themed scrollbar component"
```

---

### Task 3：在文件 pane 中集成自定义滚动条

**文件：**
- 修改：`crates/app/src/workspace/render.rs` 中约 760-872 行的 file-pane `uniform_list` block。

**Step 1：确认 file-pane render 的位置**

local/remote pane 在 `render.rs` 约 868-869 行渲染 `uniform_list(..).track_scroll(self.scroll_handle(side).clone())`，并在约 872 行返回 `into_any_element()`。随后，该 element 会置于约 875 行的 pane container `div()` 中。

**Step 2：隐藏原生滚动条并叠加自定义滚动条**

修改 `uniform_list` block 的末尾。修改前：
```rust
            )
            .track_scroll(self.scroll_handle(side).clone())
            .h_full()
            .w_full()
            .into_any_element()
```
修改后：
```rust
            )
            .track_scroll(self.scroll_handle(side).clone())
            .scrollbar_width(px(0.0))
            .h_full()
            .w_full()
            .into_any_element()
```

然后使用 `relative()` container 包含 pane content，并增加与 content 同级的自定义滚动条。在约 875 行的 pane container `div()` 上增加 `relative()`，并增加 `.child(macsftp_ui::Scrollbar::vertical_uniform(self.scroll_handle(side), cx))`。具体而言，在约 875-880 行构建 pane `div()` 时增加 `.relative()`，然后在 `uniform_list` child 之后增加 scrollbar child：

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

> 确认 `render.rs` 已经导入 `px` 和 `macsftp_ui`。该文件的其他位置已经使用 `px`，而 `macsftp_ui` 是 app 对 ui crate 的别名；但是，仍需确认 `render.rs` 使用的准确 import path，并与其保持一致。

**Step 3：确认编译成功且现有 pane test 通过**

```
cargo build -p macsftp-app
cargo test -p macsftp-app file_pane   # or whatever existing pane tests exist
```
预期编译成功，并且现有测试通过。如果可以显示 window，则人工确认文件 pane 显示自定义 thumb，并且滚动功能正常。

**Step 4：执行质量检查并提交变更**

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

### Task 4：在 transfer drawer、command palette 和 profile-picker modal 中集成自定义滚动条

**文件：**
- 修改：`crates/app/src/workspace/mod.rs` 中约 123-124 行的 `Workspace` struct fields 和约 238-239 行的初始化代码，并增加持久化 `ScrollHandle`。
- 修改：`crates/app/src/workspace/transfer_render.rs` 约 279-285 行。
- 修改：`crates/app/src/workspace/command_palette.rs` 约 322-330 行。
- 修改：`crates/app/src/workspace/modals.rs` 约 657-664 行。

**Step 1：为 `Workspace` 增加持久化 ScrollHandle 字段**

在 `crates/app/src/workspace/mod.rs` 约 123-124 行的 `local_scroll` 和 `remote_scroll` 旁边增加：
```rust
    local_scroll: UniformListScrollHandle,
    remote_scroll: UniformListScrollHandle,
    transfer_scroll: gpui::ScrollHandle,
    command_palette_scroll: gpui::ScrollHandle,
    profile_picker_scroll: gpui::ScrollHandle,
```
在约 238-239 行的 constructor 中初始化：
```rust
            local_scroll: UniformListScrollHandle::new(),
            remote_scroll: UniformListScrollHandle::new(),
            transfer_scroll: gpui::ScrollHandle::new(),
            command_palette_scroll: gpui::ScrollHandle::new(),
            profile_picker_scroll: gpui::ScrollHandle::new(),
```
在约 433 行的 `scroll_handle` 旁边增加 accessor：
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

**Step 2：修改 transfer drawer**

修改 `transfer_render.rs:279-285`。修改前：
```rust
        let mut body = div()
            .id("transfer-drawer-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
```
修改后如下。使用 `scroll_area`，因此 body 会保留原生滚动行为并叠加自定义滚动条：
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
> `scroll_area` 返回 element。由于后续代码会根据条件通过 `.child(...)` 修改 `body`，因此需要确认 `scroll_area` 的返回类型支持相同的 builder call。也可以调整结构：将 `body` 保留为内部 content `div`，然后在返回位置使用 `scroll_area("transfer-drawer-body", body, self.transfer_scroll(), cx)` 包含该 element。后一种方式不会受 builder type 限制，因此优先采用。具体实现是先构建不含 `overflow_y_scroll` 的普通 `div().id("transfer-drawer-body").flex().flex_col().flex_1().min_h_0()`，再增加 children，最后在返回位置使用 `.child(macsftp_ui::scroll_area("transfer-drawer-body", body, self.transfer_scroll(), cx))`。最终结构需要与函数当前的返回值保持一致。

**Step 3：修改 command palette**

在 `command_palette.rs:322-330` 中替换以下代码：
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
使用 `scroll_area` 包含不含 `overflow_y_scroll` 的普通 list `div`，并传入 `self.command_palette_scroll()`。如果 `self` 是 `Workspace`，则使用等价的 `workspace.command_palette_scroll()`。同时，将 `.when(..)` conditional child 移到内部 content `div`。

**Step 4：修改 profile-picker modal**

在 `modals.rs:657-664` 中替换以下代码：
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
使用 `scroll_area("profile-picker-panel", <inner div without overflow>, self.profile_picker_scroll(), cx)`，并且在 wrapper 上保留 `max_h(px(200.0))`、border、radius 和 bg。

**Step 5：执行编译与 smoke test**

```
cargo build -p macsftp-app
cargo test -p macsftp-app
```
预期编译成功，并且现有测试通过。

**Step 6：执行质量检查并提交变更**

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

### Task 5：执行回归测试与完整质量检查

**文件：**
- 仅验证；除非需要增加测试，否则不创建新文件。

**Step 1：确认 scrollbar_width(0) 仍然支持原生滚轮滚动**

如果可以显示 window，则启动 app 并验证滚轮滚动。否则，增加一个 `#[gpui::test]`：构建 content 高于 viewport 的 `scroll_area`，分派 scroll-wheel event，然后断言 `handle.offset()` 已经变化。如果 `scrollbar_width(0)` 阻止滚轮滚动，则采用备用方案，在 `scroll_area` container 上增加 `.on_scroll_wheel`，并由该 handler 调用 `handle.set_offset(handle.offset() + delta)`。

**Step 2：确认 scroll_to_item 仍然同步**

增加或确认一个测试：通过 `handle.set_offset` 完成自定义滚动条拖动后，调用 `uniform_list` 的 `scroll_handle.scroll_to_item(ix, ScrollStrategy::Top)` 仍然可以正确调整位置，因为二者共享同一个 handle。`panes.rs` 中现有的 `scroll_to_item` 调用必须保持有效。

**Step 3：执行完整质量检查**

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
cargo test -p macsftp-ui
cargo test -p macsftp-app
```
预期全部检查通过。如果本地环境缺少 `metal`，app/GPUI render test 可能无法执行，因此由 CI 执行这些测试。

**Step 4：在增加测试或修复后提交变更**

```bash
git add -A
git commit -m "Verify scrollbar scroll behavior and regression"
```

---

## 验收清单

- 所有可滚动区域，包括 file pane、transfer drawer、command palette 和 profile-picker modal，均显示统一、始终可见、具有圆角并且适配浅色与深色主题的 scrollbar thumb。
- 显示自定义滚动条的区域均通过 `scrollbar_width(0)` 隐藏 GPUI 自动消失的原生滚动条。
- 原生滚轮、触控和键盘滚动仍然有效。
- 拖动 thumb 可以滚动内容，并且单击 thumb 上方或下方的轨道可以向上或向下翻页。
- `uniform_list` 的 `scroll_to_item` 仍然有效，因为它与自定义滚动条共享同一个 handle。
- 没有 overflow 时，不渲染 thumb。
- `cargo fmt`、`clippy -D warnings`、`check_architecture.sh` 和 `check_sensitive_logs.sh` 均通过。

## 范围之外的事项（YAGNI）

- 横向滚动条。path bar 的 `render.rs:56` 保留 GPUI 默认行为。
- Tab-switcher list。`render.rs:174-181` 是没有持久化 handle 的临时 popup，因此 v1 不包含该区域。
- 自动隐藏或 fade animation。v1 的滚动条始终可见。
- 新的键盘滚动交互。
