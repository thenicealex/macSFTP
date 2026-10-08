use gpui::{
    AnyView, App, ClickEvent, Context, ElementId, Hsla, IntoElement, ParentElement, Render,
    RenderOnce, SharedString, Styled, Window, div, prelude::*, px,
};

use gpui_component::{
    Disableable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
};

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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let icon_color = self.icon_color.unwrap_or(theme.colors.text);
        let hover_background = theme.colors.element_hover;

        Button::new(self.id)
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(gpui::hsla(0.0, 0.0, 0.0, 0.0))
                    .foreground(icon_color)
                    .hover(hover_background)
                    .active(theme.colors.element_active)
                    .shadow(false),
            )
            .compact()
            .size(px(22.0))
            .rounded_sm()
            .disabled(self.disabled)
            .accessibility_label(self.tooltip_label.clone())
            .tooltip(self.tooltip_label)
            .opacity(if self.disabled {
                ICON_BUTTON_DISABLED_OPACITY
            } else {
                ICON_BUTTON_OPACITY
            })
            .when_some(
                self.on_click.filter(|_| !self.disabled),
                |button, handler| {
                    button.on_click(move |event, window, cx| handler(event, window, cx))
                },
            )
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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let hover_background = theme.colors.element_hover;
        let (background, border_color, text_color) = if self.danger {
            (
                Some(theme.colors.error),
                theme.colors.error,
                theme.colors.background,
            )
        } else if self.primary {
            (
                Some(theme.colors.accent),
                theme.colors.accent,
                theme.colors.background,
            )
        } else {
            (None, theme.colors.border, theme.colors.text)
        };

        Button::new(self.id)
            .small()
            .compact()
            .label(self.label)
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(background.unwrap_or(theme.colors.surface))
                    .foreground(text_color)
                    .hover(if self.primary || self.danger {
                        border_color
                    } else {
                        hover_background
                    })
                    .active(if self.primary || self.danger {
                        border_color
                    } else {
                        theme.colors.element_active
                    })
                    .shadow(false),
            )
            .px_3()
            .h(px(26.0))
            .border_1()
            .rounded_sm()
            .border_color(border_color)
            .text_size(px(13.0))
            .text_color(text_color)
            .font_family(theme.fonts.ui_family.clone())
            .when_some(background, |button, background| button.bg(background))
            .when_some(self.on_click, |button, handler| {
                button.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
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
