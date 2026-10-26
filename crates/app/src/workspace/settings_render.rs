use crate::resources::ActiveResources;
use crate::workspace::TextInputTarget;
use crate::workspace::Workspace;
use crate::workspace::profiles::{
    PassphrasePolicy, ProfileEditorField, ProfileRouteKind, SettingsSection, profile_list_label,
};
use gpui_kit::base::{Button, Toggle};
use gpui_kit::{
    App, ClickEvent, Context, FontWeight, IntoElement, ParentElement, Styled, Window,
    WindowControlArea, div, prelude::*, px,
};
use macsftp_core::{AuthMethodKind, ConnectionRoute, is_bidi_or_control};
use macsftp_storage::AppearancePreference;
use macsftp_ui::{
    ActiveTheme, IconName, InputState, TextFieldModel, ThemeColors, empty_state, icon,
    ordinary_text_field, plain_text_field, text_button, text_field, text_tooltip,
};

impl crate::workspace::Workspace {
    pub(crate) fn render_settings(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();
        let selected_section = self.settings.section;
        let sidebar_item = |id: &'static str,
                            label: &'static str,
                            icon_name: IconName,
                            section: SettingsSection,
                            cx: &mut Context<Self>| {
            let selected = selected_section == section;
            Button::new(id)
                .w_full()
                .justify_start()
                .h(px(32.0))
                .px_2p5()
                .rounded(theme.sizes.button_radius)
                // The transparent border reserves the one pixel the focus ring
                // needs, so focusing never shifts the label.
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .bg(if selected {
                    theme.colors.element_selected
                } else {
                    theme.colors.surface
                })
                .text_size(px(13.0))
                .line_height(gpui_kit::relative(1.2))
                .text_color(if selected {
                    theme.colors.text
                } else {
                    theme.colors.text_muted
                })
                .font_weight(if selected {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
                .hover(|style| style.bg(theme.colors.element_hover))
                .focus_visible(|style| style.border_color(theme.colors.border_focused))
                .accessibility_label(label)
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    workspace.set_settings_section(section, cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(
                            icon_name,
                            if selected {
                                theme.colors.accent
                            } else {
                                theme.colors.text_muted
                            },
                        ))
                        .child(label),
                )
        };

        let body = match self.settings.section {
            SettingsSection::General => self.render_settings_general(window, cx),
            SettingsSection::Editor => self.render_settings_editor(window, cx),
            SettingsSection::Profiles => self.render_settings_profiles(window, cx),
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.colors.text)
                            .child("Settings"),
                    )
                    .child(div().flex_1())
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
                            .w(px(160.0))
                            .flex_none()
                            .p_2p5()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .bg(theme.colors.surface)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(sidebar_item(
                                "settings-section-general",
                                "General",
                                IconName::Folder,
                                SettingsSection::General,
                                cx,
                            ))
                            .child(sidebar_item(
                                "settings-section-editor",
                                "Editor",
                                IconName::File,
                                SettingsSection::Editor,
                                cx,
                            ))
                            .child(sidebar_item(
                                "settings-section-profiles",
                                "Saved Connections",
                                IconName::Server,
                                SettingsSection::Profiles,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .children(self.config_error.clone().map(|error| {
                                div()
                                    .debug_selector(|| "settings-config-error".into())
                                    .mx_6()
                                    .mt_3()
                                    .p_3()
                                    .rounded(theme.sizes.button_radius)
                                    .border_1()
                                    .border_color(theme.colors.error)
                                    .bg(theme.colors.surface)
                                    .text_size(px(12.5))
                                    .text_color(theme.colors.error)
                                    .child(error)
                            }))
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_settings_general(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();
        let selected_appearance = cx.resources().config.config().appearance;
        let confirm_delete = cx.resources().config.config().confirm_delete;
        let show_hidden_files = cx.resources().config.config().show_hidden_files;
        let preserve_mtime = cx
            .resources()
            .config
            .config()
            .metadata_policy
            .preserve_mtime;
        let preserve_permissions = cx
            .resources()
            .config
            .config()
            .metadata_policy
            .preserve_permissions;

        let appearance_segment = |id: &'static str,
                                  label: &'static str,
                                  appearance: AppearancePreference,
                                  cx: &mut Context<Self>| {
            let selected = selected_appearance == appearance;
            div()
                .id(id)
                .flex_1()
                .py_1p5()
                .px_3()
                .text_center()
                .text_size(px(13.0))
                .rounded(theme.sizes.button_radius)
                .cursor_pointer()
                .when(selected, |seg| {
                    seg.bg(theme.colors.surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .shadow(theme.shadow_chip())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.colors.text)
                })
                .when(!selected, |seg| {
                    seg.text_color(theme.colors.text_muted)
                        .hover(|style| style.bg(theme.colors.element_hover))
                })
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.set_appearance(appearance, window, cx);
                }))
                .child(label)
        };

        let section_card = |title: &'static str,
                            description: &'static str,
                            children: Vec<gpui_kit::AnyElement>| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.colors.text)
                                .child(title),
                        )
                        .when(!description.is_empty(), |el| {
                            el.child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.colors.text_muted)
                                    .line_height(gpui_kit::relative(1.35))
                                    .child(description),
                            )
                        }),
                )
                .child(
                    div()
                        .rounded(theme.sizes.modal_radius)
                        .border_1()
                        .border_color(theme.colors.border)
                        .bg(theme.colors.surface)
                        .overflow_hidden()
                        .children(children),
                )
        };

        let weak = cx.weak_entity();
        let on_confirm_delete =
            settings_toggle_handler(weak.clone(), Workspace::set_confirm_delete_setting);
        let on_show_hidden =
            settings_toggle_handler(weak.clone(), Workspace::set_show_hidden_files_setting);
        let on_preserve_mtime =
            settings_toggle_handler(weak.clone(), Workspace::set_metadata_preserve_mtime);
        let on_preserve_permissions =
            settings_toggle_handler(weak, Workspace::set_metadata_preserve_permissions);

        let form = div()
            .flex_none()
            .w_full()
            .p_6()
            .track_focus(&self.workspace_focus)
            .child(
                div()
                    .max_w(px(580.0))
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(section_card(
                        "Appearance",
                        "Match your Mac’s appearance, or always use Light or Dark.",
                        vec![
                            div()
                                .p_3()
                                .child(
                                    div()
                                        .flex()
                                        .gap_1()
                                        .p_1()
                                        .rounded(theme.sizes.button_radius)
                                        .bg(theme.colors.element_active)
                                        .border_1()
                                        .border_color(theme.colors.border)
                                        .child(appearance_segment(
                                            "appearance-system",
                                            "Automatic",
                                            AppearancePreference::System,
                                            cx,
                                        ))
                                        .child(appearance_segment(
                                            "appearance-light",
                                            "Light",
                                            AppearancePreference::Light,
                                            cx,
                                        ))
                                        .child(appearance_segment(
                                            "appearance-dark",
                                            "Dark",
                                            AppearancePreference::Dark,
                                            cx,
                                        )),
                                )
                                .into_any_element(),
                        ],
                    ))
                    .child(section_card(
                        "Behavior",
                        "Choose what macSFTP asks before destructive actions and which items it lists.",
                        vec![
                            settings_switch_row(
                                "settings-confirm-delete",
                                "Confirm before delete",
                                "Ask before removing files. Turn this back on if you dismissed the prompt earlier.",
                                confirm_delete,
                                on_confirm_delete,
                                &theme.colors,
                            ),
                            div().h(px(1.0)).bg(theme.colors.border).into_any_element(),
                            settings_switch_row(
                                "settings-show-hidden-files",
                                "Show hidden files",
                                "Include items whose names start with a dot.",
                                show_hidden_files,
                                on_show_hidden,
                                &theme.colors,
                            ),
                        ],
                    ))
                    .child(section_card(
                        "Transfers",
                        "Applied to new uploads and downloads; retries keep the policy they started with. Remote-edit uploads always keep their timestamp. If the destination can’t apply metadata, the transfer still completes with a warning.",
                        vec![
                            settings_switch_row(
                                "settings-preserve-mtime",
                                "Preserve modification time",
                                "Keep the source date. When off, the destination keeps the time written by the transfer.",
                                preserve_mtime,
                                on_preserve_mtime,
                                &theme.colors,
                            ),
                            div().h(px(1.0)).bg(theme.colors.border).into_any_element(),
                            settings_switch_row(
                                "settings-preserve-permissions",
                                "Preserve permissions",
                                "Keep the source file mode where the server allows it. When off, the destination applies its own defaults.",
                                preserve_permissions,
                                on_preserve_permissions,
                                &theme.colors,
                            ),
                        ],
                    ))
                    .child(section_card(
                        "Troubleshooting",
                        "Open the log folder or copy build details for a bug report.",
                        vec![
                            div()
                                .p_3p5()
                                .flex()
                                .gap_3()
                                .items_center()
                                .child(
                                    text_button("open-log-folder", "Show Logs").on_click(
                                        cx.listener(|workspace, _event, _window, cx| {
                                            workspace.open_log_folder(cx);
                                        }),
                                    ),
                                )
                                .child(
                                    text_button("copy-version", "Copy Version Info").on_click(
                                        cx.listener(|workspace, _event, _window, cx| {
                                            workspace.copy_version_info(cx);
                                        }),
                                    ),
                                )
                                .into_any_element(),
                        ],
                    )),
            );

        macsftp_ui::scroll_area(
            "settings-general-scroll",
            form,
            &self.settings.general_scroll,
            &self.settings.general_scrollbar,
            window,
            cx,
        )
        .into_any_element()
    }

    pub(crate) fn render_settings_editor(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();

        let section_card =
            |title: &'static str, description: &'static str, content: gpui_kit::AnyElement| {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.colors.text)
                                    .child(title),
                            )
                            .when(!description.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.colors.text_muted)
                                        .line_height(gpui_kit::relative(1.35))
                                        .child(description),
                                )
                            }),
                    )
                    .child(
                        div()
                            .rounded(theme.sizes.modal_radius)
                            .border_1()
                            .border_color(theme.colors.border)
                            .bg(theme.colors.surface)
                            .overflow_hidden()
                            .child(content),
                    )
            };

        let file_editor_card = section_card(
            "File Editor",
            "Enter an app or command. Leave blank to use your Mac’s default app.",
            div()
                .p_3p5()
                .child(
                    div()
                        .id("settings-external-editor")
                        .on_click(cx.listener(|workspace, _event, window, cx| {
                            workspace.focus_external_editor(window, cx);
                        }))
                        .child(ordinary_text_field(
                            "settings-external-editor-input",
                            &self.settings.external_editor_input,
                        )),
                )
                .into_any_element(),
        );

        let editor_rules_card = section_card(
            "Editor Associations",
            "Route files to specific editors by filename pattern (e.g. *.rs or *.png).",
            self.render_editor_rules(cx),
        );

        let form = div()
            .flex_none()
            .w_full()
            .p_6()
            .track_focus(&self.workspace_focus)
            .child(
                div()
                    .max_w(px(580.0))
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(file_editor_card)
                    .child(editor_rules_card),
            );

        macsftp_ui::scroll_area(
            "settings-editor-scroll",
            form,
            &self.settings.editor_page_scroll,
            &self.settings.editor_page_scrollbar,
            window,
            cx,
        )
        .into_any_element()
    }

    pub(crate) fn set_confirm_delete_setting(
        &mut self,
        confirm_delete: bool,
        cx: &mut Context<Self>,
    ) {
        let result = cx.resources_mut().config.set_confirm_delete(confirm_delete);
        self.finish_config_change(result, "confirm_delete", cx);
        cx.notify();
    }

    pub(crate) fn set_show_hidden_files_setting(
        &mut self,
        show_hidden_files: bool,
        cx: &mut Context<Self>,
    ) {
        let result = cx
            .resources_mut()
            .config
            .set_show_hidden_files(show_hidden_files);
        self.finish_config_change(result, "show_hidden_files", cx);
        cx.notify();
    }

    pub(crate) fn set_metadata_preserve_mtime(
        &mut self,
        preserve_mtime: bool,
        cx: &mut Context<Self>,
    ) {
        let result = cx
            .resources_mut()
            .config
            .set_metadata_preserve_mtime(preserve_mtime);
        self.finish_config_change(result, "metadata preserve_mtime", cx);
        cx.notify();
    }

    pub(crate) fn set_metadata_preserve_permissions(
        &mut self,
        preserve_permissions: bool,
        cx: &mut Context<Self>,
    ) {
        let result = cx
            .resources_mut()
            .config
            .set_metadata_preserve_permissions(preserve_permissions);
        self.finish_config_change(result, "metadata preserve_permissions", cx);
        cx.notify();
    }

    /// Focus the Settings → Editor external-editor field (click or key entry).
    pub(crate) fn focus_external_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings
            .external_editor_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Persist the External editor field to config on every edit. An empty
    /// value (after trim) clears the override (`None`); otherwise the trimmed
    /// value is stored. Mirrors the other config setters' error handling.
    pub(crate) fn commit_external_editor(&mut self, cx: &mut Context<Self>) {
        let trimmed = self
            .settings
            .external_editor_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if trimmed.chars().any(is_bidi_or_control) {
            self.config_error = Some("Editor command contains invalid characters.".into());
            cx.notify();
            return;
        }
        let editor = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        let result = cx.resources_mut().config.set_external_editor(editor);
        self.record_config_result(result, "external_editor");
    }

    pub(crate) fn render_editor_rules(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();
        let rules = cx.resources().config.config().editor_rules.clone();

        let mut content = div().flex().flex_col();

        if rules.is_empty() {
            content = content.child(
                div()
                    .px_4()
                    .py_3p5()
                    .text_size(px(12.5))
                    .text_color(theme.colors.text_muted)
                    .line_height(gpui_kit::relative(1.35))
                    .child("No rules configured. Files open in the fallback editor, or in the system text editor."),
            );
        } else {
            for (index, rule) in rules.iter().enumerate() {
                if index > 0 {
                    content = content.child(div().h(px(1.0)).bg(theme.colors.border));
                }
                content = content.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_4()
                        .py_2p5()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2p5()
                                .child(
                                    div()
                                        .px_2()
                                        .py_0p5()
                                        .rounded(theme.sizes.button_radius)
                                        .bg(theme.colors.element_active)
                                        .text_size(px(12.5))
                                        .font_family(theme.fonts.mono_family.clone())
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.colors.text)
                                        .child(rule.pattern.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.colors.text_muted)
                                        .child("→"),
                                )
                                .child(
                                    div()
                                        .text_size(px(13.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.colors.text)
                                        .child(rule.app_name.clone()),
                                ),
                        )
                        .child(
                            text_button(("remove-rule", index), "Remove")
                                .danger(true)
                                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                                    workspace.remove_editor_rule(index, cx);
                                })),
                        ),
                );
            }
        }

        let add_form = div()
            .border_t_1()
            .border_color(theme.colors.border)
            .p_3p5()
            .bg(theme.colors.surface)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(ordinary_text_field(
                        "new-rule-pattern",
                        &self.settings.new_rule_pattern,
                    )))
                    .child(div().flex_1().child(ordinary_text_field(
                        "new-rule-app",
                        &self.settings.new_rule_app,
                    )))
                    .child(
                        text_button("add-editor-rule-button", "Add Rule")
                            .primary(true)
                            .on_click(cx.listener(|workspace, _event, window, cx| {
                                workspace.add_editor_rule(window, cx);
                            })),
                    ),
            );

        content.child(add_form).into_any_element()
    }

    pub(crate) fn add_editor_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pattern = self
            .settings
            .new_rule_pattern
            .read(cx)
            .value()
            .trim()
            .to_string();
        let app_name = self
            .settings
            .new_rule_app
            .read(cx)
            .value()
            .trim()
            .to_string();

        if pattern.is_empty() || app_name.is_empty() {
            self.config_error = Some("Enter both a pattern and an app name.".into());
            cx.notify();
            return;
        }

        if pattern.chars().any(is_bidi_or_control) || app_name.chars().any(is_bidi_or_control) {
            self.config_error =
                Some("Rule pattern and app name cannot contain control characters.".into());
            cx.notify();
            return;
        }

        let mut rules = cx.resources().config.config().editor_rules.clone();
        rules.push(macsftp_core::EditorRule { pattern, app_name });
        macsftp_core::sort_editor_rules(&mut rules);

        let result = cx.resources_mut().config.set_editor_rules(rules);
        if self.finish_config_change(result, "editor rules", cx) {
            self.settings
                .new_rule_pattern
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.settings
                .new_rule_app
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    pub(crate) fn remove_editor_rule(&mut self, index: usize, cx: &mut Context<Self>) {
        let mut rules = cx.resources().config.config().editor_rules.clone();
        if index < rules.len() {
            rules.remove(index);
            macsftp_core::sort_editor_rules(&mut rules);
            let result = cx.resources_mut().config.set_editor_rules(rules);
            self.finish_config_change(result, "editor rules after removal", cx);
            cx.notify();
        }
    }

    /// Settings → Profiles: list on the left, editor form on the right.
    pub(crate) fn render_settings_profiles(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
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

        // Materialize rows before the editor borrow of `cx` (listeners capture).
        let list_rows: Vec<_> = filtered
            .into_iter()
            .map(|profile| {
                let profile_id = profile.id;
                let selected = selected_id == Some(profile_id) && !editing_new;
                let full_label = profile_list_label(profile);
                let endpoint = format!("{}@{}", profile.username, profile.host);
                let port = format!(":{}", profile.port);
                let hover_background = if selected {
                    theme.colors.element_active
                } else {
                    theme.colors.element_hover
                };
                let profile_icon = div()
                    .size(px(28.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(if selected {
                        theme.colors.element_active
                    } else {
                        theme.colors.elevated_surface
                    })
                    .child(icon(
                        IconName::Server,
                        if selected {
                            theme.colors.accent
                        } else {
                            theme.colors.text_muted
                        },
                    ));
                let profile_name = div()
                    .truncate()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.colors.text)
                    .child(profile.name.clone());
                let profile_endpoint = div().flex_1().min_w_0().truncate().child(endpoint);
                let profile_port = div()
                    .flex_none()
                    .px_1p5()
                    .py_0p5()
                    .rounded_sm()
                    .bg(theme.colors.element_active)
                    .text_size(px(11.0))
                    .text_color(theme.colors.text_muted)
                    .child(port);
                let row = div()
                    .h(px(52.0))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2p5()
                    .px_2p5()
                    .cursor_pointer()
                    .rounded(theme.sizes.button_radius)
                    .border_1()
                    .border_color(if selected {
                        theme.colors.accent
                    } else {
                        theme.colors.border
                    })
                    .bg(if selected {
                        theme.colors.element_selected
                    } else {
                        theme.colors.surface
                    });
                let row = if selected {
                    row.id("settings-selected-profile-row")
                } else {
                    row.id(("settings-profile-row", profile_id.0))
                };
                row.tooltip(text_tooltip(full_label))
                    .hover(move |style| style.bg(hover_background))
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        workspace.select_profile_in_settings(profile_id, cx);
                    }))
                    .child(profile_icon)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(profile_name)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .min_w_0()
                                    .gap_1()
                                    .text_size(px(11.5))
                                    .text_color(theme.colors.text_muted)
                                    .child(profile_endpoint)
                                    .child(profile_port),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        let list_body: gpui_kit::AnyElement = if total_count == 0 {
            empty_state(
                "No saved connections",
                vec![
                    text_button("settings-empty-new-profile", "Add Connection").on_click(
                        cx.listener(|workspace, _event, _window, cx| {
                            workspace.start_new_profile(cx);
                        }),
                    ),
                ],
            )
            .into_any_element()
        } else if filtered_count == 0 && filter_active {
            empty_state("No matching items", vec![]).into_any_element()
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
                .child("Add a connection to save your server details.")
                .into_any_element()
        } else {
            div()
                .text_size(px(13.0))
                .text_color(theme.colors.text_muted)
                .child("Select a saved connection or add a new one.")
                .into_any_element()
        };

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                div()
                    .w(px(195.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2p5()
                    .border_r_1()
                    .border_color(theme.colors.border)
                    .track_focus(&self.workspace_focus)
                    .child(
                        text_button("settings-new-profile", "Add Connection").on_click(
                            cx.listener(|workspace, _event, _window, cx| {
                                workspace.start_new_profile(cx);
                            }),
                        ),
                    )
                    .child(
                        div()
                            .id("settings-profile-filter")
                            .on_click(cx.listener(|workspace, _event, window, cx| {
                                workspace.focus_profile_filter(window, cx);
                            }))
                            .child(self.render_text_input(
                                TextInputTarget::ProfileFilter,
                                "settings-profile-filter-input",
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_1()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.colors.text_disabled)
                            .child("SAVED CONNECTIONS")
                            .child(if filter_active {
                                format!("{filtered_count} of {total_count}")
                            } else {
                                total_count.to_string()
                            }),
                    )
                    .child(macsftp_ui::scroll_area(
                        "settings-profile-list-scroll",
                        div().flex().flex_col().gap_1().pr_1().child(list_body),
                        &self.settings.profile_list_scroll,
                        &self.settings.profile_list_scrollbar,
                        window,
                        cx,
                    )),
            )
            .child(div().flex().flex_col().flex_1().min_w_0().min_h_0().child(
                macsftp_ui::scroll_area(
                    "settings-profile-editor-scroll",
                    div().flex_none().w_full().px_4().py_3p5().child(detail),
                    &self.settings.editor_scroll,
                    &self.settings.editor_scrollbar,
                    window,
                    cx,
                ),
            ))
            .into_any_element()
    }

    pub(crate) fn render_profile_editor(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(editor) = self.settings.profile_editor.as_ref() else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let jump_candidates = cx
            .resources()
            .profiles
            .profiles()
            .iter()
            .filter(|profile| {
                Some(profile.id) != editor.profile_id
                    && matches!(&profile.route, ConnectionRoute::Direct)
            })
            .map(|profile| (profile.id, profile.name.clone()))
            .collect::<Vec<_>>();

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
                    if let Some(input) = workspace
                        .settings
                        .profile_editor
                        .as_ref()
                        .and_then(|editor| editor.inputs.get(&field))
                    {
                        input.focus(window, cx);
                    } else {
                        window.focus(&workspace.modal_focus, cx);
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(112.0))
                        .flex_none()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(if masked {
                    text_field(
                        ("profile-editor-input", field as usize),
                        TextFieldModel {
                            state,
                            placeholder,
                            focused: focused == field,
                            masked,
                        },
                        cx,
                    )
                    .into_any_element()
                } else {
                    plain_text_field(
                        ("profile-editor-input", field as usize),
                        editor
                            .inputs
                            .get(&field)
                            .expect("visible ordinary fields must have prepared component inputs"),
                        cx,
                    )
                    .into_any_element()
                }))
                .into_any_element()
        };

        let auth_toggle = |label: &'static str,
                           method: AuthMethodKind,
                           id: &'static str,
                           active: AuthMethodKind,
                           cx: &mut Context<Self>| {
            text_button(id, label)
                .primary(active == method)
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    if let Some(editor) = workspace.settings.profile_editor.as_mut() {
                        editor.set_auth_method(method);
                        cx.notify();
                    }
                    workspace.focus_profile_field(window, cx);
                }))
        };

        let passphrase_policy_toggle =
            |label: &'static str,
             policy: PassphrasePolicy,
             id: &'static str,
             active: PassphrasePolicy,
             cx: &mut Context<Self>| {
                text_button(id, label)
                    .primary(active == policy)
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        if let Some(editor) = workspace.settings.profile_editor.as_mut() {
                            editor.set_passphrase_policy(policy);
                            cx.notify();
                        }
                    }))
            };

        let route_toggle = |label: &'static str,
                            route_kind: ProfileRouteKind,
                            id: &'static str,
                            active: ProfileRouteKind,
                            cx: &mut Context<Self>| {
            text_button(id, label)
                .primary(active == route_kind)
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    if let Some(editor) = workspace.settings.profile_editor.as_mut() {
                        editor.set_route_kind(route_kind);
                        cx.notify();
                    }
                    workspace.focus_profile_field(window, cx);
                }))
        };

        let section_card = |title: &'static str, children: Vec<gpui_kit::AnyElement>| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.colors.text_disabled)
                        .child(title),
                )
                .child(
                    div()
                        .rounded(theme.sizes.modal_radius)
                        .border_1()
                        .border_color(theme.colors.border)
                        .bg(theme.colors.surface)
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2p5()
                        .children(children),
                )
        };

        let focused = editor.focused_field;
        let auth_method = editor.auth_method;
        let title = if editor.is_new {
            "Add Connection"
        } else {
            "Edit Connection"
        };

        let mut form = div()
            .id("profile-editor")
            .key_context(crate::app_actions::PROFILE_EDITOR_CONTEXT)
            .on_action(cx.listener(
                |workspace, _: &crate::app_actions::FocusNextField, window, cx| {
                    workspace.cycle_profile_focus(false, window, cx);
                },
            ))
            .on_action(cx.listener(
                |workspace, _: &crate::app_actions::FocusPreviousField, window, cx| {
                    workspace.cycle_profile_focus(true, window, cx);
                },
            ))
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(Self::handle_profile_editor_key))
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .max_w(px(580.0))
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.colors.text)
                    .child(title),
            );

        if let Some(error) = &editor.error {
            form = form.child(
                div()
                    .p_2p5()
                    .rounded(theme.sizes.button_radius)
                    .border_1()
                    .border_color(theme.colors.error)
                    .bg(theme.colors.surface)
                    .text_size(px(12.0))
                    .text_color(theme.colors.error)
                    .child(error.clone()),
            );
        }

        // Section 1: Server Details
        let server_details_card = section_card(
            "SERVER DETAILS",
            vec![
                field_row(
                    "Connection Name",
                    ProfileEditorField::Name,
                    &editor.name,
                    ProfileEditorField::Name.placeholder(),
                    false,
                    focused,
                    cx,
                ),
                field_row(
                    "Server Address",
                    ProfileEditorField::Host,
                    &editor.host,
                    ProfileEditorField::Host.placeholder(),
                    false,
                    focused,
                    cx,
                ),
                field_row(
                    "Port",
                    ProfileEditorField::Port,
                    &editor.port,
                    ProfileEditorField::Port.placeholder(),
                    false,
                    focused,
                    cx,
                ),
                field_row(
                    "Username",
                    ProfileEditorField::Username,
                    &editor.username,
                    ProfileEditorField::Username.placeholder(),
                    false,
                    focused,
                    cx,
                ),
            ],
        );
        form = form.child(server_details_card);

        // Section 2: Authentication
        let mut auth_elements = vec![
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(112.0))
                        .flex_none()
                        .pt_1()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.colors.text_muted)
                        .child("Sign-In Method"),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .child(auth_toggle(
                            "Password",
                            AuthMethodKind::Password,
                            "profile-auth-password",
                            auth_method,
                            cx,
                        ))
                        .child(auth_toggle(
                            "Key File",
                            AuthMethodKind::PrivateKey,
                            "profile-auth-private-key",
                            auth_method,
                            cx,
                        ))
                        .child(auth_toggle(
                            "Server Prompts",
                            AuthMethodKind::KeyboardInteractive,
                            "profile-auth-keyboard-interactive",
                            auth_method,
                            cx,
                        ))
                        .child(auth_toggle(
                            "SSH Agent",
                            AuthMethodKind::SshAgent,
                            "profile-auth-agent",
                            auth_method,
                            cx,
                        )),
                )
                .into_any_element(),
        ];

        match auth_method {
            AuthMethodKind::Password => {
                auth_elements.push(field_row(
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
                ));
            }
            AuthMethodKind::PrivateKey => {
                let policy = editor.passphrase_policy;
                auth_elements.push(field_row(
                    "Key File Path",
                    ProfileEditorField::KeyPath,
                    &editor.key_path,
                    ProfileEditorField::KeyPath.placeholder(),
                    false,
                    focused,
                    cx,
                ));
                auth_elements.push(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .w(px(112.0))
                                .flex_none()
                                .pt_1()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text_muted)
                                .child("Key Password"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1p5()
                                .child(passphrase_policy_toggle(
                                    "No Password",
                                    PassphrasePolicy::NoPassphrase,
                                    "profile-pass-none",
                                    policy,
                                    cx,
                                ))
                                .child(passphrase_policy_toggle(
                                    "Ask Every Time",
                                    PassphrasePolicy::AskEveryTime,
                                    "profile-pass-ask",
                                    policy,
                                    cx,
                                ))
                                .child(passphrase_policy_toggle(
                                    "Remember Password",
                                    PassphrasePolicy::Remember,
                                    "profile-pass-remember",
                                    policy,
                                    cx,
                                )),
                        )
                        .into_any_element(),
                );
                if policy != PassphrasePolicy::NoPassphrase {
                    let placeholder = match (policy, editor.secret_present_hint) {
                        (PassphrasePolicy::Remember, true) => "leave blank to keep",
                        (PassphrasePolicy::AskEveryTime, _) => "not saved — enter it each time",
                        (PassphrasePolicy::Remember, false) => "",
                        (PassphrasePolicy::NoPassphrase, _) => "",
                    };
                    auth_elements.push(field_row(
                        "Key Password",
                        ProfileEditorField::Passphrase,
                        editor.passphrase.as_input_state(),
                        placeholder,
                        true,
                        focused,
                        cx,
                    ));
                }
            }
            AuthMethodKind::KeyboardInteractive => {}
            AuthMethodKind::SshAgent => {
                auth_elements.push(field_row(
                    "Agent Location",
                    ProfileEditorField::AgentSocket,
                    &editor.agent_socket,
                    ProfileEditorField::AgentSocket.placeholder(),
                    false,
                    focused,
                    cx,
                ));
            }
        }
        form = form.child(section_card("AUTHENTICATION", auth_elements));

        // Section 3: Connection Route
        let route_kind = editor.route_kind;
        let mut route_elements = vec![
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(112.0))
                        .flex_none()
                        .pt_1()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.colors.text_muted)
                        .child("Connection Method"),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1p5()
                        .child(route_toggle(
                            "Direct",
                            ProfileRouteKind::Direct,
                            "profile-route-direct",
                            route_kind,
                            cx,
                        ))
                        .child(route_toggle(
                            "Via Another Server",
                            ProfileRouteKind::JumpHost,
                            "profile-route-jump",
                            route_kind,
                            cx,
                        ))
                        .child(route_toggle(
                            "Run a Command",
                            ProfileRouteKind::ProxyCommand,
                            "profile-route-command",
                            route_kind,
                            cx,
                        )),
                )
                .into_any_element(),
        ];

        match route_kind {
            ProfileRouteKind::Direct => {}
            ProfileRouteKind::ProxyCommand => {
                route_elements.push(field_row(
                    "Connection Command",
                    ProfileEditorField::ProxyCommand,
                    &editor.proxy_command,
                    ProfileEditorField::ProxyCommand.placeholder(),
                    false,
                    focused,
                    cx,
                ));
                route_elements.push(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().w(px(112.0)).flex_none())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.5))
                                .text_color(theme.colors.text_muted)
                                .line_height(gpui_kit::relative(1.35))
                                .child(
                                    "This command runs on your Mac. Never include passwords or other secrets.",
                                )
                                .child(div().child("%h: server address · %p: port · %r: username")),
                        )
                        .into_any_element(),
                );
            }
            ProfileRouteKind::JumpHost => {
                let mut choices = div().flex().flex_wrap().gap_1p5();
                for (profile_id, label) in &jump_candidates {
                    let profile_id = *profile_id;
                    let selected = editor.jump_profile_id == Some(profile_id);
                    choices = choices.child(
                        text_button(("jump-profile", profile_id.0), label.clone())
                            .primary(selected)
                            .on_click(cx.listener(move |workspace, _event, _window, cx| {
                                if let Some(editor) = workspace.settings.profile_editor.as_mut() {
                                    editor.set_jump_profile(profile_id);
                                    cx.notify();
                                }
                            })),
                    );
                }
                if jump_candidates.is_empty() {
                    choices = choices.child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.colors.text_muted)
                            .child("First, add a saved connection that uses Direct."),
                    );
                }
                route_elements.push(
                    div()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .w(px(112.0))
                                .flex_none()
                                .pt_1()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.colors.text_muted)
                                .child("Intermediate Server"),
                        )
                        .child(choices)
                        .into_any_element(),
                );
            }
        }
        form = form.child(section_card("CONNECTION ROUTE", route_elements));

        // Section 4: Preferences
        let preferences_card = section_card(
            "PREFERENCES",
            vec![field_row(
                "Starting Folder",
                ProfileEditorField::DefaultRemotePath,
                &editor.default_remote_path,
                ProfileEditorField::DefaultRemotePath.placeholder(),
                false,
                focused,
                cx,
            )],
        );
        form = form.child(preferences_card);

        if editor.secret_present_hint {
            let note = match (editor.auth_method, editor.passphrase_policy) {
                (AuthMethodKind::PrivateKey, PassphrasePolicy::NoPassphrase) => {
                    "Saving removes the remembered key password from your Mac’s Keychain."
                }
                _ => {
                    "Sign-in details are stored in your Mac’s Keychain. Leave blank to keep existing values."
                }
            };
            form = form.child(
                div()
                    .px_1()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .line_height(gpui_kit::relative(1.35))
                    .child(note),
            );
        }

        form = form.child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .pt_1()
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

/// Bridges a `Toggle` change (which carries the new pressed state, not an
/// event) back to a workspace method.
fn settings_toggle_handler(
    workspace: gpui_kit::WeakEntity<Workspace>,
    apply: fn(&mut Workspace, bool, &mut Context<Workspace>),
) -> impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static {
    move |pressed, _event, _window, cx| {
        if workspace
            .update(cx, |workspace, cx| apply(workspace, pressed, cx))
            .is_err()
        {
            // Settings closed before the toggle fired.
        }
    }
}

/// A settings row with a label, explanation, and a controlled native-style
/// switch. The Base `Toggle` owns focus, Enter/Space activation and the
/// toggled accessibility state; this helper owns the macOS-like track.
fn settings_switch_row(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    pressed: bool,
    on_change: impl Fn(bool, &ClickEvent, &mut Window, &mut App) + 'static,
    colors: &ThemeColors,
) -> gpui_kit::AnyElement {
    let track = if pressed {
        colors.accent
    } else {
        colors.text_disabled
    };
    let toggle = Toggle::new(id)
        .pressed(pressed)
        .accessibility_label(title)
        .w(px(36.0))
        .h(px(22.0))
        .flex_none()
        .p(px(3.0))
        .rounded_full()
        .bg(track)
        .when(pressed, |toggle| toggle.justify_end())
        .when(!pressed, |toggle| toggle.justify_start())
        .hover(|style| style.opacity(0.9))
        .focus_visible(|style| style.border_1().border_color(colors.border_focused))
        .on_change(on_change)
        .child(
            div()
                .size(px(16.0))
                .flex_none()
                .rounded_full()
                .bg(colors.switch_knob),
        );

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.text)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(colors.text_muted)
                        .line_height(gpui_kit::relative(1.35))
                        .child(description),
                ),
        )
        .child(toggle)
        .into_any_element()
}
