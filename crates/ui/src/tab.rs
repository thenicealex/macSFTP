use gpui_kit::{
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
    /// Hover group this tab publishes: the close affordance appears while the
    /// pointer is anywhere on the tab (the Safari convention). Needing a group
    /// rather than a plain `hover` is why the app names it — the button itself
    /// is not drawn until then.
    hover_group: Option<SharedString>,
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
        hover_group: None,
        on_activate: None,
        on_close: None,
    }
}

impl Tab {
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Reveal the close affordance while the pointer is on this tab. Pass a
    /// name unique to the tab; without it the close button stays always
    /// visible, which is the pre-existing behavior.
    pub fn hover_group(mut self, group: impl Into<SharedString>) -> Self {
        self.hover_group = Some(group.into());
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
        let close_id = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "close".into());
        // The 26x22 close slot keeps its place in the layout on every tab, so
        // revealing it never shifts the title. On an inactive tab it is
        // `Visibility::Hidden` rather than transparent: a hidden element takes
        // no clicks, so a click aimed at the tab body can never land on a close
        // button the user has not seen.
        let mut close_slot = div().flex_none().debug_selector(|| "tab-close".into());
        if !self.active {
            close_slot = close_slot.invisible();
        }
        if let Some(group) = self.hover_group.clone() {
            close_slot = close_slot.group_hover(group, |style| style.visible());
        }

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
            .when_some(self.hover_group.clone(), |element, group| {
                element.group(group)
            })
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
            .child(close_slot.child(
                icon_button(close_id, IconName::Close, "Close Tab").when_some(
                    self.on_close,
                    |button, handler| {
                        button.on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            handler(event, window, cx);
                        })
                    },
                ),
            ))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui_kit::{Context, Modifiers, MouseButton, TestAppContext, point, rgb, size};

    use super::*;

    struct TabHarness {
        active: bool,
        activated: Rc<Cell<usize>>,
        closed: Rc<Cell<usize>>,
    }

    impl Render for TabHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let activated = self.activated.clone();
            let closed = self.closed.clone();
            div().flex().w(px(240.0)).h(px(40.0)).child(
                tab("t1", "example.com", rgb(0x00ff00).into(), "Connected")
                    .active(self.active)
                    .hover_group("tab-1")
                    .on_activate(move |_, _, _| activated.set(activated.get() + 1))
                    .on_close(move |_, _, _| closed.set(closed.get() + 1)),
            )
        }
    }

    #[gpui_kit::test]
    fn inactive_tab_hides_its_close_button_until_hovered(cx: &mut TestAppContext) {
        cx.update(|cx| crate::theme::Theme::one_dark().install(cx));
        let activated = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        let (_view, cx) = cx.add_window_view({
            let activated = activated.clone();
            let closed = closed.clone();
            move |_window, _cx| TabHarness {
                active: false,
                activated,
                closed,
            }
        });
        cx.simulate_resize(size(px(240.0), px(40.0)));
        cx.run_until_parked();
        let center = cx
            .debug_bounds("tab-close")
            .expect("the close slot must be laid out on every tab")
            .center();
        // The test window parks the pointer in its middle, which is on the tab;
        // move it off the tab first so "not hovered" really holds.
        cx.simulate_mouse_move(
            point(px(230.0), px(30.0)),
            None::<MouseButton>,
            Modifiers::default(),
        );
        cx.run_until_parked();
        // No hover: a click aimed at the tab body must not close it.
        cx.simulate_click(center, Modifiers::default());
        assert_eq!(
            closed.get(),
            0,
            "a close button the user has not seen must not close the tab"
        );
        assert_eq!(activated.get(), 1, "the click must reach the tab body");
        // Hovering the tab reveals the button, and only then it takes clicks.
        cx.simulate_mouse_move(center, None::<MouseButton>, Modifiers::default());
        cx.run_until_parked();
        cx.simulate_click(center, Modifiers::default());
        assert_eq!(closed.get(), 1, "the revealed close button must work");
        assert_eq!(activated.get(), 1, "closing must not also activate the tab");
    }

    #[gpui_kit::test]
    fn active_tab_keeps_its_close_button_visible(cx: &mut TestAppContext) {
        cx.update(|cx| crate::theme::Theme::one_dark().install(cx));
        let activated = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        let (_view, cx) = cx.add_window_view({
            let activated = activated.clone();
            let closed = closed.clone();
            move |_window, _cx| TabHarness {
                active: true,
                activated,
                closed,
            }
        });
        cx.simulate_resize(size(px(240.0), px(40.0)));
        cx.run_until_parked();
        let center = cx
            .debug_bounds("tab-close")
            .expect("the close slot must be laid out on every tab")
            .center();
        // An active tab shows its close button even while the pointer is away
        // from the tab.
        cx.simulate_mouse_move(
            point(px(230.0), px(30.0)),
            None::<MouseButton>,
            Modifiers::default(),
        );
        cx.run_until_parked();
        cx.simulate_click(center, Modifiers::default());
        assert_eq!(
            closed.get(),
            1,
            "the active tab always shows its close button"
        );
        assert_eq!(activated.get(), 0, "closing must not also activate the tab");
    }
}
