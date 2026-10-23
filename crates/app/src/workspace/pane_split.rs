//! The draggable divider between the local and remote file panes.
//!
//! The two panes used to split the content area down the middle, with no way
//! to give one side more room. A wide remote listing or a long local path
//! column both want a different balance, so the divider can be dragged.
//!
//! The split is stored as a ratio rather than a pixel width: the window can
//! be resized afterwards and the panes keep the same relative balance.

use gpui_kit::{Pixels, px};

/// Default share of the content area given to the local pane.
pub(crate) const DEFAULT_SPLIT_RATIO: f32 = 0.5;
/// Smallest share either pane may shrink to, whatever the window width.
pub(crate) const MIN_PANE_RATIO: f32 = 0.2;
/// Largest share one pane may take.
pub(crate) const MAX_PANE_RATIO: f32 = 0.8;
/// Narrowest a pane may get while both still fit. The ratio floor alone is not
/// enough: twenty percent of a 720 pt window is 142 pt, which is less than the
/// path bar's navigation, breadcrumb, file-action and transfer clusters need,
/// so the bar would clip. This is the pixel floor that stops it.
pub(crate) const MIN_PANE_WIDTH: Pixels = px(300.0);
/// Width the divider occupies in the layout. Wide enough to grab by itself
/// (the rule itself is one pixel, the rest is transparent), narrow enough
/// that a 360 pt pane does not feel the difference.
pub(crate) const DIVIDER_WIDTH: Pixels = px(5.0);
/// Width of the drawn rule inside the divider's layout box.
pub(crate) const DIVIDER_RULE_WIDTH: Pixels = px(1.0);

/// Clamp a requested split into the range both panes stay usable in.
pub(crate) fn clamp_split_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        return DEFAULT_SPLIT_RATIO;
    }
    ratio.clamp(MIN_PANE_RATIO, MAX_PANE_RATIO)
}

/// Clamp a split so neither pane drops below [`MIN_PANE_WIDTH`].
///
/// A window too narrow for two panes at that width falls back to an even split:
/// the panes are both cramped either way, and an even split is the honest
/// rendering of "there is not enough room", not a lopsided one.
pub(crate) fn clamp_split_ratio_for_width(ratio: f32, content_width: Pixels) -> f32 {
    let ratio = clamp_split_ratio(ratio);
    let usable = content_width - DIVIDER_WIDTH;
    if usable <= px(0.0) {
        return DEFAULT_SPLIT_RATIO;
    }
    let pixel_floor = MIN_PANE_WIDTH / usable;
    if pixel_floor * 2.0 >= 1.0 {
        return DEFAULT_SPLIT_RATIO;
    }
    ratio.clamp(pixel_floor, 1.0 - pixel_floor)
}

/// The split that results from dragging the divider so that it lands at
/// `divider_x` within a content area `content_width` wide.
///
/// The divider's own half-width is subtracted so the divider tracks the
/// pointer instead of sitting half a line away from it.
pub(crate) fn split_ratio_for_pointer(
    divider_x: Pixels,
    content_width: Pixels,
    divider_width: Pixels,
) -> f32 {
    if content_width <= px(0.0) {
        return DEFAULT_SPLIT_RATIO;
    }
    let usable = (content_width - divider_width).max(px(0.0));
    let local_width = (divider_x - divider_width / 2.0).clamp(px(0.0), usable);
    clamp_split_ratio_for_width(local_width / usable, content_width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_both_panes_usable() {
        assert_eq!(clamp_split_ratio(0.0), MIN_PANE_RATIO);
        assert_eq!(clamp_split_ratio(1.0), MAX_PANE_RATIO);
        assert_eq!(clamp_split_ratio(0.5), 0.5);
        assert_eq!(clamp_split_ratio(f32::NAN), DEFAULT_SPLIT_RATIO);
    }

    #[test]
    fn pointer_position_maps_to_a_ratio() {
        // Divider dragged to the middle of a 1000 pt area splits it evenly.
        let ratio = split_ratio_for_pointer(px(500.0), px(1000.0), DIVIDER_WIDTH);
        assert!((ratio - 0.5).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn pointer_at_the_edges_is_clamped() {
        // 300 of 995 usable points is the pixel floor, which is wider than the
        // 20 % ratio floor; the divider stops at whichever keeps the pane usable.
        let floor = MIN_PANE_WIDTH / (px(1000.0) - DIVIDER_WIDTH);
        let far_left = split_ratio_for_pointer(px(0.0), px(1000.0), DIVIDER_WIDTH);
        assert!((far_left - floor).abs() < 0.001, "{far_left} vs {floor}");
        let far_right = split_ratio_for_pointer(px(1000.0), px(1000.0), DIVIDER_WIDTH);
        assert!((far_right - (1.0 - floor)).abs() < 0.001, "{far_right}");
    }

    #[test]
    fn the_pixel_floor_wins_over_the_ratio_floor() {
        // A wide window can honour the 20 % ratio floor: 300 pt is only 12.5 %.
        let wide = clamp_split_ratio_for_width(0.05, px(2400.0));
        assert!((wide - MIN_PANE_RATIO).abs() < 0.001, "{wide}");

        // A narrow one cannot: 20 % of 900 pt is 180 pt, less than the path bar
        // needs, so the pixel floor takes over.
        let narrow = clamp_split_ratio_for_width(0.05, px(900.0));
        assert!(
            narrow * px(900.0) >= MIN_PANE_WIDTH,
            "a narrow window keeps the pixel floor: {narrow}"
        );
    }

    #[test]
    fn a_window_too_narrow_for_two_panes_splits_evenly() {
        let ratio = clamp_split_ratio_for_width(0.8, px(400.0));
        assert_eq!(
            ratio, DEFAULT_SPLIT_RATIO,
            "neither pane can reach the pixel floor, so they share the space"
        );
    }

    #[test]
    fn a_zero_width_area_falls_back_to_the_default() {
        assert_eq!(
            split_ratio_for_pointer(px(10.0), px(0.0), DIVIDER_WIDTH),
            DEFAULT_SPLIT_RATIO
        );
    }
}
