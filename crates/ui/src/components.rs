use gpui::{
    AnyView, App, ClickEvent, Context, ElementId, Hsla, IntoElement, ParentElement, Render,
    RenderOnce, SharedString, Styled, Window, div, prelude::*, px, relative,
};

use gpui_base::Button;

use crate::icon::{IconName, icon_with_size};
use crate::theme::ActiveTheme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

const ICON_BUTTON_ICON_SIZE: gpui::Pixels = px(16.0);
const ICON_BUTTON_OPACITY: f32 = 0.65;
const ICON_BUTTON_DISABLED_OPACITY: f32 = 0.25;

/// Minimal text tooltip view used by icon-only buttons, which must all
/// carry a label per the accessibility rules.
pub struct Tooltip {
    label: SharedString,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(theme.colors.elevated_surface)
            .border_1()
            .border_color(theme.colors.border)
            .text_size(px(12.0))
            .text_color(theme.colors.text)
            .font_family(theme.fonts.ui_family.clone())
            .child(self.label.clone())
    }
}

/// Build a tooltip callback for GPUI's `.tooltip(...)` from a plain label.
pub fn text_tooltip(
    label: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let label = label.into();
    move |_window, cx| {
        let label = label.clone();
        cx.new(|_| Tooltip { label }).into()
    }
}

/// Icon-only button with a mandatory tooltip label and a stable hit area.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    tooltip_label: SharedString,
    icon_color: Option<Hsla>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

pub fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tooltip_label: impl Into<SharedString>,
) -> IconButton {
    IconButton {
        id: id.into(),
        icon,
        tooltip_label: tooltip_label.into(),
        icon_color: None,
        disabled: false,
        on_click: None,
    }
}

impl IconButton {
    pub fn icon_color(mut self, color: Hsla) -> Self {
        self.icon_color = Some(color);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let icon_color = self.icon_color.unwrap_or(theme.colors.text);
        button_base(self.id, None, self.disabled, window, cx)
            .w(theme.sizes.icon_button_width)
            .h(theme.sizes.icon_button_height)
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.0))
            .text_color(icon_color)
            .disabled(self.disabled)
            .accessibility_label(self.tooltip_label.clone())
            .tooltip(text_tooltip(self.tooltip_label))
            .opacity(if self.disabled {
                ICON_BUTTON_DISABLED_OPACITY
            } else {
                ICON_BUTTON_OPACITY
            })
            .when(!self.disabled, |button| {
                button
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .active(|style| style.bg(theme.colors.element_active))
            })
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(icon_with_size(self.icon, icon_color, ICON_BUTTON_ICON_SIZE))
    }
}

/// Small bordered text button for primary next-step actions in empty
/// states and dialogs.
#[derive(IntoElement)]
pub struct TextButton {
    id: ElementId,
    label: SharedString,
    primary: bool,
    danger: bool,
    on_click: Option<ClickHandler>,
}

pub fn text_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> TextButton {
    TextButton {
        id: id.into(),
        label: label.into(),
        primary: false,
        danger: false,
        on_click: None,
    }
}

impl TextButton {
    /// Accent-filled variant for a modal's main action.
    pub fn primary(mut self, primary: bool) -> Self {
        self.primary = primary;
        self
    }

    /// Destructive action styling (e.g. Delete confirm).
    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for TextButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (background, border_color, text_color) = if self.danger {
            (
                theme.colors.error,
                theme.colors.error,
                theme.colors.background,
            )
        } else if self.primary {
            (
                theme.colors.accent,
                theme.colors.accent,
                theme.colors.background,
            )
        } else {
            (theme.colors.surface, theme.colors.border, theme.colors.text)
        };
        button_base(self.id, Some(border_color), false, window, cx)
            .px(theme.sizes.button_padding_x)
            .h(theme.sizes.button_height)
            .min_w(theme.sizes.button_min_width)
            .border_1()
            .bg(background)
            .text_color(text_color)
            .text_size(theme.sizes.button_text_size)
            .font_family(theme.fonts.ui_family.clone())
            .hover(|style| {
                style
                    .border_color(background)
                    .bg(if self.primary || self.danger {
                        border_color
                    } else {
                        theme.colors.element_hover
                    })
            })
            .active(|style| {
                style
                    .border_color(background)
                    .bg(if self.primary || self.danger {
                        border_color
                    } else {
                        theme.colors.element_active
                    })
            })
            .accessibility_label(self.label.clone())
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(self.label),
            )
    }
}

// Keep the same keyed focus handle and mouse focus policy as Component 0.7.1.
// Toolbar clicks must not steal a pane or input's keyboard focus.
fn button_base(
    id: ElementId,
    border_color: Option<Hsla>,
    disabled: bool,
    window: &mut Window,
    cx: &mut App,
) -> Button {
    let theme = cx.theme().clone();
    let focus = window
        .use_keyed_state(id.clone(), cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let focused = !disabled && focus.is_focused(window);
    let border = if border_color.is_some() {
        px(1.0)
    } else {
        px(0.0)
    };
    Button::new(id)
        .track_focus(&focus)
        .relative()
        .flex_shrink_0()
        .cursor_default()
        .rounded(theme.sizes.button_radius)
        .line_height(relative(1.25))
        .when_some(border_color, |button, color| {
            button.border_color(if focused {
                theme.colors.border_focused
            } else {
                color
            })
        })
        .when(!disabled, |button| {
            button.on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                gpui_base::GlobalState::suppress_text_selection(cx);
            })
        })
        .when(focused, |button| {
            button.child(
                div()
                    .absolute()
                    .top(-theme.sizes.button_focus_ring - border)
                    .left(-theme.sizes.button_focus_ring - border)
                    .right(-theme.sizes.button_focus_ring - border)
                    .bottom(-theme.sizes.button_focus_ring - border)
                    .border(theme.sizes.button_focus_ring)
                    .rounded(theme.sizes.button_radius + theme.sizes.button_focus_ring)
                    .border_color(gpui::Hsla {
                        a: 0.5,
                        ..theme.colors.border_focused
                    }),
            )
        })
}

/// Empty state: one short message plus the next actions (usually one).
pub fn empty_state(
    message: impl Into<SharedString>,
    actions: Vec<TextButton>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_3()
        .text_size(px(13.0))
        .text_color(theme.colors.text_muted)
        .font_family(theme.fonts.ui_family.clone())
        .child(message.into())
        .when(!actions.is_empty(), |empty| {
            empty.child(div().flex().gap_2().children(actions))
        })
}

/// First-load / busy placeholder: static activity glyph + short message.
/// Centered like [`empty_state`]; no animation dependency required.
pub fn loading_state(message: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .text_size(px(13.0))
        .text_color(theme.colors.text_muted)
        .font_family(theme.fonts.ui_family.clone())
        .child(
            div()
                .text_size(px(18.0))
                .text_color(theme.colors.accent)
                .child("↻"),
        )
        .child(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{FocusHandle, MouseDownEvent, MouseUpEvent, TestAppContext, point, size};
    use std::{cell::Cell, rc::Rc};

    struct ButtonHarness {
        clicks: Rc<Cell<usize>>,
        disabled: bool,
        pane_focus: FocusHandle,
    }

    impl Render for ButtonHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let text_clicks = self.clicks.clone();
            let icon_clicks = self.clicks.clone();
            window.set_rem_size(cx.theme().sizes.button_text_size / 0.875);
            div()
                .flex()
                .flex_col()
                .items_start()
                .tab_group()
                .track_focus(&self.pane_focus)
                .child(
                    div().debug_selector(|| "text-button".into()).child(
                        text_button("test-text-button", "Continue")
                            .on_click(move |_, _, _| text_clicks.set(text_clicks.get() + 1)),
                    ),
                )
                .child(
                    div().debug_selector(|| "icon-button".into()).child(
                        icon_button("test-icon-button", IconName::Refresh, "Refresh")
                            .disabled(self.disabled)
                            .on_click(move |_, _, _| icon_clicks.set(icon_clicks.get() + 1)),
                    ),
                )
        }
    }

    #[gpui::test]
    fn base_buttons_preserve_frozen_bounds_and_keyboard_activation(cx: &mut TestAppContext) {
        cx.update(|cx| crate::theme::Theme::one_dark().install(cx));
        let clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, cx| ButtonHarness {
                clicks,
                disabled: false,
                pane_focus: cx.focus_handle().tab_stop(false),
            }
        });
        for theme in [
            crate::theme::Theme::one_dark(),
            crate::theme::Theme::one_light(),
        ] {
            cx.update(|_, cx| {
                theme.install(cx);
                view.update(cx, |_, cx| cx.notify());
            });
            for width in [160.0, 480.0] {
                cx.simulate_resize(size(px(width), px(160.0)));
                cx.run_until_parked();
                // Frozen Component 0.7.1 bounds measured before removing that dependency.
                for (selector, expected) in [
                    ("text-button", size(px(76.0), px(26.0))),
                    ("icon-button", size(px(26.0), px(22.0))),
                ] {
                    let actual = cx
                        .debug_bounds(selector)
                        .expect("Base button must be rendered");
                    assert_eq!(
                        actual.size, expected,
                        "migration must preserve button geometry"
                    );
                }
            }
        }
        cx.update(|window, cx| window.focus_next(cx));
        for key in ["enter", "space"] {
            let keystroke = gpui::Keystroke::parse(key).expect("button activation key must parse");
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(gpui::KeyUpEvent { keystroke });
        }
        assert_eq!(
            clicks.get(),
            2,
            "text button must activate exactly once per keyboard press"
        );
        cx.update(|window, cx| window.focus_next(cx));
        for key in ["enter", "space"] {
            let keystroke = gpui::Keystroke::parse(key).expect("button activation key must parse");
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(gpui::KeyUpEvent { keystroke });
        }
        assert_eq!(
            clicks.get(),
            4,
            "icon button must remain keyboard reachable"
        );
    }

    #[gpui::test]
    fn disabled_icon_button_rejects_clicks_and_toolbar_click_preserves_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| crate::theme::Theme::one_dark().install(cx));
        let clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |window, cx| {
                let pane_focus = cx.focus_handle().tab_stop(false);
                pane_focus.focus(window, cx);
                ButtonHarness {
                    clicks,
                    disabled: true,
                    pane_focus,
                }
            }
        });
        cx.simulate_resize(size(px(240.0), px(160.0)));
        cx.run_until_parked();
        let icon_bounds = cx
            .debug_bounds("icon-button")
            .expect("disabled icon button must be rendered");
        let text_bounds = cx
            .debug_bounds("text-button")
            .expect("text button must be rendered");
        for bounds in [icon_bounds, text_bounds] {
            let position = point(bounds.left() + px(5.0), bounds.top() + px(5.0));
            cx.simulate_event(MouseDownEvent {
                position,
                button: gpui::MouseButton::Left,
                click_count: 1,
                ..Default::default()
            });
            cx.simulate_event(MouseUpEvent {
                position,
                button: gpui::MouseButton::Left,
                click_count: 1,
                ..Default::default()
            });
        }
        assert_eq!(
            clicks.get(),
            1,
            "only the enabled button must dispatch its callback"
        );
        cx.update(|window, cx| {
            assert!(
                view.read(cx).pane_focus.is_focused(window),
                "mouse activation must preserve pane focus"
            );
        });
    }
}
