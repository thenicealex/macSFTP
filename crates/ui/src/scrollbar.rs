use gpui::{
    App, Context, ElementId, Hsla, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollHandle, Styled, UniformListScrollHandle, Window, div,
    prelude::*, px,
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

    pub fn vertical(handle: ScrollHandle, cx: &mut App) -> impl IntoElement {
        cx.new(|_| Scrollbar::new(handle))
    }

    pub fn vertical_uniform(handle: &UniformListScrollHandle, cx: &mut App) -> impl IntoElement {
        let base = handle.0.borrow().base_handle.clone();
        cx.new(|_| Scrollbar::new(base))
    }

    fn thumb_color(&self, cx: &App) -> Hsla {
        let c = cx.theme().colors;
        if self.dragging {
            c.scrollbar_thumb_active
        } else {
            c.scrollbar_thumb
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
            return div().into_any_element();
        };
        let width = theme.sizes.scrollbar_width;
        let thumb_color = self.thumb_color(cx);
        let track_color = cx.theme().colors.scrollbar_track;
        let thumb_hover = cx.theme().colors.scrollbar_thumb_hover;

        div()
            .id("custom-scrollbar")
            .absolute()
            .top_0()
            .right_0()
            .h(geom.track_height)
            .w(width)
            .bg(track_color)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _w, _cx| {
                    let viewport = this.handle.bounds().size.height;
                    let scrollable = this.handle.max_offset().height;
                    let scrolled = (-this.handle.offset().y).max(px(0.0));
                    let Some(g) = thumb_geometry(viewport, scrollable, scrolled) else {
                        return;
                    };
                    // `ev.position.y` is window-relative, but `g.thumb_top` /
                    // `g.thumb_height` are track-relative (0 at top of track).
                    // The scrollbar is `absolute().top_0()` inside the
                    // `relative()` scroll_area, so its top == the scroll
                    // container's window origin.y.
                    let origin_y = this.handle.bounds().origin.y;
                    let click_y = ev.position.y - origin_y;
                    match classify_click(click_y, g.thumb_top, g.thumb_height) {
                        ClickAction::PageUp => page(&this.handle, viewport, false),
                        ClickAction::PageDown => page(&this.handle, viewport, true),
                        ClickAction::Drag => {
                            this.dragging = true;
                            this.drag_start_y = ev.position.y;
                            this.drag_start_offset = (-this.handle.offset().y).max(px(0.0));
                        }
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _w, _cx| {
                if !this.dragging {
                    return;
                }
                // `on_mouse_up` only fires while the cursor is over the
                // scrollbar hitbox. If the user releases outside it, the
                // up event never fires and `dragging` would stay `true`,
                // causing a wild jump on the next no-button hover. Guard
                // with the pressed-button state from the move event.
                if ev.pressed_button != Some(MouseButton::Left) {
                    this.dragging = false;
                    return;
                }
                let delta = ev.position.y - this.drag_start_y;
                let viewport = this.handle.bounds().size.height;
                let scrollable = this.handle.max_offset().height;
                let content = viewport + scrollable;
                let thumb_h = (viewport * (viewport / content)).max(MIN_THUMB);
                let travel = (viewport - thumb_h).max(px(0.0));
                let new_scrolled = if travel > px(0.0) {
                    this.drag_start_offset + (delta / travel) * scrollable
                } else {
                    px(0.0)
                };
                let clamped = new_scrolled.max(px(0.0)).min(scrollable);
                this.handle.set_offset(Point::new(px(0.0), -clamped));
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _ev: &MouseUpEvent, _w, _cx| {
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
                    .hover(|t| t.bg(thumb_hover)),
            )
            .into_any_element()
    }
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
}
