use gpui::{
    Context, FontWeight, IntoElement, KeyDownEvent, ParentElement, Styled, Window,
    WindowControlArea, div, prelude::*, px,
};
use macsftp_core::AuthMethodKind;
use macsftp_ui::{
    ActiveTheme, InputKeyResult, InputState, TextFieldModel, empty_state, text_button, text_field,
};
use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::profiles::{ProfileEditorField, SettingsSection, profile_list_label};
use macsftp_storage::AppearancePreference;

impl crate::workspace::Workspace {
    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let selected_section = self.settings.section;
        let selected_appearance = cx.resources().config.config().appearance;
        let appearance_button = |id: &'static str,
                                 label: &'static str,
                                 appearance: AppearancePreference,
                                 cx: &mut Context<Self>| {
            let selected = selected_appearance == appearance;
            div()
                .id(id)
                .flex_1()
                .px_3()
                .py_2()
                .text_center()
                .text_size(px(12.0))
                .rounded_sm()
                .border_1()
                .border_color(if selected {
                    theme.colors.accent
                } else {
                    theme.colors.border
                })
                .bg(if selected {
                    theme.colors.element_selected
                } else {
                    theme.colors.background
                })
                .text_color(if selected {
                    theme.colors.accent
                } else {
                    theme.colors.text
                })
                .hover(|style| style.bg(theme.colors.element_hover))
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.set_appearance(appearance, window, cx);
                }))
                .child(label)
        };

        let sidebar_item = |id: &'static str,
                            label: &'static str,
                            section: SettingsSection,
                            cx: &mut Context<Self>| {
            let selected = selected_section == section;
            div()
                .id(id)
                .px_2()
                .py_2()
                .rounded_sm()
                .bg(if selected {
                    theme.colors.element_selected
                } else {
                    theme.colors.surface
                })
                .text_size(px(12.0))
                .text_color(if selected {
                    theme.colors.text
                } else {
                    theme.colors.text_muted
                })
                .hover(|style| style.bg(theme.colors.element_hover))
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    workspace.set_settings_section(section, cx);
                }))
                .child(label)
        };

        let body = match self.settings.section {
            SettingsSection::General => div()
                .flex_1()
                .min_w_0()
                .p_6()
                .track_focus(&self.workspace_focus)
                .on_key_down(cx.listener(Self::handle_external_editor_key))
                .child(
                    div()
                        .max_w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap_5()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(16.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Appearance"),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.colors.text_muted)
                                        .child(
                                            "System follows macOS; fixed modes use One Light or One Dark.",
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(appearance_button(
                                    "appearance-system",
                                    "System",
                                    AppearancePreference::System,
                                    cx,
                                ))
                                .child(appearance_button(
                                    "appearance-light",
                                    "One Light",
                                    AppearancePreference::Light,
                                    cx,
                                ))
                                .child(appearance_button(
                                    "appearance-dark",
                                    "One Dark",
                                    AppearancePreference::Dark,
                                    cx,
                                )),
                        )
                        .children(self.config_error.clone().map(|error| {
                            div()
                                .p_3()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.colors.error)
                                .text_size(px(12.0))
                                .text_color(theme.colors.error)
                                .child(error)
                        }))
                        .child(div().h(px(1.0)).bg(theme.colors.border))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("External editor"),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.colors.text_muted)
                                        .child(
                                            "Command or app used to open remote files for editing. \
                                             Leave blank for the system default.",
                                        ),
                                )
                                .child(
                                    div()
                                        .id("settings-external-editor")
                                        .on_click(cx.listener(|workspace, _event, window, cx| {
                                            workspace.focus_external_editor(window, cx);
                                        }))
                                        .child(text_field(
                                            ("settings-external-editor-input", 0usize),
                                            TextFieldModel {
                                                state: &self.settings.external_editor_input,
                                                placeholder: "System default",
                                                focused: self.settings.external_editor_focused,
                                                masked: false,
                                            },
                                            cx,
                                        )),
                                ),
                        )
                        .child(div().h(px(1.0)).bg(theme.colors.border))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Diagnostics"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            text_button("open-log-folder", "Open Log Folder")
                                                .on_click(cx.listener(
                                                    |workspace, _event, _window, cx| {
                                                        workspace.open_log_folder(cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            text_button("copy-version", "Copy Version Info")
                                                .on_click(cx.listener(
                                                    |workspace, _event, _window, cx| {
                                                        workspace.copy_version_info(cx);
                                                    },
                                                )),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
            SettingsSection::Profiles => self.render_settings_profiles(cx),
        };

        div()
            .key_context("Settings")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(theme.colors.background)
            .child(
                div()
                    .h(theme.sizes.tab_bar_height)
                    .flex_none()
                    .flex()
                    .items_center()
                    // Same transparent-titlebar strip as the tab bar: reserve
                    // room for the native traffic lights, drag moves window.
                    .window_control_area(WindowControlArea::Drag)
                    .px_3()
                    .bg(theme.colors.surface)
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .child(
                        div()
                            .w(px(crate::workspace::render::TRAFFIC_LIGHT_RESERVE))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Settings"),
                    )
                    .child(text_button("settings-done", "Done").on_click(cx.listener(
                        |workspace, _event, window, cx| {
                            workspace.leave_settings(window, cx);
                        },
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(180.0))
                            .flex_none()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .bg(theme.colors.surface)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(sidebar_item(
                                "settings-section-general",
                                "General",
                                SettingsSection::General,
                                cx,
                            ))
                            .child(sidebar_item(
                                "settings-section-profiles",
                                "Profiles",
                                SettingsSection::Profiles,
                                cx,
                            )),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    /// Focus the Settings → General external-editor field (click or key entry).
    pub(crate) fn focus_external_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.external_editor_focused = true;
        window.focus(&self.workspace_focus);
        cx.notify();
    }

    pub(crate) fn handle_external_editor_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.external_editor_focused {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.settings.external_editor_input.insert(&text);
                self.commit_external_editor(cx);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self
            .settings
            .external_editor_input
            .handle_keystroke(keystroke)
            == InputKeyResult::Handled
        {
            self.commit_external_editor(cx);
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Persist the External editor field to config on every edit. An empty
    /// value (after trim) clears the override (`None`); otherwise the trimmed
    /// value is stored. Mirrors the other config setters' error handling.
    pub(crate) fn commit_external_editor(&mut self, cx: &mut Context<Self>) {
        let trimmed = self
            .settings
            .external_editor_input
            .value()
            .trim()
            .to_string();
        let editor = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        match cx.resources_mut().config.set_external_editor(editor) {
            Ok(()) => self.config_error = None,
            Err(error) => {
                warn!(error = %error, "could not save external_editor");
                self.config_error =
                    Some("Could not write config.json. Check file permissions.".into());
            }
        }
    }

    /// Settings → Profiles: list on the left, editor form on the right.
    pub(crate) fn render_settings_profiles(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let profiles = cx.resources().profiles.profiles().to_vec();
        let total_count = profiles.len();
        let filtered = self.filtered_profiles(&profiles);
        let filtered_count = filtered.len();
        let filter_active = !self.settings.profile_filter.value().trim().is_empty();
        let selected_id = self.settings.selected_profile_id;
        let editing_new = self
            .settings
            .profile_editor
            .as_ref()
            .is_some_and(|editor| editor.is_new);
        let filter_focused = self.settings.profile_filter_focused;

        // Materialize rows before the editor borrow of `cx` (listeners capture).
        let list_rows: Vec<_> = filtered
            .into_iter()
            .map(|profile| {
                let profile_id = profile.id;
                let selected = selected_id == Some(profile_id) && !editing_new;
                let label = profile_list_label(profile);
                div()
                    .id(("settings-profile-row", profile_id.0))
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .bg(if selected {
                        theme.colors.element_selected
                    } else {
                        theme.colors.background
                    })
                    .text_size(px(12.0))
                    .text_color(theme.colors.text)
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        workspace.select_profile_in_settings(profile_id, cx);
                    }))
                    .child(label)
            })
            .collect();

        let list_body: gpui::AnyElement = if total_count == 0 {
            empty_state(
                "No saved profiles",
                vec![
                    text_button("settings-empty-new-profile", "New Profile").on_click(cx.listener(
                        |workspace, _event, _window, cx| {
                            workspace.start_new_profile(cx);
                        },
                    )),
                ],
                cx,
            )
            .into_any_element()
        } else if filtered_count == 0 && filter_active {
            empty_state("No matches", vec![], cx).into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .min_h_0()
                .children(list_rows)
                .into_any_element()
        };

        let detail = if self.settings.profile_editor.is_some() {
            self.render_profile_editor(cx)
        } else if total_count == 0 {
            div()
                .text_size(px(13.0))
                .text_color(theme.colors.text_muted)
                .child("Create a profile to get started")
                .into_any_element()
        } else {
            div()
                .text_size(px(13.0))
                .text_color(theme.colors.text_muted)
                .child("Select a profile or create a new one")
                .into_any_element()
        };

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                div()
                    .w(px(220.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .border_r_1()
                    .border_color(theme.colors.border)
                    .track_focus(&self.workspace_focus)
                    .on_key_down(cx.listener(Self::handle_profile_filter_key))
                    .child(text_button("settings-new-profile", "New Profile").on_click(
                        cx.listener(|workspace, _event, _window, cx| {
                            workspace.start_new_profile(cx);
                        }),
                    ))
                    .child(
                        div()
                            .id("settings-profile-filter")
                            .on_click(cx.listener(|workspace, _event, window, cx| {
                                workspace.focus_profile_filter(window, cx);
                            }))
                            .child(text_field(
                                "settings-profile-filter-input",
                                TextFieldModel {
                                    state: &self.settings.profile_filter,
                                    placeholder: "Filter profiles…",
                                    focused: filter_focused,
                                    masked: false,
                                },
                                cx,
                            )),
                    )
                    .child(list_body),
            )
            .child(div().flex_1().min_w_0().p_6().child(detail))
            .into_any_element()
    }

    pub(crate) fn render_profile_editor(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(editor) = self.settings.profile_editor.as_ref() else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();

        let field_row = |label: &'static str,
                         field: ProfileEditorField,
                         state: &InputState,
                         placeholder: &'static str,
                         masked: bool,
                         focused: ProfileEditorField,
                         cx: &mut Context<Self>| {
            div()
                .id(("profile-editor-field", field as usize))
                .flex()
                .items_center()
                .gap_2()
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.settings.profile_filter_focused = false;
                    if let Some(editor) = workspace.settings.profile_editor.as_mut() {
                        editor.focused_field = field;
                    }
                    window.focus(&workspace.modal_focus);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(120.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(text_field(
                    ("profile-editor-input", field as usize),
                    TextFieldModel {
                        state,
                        placeholder,
                        focused: focused == field,
                        masked,
                    },
                    cx,
                )))
        };

        let auth_toggle = |label: &'static str,
                           method: AuthMethodKind,
                           id: &'static str,
                           active: AuthMethodKind,
                           cx: &mut Context<Self>| {
            text_button(id, label)
                .primary(active == method)
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    if let Some(editor) = workspace.settings.profile_editor.as_mut() {
                        editor.set_auth_method(method);
                        cx.notify();
                    }
                }))
        };

        let focused = editor.focused_field;
        let auth_method = editor.auth_method;
        let title = if editor.is_new {
            "New Profile"
        } else {
            "Edit Profile"
        };

        let mut form = div()
            .id("profile-editor")
            .key_context("ProfileEditor")
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(Self::handle_profile_editor_key))
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .max_w(px(560.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.colors.text)
                    .child(title),
            );

        if let Some(error) = &editor.error {
            form = form.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.error)
                    .child(error.clone()),
            );
        }

        form = form
            .child(field_row(
                "Name",
                ProfileEditorField::Name,
                &editor.name,
                "optional",
                false,
                focused,
                cx,
            ))
            .child(field_row(
                "Host",
                ProfileEditorField::Host,
                &editor.host,
                "example.com",
                false,
                focused,
                cx,
            ))
            .child(field_row(
                "Port",
                ProfileEditorField::Port,
                &editor.port,
                "22",
                false,
                focused,
                cx,
            ))
            .child(field_row(
                "Username",
                ProfileEditorField::Username,
                &editor.username,
                "user",
                false,
                focused,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(120.0))
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
                                "profile-auth-password",
                                auth_method,
                                cx,
                            ))
                            .child(auth_toggle(
                                "Private Key",
                                AuthMethodKind::PrivateKey,
                                "profile-auth-private-key",
                                auth_method,
                                cx,
                            )),
                    ),
            );

        form = match auth_method {
            AuthMethodKind::Password => form.child(field_row(
                "Password",
                ProfileEditorField::Password,
                editor.password.as_input_state(),
                if editor.secret_present_hint {
                    "leave blank to keep"
                } else {
                    ""
                },
                true,
                focused,
                cx,
            )),
            AuthMethodKind::PrivateKey => form
                .child(field_row(
                    "Key path",
                    ProfileEditorField::KeyPath,
                    &editor.key_path,
                    "~/.ssh/id_ed25519",
                    false,
                    focused,
                    cx,
                ))
                .child(field_row(
                    "Passphrase",
                    ProfileEditorField::Passphrase,
                    editor.passphrase.as_input_state(),
                    if editor.secret_present_hint {
                        "leave blank to keep"
                    } else {
                        ""
                    },
                    true,
                    focused,
                    cx,
                )),
        };

        form = form.child(field_row(
            "Remote path",
            ProfileEditorField::DefaultRemotePath,
            &editor.default_remote_path,
            "/home/user",
            false,
            focused,
            cx,
        ));

        if editor.secret_present_hint {
            form = form.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.colors.text_muted)
                    .child("Credentials are stored in the Keychain. Leave blank to keep."),
            );
        }

        form = form.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    text_button("settings-save-profile", "Save")
                        .primary(true)
                        .on_click(cx.listener(|workspace, _event, _window, cx| {
                            workspace.save_profile_editor(cx);
                        })),
                )
                .when(!editor.is_new, |row| {
                    let profile_id = editor.profile_id;
                    row.child(
                        text_button("settings-delete-profile", "Delete…")
                            .danger(true)
                            .on_click(cx.listener(move |workspace, _event, window, cx| {
                                if let Some(id) = profile_id {
                                    workspace.request_delete_profile(id, window, cx);
                                } else if let Some(id) = workspace.settings.selected_profile_id {
                                    workspace.request_delete_profile(id, window, cx);
                                }
                            })),
                    )
                }),
        );

        form.into_any_element()
    }
}
