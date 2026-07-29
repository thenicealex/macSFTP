use std::{cell::Cell, rc::Rc};

use gpui::{
    Context, DragMoveEvent, ElementId, IntoElement, MouseButton, MouseDownEvent, Pixels, Point,
    Render, ScrollHandle, Styled, UniformListScrollHandle, Window, div, prelude::*, px,
};

use crate::theme::ActiveTheme;

/// Minimum thumb height so a tiny thumb stays grabbable.
pub const MIN_THUMB: Pixels = px(24.0);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbGeometry {
    pub thumb_height: Pixels,
    pub thumb_top: Pixels,
    pub track_height: Pixels,
}

/// Compute thumb geometry from viewport height, scrollable distance
/// (`max_offset().height`), and current scrolled distance (`-offset().y`,
/// clamped to `[0, scrollable]`). Returns `None` when content fits viewport.
///
/// `Pixels / Pixels` yields `f32` and `Pixels * Pixels` is unsupported, so the
/// ratio `viewport² / content` is expressed as `viewport * (viewport / content)`,
/// which keeps the result in `Pixels` via `impl Mul<f32> for Pixels`.
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
    // Clamp to track_height so a sub-MIN_THUMB viewport can't produce a thumb
    // taller than the track (which would push thumb_top negative).
    let thumb_height = (viewport_h * (viewport_h / content_h))
        .max(MIN_THUMB)
        .min(track_height);
    let scrolled = scrolled.max(px(0.0)).min(scrollable);
    let thumb_top = (scrolled / scrollable) * (track_height - thumb_height);
    Some(ThumbGeometry {
        thumb_height,
        thumb_top,
        track_height,
    })
}

/// What a click on the scrollbar track should do, based on where it lands
/// relative to the thumb. `click_y` is in track-relative coordinates
/// (0 at the top of the track).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    PageUp,
    PageDown,
    Drag,
}

/// Classify a track-relative click by where it lands relative to the thumb.
/// Used by `on_mouse_down` to decide between paging and starting a drag.
fn classify_click(click_y: Pixels, thumb_top: Pixels, thumb_height: Pixels) -> ClickAction {
    if click_y < thumb_top {
        ClickAction::PageUp
    } else if click_y > thumb_top + thumb_height {
        ClickAction::PageDown
    } else {
        ClickAction::Drag
    }
}

#[derive(Clone)]
struct ScrollbarDrag {
    handle: ScrollHandle,
    start_y: Rc<Cell<Pixels>>,
    start_offset: Rc<Cell<Pixels>>,
}

struct ScrollbarDragGhost;

impl Render for ScrollbarDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.0)).h(px(1.0))
    }
}

/// A vertical, always-visible, theme-aware scrollbar overlaid on a scroll
/// container. Bind it to the same `ScrollHandle` the container tracks.
pub struct Scrollbar;

/// Per-scroll-area render synchronization. Keep one instance alongside each
/// scroll handle so a zero-sized or deferred layout schedules at most one
/// follow-up render until valid geometry is observed.
#[derive(Clone, Default)]
pub struct ScrollbarState {
    layout_sync_scheduled: Rc<Cell<bool>>,
}

impl ScrollbarState {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Scrollbar {
    pub fn vertical<V: 'static>(
        id: impl Into<ElementId>,
        handle: ScrollHandle,
        state: &ScrollbarState,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> impl IntoElement {
        render_scrollbar(id.into(), handle, state, false, window, cx)
    }

    pub fn vertical_uniform<V: 'static>(
        id: impl Into<ElementId>,
        handle: &UniformListScrollHandle,
        state: &ScrollbarState,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> impl IntoElement {
        let list_state = handle.0.borrow();
        let base = list_state.base_handle.clone();
        let has_deferred_scroll = list_state.deferred_scroll_to_item.is_some();
        drop(list_state);
        render_scrollbar(id.into(), base, state, has_deferred_scroll, window, cx)
    }
}

fn schedule_next_render<V: 'static>(
    state: &ScrollbarState,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    if state.layout_sync_scheduled.replace(true) {
        return;
    }
    let view = cx.entity();
    window.defer(cx, move |_window, cx| {
        view.update(cx, |_view, cx| cx.notify());
    });
}

fn render_scrollbar<V: 'static>(
    id: ElementId,
    handle: ScrollHandle,
    state: &ScrollbarState,
    has_deferred_layout: bool,
    window: &mut Window,
    cx: &mut Context<V>,
) -> gpui::AnyElement {
    let viewport_h = handle.bounds().size.height;
    if viewport_h <= px(0.0) || has_deferred_layout {
        // ScrollHandle geometry is populated during layout, after this parent
        // render. One follow-up render makes an overflowing scrollbar visible
        // on its first presented frame without maintaining a parallel model.
        schedule_next_render(state, window, cx);
    } else {
        state.layout_sync_scheduled.set(false);
    }

    let scrollable = handle.max_offset().height;
    let scrolled = (-handle.offset().y).max(px(0.0));
    let Some(geometry) = thumb_geometry(viewport_h, scrollable, scrolled) else {
        return div().into_any_element();
    };

    let theme = cx.theme();
    let width = theme.sizes.scrollbar_width;
    let thumb_color = theme.colors.scrollbar_thumb;
    let thumb_hover = theme.colors.scrollbar_thumb_hover;
    let thumb_active = theme.colors.scrollbar_thumb_active;
    let track_color = theme.colors.scrollbar_track;
    let thumb_id = ElementId::NamedChild(Box::new(id.clone()), "thumb".into());
    let track_debug_selector = id.to_string();
    let thumb_debug_selector = thumb_id.to_string();
    let click_handle = handle.clone();
    let drag = ScrollbarDrag {
        handle: handle.clone(),
        start_y: Rc::new(Cell::new(px(0.0))),
        start_offset: Rc::new(Cell::new(px(0.0))),
    };

    div()
        .id(id)
        .debug_selector(move || track_debug_selector)
        .absolute()
        .top_0()
        .right_0()
        .h(geometry.track_height)
        .w(width)
        .bg(track_color)
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_view, event: &MouseDownEvent, _window, cx| {
                let viewport = click_handle.bounds().size.height;
                let scrollable = click_handle.max_offset().height;
                let scrolled = (-click_handle.offset().y).max(px(0.0));
                let Some(geometry) = thumb_geometry(viewport, scrollable, scrolled) else {
                    return;
                };
                let click_y = event.position.y - click_handle.bounds().origin.y;
                match classify_click(click_y, geometry.thumb_top, geometry.thumb_height) {
                    ClickAction::PageUp => page(&click_handle, viewport, false),
                    ClickAction::PageDown => page(&click_handle, viewport, true),
                    ClickAction::Drag => return,
                }
                cx.notify();
            }),
        )
        .on_drag_move(cx.listener(
            move |_view, event: &DragMoveEvent<ScrollbarDrag>, _window, cx| {
                let (handle, start_y, start_offset) = {
                    let drag = event.drag(cx);
                    (
                        drag.handle.clone(),
                        drag.start_y.get(),
                        drag.start_offset.get(),
                    )
                };
                let viewport = handle.bounds().size.height;
                let scrollable = handle.max_offset().height;
                let scrolled = dragged_scroll_position(
                    viewport,
                    scrollable,
                    start_offset,
                    event.event.position.y - start_y,
                );
                handle.set_offset(Point::new(px(0.0), -scrolled));
                cx.notify();
            },
        ))
        .child(
            div()
                .id(thumb_id)
                .debug_selector(move || thumb_debug_selector)
                .absolute()
                .top(geometry.thumb_top)
                .left_0()
                .w(width)
                .h(geometry.thumb_height)
                .rounded(geometry.thumb_height / 2.0)
                .bg(thumb_color)
                .hover(|thumb| thumb.bg(thumb_hover))
                .active(|thumb| thumb.bg(thumb_active))
                .on_drag(drag, |drag, _offset, window, cx| {
                    drag.start_y.set(window.mouse_position().y);
                    drag.start_offset
                        .set((-drag.handle.offset().y).max(px(0.0)));
                    cx.new(|_| ScrollbarDragGhost)
                }),
        )
        .into_any_element()
}

fn dragged_scroll_position(
    viewport: Pixels,
    scrollable: Pixels,
    drag_start_offset: Pixels,
    delta: Pixels,
) -> Pixels {
    let Some(geometry) = thumb_geometry(viewport, scrollable, drag_start_offset) else {
        return px(0.0);
    };
    let travel = (viewport - geometry.thumb_height).max(px(0.0));
    if travel <= px(0.0) {
        return px(0.0);
    }
    (drag_start_offset + (delta / travel) * scrollable)
        .max(px(0.0))
        .min(scrollable)
}

/// Page the scroll handle by ~90% of the viewport in the given direction.
fn page(handle: &ScrollHandle, viewport: Pixels, down: bool) {
    let cur = (-handle.offset().y).max(px(0.0));
    let scrollable = handle.max_offset().height;
    let step = viewport * 0.9;
    let next = if down { cur + step } else { cur - step };
    let clamped = next.max(px(0.0)).min(scrollable);
    handle.set_offset(Point::new(px(0.0), -clamped));
}

/// A relative scroll container that suppresses the native scrollbar (via
/// `scrollbar_width(0)`) and overlays a `Scrollbar` bound to the same handle.
pub fn scroll_area<V: 'static>(
    id: impl Into<ElementId>,
    content: impl IntoElement,
    handle: &ScrollHandle,
    state: &ScrollbarState,
    window: &mut Window,
    cx: &mut Context<V>,
) -> impl IntoElement {
    let content_id = id.into();
    let scrollbar_id = ElementId::NamedChild(Box::new(content_id.clone()), "scrollbar".into());
    let scrollbar = Scrollbar::vertical(scrollbar_id, handle.clone(), state, window, cx);
    div()
        .flex()
        .flex_col()
        .relative()
        .flex_1()
        .min_h_0()
        .child(
            div()
                .id(content_id)
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .scrollbar_width(px(0.0))
                .track_scroll(handle)
                .child(content),
        )
        .child(scrollbar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Range;

    use gpui::{
        Modifiers, ParentElement, Render, ScrollDelta, ScrollWheelEvent, Styled, TestAppContext,
        Window, div, point, size, uniform_list,
    };

    use crate::theme::Theme;

    struct ScrollAreaTestView {
        handle: ScrollHandle,
        scrollbar: ScrollbarState,
    }

    impl Render for ScrollAreaTestView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().flex().w(px(200.0)).h(px(200.0)).child(scroll_area(
                "test-scroll-area",
                div().flex_none().w_full().h(px(1_000.0)),
                &self.handle,
                &self.scrollbar,
                window,
                cx,
            ))
        }
    }

    struct UniformListTestView {
        handle: UniformListScrollHandle,
        scrollbar: ScrollbarState,
    }

    struct ZeroHeightScrollTestView {
        handle: ScrollHandle,
        scrollbar: ScrollbarState,
        render_count: Rc<Cell<usize>>,
    }

    impl Render for ZeroHeightScrollTestView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.render_count.set(self.render_count.get() + 1);
            div().h(px(0.0)).child(scroll_area(
                "zero-height-scroll-area",
                div().h(px(1_000.0)),
                &self.handle,
                &self.scrollbar,
                window,
                cx,
            ))
        }
    }

    impl Render for UniformListTestView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let mut list = uniform_list(
                "test-uniform-list",
                100,
                cx.processor(|_this, range: Range<usize>, _window, _cx| {
                    range
                        .map(|index| {
                            div()
                                .id(("test-row", index))
                                .flex_none()
                                .h(px(20.0))
                                .child(index.to_string())
                        })
                        .collect()
                }),
            )
            .track_scroll(self.handle.clone())
            .h_full()
            .w_full();
            list.style().scrollbar_width = Some(px(0.0).into());

            div()
                .flex()
                .relative()
                .w(px(200.0))
                .h(px(200.0))
                .child(list)
                .child(Scrollbar::vertical_uniform(
                    "test-uniform-scrollbar",
                    &self.handle,
                    &self.scrollbar,
                    window,
                    cx,
                ))
        }
    }

    fn install_test_theme(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::one_dark()));
    }

    #[test]
    fn no_thumb_when_content_fits_viewport() {
        assert!(thumb_geometry(px(400.0), px(0.0), px(0.0)).is_none());
        assert!(thumb_geometry(px(400.0), px(-10.0), px(0.0)).is_none());
    }
    #[test]
    fn thumb_at_top_when_not_scrolled() {
        let g = thumb_geometry(px(400.0), px(400.0), px(0.0))
            .expect("scrollable content yields a thumb");
        assert_eq!(g.thumb_top, px(0.0));
        assert_eq!(g.thumb_height, px(200.0)); // 400*400/800
        assert_eq!(g.track_height, px(400.0));
    }
    #[test]
    fn thumb_clamps_to_min() {
        let g = thumb_geometry(px(400.0), px(100_000.0), px(0.0))
            .expect("vastly overflowed content yields a thumb");
        assert_eq!(g.thumb_height, MIN_THUMB);
    }
    #[test]
    fn thumb_at_bottom_when_fully_scrolled() {
        let g = thumb_geometry(px(400.0), px(400.0), px(400.0))
            .expect("scrollable content yields a thumb");
        assert_eq!(g.thumb_top, px(200.0));
    }
    #[test]
    fn scrolled_is_clamped_to_scrollable_range() {
        assert_eq!(
            thumb_geometry(px(400.0), px(400.0), px(999.0))
                .expect("scrollable content yields a thumb")
                .thumb_top,
            px(200.0)
        );
        assert_eq!(
            thumb_geometry(px(400.0), px(400.0), px(-50.0))
                .expect("scrollable content yields a thumb")
                .thumb_top,
            px(0.0)
        );
    }
    #[test]
    fn thumb_height_le_track_when_viewport_tiny() {
        // Viewport (10px) smaller than MIN_THUMB (24px). Without the
        // track-height clamp, thumb_height would be MIN_THUMB and thumb_top
        // would go negative. With the clamp it stays <= track_height.
        let g = thumb_geometry(px(10.0), px(100.0), px(0.0))
            .expect("overflowing content yields a thumb");
        assert!(g.thumb_height <= px(10.0));
        assert_eq!(g.thumb_top, px(0.0));
    }
    #[test]
    fn classify_click_pages_above_thumb() {
        assert_eq!(
            classify_click(px(0.0), px(50.0), px(24.0)),
            ClickAction::PageUp
        );
        assert_eq!(
            classify_click(px(49.0), px(50.0), px(24.0)),
            ClickAction::PageUp
        );
    }
    #[test]
    fn classify_click_pages_below_thumb() {
        // Boundary is thumb_top + thumb_height = 74; strictly greater pages down.
        assert_eq!(
            classify_click(px(75.0), px(50.0), px(24.0)),
            ClickAction::PageDown
        );
        assert_eq!(
            classify_click(px(74.0), px(50.0), px(24.0)),
            ClickAction::Drag
        );
    }
    #[test]
    fn classify_click_drags_inside_thumb() {
        assert_eq!(
            classify_click(px(50.0), px(50.0), px(24.0)),
            ClickAction::Drag
        );
        assert_eq!(
            classify_click(px(60.0), px(50.0), px(24.0)),
            ClickAction::Drag
        );
        assert_eq!(
            classify_click(px(74.0), px(50.0), px(24.0)),
            ClickAction::Drag
        );
    }

    #[gpui::test]
    fn track_click_repaints_thumb_immediately(cx: &mut TestAppContext) {
        install_test_theme(cx);
        let handle = ScrollHandle::new();
        let test_handle = handle.clone();
        let (_view, cx) = cx.add_window_view(move |_window, _cx| ScrollAreaTestView {
            handle,
            scrollbar: ScrollbarState::new(),
        });
        cx.simulate_resize(size(px(240.0), px(240.0)));
        cx.run_until_parked();

        let track = cx
            .debug_bounds("test-scroll-area-scrollbar")
            .expect("overflowing content must render a scrollbar track");
        let thumb_before = cx
            .debug_bounds("test-scroll-area-scrollbar-thumb")
            .expect("overflowing content must render a scrollbar thumb");
        cx.simulate_click(
            point(track.center().x, track.bottom() - px(2.0)),
            Modifiers::default(),
        );

        assert!(test_handle.offset().y < px(0.0));
        let thumb_after = cx
            .debug_bounds("test-scroll-area-scrollbar-thumb")
            .expect("track click must keep the scrollbar thumb rendered");
        assert!(
            thumb_after.top() > thumb_before.top(),
            "thumb must move after paging: before={thumb_before:?}, after={thumb_after:?}, offset={:?}",
            test_handle.offset()
        );
    }

    #[gpui::test]
    fn thumb_drag_continues_outside_track_and_stops_on_release(cx: &mut TestAppContext) {
        install_test_theme(cx);
        let handle = ScrollHandle::new();
        let test_handle = handle.clone();
        let (_view, cx) = cx.add_window_view(move |_window, _cx| ScrollAreaTestView {
            handle,
            scrollbar: ScrollbarState::new(),
        });
        cx.simulate_resize(size(px(240.0), px(240.0)));
        cx.run_until_parked();

        let track = cx
            .debug_bounds("test-scroll-area-scrollbar")
            .expect("overflowing content must render a scrollbar track");
        let thumb = cx
            .debug_bounds("test-scroll-area-scrollbar-thumb")
            .expect("overflowing content must render a scrollbar thumb");
        let start = thumb.center();
        let outside = point(track.left() - px(40.0), start.y + px(80.0));

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(track.left() - px(20.0), start.y + px(20.0)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
        assert!(test_handle.offset().y < px(0.0));

        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
        let released_offset = test_handle.offset();
        cx.simulate_mouse_move(
            point(outside.x, outside.y + px(60.0)),
            None,
            Modifiers::default(),
        );
        assert_eq!(test_handle.offset(), released_offset);
    }

    #[gpui::test]
    fn hidden_native_scrollbar_keeps_wheel_scrolling_and_repaints_thumb(cx: &mut TestAppContext) {
        install_test_theme(cx);
        let handle = ScrollHandle::new();
        let test_handle = handle.clone();
        let (_view, cx) = cx.add_window_view(move |_window, _cx| ScrollAreaTestView {
            handle,
            scrollbar: ScrollbarState::new(),
        });
        cx.simulate_resize(size(px(240.0), px(240.0)));
        cx.run_until_parked();

        let thumb_before = cx
            .debug_bounds("test-scroll-area-scrollbar-thumb")
            .expect("overflowing content must render a scrollbar thumb");
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(100.0), px(100.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-80.0))),
            ..Default::default()
        });

        assert!(test_handle.offset().y < px(0.0));
        let thumb_after = cx
            .debug_bounds("test-scroll-area-scrollbar-thumb")
            .expect("wheel scrolling must keep the custom thumb rendered");
        assert!(thumb_after.top() > thumb_before.top());
    }

    #[gpui::test]
    fn uniform_list_and_scrollbar_share_one_scroll_position(cx: &mut TestAppContext) {
        install_test_theme(cx);
        let handle = UniformListScrollHandle::new();
        let test_handle = handle.clone();
        let (view, cx) = cx.add_window_view(move |_window, _cx| UniformListTestView {
            handle,
            scrollbar: ScrollbarState::new(),
        });
        cx.simulate_resize(size(px(240.0), px(240.0)));
        cx.run_until_parked();

        let thumb_before = cx
            .debug_bounds("test-uniform-scrollbar-thumb")
            .expect("overflowing uniform list must render a scrollbar thumb");
        test_handle.scroll_to_item_strict(80, gpui::ScrollStrategy::Center);
        cx.update(|_window, cx| {
            view.update(cx, |_view, cx| cx.notify());
        });
        cx.run_until_parked();
        cx.run_until_parked();

        let base_handle = test_handle.0.borrow().base_handle.clone();
        assert!(base_handle.offset().y < px(0.0));
        let thumb_after = cx
            .debug_bounds("test-uniform-scrollbar-thumb")
            .expect("uniform-list scrolling must keep the thumb rendered");
        assert!(
            thumb_after.top() > thumb_before.top(),
            "uniform-list thumb must follow its base handle: before={thumb_before:?}, after={thumb_after:?}, offset={:?}",
            base_handle.offset()
        );
    }

    #[gpui::test]
    fn zero_height_scroll_area_does_not_schedule_an_infinite_render_loop(cx: &mut TestAppContext) {
        install_test_theme(cx);
        let render_count = Rc::new(Cell::new(0));
        let observed_count = render_count.clone();
        let (_view, cx) = cx.add_window_view(move |_window, _cx| ZeroHeightScrollTestView {
            handle: ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
            render_count,
        });
        cx.run_until_parked();

        assert_eq!(
            observed_count.get(),
            2,
            "zero geometry should receive exactly one bounded follow-up render"
        );
        assert!(
            cx.debug_bounds("zero-height-scroll-area-scrollbar")
                .is_none()
        );
    }
}
