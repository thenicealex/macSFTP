use gpui::{
    App, ClickEvent, ElementId, Hsla, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, prelude::*, px,
};

use crate::components::{icon_button, text_tooltip};
use crate::icon::IconName;
use crate::theme::ActiveTheme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One tab in the tab bar: connection status dot, title, close button.
/// Purely presentational — the app maps `ConnectionState` to the status
/// color/label and owns activation/close behavior.
#[derive(IntoElement)]
pub struct Tab {
    id: ElementId,
    title: SharedString,
    status_color: Hsla,
    status_label: SharedString,
    active: bool,
    on_activate: Option<ClickHandler>,
    on_close: Option<ClickHandler>,
}

pub fn tab(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    status_color: Hsla,
    status_label: impl Into<SharedString>,
) -> Tab {
    Tab {
        id: id.into(),
        title: title.into(),
        status_color,
        status_label: status_label.into(),
        active: false,
        on_activate: None,
        on_close: None,
    }
}

impl Tab {
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn on_activate(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_activate = Some(Box::new(handler));
        self
    }

    pub fn on_close(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let background = if self.active {
            theme.colors.background
        } else {
            theme.colors.surface
        };
        let text_color = if self.active {
            theme.colors.text
        } else {
            theme.colors.text_muted
        };
        let hover_background = theme.colors.element_hover;
        let close_id = ElementId::NamedChild(Box::new(self.id.clone()), "close".into());

        div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h_full()
            .pl_3()
            .pr_1()
            .max_w(px(220.0))
            .bg(background)
            .border_r_1()
            .border_color(theme.colors.border)
            .text_size(px(13.0))
            .text_color(text_color)
            .font_family(theme.fonts.ui_family.clone())
            .when(!self.active, |element| {
                element.hover(move |style| style.bg(hover_background))
            })
            .when_some(self.on_activate, |element, handler| {
                element.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(
                div()
                    .id("status")
                    .size(px(8.0))
                    .flex_none()
                    .rounded_full()
                    .bg(self.status_color)
                    .tooltip(text_tooltip(self.status_label)),
            )
            .child(div().min_w_0().truncate().child(self.title))
            .child(
                icon_button(close_id, IconName::Close, "Close Tab").when_some(
                    self.on_close,
                    |button, handler| {
                        button.on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            handler(event, window, cx);
                        })
                    },
                ),
            )
    }
}
