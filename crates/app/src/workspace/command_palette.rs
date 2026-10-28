//! Command palette overlay: open/filter/navigate/execute explicit registry ids.

use crate::workspace::TextInputTarget;

use gpui_kit::{
    Context, FontWeight, IntoElement, KeyDownEvent, ParentElement, SharedString, Styled, Window,
    div, prelude::*, px,
};
use macsftp_core::ConnectionState;
use macsftp_ui::ActiveTheme;

use crate::palette_commands::{PaletteCommand, PaletteContext, filter_palette_commands};
use crate::workspace::Workspace;
use crate::workspace::overlay::OverlayFade;

impl Workspace {
    pub(crate) fn palette_context(&self) -> PaletteContext {
        PaletteContext {
            has_tabs: !self.state.tabs.tabs().is_empty(),
            has_active_tab: self.state.tabs.active_tab_id().is_some(),
            remote_connected: self
                .active_tab()
                .is_some_and(|tab| matches!(tab.connection, ConnectionState::Connected { .. })),
        }
    }

    pub(crate) fn filtered_palette_commands(&self) -> Vec<&'static PaletteCommand> {
        filter_palette_commands(self.palette.input.value(), &self.palette_context())
    }

    pub(crate) fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_inputs.remove(&TextInputTarget::CommandPalette);
        self.palette.open = true;
        self.palette.input.clear();
        self.palette.selected = 0;
        window.focus(&self.modal_focus, cx);
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
        let action = (command.action)();
        let workspace_focus = self.workspace_focus.clone();
        self.close_command_palette(window, cx);
        // Same path as the menu bar and the keymap: one action, one handler.
        // Deferred, because the handler is a listener on this same entity,
        // which GPUI cannot update while this update is still in progress.
        // The Workspace root, unlike the focused pane, renders on every surface.
        window.defer(cx, move |window, cx| {
            workspace_focus.dispatch_action(action.as_ref(), window, cx);
        });
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
        }
    }

    pub(crate) fn render_command_palette(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let fade = OverlayFade::sample("command-palette", self.palette.open, window, cx);
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
        let transfers_open = self.transfer_drawer.open;
        let rows = commands.into_iter().enumerate().map(|(index, command)| {
            let is_selected = index == selected;
            let title: SharedString = if command.id == "ShowTransferDrawer" {
                if transfers_open {
                    "Hide Transfers"
                } else {
                    "Show Transfers"
                }
            } else {
                command.title
            }
            .into();
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
                .rounded(theme.sizes.button_radius)
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
                        .font_weight(if is_selected {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
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

        let top_padding = px(48.0);
        let max_palette_height = (window.viewport_size().height - top_padding - px(32.0))
            .min(px(420.0))
            .max(px(240.0));

        Some(
            div()
                .id("command-palette-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_start()
                .justify_center()
                .pt(top_padding)
                .bg(theme.scrim())
                .opacity(fade.opacity())
                .child(
                    div()
                        .id("command-palette-card")
                        .debug_selector(|| "command-palette-card".into())
                        .key_context("CommandPalette")
                        .capture_action(cx.listener(
                            |workspace, _: &gpui_kit::base::input::MoveUp, _window, cx| {
                                cx.stop_propagation();
                                workspace.move_palette_selection(-1, cx);
                            },
                        ))
                        .capture_action(cx.listener(
                            |workspace, _: &gpui_kit::base::input::MoveDown, _window, cx| {
                                cx.stop_propagation();
                                workspace.move_palette_selection(1, cx);
                            },
                        ))
                        .track_focus(&self.modal_focus)
                        .on_key_down(cx.listener(Self::handle_command_palette_key))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .w(px(520.0))
                        .max_h(max_palette_height)
                        .p_3()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded(theme.sizes.modal_radius)
                        .shadow(theme.shadow_popover())
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.colors.text)
                                        .child("Find an Action"),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_family(theme.fonts.mono_family.clone())
                                        .text_color(theme.colors.text_muted)
                                        .child("⌘⇧P"),
                                ),
                        )
                        .child(self.render_text_input(
                            TextInputTarget::CommandPalette,
                            "command-palette-input",
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
                                            .child("No matching actions"),
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
                                .child("↑↓ to select · Enter to run · Esc to close"),
                        ),
                )
                .into_any_element(),
        )
    }
}
