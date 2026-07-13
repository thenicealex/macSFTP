#![allow(unused_imports)]
use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use gpui::{
    App, ClickEvent, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, Hsla, IntoElement,
    KeyDownEvent, ParentElement, Render, ScrollStrategy, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use macsftp_core::{
    AppCommand, AppEvent, AppState, AuthCredential, AuthMethod, AuthMethodKind,
    CommandDispatchError, ConflictDecision, ConflictDecisionCommand, ConflictRequest,
    ConflictRequestId, ConnectCommand, ConnectionProfile, ConnectionSettings, ConnectionState,
    DisconnectReason, EntryPath, ErrorCode, FileKind, HostKeyDecisionCommand, HostKeyPrompt,
    LocalPath, ModalRequest, ModalRequestId, ProfileId, RemotePath, SecretRef, TabId, TabState,
    Timestamp, TransferConflictPrompt, TransferDirection, TransferEndpoint, TransferHistoryId,
    TransferHistoryRecord, TransferHistoryStatus, TransferJob, TransferState, TrustRequestId,
    UserFacingError, history_status_for_plan, sort_entries,
};
use macsftp_platform::{AppPaths, read_local_directory};
use macsftp_sftp::{EventReceiver, RuntimeClient};
use macsftp_storage::{
    AppearancePreference, ConfigStore, KeychainError, KeychainStore, ProfileStore,
    ResidualTempStore, TransferHistoryStore,
};
use macsftp_ui::{
    ActiveTheme, DragPreview, FileRowModel, IconName, InputKeyResult, InputState, TextFieldModel,
    Theme, TransferRow, connection_status, copy_name, empty_state, file_row, file_table_header,
    format_size, format_timestamp, icon, icon_button, section_header_static, tab, text_button,
    text_field, text_tooltip, transfer_history_detail, transfer_history_title, transfer_row,
    transfer_title,
};

use tracing::{debug, warn};

use crate::app_actions::*;
use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::connect_form::*;
use crate::workspace::helpers::*;
use crate::workspace::nav::HistoryOp;
use crate::workspace::*;

impl crate::workspace::Workspace {
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
        cx.transfers_mut()
            .pending_conflicts
            .retain(|conflict| conflict.id != request_id);
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
        self.remove_conflict_modal(prompt.request_id, cx);
        self.send_command(
            AppCommand::ResolveTransferConflict(ConflictDecisionCommand {
                plan_id: prompt.plan_id,
                request_id: prompt.request_id,
                decision,
            }),
            cx,
        );
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
        self.remove_modal(request_id);
        self.send_command(
            AppCommand::AcceptHostKey(HostKeyDecisionCommand { request_id }),
            cx,
        );
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
        self.remove_modal(request_id);
        self.send_command(AppCommand::RejectHostKey { request_id }, cx);
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
        // Go to Path is highest priority so Esc always dismisses the path
        // field even if another surface is also "open" in theory.
        if self.go_to_path_open {
            self.close_go_to_path(window, cx);
            return;
        }
        if self.about_open {
            self.about_open = false;
            cx.notify();
            return;
        }
        if self.surface == WorkspaceSurface::Settings {
            self.surface = WorkspaceSurface::Files;
            self.workspace_focus.focus(window);
            cx.notify();
            return;
        }
        if self.delete_confirm.is_some() {
            self.cancel_delete_confirm(window, cx);
            return;
        }
        if self.context_menu.is_some() {
            self.close_context_menu(cx);
            return;
        }
        if self.inline_edit.is_some() {
            self.cancel_inline_edit(cx);
            return;
        }
        if self.connect_form.is_some() {
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
                if !Path::new(path.as_str()).exists() {
                    self.go_to_path_error = Some("Path not found".into());
                    self.status_message = Some("Path not found".into());
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
                                .justify_end()
                                .gap_2()
                                .child(
                                    text_button("go-to-path-cancel", "Cancel").on_click(
                                        cx.listener(|workspace, _event, window, cx| {
                                            workspace.close_go_to_path(window, cx);
                                        }),
                                    ),
                                )
                                .child(
                                    text_button("go-to-path-go", "Go")
                                        .primary(true)
                                        .on_click(cx.listener(|workspace, _event, window, cx| {
                                            workspace.submit_go_to_path(window, cx);
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
    pub(crate) fn render_connect_form_modal(
        &self,
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

        // Saved profiles: pick one to prefill the form, or delete it.
        let saved_profiles: Vec<(ProfileId, String, String)> = cx
            .resources()
            .profiles
            .profiles()
            .iter()
            .map(|profile| {
                (
                    profile.id,
                    profile.name.clone(),
                    format!("{}@{}:{}", profile.username, profile.host, profile.port),
                )
            })
            .collect();
        if !saved_profiles.is_empty() {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Saved profiles"),
                    )
                    .children(
                        saved_profiles
                            .into_iter()
                            .map(|(profile_id, name, summary)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_size(px(12.0))
                                                    .text_color(theme.colors.text)
                                                    .child(name),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.0))
                                                    .text_color(theme.colors.text_muted)
                                                    .child(summary),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .gap_1()
                                            .child(
                                                text_button(("use-profile", profile_id.0), "Use")
                                                    .on_click(cx.listener(
                                                        move |workspace, _event, _window, _cx| {
                                                            workspace.use_profile(profile_id, _cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                text_button(
                                                    ("delete-profile", profile_id.0),
                                                    "Delete",
                                                )
                                                .on_click(cx.listener(
                                                    move |workspace, _event, _window, _cx| {
                                                        workspace.delete_profile(profile_id, _cx);
                                                    },
                                                )),
                                            ),
                                    )
                            }),
                    )
                    .child(div().h(px(1.0)).bg(theme.colors.border).my_1()),
            );
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
                &form.password,
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
                    &form.passphrase,
                    "",
                    true,
                    cx,
                )),
        };

        // Save the current connection as a profile. The secret is mapped
        // to a SecretRef stub and never written to disk (plan §5).
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

        card = card.child(
            div()
                .flex()
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
}
