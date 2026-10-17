//! Enter motion for overlay surfaces (modals, command palette, tab switcher,
//! About, Go to Path).
//!
//! Every surface fades in through one shared timeline built on
//! `gpui-base::motion::Presence`, which keys its state in the window and hands
//! back the target at once when the system reduce-motion switch is on, so the
//! fade needs no configuration and no clock of its own.
//!
//! Sampling runs on every frame, including the frames where the surface is not
//! drawn, so the timeline learns about each dismissal and the next open fades
//! in from zero again.
//!
//! Dismissal stays instant. A mirrored fade-out would have to draw a surface
//! that has already lost its content: the palette query and the go-to-path
//! input are cleared on close, and confirmation models are dropped on
//! dismissal precisely so a dismissed request can never be acted on (§6).
//! macOS dismisses its alerts the same way — instantly.

use std::time::Duration;

use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::{App, Window};

/// Long enough to read as a dissolve, short enough to feel immediate.
pub(crate) const OVERLAY_FADE: Duration = Duration::from_millis(150);

/// This frame's fade sample for one overlay.
pub(crate) struct OverlayFade {
    progress: f32,
}

impl OverlayFade {
    /// Sample the fade of the surface whose own state is `open`; pass the raw
    /// flag, not a content check.
    pub(crate) fn sample(id: &'static str, open: bool, window: &mut Window, cx: &mut App) -> Self {
        let sample = Presence::new(id, open)
            .transition(Transition::new(OVERLAY_FADE))
            .sample(window, cx);
        Self {
            progress: sample.progress,
        }
    }

    /// Opacity for the scrim; the card inside inherits it.
    ///
    /// Opacity is the whole treatment: this GPUI snapshot has no element
    /// transform (only `Svg` carries one), so the scale a popover would use on
    /// macOS is not available — architecture §12.13 records the deviation.
    pub(crate) fn opacity(&self) -> f32 {
        self.progress
    }
}
