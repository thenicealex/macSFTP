use std::path::Path;

use gpui::{
    ClickEvent, Context, FontWeight, IntoElement, KeyDownEvent, ParentElement, SharedString,
    Styled, Window, div, prelude::*, px,
};
use macsftp_core::{
    AppCommand, AuthMethodKind, ConflictDecision, ConflictDecisionCommand, ConflictRequestId,
    ConnectionState, DisconnectReason, HostKeyDecisionCommand, HostKeyPrompt, LocalPath,
    ModalRequest, ModalRequestId, RemotePath, TransferConflictPrompt, TransferEndpoint,
    TrustRequestId,
};
use macsftp_ui::{
    ActiveTheme, InputKeyResult, InputState, TextFieldModel, copy_name, format_size,
    format_timestamp, text_button, text_field,
};

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::connect_form::*;
use crate::workspace::helpers::*;
use crate::workspace::profiles::{SettingsSection, profile_list_label};
use crate::workspace::*;
use macsftp_core::HistoryOp;

impl crate::workspace::Workspace {
    pub(crate) fn has_transfer_conflict_modal(&self, request_id: ConflictRequestId) -> bool {
        self.state
            .modals
            .active
            .iter()
            .any(|modal| modal.request_id() == Some(ModalRequestId::Conflict(request_id)))
    }

    pub(crate) fn present_transfer_conflict(
        &mut self,
        prompt: TransferConflictPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_transfer_conflict_modal(prompt.request_id) {
            return;
        }
        let default_rename = copy_name(&prompt.destination);
        self.state
            .modals
            .active
            .push(ModalRequest::TransferConflict(prompt));
        self.conflict_rename.set_value(default_rename);
        self.conflict_rename_error = None;
        window.focus(&self.modal_focus);
        cx.notify();
    }

    pub(crate) fn active_host_key_prompt(&self) -> Option<&HostKeyPrompt> {
        let active_tab_id = self.state.tabs.active_tab_id?;
        self.state
            .modals
            .active
            .iter()
            .rev()
            .find_map(|modal| match modal {
                ModalRequest::HostKey(prompt) if prompt.tab_id == active_tab_id => Some(prompt),
                _ => None,
            })
    }
    pub(crate) fn active_transfer_conflict_prompt(&self) -> Option<&TransferConflictPrompt> {
        self.state
            .modals
            .active
            .iter()
            .rev()
            .find_map(|modal| match modal {
                ModalRequest::TransferConflict(prompt) => Some(prompt),
                _ => None,
            })
    }
    pub(crate) fn remove_modal(&mut self, request_id: TrustRequestId) {
        self.state
            .modals
            .active
            .retain(|modal| modal.request_id() != Some(ModalRequestId::Trust(request_id)));
    }
    pub(crate) fn remove_conflict_modal(
        &mut self,
        request_id: ConflictRequestId,
        cx: &mut Context<Self>,
    ) {
        self.state
            .modals
            .active
            .retain(|modal| modal.request_id() != Some(ModalRequestId::Conflict(request_id)));
        cx.remove_transfer_conflict(request_id);
        self.conflict_rename_error = None;
        if let Some(default_name) = self
            .active_transfer_conflict_prompt()
            .map(|prompt| copy_name(&prompt.destination))
        {
            self.conflict_rename.set_value(default_name);
        } else {
            self.conflict_rename.clear();
        }
    }
    pub(crate) fn resolve_transfer_conflict(
        &mut self,
        prompt: &TransferConflictPrompt,
        decision: ConflictDecision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.send_command(
            AppCommand::ResolveTransferConflict(ConflictDecisionCommand {
                plan_id: prompt.plan_id,
                request_id: prompt.request_id,
                decision,
            }),
            cx,
        ) {
            return;
        }
        self.remove_conflict_modal(prompt.request_id, cx);
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
    pub(crate) fn submit_transfer_rename(
        &mut self,
        apply_to_all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.active_transfer_conflict_prompt().cloned() else {
            return;
        };
        let new_name = self.conflict_rename.value().trim().to_string();
        if new_name.is_empty() || new_name == "." || new_name == ".." || new_name.contains('/') {
            self.conflict_rename_error =
                Some("Enter a file name without path separators or parent-directory names.".into());
            cx.notify();
            return;
        }

        self.resolve_transfer_conflict(
            &prompt,
            ConflictDecision::Rename {
                new_name,
                apply_to_all,
            },
            window,
            cx,
        );
    }
    pub(crate) fn handle_transfer_conflict_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_transfer_conflict_prompt().is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.submit_transfer_rename(false, window, cx);
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.conflict_rename.insert(&text);
                self.conflict_rename_error = None;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.conflict_rename.handle_keystroke(keystroke) == InputKeyResult::Handled {
            self.conflict_rename_error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }
    pub(crate) fn accept_host_key(
        &mut self,
        request_id: TrustRequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.send_command(
            AppCommand::AcceptHostKey(HostKeyDecisionCommand { request_id }),
            cx,
        ) {
            return;
        }
        self.remove_modal(request_id);
        // Handshake continues; show Connecting until TabConnected lands.
        if let Some(tab) = self.active_tab_mut()
            && let ConnectionState::AwaitingHostKey {
                session_id,
                session_epoch,
                request_id: pending_request,
            } = tab.connection
            && pending_request == request_id
        {
            tab.connection = ConnectionState::Connecting {
                session_id,
                session_epoch,
            };
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
    pub(crate) fn reject_host_key(
        &mut self,
        request_id: TrustRequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.send_command(AppCommand::RejectHostKey { request_id }, cx) {
            return;
        }
        self.remove_modal(request_id);
        // The user declined — reflect it immediately. The actor's own
        // TabDisconnected for this session is dropped by the guard once
        // the state no longer matches, which is fine: same outcome.
        if let Some(tab) = self.active_tab_mut()
            && let ConnectionState::AwaitingHostKey {
                request_id: pending_request,
                ..
            } = tab.connection
            && pending_request == request_id
        {
            tab.disconnect(DisconnectReason::UserRequested);
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
    pub(crate) fn cancel_active_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Command palette sits above other modals (phase 4 design §2.3).
        if self.palette.open {
            self.close_command_palette(window, cx);
            return;
        }
        // MRU tab switcher next (phase 4 design §4.3 — Esc cancels without switching).
        if self.tab_switcher_open {
            self.close_tab_switcher(window, cx);
            return;
        }
        // Go to Path next so Esc dismisses the path field when palette is closed.
        if self.go_to_path_open {
            self.close_go_to_path(window, cx);
            return;
        }
        if self.about_open {
            self.close_about(window, cx);
            return;
        }
        // Profile-delete confirm sits above Settings / Connect so Esc dismisses
        // the confirm first rather than leaving the parent surface.
        if self.profile_delete_confirm.is_some() {
            self.cancel_delete_profile(window, cx);
            return;
        }
        if self.surface == WorkspaceSurface::Settings {
            self.leave_settings(window, cx);
            return;
        }
        if self.delete_confirm.is_some() {
            self.cancel_delete_confirm(window, cx);
            return;
        }
        if self.context_menu.is_some() {
            self.close_context_menu(cx);
            self.focus_pane(self.focused_side, window, cx);
            return;
        }
        if self.inline_edit.is_some() {
            self.cancel_inline_edit(cx);
            self.focus_pane(self.focused_side, window, cx);
            return;
        }
        // Connect profile picker sits above the form: Esc closes the picker
        // first, then a second Esc dismisses Connect.
        if let Some(form) = &mut self.connect_form {
            if form.profile_picker_open {
                form.close_profile_picker();
                cx.notify();
                return;
            }
            self.close_connect_form(window, cx);
            return;
        }
        if let Some(prompt) = self.active_host_key_prompt() {
            let request_id = prompt.request_id;
            self.reject_host_key(request_id, window, cx);
        } else if let Some(prompt) = self.active_transfer_conflict_prompt().cloned() {
            self.resolve_transfer_conflict(&prompt, ConflictDecision::CancelJob, window, cx);
        }
    }

    pub(crate) fn open_go_to_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connect_form.is_some()
            || self.active_host_key_prompt().is_some()
            || self.active_transfer_conflict_prompt().is_some()
            || self.delete_confirm.is_some()
            || self.inline_edit.is_some()
        {
            return;
        }
        self.about_open = false;
        self.go_to_path_open = true;
        self.go_to_path_input.clear();
        self.go_to_path_error = None;
        window.focus(&self.modal_focus);
        cx.notify();
    }

    pub(crate) fn close_go_to_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.go_to_path_open = false;
        self.go_to_path_input.clear();
        self.go_to_path_error = None;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    /// Dismiss About and restore keyboard-usable pane focus.
    pub(crate) fn close_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.about_open {
            return;
        }
        self.about_open = false;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn submit_go_to_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.go_to_path_input.value().trim().to_string();
        if raw.is_empty() {
            self.go_to_path_error = Some("Enter a path".into());
            cx.notify();
            return;
        }

        match self.focused_side {
            PaneSide::Local => {
                let expanded = expand_home(&raw);
                let path = LocalPath::new(expanded);
                let fs_path = Path::new(path.as_str());
                if !fs_path.is_dir() {
                    let message = if fs_path.exists() {
                        "Not a directory"
                    } else {
                        "Path not found"
                    };
                    self.go_to_path_error = Some(message.into());
                    self.status_message = Some(message.into());
                    cx.notify();
                    return;
                }
                self.go_to_path_open = false;
                self.go_to_path_input.clear();
                self.go_to_path_error = None;
                self.navigate_pane_local(path, HistoryOp::Push, window, cx);
            }
            PaneSide::Remote => {
                self.go_to_path_open = false;
                self.go_to_path_input.clear();
                self.go_to_path_error = None;
                self.navigate_pane_remote(RemotePath::new(raw), HistoryOp::Push, cx);
            }
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn handle_go_to_path_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.go_to_path_open {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.submit_go_to_path(window, cx);
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.go_to_path_input.insert(&text);
                self.go_to_path_error = None;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.go_to_path_input.handle_keystroke(keystroke) == InputKeyResult::Handled {
            self.go_to_path_error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn render_go_to_path_modal(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.go_to_path_open {
            return None;
        }
        let theme = cx.theme().clone();
        let side_label = match self.focused_side {
            PaneSide::Local => "Local",
            PaneSide::Remote => "Remote",
        };

        Some(
            div()
                .id("go-to-path-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .key_context("GoToPath")
                        .track_focus(&self.modal_focus)
                        .on_key_down(cx.listener(Self::handle_go_to_path_key))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w(px(460.0))
                        .p_4()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text)
                                .child(format!("Go to Path ({side_label})")),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child("Enter an absolute path. Enter navigates · Esc cancels"),
                        )
                        .child(text_field(
                            "go-to-path-input",
                            TextFieldModel {
                                state: &self.go_to_path_input,
                                placeholder: "Absolute path",
                                focused: true,
                                masked: false,
                            },
                            cx,
                        ))
                        .when_some(self.go_to_path_error.clone(), |card, error| {
                            card.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.colors.error)
                                    .child(error),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .child(text_button("go-to-path-cancel", "Cancel").on_click(
                                    cx.listener(|workspace, _event, window, cx| {
                                        workspace.close_go_to_path(window, cx);
                                    }),
                                ))
                                .child(text_button("go-to-path-go", "Go").primary(true).on_click(
                                    cx.listener(|workspace, _event, window, cx| {
                                        workspace.submit_go_to_path(window, cx);
                                    }),
                                )),
                        ),
                )
                .into_any_element(),
        )
    }
    pub(crate) fn render_connect_form_modal(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let form = self.connect_form.as_ref()?;
        let theme = cx.theme().clone();

        let field_row = |label: &'static str,
                         field: ConnectField,
                         state: &InputState,
                         placeholder: &'static str,
                         masked: bool,
                         cx: &mut Context<Self>| {
            div()
                .id(("connect-field", field as usize))
                .flex()
                .items_center()
                .gap_2()
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    if let Some(form) = &mut workspace.connect_form {
                        form.focused_field = field;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(text_field(
                    ("connect-input", field as usize),
                    TextFieldModel {
                        state,
                        placeholder,
                        focused: form.focused_field == field,
                        masked,
                    },
                    cx,
                )))
        };

        let auth_toggle = |label: &'static str,
                           method: AuthMethodKind,
                           id: &'static str,
                           cx: &mut Context<Self>| {
            text_button(id, label)
                .primary(form.auth_method == method)
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    if let Some(form) = &mut workspace.connect_form {
                        form.set_auth_method(method);
                        cx.notify();
                    }
                }))
        };

        let mut card = div()
            .key_context("ConnectForm")
            .track_focus(&self.connect_form_focus)
            .on_key_down(cx.listener(Self::handle_connect_form_key))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(460.0))
            .p_4()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded_md()
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.colors.text)
                    .child("Connect to Server"),
            );

        if let Some(error) = &form.error {
            card = card.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.error)
                    .child(error.clone()),
            );
        }

        let profiles = cx.resources().profiles.profiles().to_vec();
        let trigger_label = form.profile_trigger_label(&profiles);
        let profile_picker_open = form.profile_picker_open;

        card =
            card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(96.0))
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Profile"),
                    )
                    .child(
                        div()
                            .id("profile-picker-trigger")
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .border_1()
                            .border_color(theme.colors.border)
                            .rounded_md()
                            .cursor_pointer()
                            .on_click(cx.listener(|workspace, _event, _window, cx| {
                                if let Some(form) = &mut workspace.connect_form {
                                    form.profile_picker_open = !form.profile_picker_open;
                                    cx.notify();
                                }
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.0))
                                    .text_color(theme.colors.text)
                                    .child(trigger_label),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.0))
                                    .text_color(theme.colors.text_muted)
                                    .child(if profile_picker_open { "▴" } else { "▾" }),
                            ),
                    )
                    .child(text_button("connect-manage-profiles", "Manage…").on_click(
                        cx.listener(|workspace, _event, window, cx| {
                            // OpenProfiles is gated on connect_form being closed;
                            // dismiss Connect first so Settings Profiles can open.
                            workspace.close_connect_form(window, cx);
                            workspace.about_open = false;
                            workspace.surface = WorkspaceSurface::Settings;
                            workspace.set_settings_section(SettingsSection::Profiles, cx);
                            workspace.workspace_focus.focus(window);
                            cx.notify();
                        }),
                    )),
            );

        if profile_picker_open {
            let filtered = self.filtered_connect_profiles(cx);

            let mut picker_content = div().flex().flex_col().gap_0().child(
                div()
                    .id("profile-picker-filter")
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .child(text_field(
                        "profile-picker-filter-input",
                        TextFieldModel {
                            state: &form.profile_picker_filter,
                            placeholder: "Filter profiles…",
                            focused: true,
                            masked: false,
                        },
                        cx,
                    )),
            );
            if profiles.is_empty() {
                picker_content = picker_content.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(12.0))
                        .text_color(theme.colors.text_muted)
                        .child("No saved profiles — manage in Settings"),
                );
            } else if filtered.is_empty() {
                picker_content = picker_content.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(12.0))
                        .text_color(theme.colors.text_muted)
                        .child("No matches"),
                );
            } else {
                picker_content = picker_content.children(filtered.into_iter().map(|profile| {
                    let profile_id = profile.id;
                    let label = profile_list_label(profile);
                    div()
                        .id(("profile-picker-row", profile.id.0))
                        .px_2()
                        .py_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_size(px(12.0))
                        .text_color(theme.colors.text)
                        .on_click(cx.listener(move |workspace, _event, _window, cx| {
                            workspace.select_connect_profile(profile_id, cx);
                        }))
                        .child(label)
                }));
            }
            picker_content = picker_content.child(
                div()
                    .id("profile-picker-manual-entry")
                    .px_2()
                    .py_1()
                    .min_w_0()
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        if let Some(form) = &mut workspace.connect_form {
                            form.switch_to_manual_entry();
                            cx.notify();
                        }
                    }))
                    .child("Manual entry"),
            );
            let picker_panel = div()
                .id("profile-picker-panel")
                .ml(px(104.0))
                .max_h(px(200.0))
                .relative()
                .border_1()
                .border_color(theme.colors.border)
                .rounded_md()
                .bg(theme.colors.surface)
                .child(
                    div()
                        .id("profile-picker-scroll")
                        .max_h(px(200.0))
                        .overflow_y_scroll()
                        .scrollbar_width(px(0.0))
                        .track_scroll(self.profile_picker_scroll())
                        .child(picker_content),
                )
                .child(macsftp_ui::Scrollbar::vertical(
                    "profile-picker-scrollbar",
                    self.profile_picker_scroll().clone(),
                    self.profile_picker_scrollbar(),
                    window,
                    cx,
                ));
            card = card.child(picker_panel);
        }

        // When prefilled from a saved profile, `use_profile` restores the
        // secret from the Keychain into the form, so the field is usually
        // already filled. If the Keychain entry is missing, the field is
        // blank and the user re-enters it.
        if form.source_profile_id.is_some() {
            card = card.child(
                div()
                    .text_size(px(10.0))
                    .text_color(theme.colors.text_muted)
                    .child("Saved profile — credentials are restored from the Keychain."),
            );
        }

        card = card
            .child(field_row(
                "Host",
                ConnectField::Host,
                &form.host,
                "example.com",
                false,
                cx,
            ))
            .child(field_row(
                "Port",
                ConnectField::Port,
                &form.port,
                "22",
                false,
                cx,
            ))
            .child(field_row(
                "Username",
                ConnectField::Username,
                &form.username,
                "user",
                false,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(96.0))
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Auth"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(auth_toggle(
                                "Password",
                                AuthMethodKind::Password,
                                "auth-password",
                                cx,
                            ))
                            .child(auth_toggle(
                                "Private Key",
                                AuthMethodKind::PrivateKey,
                                "auth-private-key",
                                cx,
                            )),
                    ),
            );

        card = match form.auth_method {
            AuthMethodKind::Password => card.child(field_row(
                "Password",
                ConnectField::Password,
                form.password.as_input_state(),
                "",
                true,
                cx,
            )),
            AuthMethodKind::PrivateKey => card
                .child(field_row(
                    "Key path",
                    ConnectField::KeyPath,
                    &form.key_path,
                    "~/.ssh/id_ed25519",
                    false,
                    cx,
                ))
                .child(field_row(
                    "Passphrase",
                    ConnectField::Passphrase,
                    form.passphrase.as_input_state(),
                    "",
                    true,
                    cx,
                )),
        };

        // Save as is collapsed by default so the form stays short for one-off
        // connects. Expanding reveals the optional name field + Save profile.
        // Secrets are mapped to a SecretRef stub and never written to disk
        // (plan §5).
        if form.save_as_expanded {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(96.0))
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Save as"),
                    )
                    .child(
                        div()
                            .id("profile-name-row")
                            .flex_1()
                            .min_w_0()
                            .on_click(cx.listener(
                                move |workspace, _event: &ClickEvent, _window, cx| {
                                    if let Some(form) = &mut workspace.connect_form {
                                        form.focused_field = ConnectField::ProfileName;
                                        cx.notify();
                                    }
                                },
                            ))
                            .child(text_field(
                                "profile-name-input",
                                TextFieldModel {
                                    state: &form.profile_name,
                                    placeholder: "profile name (optional)",
                                    focused: form.focused_field == ConnectField::ProfileName,
                                    masked: false,
                                },
                                cx,
                            )),
                    )
                    .child(
                        text_button("save-profile", "Save profile").on_click(cx.listener(
                            |workspace, _event, _window, cx| {
                                workspace.save_current_profile(cx);
                            },
                        )),
                    ),
            );
        } else {
            card = card.child(
                div()
                    .id("save-as-profile-expand")
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        if let Some(form) = &mut workspace.connect_form {
                            form.save_as_expanded = true;
                            form.focused_field = ConnectField::ProfileName;
                            cx.notify();
                        }
                    }))
                    .child("Save as profile…"),
            );
        }

        card = card.child(
            div()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .child(
                    text_button("connect-cancel", "Cancel").on_click(cx.listener(
                        |workspace, _event, window, cx| {
                            workspace.close_connect_form(window, cx);
                        },
                    )),
                )
                .child(
                    text_button("connect-submit", "Connect")
                        .primary(true)
                        .on_click(cx.listener(|workspace, _event, window, cx| {
                            workspace.submit_connect_form(window, cx);
                        })),
                ),
        );

        Some(
            div()
                .id("connect-form-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(card)
                .into_any_element(),
        )
    }
    pub(crate) fn render_host_key_modal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let prompt = self.active_host_key_prompt()?.clone();
        let theme = cx.theme().clone();
        let request_id = prompt.request_id;

        let info_row = |label: &'static str, value: SharedString, mono: bool| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(if mono { px(11.0) } else { px(12.0) })
                        .text_color(theme.colors.text)
                        .when(mono, |value_cell| {
                            value_cell.font_family(theme.fonts.mono_family.clone())
                        })
                        .child(value),
                )
        };

        Some(
            div()
                .id("host-key-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .key_context("HostKeyModal")
                        .track_focus(&self.modal_focus)
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w(px(460.0))
                        .p_4()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text)
                                .child("Unknown Host Key"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child(
                                    "The authenticity of this server can't be established. \
                                     Verify the fingerprint before trusting it.",
                                ),
                        )
                        .child(info_row(
                            "Host",
                            format!("{}:{}", prompt.host, prompt.port).into(),
                            false,
                        ))
                        .child(info_row("Key type", prompt.algorithm.clone().into(), false))
                        .child(info_row(
                            "Fingerprint",
                            prompt.fingerprint_sha256.clone().into(),
                            true,
                        ))
                        .child(info_row(
                            "Saves to",
                            "~/Library/Application Support/macSFTP/known_hosts".into(),
                            true,
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .child(text_button("host-key-cancel", "Cancel").on_click(
                                    cx.listener(move |workspace, _event, window, cx| {
                                        workspace.reject_host_key(request_id, window, cx);
                                    }),
                                ))
                                .child(
                                    text_button("host-key-trust", "Trust and Save")
                                        .primary(true)
                                        .on_click(cx.listener(
                                            move |workspace, _event, window, cx| {
                                                workspace.accept_host_key(request_id, window, cx);
                                            },
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
    pub(crate) fn render_transfer_conflict_modal(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let prompt = self.active_transfer_conflict_prompt()?.clone();
        let theme = cx.theme().clone();
        let source = match &prompt.source {
            TransferEndpoint::Local(path) => path.as_str().to_string(),
            TransferEndpoint::Remote(path) => path.as_str().to_string(),
        };
        let destination = match &prompt.destination {
            TransferEndpoint::Local(path) => path.as_str().to_string(),
            TransferEndpoint::Remote(path) => path.as_str().to_string(),
        };
        let detail_row = |label: &'static str, value: SharedString| {
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .w(px(96.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text)
                        .child(value),
                )
        };
        let decision_button = |id: &'static str,
                               label: &'static str,
                               decision: ConflictDecision,
                               primary: bool,
                               cx: &mut Context<Self>| {
            let prompt = prompt.clone();
            text_button(id, label)
                .primary(primary)
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.resolve_transfer_conflict(&prompt, decision.clone(), window, cx);
                }))
        };

        Some(
            div()
                .id("transfer-conflict-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .key_context("TransferConflictModal")
                        .track_focus(&self.modal_focus)
                        .on_key_down(cx.listener(Self::handle_transfer_conflict_key))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .w(px(460.0))
                        .p_4()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .text_size(px(14.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text)
                                .child("File already exists"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child("Choose how to handle the existing destination."),
                        )
                        .child(detail_row("Source", source.into()))
                        .child(detail_row("Destination", destination.into()))
                        .child(detail_row("Size", format_size(prompt.source_size)))
                        .child(detail_row(
                            "Modified",
                            format_timestamp(prompt.source_modified_at),
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child("All applies to remaining conflicts in this transfer."),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child("Enter renames · Esc cancels"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(96.0))
                                        .flex_none()
                                        .text_size(px(11.0))
                                        .text_color(theme.colors.text_muted)
                                        .child("New name"),
                                )
                                .child(div().flex_1().min_w_0().child(text_field(
                                    "conflict-rename-input",
                                    TextFieldModel {
                                        state: &self.conflict_rename,
                                        placeholder: "new file name",
                                        focused: true,
                                        masked: false,
                                    },
                                    cx,
                                ))),
                        )
                        .when_some(self.conflict_rename_error.clone(), |card, error| {
                            card.child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme.colors.error)
                                    .child(error),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .child(decision_button(
                                    "conflict-cancel",
                                    "Cancel",
                                    ConflictDecision::CancelJob,
                                    false,
                                    cx,
                                ))
                                .child(decision_button(
                                    "conflict-skip",
                                    "Skip",
                                    ConflictDecision::Skip {
                                        apply_to_all: false,
                                    },
                                    false,
                                    cx,
                                ))
                                .child(decision_button(
                                    "conflict-skip-all",
                                    "Skip All",
                                    ConflictDecision::Skip { apply_to_all: true },
                                    false,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .child(
                                    text_button("conflict-rename", "Rename")
                                        .primary(true)
                                        .on_click(cx.listener(|workspace, _event, window, cx| {
                                            workspace.submit_transfer_rename(false, window, cx);
                                        })),
                                )
                                .child(text_button("conflict-rename-all", "Rename All").on_click(
                                    cx.listener(|workspace, _event, window, cx| {
                                        workspace.submit_transfer_rename(true, window, cx);
                                    }),
                                ))
                                .child(decision_button(
                                    "conflict-overwrite",
                                    "Overwrite",
                                    ConflictDecision::Overwrite {
                                        apply_to_all: false,
                                    },
                                    false,
                                    cx,
                                ))
                                .child(decision_button(
                                    "conflict-overwrite-all",
                                    "Overwrite All",
                                    ConflictDecision::Overwrite { apply_to_all: true },
                                    false,
                                    cx,
                                )),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Confirm deleting a saved connection profile (Settings or Connect form).
    pub(crate) fn render_profile_delete_confirm_modal(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let profile_id = self.profile_delete_confirm?;
        let body = {
            let profile = cx.resources().profiles.find_profile(profile_id)?;
            format!(
                "Delete \"{}\" ({}@{})? This cannot be undone.",
                profile.name, profile.username, profile.host
            )
        };
        let theme = cx.theme().clone();

        let card = div()
            .key_context("ProfileDeleteConfirmModal")
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(|workspace, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    workspace.cancel_delete_profile(window, cx);
                } else if event.keystroke.key == "enter" && event.keystroke.modifiers.platform {
                    cx.stop_propagation();
                    workspace.confirm_delete_profile(window, cx);
                }
            }))
            .flex()
            .flex_col()
            .gap_3()
            .w(px(420.0))
            .p_4()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .rounded_md()
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.colors.text)
                    .child("Delete Profile?"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("profile-delete-cancel", "Cancel")
                            .primary(true)
                            .on_click(cx.listener(|workspace, _event, window, cx| {
                                workspace.cancel_delete_profile(window, cx);
                            })),
                    )
                    .child(
                        text_button("profile-delete-confirm", "Delete")
                            .danger(true)
                            .on_click(cx.listener(|workspace, _event, window, cx| {
                                workspace.confirm_delete_profile(window, cx);
                            })),
                    ),
            );

        Some(
            div()
                .id("profile-delete-confirm-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(card)
                .into_any_element(),
        )
    }
}
