//! View-state group structs for the `Workspace` view.
//!
//! Each struct owns one cohesive feature's UI state (filters, scroll
//! handles, popover/modal inputs). Grouping keeps the `Workspace` struct's
//! top-level surface small and makes per-feature ownership explicit. These
//! are plain data holders: no behavior beyond trivial constructors, and
//! nothing here is persisted.

use gpui::UniformListScrollHandle;
use macsftp_ui::{InputState, ScrollbarState};

/// Per-pane type-to-filter / cmd-f state (view-only; not persisted).
#[derive(Debug, Clone, Default)]
pub(crate) struct PaneFilter {
    pub(crate) query: String,
    pub(crate) input: InputState,
    /// `true` after `cmd-f`: key events prefer the filter input.
    pub(crate) explicit_focus: bool,
}

impl PaneFilter {
    pub(crate) fn is_active(&self) -> bool {
        !self.query.is_empty() || self.explicit_focus
    }

    pub(crate) fn clear(&mut self) {
        self.query.clear();
        self.input.clear();
        self.explicit_focus = false;
    }
}

/// Filter + scrolling state for one file pane.
#[derive(Default)]
pub(crate) struct PaneUi {
    pub(crate) filter: PaneFilter,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl PaneUi {
    pub(crate) fn new() -> Self {
        Self {
            filter: PaneFilter::default(),
            scroll: UniformListScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}
