use gpui::{
    App, ClickEvent, DefiniteLength, ElementId, Hsla, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, prelude::*, px,
};
use macsftp_core::TransferDirection;

use crate::components::icon_button;
use crate::icon::{IconName, icon};
use crate::theme::ActiveTheme;

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
                ElementId::NamedChild(Box::new(self.id.clone()), "cancel".into()),
                IconName::Close,
                "Cancel Transfer",
            )
            .on_click(move |event, window, cx| handler(event, window, cx))
        });
        let retry_button = self.on_retry.map(|handler| {
            icon_button(
                ElementId::NamedChild(Box::new(self.id.clone()), "retry".into()),
                IconName::Refresh,
                "Retry Transfer",
            )
            .on_click(move |event, window, cx| handler(event, window, cx))
        });

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
            .font_family(theme.fonts.ui_family.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(direction_icon, theme.colors.text_muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(theme.colors.text)
                            .child(self.title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(self.state_color)
                            .child(self.state_label),
                    )
                    .children(cancel_button)
                    .children(retry_button),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl(px(22.0))
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
                            .flex_none()
                            .text_size(px(10.0))
                            .text_color(theme.colors.text_muted)
                            .font_family(theme.fonts.mono_family.clone())
                            .child(self.detail_label),
                    ),
            )
    }
}
