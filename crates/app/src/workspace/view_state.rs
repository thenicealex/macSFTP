//! View-state group structs for the `Workspace` view.
//!
//! Each struct owns one cohesive feature's UI state (filters, scroll
//! handles, popover/modal inputs). Grouping keeps the `Workspace` struct's
//! top-level surface small and makes per-feature ownership explicit. These
//! are plain data holders: no behavior beyond trivial constructors, and
//! nothing here is persisted.

use gpui::{Pixels, UniformListScrollHandle};
use macsftp_ui::{InputState, ScrollbarState};

use crate::workspace::drawer_height;

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

/// Command palette popover state.
pub(crate) struct CommandPaletteUi {
    pub(crate) open: bool,
    pub(crate) input: InputState,
    pub(crate) selected: usize,
    pub(crate) scroll: gpui::ScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl CommandPaletteUi {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            input: InputState::new(),
            selected: 0,
            scroll: gpui::ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

/// Tab switcher (ctrl-tab) overlay state.
pub(crate) struct TabSwitcherUi {
    pub(crate) open: bool,
    pub(crate) index: usize,
    pub(crate) scroll: gpui::ScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl TabSwitcherUi {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            index: 0,
            scroll: gpui::ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

/// Go-to-path popover state (path jump within the focused pane).
pub(crate) struct GoToPathUi {
    pub(crate) open: bool,
    pub(crate) input: InputState,
    pub(crate) error: Option<gpui::SharedString>,
}

impl GoToPathUi {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            input: InputState::new(),
            error: None,
        }
    }
}

/// Transfer drawer visibility, geometry, and section expansion state.
pub(crate) struct TransferDrawerUi {
    pub(crate) open: bool,
    pub(crate) height: Pixels,
    pub(crate) resize: Option<drawer_height::TransferDrawerResize>,
    pub(crate) completed_section_expanded: bool,
    pub(crate) failed_section_expanded: bool,
    pub(crate) scroll: gpui::ScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl TransferDrawerUi {
    /// The product default ships with the drawer open; `open: false` would
    /// hide transfers on first launch.
    pub(crate) fn new() -> Self {
        Self {
            open: true,
            height: drawer_height::DEFAULT_DRAWER_HEIGHT,
            resize: None,
            completed_section_expanded: false,
            failed_section_expanded: false,
            scroll: gpui::ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

/// Connect form sheet state (form model + focus handle).
pub(crate) struct ConnectFormUi {
    pub(crate) form: Option<crate::workspace::connect_form::ConnectForm>,
    pub(crate) focus: gpui::FocusHandle,
}

impl ConnectFormUi {
    pub(crate) fn new(focus: gpui::FocusHandle) -> Self {
        Self { form: None, focus }
    }
}

/// Modal and popover input state owned by the view while a prompt is open.
pub(crate) struct ModalInputsUi {
    /// Draft name for the active transfer-conflict rename decision. It is
    /// view-local because the runtime only receives a decision after the
    /// user submits it.
    pub(crate) conflict_rename: InputState,
    pub(crate) conflict_rename_error: Option<gpui::SharedString>,
    /// Phase 1 delete confirmation modal (view-local).
    pub(crate) delete_confirm: Option<crate::workspace::file_ops::DeleteConfirmState>,
    /// Phase 1 inline rename / new-folder editor.
    pub(crate) inline_edit: Option<crate::workspace::file_ops::InlineEditState>,
    /// Phase 1 context menu for the focused file pane.
    pub(crate) context_menu: Option<crate::workspace::file_ops::ContextMenuState>,
    /// Confirmation before opening very large remote files for edit.
    pub(crate) large_edit_confirm: Option<crate::workspace::remote_edit::PendingEdit>,
    pub(crate) about_open: bool,
}

impl ModalInputsUi {
    pub(crate) fn new() -> Self {
        Self {
            conflict_rename: InputState::new(),
            conflict_rename_error: None,
            delete_confirm: None,
            inline_edit: None,
            context_menu: None,
            large_edit_confirm: None,
            about_open: false,
        }
    }
}

/// Settings surface state (sidebar section, Profiles list, editor fields).
pub(crate) struct SettingsUi {
    /// Active sidebar section when the workspace shows Settings.
    pub(crate) section: crate::workspace::profiles::SettingsSection,
    /// Free-text filter for the Settings → Profiles list.
    pub(crate) profile_filter: InputState,
    /// `true` while the Profiles filter field owns key events.
    pub(crate) profile_filter_focused: bool,
    /// Settings → General external-editor override field. Committed to config
    /// on every edit (empty → None).
    pub(crate) external_editor_input: InputState,
    /// `true` while the External editor field owns key events.
    pub(crate) external_editor_focused: bool,
    /// Selected profile in Settings → Profiles (None when the list is empty).
    pub(crate) selected_profile_id: Option<macsftp_core::ProfileId>,
    /// Draft for New / Edit profile in Settings → Profiles.
    pub(crate) profile_editor: Option<crate::workspace::profiles::ProfileEditorState>,
    /// Pending profile delete confirmation (Settings / Connect). View-local.
    pub(crate) profile_delete_confirm: Option<macsftp_core::ProfileId>,
    pub(crate) picker_scroll: gpui::ScrollHandle,
    pub(crate) picker_scrollbar: ScrollbarState,
}

impl SettingsUi {
    pub(crate) fn new(external_editor_input: InputState) -> Self {
        Self {
            section: crate::workspace::profiles::SettingsSection::General,
            profile_filter: InputState::new(),
            profile_filter_focused: false,
            external_editor_input,
            external_editor_focused: false,
            selected_profile_id: None,
            profile_editor: None,
            profile_delete_confirm: None,
            picker_scroll: gpui::ScrollHandle::new(),
            picker_scrollbar: ScrollbarState::new(),
        }
    }
}
