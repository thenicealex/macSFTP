use std::collections::HashSet;

use gpui::{Context, ElementId, IntoElement, Window};
use gpui_base::input::InputEvent;
use macsftp_core::ConflictRequestId;
use macsftp_ui::{PlainInput, plain_text_field};

use super::profiles::SettingsSection;
use super::{PaneSide, Workspace, WorkspaceSurface};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TextInputTarget {
    ProfileFilter,
    GoToPath,
    ConflictRename(ConflictRequestId),
    InlineEdit,
    CommandPalette,
    LocalFilter,
    RemoteFilter,
}

impl Workspace {
    pub(crate) fn prepare_text_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut drafts = Vec::new();
        if self.surface == WorkspaceSurface::Settings
            && self.settings.section == SettingsSection::Profiles
        {
            drafts.push((
                TextInputTarget::ProfileFilter,
                self.settings.profile_filter.value().to_string(),
                "Filter saved connections…",
            ));
        }
        if self.go_to_path.open {
            drafts.push((
                TextInputTarget::GoToPath,
                self.go_to_path.input.value().to_string(),
                "Full folder path",
            ));
        }
        if let Some(prompt) = self.active_transfer_conflict_prompt() {
            drafts.push((
                TextInputTarget::ConflictRename(prompt.request_id),
                self.modal_inputs.conflict_rename.value().to_string(),
                "Enter a new name",
            ));
        }
        if let Some(edit) = &self.modal_inputs.inline_edit {
            drafts.push((
                TextInputTarget::InlineEdit,
                edit.input.value().to_string(),
                "Enter a name",
            ));
        }
        if self.palette.open {
            drafts.push((
                TextInputTarget::CommandPalette,
                self.palette.input.value().to_string(),
                "Find an action…",
            ));
        }
        for (side, target) in [
            (PaneSide::Local, TextInputTarget::LocalFilter),
            (PaneSide::Remote, TextInputTarget::RemoteFilter),
        ] {
            if self.pane_filter(side).explicit_focus {
                drafts.push((
                    target,
                    self.pane_filter(side).input.value().to_string(),
                    "Filter by name…",
                ));
            }
        }
        let active: HashSet<_> = drafts.iter().map(|(target, _, _)| *target).collect();
        self.text_inputs.retain(|target, _| active.contains(target));
        for (target, value, placeholder) in drafts {
            let fresh = !self.text_inputs.contains_key(&target);
            let input = self.text_inputs.entry(target).or_insert_with(|| {
                PlainInput::new(
                    &value,
                    placeholder,
                    window,
                    cx,
                    move |workspace, input, event, window, cx| {
                        if workspace
                            .text_inputs
                            .get(&target)
                            .is_none_or(|binding| binding.state() != input)
                        {
                            return;
                        }
                        match event {
                            InputEvent::Change => {
                                let value = input.read(cx).value().to_string();
                                match target {
                                    TextInputTarget::ProfileFilter => {
                                        workspace.settings.profile_filter.set_value(value);
                                        workspace.settings.profile_list_scroll =
                                            gpui::ScrollHandle::new();
                                    }
                                    TextInputTarget::GoToPath if workspace.go_to_path.open => {
                                        workspace.go_to_path.input.set_value(value);
                                        workspace.go_to_path.error = None;
                                    }
                                    TextInputTarget::ConflictRename(request_id) => {
                                        if workspace
                                            .active_transfer_conflict_prompt()
                                            .is_some_and(|prompt| prompt.request_id == request_id)
                                        {
                                            workspace.modal_inputs.conflict_rename.set_value(value);
                                            workspace.modal_inputs.conflict_rename_error = None;
                                        }
                                    }
                                    TextInputTarget::InlineEdit => {
                                        if let Some(edit) =
                                            workspace.modal_inputs.inline_edit.as_mut()
                                        {
                                            edit.input.set_value(value);
                                            edit.error = None;
                                        }
                                    }
                                    TextInputTarget::CommandPalette if workspace.palette.open => {
                                        workspace.palette.input.set_value(value);
                                        workspace.palette.selected = 0;
                                    }
                                    TextInputTarget::LocalFilter
                                    | TextInputTarget::RemoteFilter => {
                                        let side = if target == TextInputTarget::LocalFilter {
                                            PaneSide::Local
                                        } else {
                                            PaneSide::Remote
                                        };
                                        let filter = workspace.pane_filter_mut(side);
                                        if filter.explicit_focus {
                                            filter.input.set_value(value.clone());
                                            filter.query = value;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            InputEvent::PressEnter {
                                secondary: false,
                                shift: false,
                            } => match target {
                                TextInputTarget::GoToPath => {
                                    workspace.submit_go_to_path(window, cx)
                                }
                                TextInputTarget::ConflictRename(request_id) => {
                                    if workspace
                                        .active_transfer_conflict_prompt()
                                        .is_some_and(|prompt| prompt.request_id == request_id)
                                    {
                                        workspace.submit_transfer_rename(false, window, cx);
                                    }
                                }
                                TextInputTarget::InlineEdit => {
                                    workspace.submit_inline_edit(window, cx)
                                }
                                TextInputTarget::CommandPalette => {
                                    workspace.execute_palette_selected(window, cx)
                                }
                                _ => {}
                            },
                            InputEvent::Focus
                                if target == TextInputTarget::ProfileFilter
                                    && workspace
                                        .text_inputs
                                        .get(&target)
                                        .is_some_and(|binding| binding.is_focused(window, cx)) =>
                            {
                                workspace.settings.profile_filter_focused = true
                            }
                            InputEvent::Blur if target == TextInputTarget::ProfileFilter => {
                                workspace.settings.profile_filter_focused = false
                            }
                            _ => {}
                        }
                        cx.notify();
                    },
                )
            });
            input.sync(&value, window, cx);
            if fresh && target != TextInputTarget::ProfileFilter {
                input.focus(window, cx);
            }
        }
    }

    pub(crate) fn focus_text_input(
        &self,
        target: TextInputTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.text_inputs.get(&target) {
            input.focus(window, cx);
        }
    }

    pub(crate) fn render_text_input(
        &self,
        target: TextInputTarget,
        id: impl Into<ElementId>,
        cx: &gpui::App,
    ) -> gpui::AnyElement {
        plain_text_field(
            id,
            self.text_inputs
                .get(&target)
                .expect("visible ordinary inputs must be prepared before rendering"),
            cx,
        )
        .into_any_element()
    }
}
