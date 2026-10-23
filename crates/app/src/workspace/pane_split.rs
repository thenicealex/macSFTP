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
/// Smallest share either pane may shrink to. Below this a pane's breadcrumb
/// and column headers stop being usable, so the divider stops there instead.
pub(crate) const MIN_PANE_RATIO: f32 = 0.2;
/// Largest share one pane may take.
pub(crate) const MAX_PANE_RATIO: f32 = 0.8;
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
    if usable <= px(0.0) {
        return DEFAULT_SPLIT_RATIO;
    }
    clamp_split_ratio(local_width / usable)
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
        let far_left = split_ratio_for_pointer(px(0.0), px(1000.0), DIVIDER_WIDTH);
        assert_eq!(far_left, MIN_PANE_RATIO);
        let far_right = split_ratio_for_pointer(px(1000.0), px(1000.0), DIVIDER_WIDTH);
        assert_eq!(far_right, MAX_PANE_RATIO);
    }

    #[test]
    fn a_zero_width_area_falls_back_to_the_default() {
        assert_eq!(
            split_ratio_for_pointer(px(10.0), px(0.0), DIVIDER_WIDTH),
            DEFAULT_SPLIT_RATIO
        );
    }
}
