use gpui_kit::{Context, IntoElement, Pixels, Render, Styled, Window, div, px};
use std::time::Duration;

use gpui_kit::base::motion::Spring;

/// Keep the queue visible without taking half of a short window.
pub(crate) const DEFAULT_DRAWER_HEIGHT: Pixels = px(160.0);
/// Smallest allowed height: handle + one header row (list may be empty).
pub(crate) const MIN_DRAWER_HEIGHT: Pixels = px(40.0);
/// Absolute ceiling regardless of window size.
pub(crate) const MAX_DRAWER_HEIGHT_ABS: Pixels = px(480.0);
/// Fraction of the main content area (tab bar bottom → status bar top).
pub(crate) const MAX_DRAWER_HEIGHT_RATIO: f32 = 0.5;
/// Hit target for the resize grip.
pub(crate) const RESIZE_HANDLE_HEIGHT: Pixels = px(5.0);

/// Open/close motion for the drawer: critically damped (the height must never
/// overshoot past the window edge) with a pixel-sized settling tolerance,
/// because the target is in pixels rather than normalized. A resize drag
/// suspends travel instead of a shorter response, so the height keeps following
/// the pointer 1:1 and settles from wherever the drag released it.
pub(crate) const DRAWER_SPRING: Spring = Spring::new(Duration::from_millis(300)).with_epsilon(0.5);

/// Approximate chrome outside the content area when measuring max height.
/// tab_bar (~36) + status_bar (theme ~22–28) — keep conservative so max is not too tall.
pub(crate) const APPROX_CHROME_HEIGHT: Pixels = px(64.0);

/// GPUI drag value for the transfer drawer resize handle.
#[derive(Clone, Debug)]
pub(crate) struct TransferDrawerResize {
    pub start_height: Pixels,
    pub start_y: Pixels,
}

/// Empty drag preview (no visual ghost required for resize; minimal transparent view).
pub(crate) struct ResizeDragGhost;

impl Render for ResizeDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.0)).h(px(1.0))
    }
}

/// Clamp a requested drawer height into [min, max(content)].
///
/// `content_area_height` is the vertical space available for panes+drawer
/// (viewport minus approximate chrome). When unknown, callers may pass
/// `viewport_height - APPROX_CHROME_HEIGHT` (floored at min).
pub(crate) fn clamp_drawer_height(height: Pixels, content_area_height: Pixels) -> Pixels {
    if f32::from(height).is_nan() || f32::from(content_area_height).is_nan() {
        return DEFAULT_DRAWER_HEIGHT;
    }
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
    if f32::from(viewport_height).is_nan() {
        return MIN_DRAWER_HEIGHT;
    }
    let raw = viewport_height - APPROX_CHROME_HEIGHT;
    if raw < MIN_DRAWER_HEIGHT {
        MIN_DRAWER_HEIGHT
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::px;

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
        assert_eq!(content_area_height_from_viewport(px(864.0)), px(800.0));
    }

    #[test]
    fn clamp_drawer_height_handles_nan() {
        assert_eq!(
            clamp_drawer_height(px(f32::NAN), px(800.0)),
            DEFAULT_DRAWER_HEIGHT
        );
        assert_eq!(
            clamp_drawer_height(px(200.0), px(f32::NAN)),
            DEFAULT_DRAWER_HEIGHT
        );
        assert_eq!(
            content_area_height_from_viewport(px(f32::NAN)),
            MIN_DRAWER_HEIGHT
        );
    }
}
