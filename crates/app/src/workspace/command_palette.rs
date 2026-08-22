//! Command palette overlay: open/filter/navigate/execute explicit registry ids.

use gpui::{
    Context, FontWeight, IntoElement, KeyDownEvent, ParentElement, SharedString, Styled, Window,
    div, prelude::*, px,
};
use macsftp_core::ConnectionState;
use macsftp_ui::{ActiveTheme, InputKeyResult, TextFieldModel, text_field};

use crate::palette_commands::{PaletteCommand, PaletteContext, filter_palette_commands};
use crate::workspace::{PaneSide, Workspace, WorkspaceSurface};
use macsftp_core::HistoryOp;

impl Workspace {
    pub(crate) fn palette_context(&self) -> PaletteContext {
        PaletteContext {
            has_tabs: !self.state.tabs.tabs.is_empty(),
            has_active_tab: self.state.tabs.active_tab_id.is_some(),
            remote_connected: self
                .active_tab()
                .is_some_and(|tab| matches!(tab.connection, ConnectionState::Connected { .. })),
        }
    }

    pub(crate) fn filtered_palette_commands(&self) -> Vec<&'static PaletteCommand> {
        filter_palette_commands(self.palette.input.value(), &self.palette_context())
    }

    pub(crate) fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette.open = true;
        self.palette.input.clear();
        self.palette.selected = 0;
        window.focus(&self.modal_focus);
        cx.notify();
    }

    pub(crate) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette.open = false;
        self.palette.input.clear();
        self.palette.selected = 0;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.filtered_palette_commands().len();
        if count == 0 {
            self.palette.selected = 0;
            cx.notify();
            return;
        }
        let next = (self.palette.selected as isize + delta).rem_euclid(count as isize) as usize;
        self.palette.selected = next;
        cx.notify();
    }

    pub(crate) fn execute_palette_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hits = self.filtered_palette_commands();
        let Some(command) = hits.get(self.palette.selected) else {
            return;
        };
        let id = command.id;
        self.close_command_palette(window, cx);
        self.dispatch_palette_id(id, window, cx);
    }

    pub(crate) fn execute_palette_at(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette.selected = index;
        self.execute_palette_selected(window, cx);
    }

    pub(crate) fn dispatch_palette_id(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            "NewTab" => self.open_new_tab(window, cx),
            "CloseTab" => {
                if let Some(tab_id) = self.state.tabs.active_tab_id {
                    self.close_tab_by_id(tab_id, window, cx);
                }
            }
            "RefreshPane" => self.refresh_focused_pane(window, cx),
            "FocusLocalPane" => self.focus_pane(PaneSide::Local, window, cx),
            "FocusRemotePane" => self.focus_pane(PaneSide::Remote, window, cx),
            "UploadSelection" => self.upload_selection(cx),
            "DownloadSelection" => self.download_selection(cx),
            "ShowTransferDrawer" => {
                self.transfer_drawer.open = !self.transfer_drawer.open;
                cx.notify();
            }
            "OpenSettings" => {
                if self.connect_form_ui.form.is_none()
                    && self.active_host_key_prompt().is_none()
                    && self.active_transfer_conflict_prompt().is_none()
                    && self.modal_inputs.delete_confirm.is_none()
                    && !self.go_to_path.open
                {
                    self.modal_inputs.about_open = false;
                    self.surface = WorkspaceSurface::Settings;
                    self.settings.section = crate::workspace::profiles::SettingsSection::General;
                    self.workspace_focus.focus(window);
                    cx.notify();
                }
            }
            "OpenProfiles" => {
                if self.connect_form_ui.form.is_none()
                    && self.active_host_key_prompt().is_none()
                    && self.active_transfer_conflict_prompt().is_none()
                    && self.modal_inputs.delete_confirm.is_none()
                    && !self.go_to_path.open
                {
                    self.modal_inputs.about_open = false;
                    self.surface = WorkspaceSurface::Settings;
                    self.set_settings_section(
                        crate::workspace::profiles::SettingsSection::Profiles,
                        cx,
                    );
                    self.workspace_focus.focus(window);
                    cx.notify();
                }
            }
            "ShowAbout" => {
                if self.connect_form_ui.form.is_none()
                    && self.active_host_key_prompt().is_none()
                    && self.active_transfer_conflict_prompt().is_none()
                    && self.modal_inputs.delete_confirm.is_none()
                    && !self.go_to_path.open
                {
                    self.modal_inputs.about_open = true;
                    cx.notify();
                }
            }
            "DeleteSelection" => self.request_delete_selection(window, cx),
            "RenameEntry" => self.begin_rename_selection(window, cx),
            "NewFolder" => self.begin_new_folder(window, cx),
            "FilterPane" => self.open_filter_pane(window, cx),
            "GoToPath" => self.open_go_to_path(window, cx),
            "NavigateBack" => self.navigate_focused(HistoryOp::Back, window, cx),
            "NavigateForward" => self.navigate_focused(HistoryOp::Forward, window, cx),
            "ParentDirectory" => self.go_to_parent_directory(window, cx),
            "ToggleHiddenFiles" => self.toggle_hidden_files(cx),
            "CopyPath" => self.copy_focused_path(cx),
            "ReconnectTab" => self.request_connect(window, cx),
            "OpenLogFolder" => self.open_log_folder(cx),
            "OpenCommandPalette" => self.open_command_palette(window, cx),
            _ => {}
        }
    }

    pub(crate) fn handle_command_palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.palette.open {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.execute_palette_selected(window, cx);
            return;
        }
        if keystroke.key == "up" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.move_palette_selection(-1, cx);
            return;
        }
        if keystroke.key == "down" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.move_palette_selection(1, cx);
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.palette.input.insert(&text);
                self.palette.selected = 0;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.palette.input.handle_keystroke(keystroke) == InputKeyResult::Handled {
            self.palette.selected = 0;
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn render_command_palette(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.palette.open {
            return None;
        }
        let theme = cx.theme().clone();
        let commands = self.filtered_palette_commands();
        let selected = if commands.is_empty() {
            0
        } else {
            self.palette.selected.min(commands.len() - 1)
        };

        let has_results = !commands.is_empty();
        let mono_family = theme.fonts.mono_family.clone();
        let rows = commands.into_iter().enumerate().map(|(index, command)| {
            let is_selected = index == selected;
            let title: SharedString = command.title.into();
            let keybinding = command.keybinding.map(SharedString::from);
            let background = if is_selected {
                theme.colors.element_selected
            } else {
                theme.colors.elevated_surface
            };
            let hover_background = theme.colors.element_hover;
            let mono = mono_family.clone();

            div()
                .id(("palette-row", index))
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(background)
                .when(!is_selected, |row| {
                    row.hover(move |style| style.bg(hover_background))
                })
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.execute_palette_at(index, window, cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .text_color(theme.colors.text)
                        .child(title),
                )
                .when_some(keybinding, |row, binding| {
                    row.child(
                        div()
                            .flex_none()
                            .min_w(px(56.0))
                            .text_size(px(12.0))
                            .font_family(mono)
                            .text_color(theme.colors.text_muted)
                            .child(binding),
                    )
                })
        });

        Some(
            div()
                .id("command-palette-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(80.0))
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .key_context("CommandPalette")
                        .track_focus(&self.modal_focus)
                        .on_key_down(cx.listener(Self::handle_command_palette_key))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .w(px(520.0))
                        .max_h(px(420.0))
                        .p_3()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.colors.text)
                                        .child("Command Palette"),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_family(theme.fonts.mono_family.clone())
                                        .text_color(theme.colors.text_muted)
                                        .child("⌘⇧P"),
                                ),
                        )
                        .child(text_field(
                            "command-palette-input",
                            TextFieldModel {
                                state: &self.palette.input,
                                placeholder: "Filter commands…",
                                focused: true,
                                masked: false,
                            },
                            cx,
                        ))
                        .child({
                            let list = div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .when(has_results, |list| list.children(rows))
                                .when(!has_results, |list| {
                                    list.child(
                                        div()
                                            .px_2()
                                            .py_2()
                                            .text_size(px(13.0))
                                            .text_color(theme.colors.text_muted)
                                            .child("No matching commands"),
                                    )
                                });
                            macsftp_ui::scroll_area(
                                "command-palette-results",
                                list,
                                self.palette_scroll(),
                                self.palette_scrollbar(),
                                window,
                                cx,
                            )
                        })
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child("↑↓ move · Enter run · Esc close"),
                        ),
                )
                .into_any_element(),
        )
    }
}
