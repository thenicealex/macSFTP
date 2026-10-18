//! View-state group structs for the `Workspace` view.
//!
//! Each struct owns one cohesive feature's UI state (filters, scroll
//! handles, popover/modal inputs). Grouping keeps the `Workspace` struct's
//! top-level surface small and makes per-feature ownership explicit. These
//! are plain data holders: no behavior beyond trivial constructors, and
//! nothing here is persisted.

use std::time::{Duration, Instant};

use gpui_kit::{Pixels, UniformListScrollHandle};
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

/// Finder-style type-select: the letters typed into a focused pane and when
/// the last one arrived. A pause longer than [`TypeSelect::CONTINUATION_WINDOW`]
/// starts a new prefix, so `b` then `a` quickly means "ba" while `b`, pause,
/// `a` means "a".
#[derive(Debug, Clone, Default)]
pub(crate) struct TypeSelect {
    prefix: String,
    last_input: Option<Instant>,
}

impl TypeSelect {
    /// Keystrokes closer together than this extend the same prefix.
    pub(crate) const CONTINUATION_WINDOW: Duration = Duration::from_millis(1000);

    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Extend the prefix with `text`, starting over when the previous
    /// keystroke is already stale.
    pub(crate) fn extend(&mut self, text: &str, now: Instant) {
        if self.is_stale(now) {
            self.prefix.clear();
        }
        self.prefix.push_str(text);
        self.last_input = Some(now);
    }

    /// Replace the prefix with `text` (fallback when the longer prefix
    /// matches nothing).
    pub(crate) fn restart(&mut self, text: &str, now: Instant) {
        self.prefix.clear();
        self.prefix.push_str(text);
        self.last_input = Some(now);
    }

    /// Drop the last character and return the prefix that remains.
    pub(crate) fn pop(&mut self) -> &str {
        self.prefix.pop();
        &self.prefix
    }

    pub(crate) fn clear(&mut self) {
        self.prefix.clear();
        self.last_input = None;
    }

    fn is_stale(&self, now: Instant) -> bool {
        self.last_input
            .is_some_and(|at| now.duration_since(at) >= Self::CONTINUATION_WINDOW)
    }
}

/// Filter, type-select and scrolling state for one file pane.
#[derive(Default)]
pub(crate) struct PaneUi {
    pub(crate) filter: PaneFilter,
    pub(crate) type_select: TypeSelect,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl PaneUi {
    pub(crate) fn new() -> Self {
        Self {
            filter: PaneFilter::default(),
            type_select: TypeSelect::default(),
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
    pub(crate) scroll: gpui_kit::ScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl CommandPaletteUi {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            input: InputState::new(),
            selected: 0,
            scroll: gpui_kit::ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

/// Tab switcher (ctrl-tab) overlay state.
pub(crate) struct TabSwitcherUi {
    pub(crate) open: bool,
    pub(crate) index: usize,
    pub(crate) scroll: gpui_kit::ScrollHandle,
    pub(crate) scrollbar: ScrollbarState,
}

impl TabSwitcherUi {
    pub(crate) fn new() -> Self {
        Self {
            open: false,
            index: 0,
            scroll: gpui_kit::ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

/// Go-to-path popover state (path jump within the focused pane).
pub(crate) struct GoToPathUi {
    pub(crate) open: bool,
    pub(crate) input: InputState,
    pub(crate) error: Option<gpui_kit::SharedString>,
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
    pub(crate) scroll: gpui_kit::ScrollHandle,
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
            scroll: gpui_kit::ScrollHandle::new(),
            scrollbar: ScrollbarState::new(),
        }
    }
}

/// Connect form sheet state (form model + focus handle).
pub(crate) struct ConnectFormUi {
    pub(crate) form: Option<crate::workspace::connect_form::ConnectForm>,
    pub(crate) focus: gpui_kit::FocusHandle,
}

impl ConnectFormUi {
    pub(crate) fn new(focus: gpui_kit::FocusHandle) -> Self {
        Self { form: None, focus }
    }
}

/// Modal and popover input state owned by the view while a prompt is open.
pub(crate) struct ModalInputsUi {
    /// Draft name for the active transfer-conflict rename decision. It is
    /// view-local because the runtime only receives a decision after the
    /// user submits it.
    pub(crate) conflict_rename: InputState,
    pub(crate) conflict_rename_error: Option<gpui_kit::SharedString>,
    /// Phase 1 delete confirmation modal (view-local).
    pub(crate) delete_confirm: Option<crate::workspace::file_ops::DeleteConfirmState>,
    /// Phase 1 inline rename / new-folder editor.
    pub(crate) inline_edit: Option<crate::workspace::file_ops::InlineEditState>,
    /// Confirmation before opening very large remote files for edit.
    pub(crate) large_edit_confirm: Option<crate::workspace::remote_edit::PendingEdit>,
    pub(crate) keyboard_interactive: std::collections::HashMap<
        macsftp_core::KeyboardInteractiveRequestId,
        KeyboardInteractiveUi,
    >,
    pub(crate) about_open: bool,
    pub(crate) conflict_compare_loading: bool,
    pub(crate) conflict_compare_error: Option<String>,
    /// Session whose comparison download owns the loading/error pair above.
    /// Without this, a comparison started in one tab shows its progress on
    /// another tab's conflict modal.
    pub(crate) conflict_compare_session: Option<macsftp_core::EditSessionId>,
    /// Cached diff-tool availability for the conflict modal, keyed by session.
    /// Detecting on every render frame would run filesystem probes on the UI
    /// thread for as long as the modal stays open.
    pub(crate) conflict_diff_tool: Option<(macsftp_core::EditSessionId, bool)>,
}

impl ModalInputsUi {
    pub(crate) fn new() -> Self {
        Self {
            conflict_rename: InputState::new(),
            conflict_rename_error: None,
            delete_confirm: None,
            inline_edit: None,
            large_edit_confirm: None,
            keyboard_interactive: std::collections::HashMap::new(),
            about_open: false,
            conflict_compare_loading: false,
            conflict_compare_error: None,
            conflict_compare_session: None,
            conflict_diff_tool: None,
        }
    }

    pub(crate) fn begin_conflict_compare(&mut self, session_id: macsftp_core::EditSessionId) {
        self.conflict_compare_session = Some(session_id);
        self.conflict_compare_loading = true;
        self.conflict_compare_error = None;
    }

    pub(crate) fn reset_conflict_compare(&mut self) {
        self.conflict_compare_session = None;
        self.conflict_compare_loading = false;
        self.conflict_compare_error = None;
    }

    /// Loading flag and error belonging to `session_id`, or `None` when the
    /// state belongs to another session (another tab's conflict modal).
    pub(crate) fn conflict_compare_for(
        &self,
        session_id: macsftp_core::EditSessionId,
    ) -> Option<(bool, Option<&str>)> {
        (self.conflict_compare_session == Some(session_id)).then_some((
            self.conflict_compare_loading,
            self.conflict_compare_error.as_deref(),
        ))
    }
}

pub(crate) enum KeyboardInteractiveInput {
    Plain(macsftp_ui::SecretInputState),
    Secret(macsftp_ui::SecretInputState),
}

impl KeyboardInteractiveInput {
    pub(crate) fn state(&self) -> &InputState {
        match self {
            Self::Plain(state) => state.as_input_state(),
            Self::Secret(state) => state.as_input_state(),
        }
    }

    pub(crate) fn state_mut(&mut self) -> &mut InputState {
        match self {
            Self::Plain(state) => state.as_input_state_mut(),
            Self::Secret(state) => state.as_input_state_mut(),
        }
    }

    pub(crate) fn value(&self) -> &str {
        self.state().value()
    }
}

pub(crate) struct KeyboardInteractiveUi {
    pub(crate) inputs: Vec<KeyboardInteractiveInput>,
    pub(crate) focused_index: usize,
}

impl KeyboardInteractiveUi {
    pub(crate) fn from_prompt(prompt: &macsftp_core::KeyboardInteractivePrompt) -> Self {
        Self {
            inputs: prompt
                .prompts
                .iter()
                .map(|field| {
                    if field.echo {
                        KeyboardInteractiveInput::Plain(macsftp_ui::SecretInputState::new())
                    } else {
                        KeyboardInteractiveInput::Secret(macsftp_ui::SecretInputState::new())
                    }
                })
                .collect(),
            focused_index: 0,
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
    pub(crate) external_editor_input: gpui_kit::Entity<gpui_kit::base::input::InputState>,
    _external_editor_subscription: gpui_kit::Subscription,
    pub(crate) new_rule_pattern: gpui_kit::Entity<gpui_kit::base::input::InputState>,
    pub(crate) new_rule_app: gpui_kit::Entity<gpui_kit::base::input::InputState>,
    /// Selected profile in Settings → Profiles (None when the list is empty).
    pub(crate) selected_profile_id: Option<macsftp_core::ProfileId>,
    /// Draft for New / Edit profile in Settings → Profiles.
    pub(crate) profile_editor: Option<crate::workspace::profiles::ProfileEditorState>,
    /// Pending profile delete confirmation (Settings / Connect). View-local.
    pub(crate) profile_delete_confirm: Option<macsftp_core::ProfileId>,
    pub(crate) picker_scroll: gpui_kit::ScrollHandle,
    pub(crate) picker_scrollbar: ScrollbarState,
    pub(crate) profile_list_scroll: gpui_kit::ScrollHandle,
    pub(crate) profile_list_scrollbar: ScrollbarState,
    pub(crate) editor_scroll: gpui_kit::ScrollHandle,
    pub(crate) editor_scrollbar: ScrollbarState,
}

impl SettingsUi {
    pub(crate) fn new(
        external_editor_input: gpui_kit::Entity<gpui_kit::base::input::InputState>,
        external_editor_subscription: gpui_kit::Subscription,
        new_rule_pattern: gpui_kit::Entity<gpui_kit::base::input::InputState>,
        new_rule_app: gpui_kit::Entity<gpui_kit::base::input::InputState>,
    ) -> Self {
        Self {
            section: crate::workspace::profiles::SettingsSection::General,
            profile_filter: InputState::new(),
            profile_filter_focused: false,
            external_editor_input,
            _external_editor_subscription: external_editor_subscription,
            new_rule_pattern,
            new_rule_app,
            selected_profile_id: None,
            profile_editor: None,
            profile_delete_confirm: None,
            picker_scroll: gpui_kit::ScrollHandle::new(),
            picker_scrollbar: ScrollbarState::new(),
            profile_list_scroll: gpui_kit::ScrollHandle::new(),
            profile_list_scrollbar: ScrollbarState::new(),
            editor_scroll: gpui_kit::ScrollHandle::new(),
            editor_scrollbar: ScrollbarState::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::TypeSelect;

    #[test]
    fn quick_keystrokes_extend_one_prefix() {
        let start = Instant::now();
        let mut select = TypeSelect::default();
        select.extend("r", start);
        select.extend("e", start + Duration::from_millis(120));
        assert_eq!(select.prefix(), "re");
    }

    #[test]
    fn a_pause_starts_a_new_prefix() {
        let start = Instant::now();
        let mut select = TypeSelect::default();
        select.extend("r", start);
        select.extend("e", start + TypeSelect::CONTINUATION_WINDOW);
        assert_eq!(select.prefix(), "e", "the stale prefix is replaced");
    }

    #[test]
    fn restart_replaces_and_pop_shortens_the_prefix() {
        let now = Instant::now();
        let mut select = TypeSelect::default();
        select.extend("readme", now);
        assert_eq!(select.pop(), "readm");
        select.restart("b", now);
        assert_eq!(select.prefix(), "b");
        assert_eq!(select.pop(), "");
        select.extend("x", now);
        select.clear();
        assert_eq!(select.prefix(), "");
    }
}
