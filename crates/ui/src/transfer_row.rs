use gpui_kit::{
    App, ClickEvent, DefiniteLength, ElementId, Hsla, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, prelude::*, px,
};
use macsftp_core::TransferDirection;

use crate::components::{icon_button, text_tooltip};
use crate::icon::{ICON_SIZE, IconName, icon};
use crate::theme::{ActiveTheme, TextRoleExt};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One row in the transfer drawer: direction, source → destination,
/// state, progress, and the primary actions. Fixed height; the progress
/// bar is always laid out so state changes never shift the layout.
#[derive(IntoElement)]
pub struct TransferRow {
    id: ElementId,
    direction: TransferDirection,
    title: SharedString,
    state_label: SharedString,
    state_color: Hsla,
    /// `None` hides the bar fill entirely (e.g. queued).
    progress: Option<f32>,
    progress_color: Option<Hsla>,
    detail_label: SharedString,
    on_cancel: Option<ClickHandler>,
    on_retry: Option<ClickHandler>,
    on_reveal: Option<ClickHandler>,
}

pub fn transfer_row(
    id: impl Into<ElementId>,
    direction: TransferDirection,
    title: impl Into<SharedString>,
    state_label: impl Into<SharedString>,
    state_color: Hsla,
) -> TransferRow {
    TransferRow {
        id: id.into(),
        direction,
        title: title.into(),
        state_label: state_label.into(),
        state_color,
        progress: None,
        progress_color: None,
        detail_label: "".into(),
        on_cancel: None,
        on_retry: None,
        on_reveal: None,
    }
}

impl TransferRow {
    pub fn progress(mut self, fraction: f32, color: Hsla) -> Self {
        self.progress = Some(fraction.clamp(0.0, 1.0));
        self.progress_color = Some(color);
        self
    }

    pub fn detail(mut self, label: impl Into<SharedString>) -> Self {
        self.detail_label = label.into();
        self
    }

    pub fn on_cancel(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_cancel = Some(Box::new(handler));
        self
    }

    pub fn on_retry(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_retry = Some(Box::new(handler));
        self
    }

    /// Offer a reveal control for a finished transfer, so the user can find the
    /// file it produced without navigating there by hand.
    pub fn on_reveal(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_reveal = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for TransferRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let direction_icon = match self.direction {
            TransferDirection::Upload => IconName::Upload,
            TransferDirection::Download => IconName::Download,
        };

        let cancel_button = self.on_cancel.map(|handler| {
            icon_button(
                ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "cancel".into()),
                IconName::Close,
                "Cancel Transfer",
            )
            .on_click(move |event, window, cx| handler(event, window, cx))
        });
        let reveal_button = self.on_reveal.map(|handler| {
            icon_button(
                ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "reveal".into()),
                IconName::Folder,
                "Show in Finder",
            )
            .on_click(move |event, window, cx| handler(event, window, cx))
        });
        let retry_button = self.on_retry.map(|handler| {
            icon_button(
                ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "retry".into()),
                IconName::Refresh,
                "Retry Transfer",
            )
            .on_click(move |event, window, cx| handler(event, window, cx))
        });
        let detail_id =
            ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "detail".into());
        let title_id = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "title".into());

        div()
            .id(self.id)
            .flex()
            .flex_col()
            .flex_none()
            .justify_center()
            .w_full()
            .h(theme.sizes.transfer_row_height)
            .px_2()
            .gap_1()
            .rounded(theme.sizes.button_radius)
            .hover(|style| style.bg(theme.colors.element_hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(direction_icon, theme.colors.text_muted))
                    .child(
                        div()
                            .id(title_id)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_role(theme, theme.typography.secondary)
                            .text_color(theme.colors.text)
                            .tooltip(text_tooltip(self.title.clone()))
                            .child(self.title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_role(theme, theme.typography.label)
                            .text_color(self.state_color)
                            .child(self.state_label),
                    )
                    .children(reveal_button)
                    .children(cancel_button)
                    .children(retry_button),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    // The width of the direction icon above, so the rail starts
                    // under the title text instead of 1.5px right of it.
                    .child(div().flex_none().w(ICON_SIZE))
                    .child(
                        div()
                            .flex_1()
                            .h(px(3.0))
                            .rounded_full()
                            .bg(theme.colors.element_active)
                            .when_some(
                                self.progress.zip(self.progress_color),
                                |bar, (fraction, color)| {
                                    bar.child(
                                        div()
                                            .h_full()
                                            .rounded_full()
                                            .bg(color)
                                            .w(DefiniteLength::Fraction(fraction)),
                                    )
                                },
                            ),
                    )
                    .child(
                        div()
                            .id(detail_id)
                            .flex_none()
                            .max_w(px(160.0))
                            .min_w_0()
                            .truncate()
                            .text_role(theme, theme.typography.mono)
                            .text_color(theme.colors.text_muted)
                            .when(!self.detail_label.is_empty(), |el| {
                                el.tooltip(text_tooltip(self.detail_label.clone()))
                            })
                            .child(self.detail_label),
                    ),
            )
    }
}
