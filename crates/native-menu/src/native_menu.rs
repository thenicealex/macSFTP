//! Narrow native presenter for Base's ordinary-input menu model.
//! GPUI actions and editing state remain outside the AppKit tracking loop.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::show;

#[cfg(not(target_os = "macos"))]
pub fn show(
    _menu: gpui_base::input::NativeMenu,
    _position: gpui::Point<gpui::Pixels>,
    _window: &mut gpui::Window,
    _cx: &mut gpui::App,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native input menus require macOS",
    ))
}

#[cfg(any(target_os = "macos", test))]
fn selected_action(
    menu: &gpui_base::input::NativeMenu,
    tag: isize,
) -> Option<Box<dyn gpui::Action>> {
    let index = usize::try_from(tag).ok()?;
    match menu.items.get(index)? {
        gpui_base::input::NativeMenuItem::Action {
            action,
            disabled: false,
            ..
        } => Some(action.boxed_clone()),
        _ => None,
    }
}

// Kept outside AppKit so window/focus lifetime guards can be exercised with
// real GPUI window updates while a deterministic tracker replaces only the OS loop.
#[cfg(any(target_os = "macos", test))]
fn schedule_menu(
    window_handle: gpui::AnyWindowHandle,
    owner_focus: Option<gpui::FocusHandle>,
    cx: &gpui::App,
    track: impl FnOnce() -> Option<Box<dyn gpui::Action>> + 'static,
) {
    cx.spawn(async move |cx| {
        // Check the window still exists before entering AppKit; keep GPUI
        // entirely unborrowed while AppKit pumps its synchronous tracking loop.
        match cx.update(|app| {
            window_handle.update(app, |_, window, app| window.focused(app) == owner_focus)
        }) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::debug!(%error, "native menu owner closed before presentation");
                return;
            }
        }
        let action = track();
        match cx.update(move |app| {
            window_handle.update(app, |_, window, app| {
                if window.focused(app) == owner_focus
                    && let Some(action) = action
                {
                    window.dispatch_action(action, app);
                }
                // Repaint/re-register mouse handlers even after Escape or dismissal.
                window.refresh();
            })
        }) {
            Ok(()) => {}
            Err(error) => tracing::debug!(%error, "native menu owner closed during tracking"),
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_base::input::{Copy, Cut, NativeMenu, SelectAll};

    gpui::actions!(menu_lifecycle, [Activate]);

    struct LifecycleView {
        first: gpui::FocusHandle,
        second: gpui::FocusHandle,
        activations: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl gpui::Render for LifecycleView {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
            gpui::div()
                .track_focus(&self.first)
                .size_full()
                .on_action(cx.listener(|view, _: &Activate, _, _| {
                    view.activations.set(view.activations.get() + 1)
                }))
                .child(gpui::div().track_focus(&self.second).size(gpui::px(20.0)))
        }
    }

    fn lifecycle_window(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::WindowHandle<LifecycleView>,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let activations = std::rc::Rc::new(std::cell::Cell::new(0));
        let handle = cx.add_window({
            let activations = activations.clone();
            move |window, cx| {
                let first = cx.focus_handle();
                first.focus(window, cx);
                LifecycleView {
                    first,
                    second: cx.focus_handle(),
                    activations,
                }
            }
        });
        (handle, activations)
    }

    #[gpui::test]
    fn closed_window_before_presentation_never_enters_tracker(cx: &mut gpui::TestAppContext) {
        let (handle, activations) = lifecycle_window(cx);
        let tracked = std::rc::Rc::new(std::cell::Cell::new(false));
        let captured = tracked.clone();
        let handle: gpui::AnyWindowHandle = handle.into();
        handle
            .update(cx, |_, window, app| {
                schedule_menu(handle, window.focused(app), app, move || {
                    captured.set(true);
                    Some(Box::new(Activate))
                });
                window.remove_window();
            })
            .expect("the owner must exist when scheduling the menu");
        cx.run_until_parked();
        assert!(!tracked.get());
        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn window_closed_during_tracking_drops_selected_action(cx: &mut gpui::TestAppContext) {
        let (handle, activations) = lifecycle_window(cx);
        let handle: gpui::AnyWindowHandle = handle.into();
        let async_app = cx.to_async();
        handle.update(cx, |_, window, app| {
            schedule_menu(handle, window.focused(app), app, move || {
                async_app.update(|app| handle.update(app, |_, window, _| window.remove_window()))
                    .expect("the simulated tracking loop must close its live owner without a GPUI borrow");
                Some(Box::new(Activate))
            });
        }).expect("the owner must exist when scheduling the menu");
        cx.run_until_parked();
        assert_eq!(activations.get(), 0);
        assert!(
            handle.update(cx, |_, _, _| ()).is_err(),
            "the tracking loop must actually close the window"
        );
    }

    #[gpui::test]
    fn changed_focus_during_tracking_drops_selected_action(cx: &mut gpui::TestAppContext) {
        let (handle, activations) = lifecycle_window(cx);
        let second = handle
            .read_with(cx, |view, _| view.second.clone())
            .expect("live view must expose its second focus");
        let handle: gpui::AnyWindowHandle = handle.into();
        let async_app = cx.to_async();
        handle
            .update(cx, |_, window, app| {
                schedule_menu(handle, window.focused(app), app, move || {
                    async_app
                        .update(|app| {
                            handle.update(app, |_, window, app| second.focus(window, app))
                        })
                        .expect(
                            "the simulated tracking loop must change focus without a GPUI borrow",
                        );
                    Some(Box::new(Activate))
                });
            })
            .expect("the owner must exist when scheduling the menu");
        cx.run_until_parked();
        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn repeated_cancellation_keeps_the_next_menu_dispatchable(cx: &mut gpui::TestAppContext) {
        let (handle, activations) = lifecycle_window(cx);
        let handle: gpui::AnyWindowHandle = handle.into();
        let tracked = std::rc::Rc::new(std::cell::Cell::new(0));
        for _ in 0..10 {
            let captured = tracked.clone();
            handle
                .update(cx, |_, window, app| {
                    schedule_menu(handle, window.focused(app), app, move || {
                        captured.set(captured.get() + 1);
                        None
                    })
                })
                .expect("cancelled menus must leave their owner available");
            cx.run_until_parked();
        }
        assert_eq!(tracked.get(), 10);
        assert_eq!(activations.get(), 0);
        handle
            .update(cx, |_, window, app| {
                schedule_menu(handle, window.focused(app), app, || {
                    Some(Box::new(Activate))
                })
            })
            .expect("the owner must accept another menu after cancellation");
        cx.run_until_parked();
        assert_eq!(
            activations.get(),
            1,
            "the next selected action must dispatch once"
        );
    }

    #[cfg(target_os = "macos")]
    #[gpui::test]
    fn menu_requires_native_main_thread_window_and_empty_is_inert(cx: &mut gpui::TestAppContext) {
        struct Content;
        impl gpui::Render for Content {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                gpui::div()
            }
        }
        let (_, cx) = cx.add_window_view(|_, _| Content);
        cx.update(|window, cx| {
            let position = gpui::point(gpui::px(0.0), gpui::px(0.0));
            assert!(show(NativeMenu::new(), position, window, cx).is_ok());
            assert!(
                show(
                    NativeMenu::new().menu("Copy", Box::new(Copy)),
                    position,
                    window,
                    cx
                )
                .is_err(),
                "a headless window must not enter AppKit or dispatch actions"
            );
        });
    }

    #[test]
    fn selection_uses_model_indices_and_rejects_disabled_or_stale_tags() {
        let menu = NativeMenu::new()
            .menu_with_disabled("Cut", true, Box::new(Cut))
            .separator()
            .menu("Copy", Box::new(Copy))
            .menu("Select All", Box::new(SelectAll));
        for tag in [-1, 0, 1, 4, isize::MAX] {
            assert!(selected_action(&menu, tag).is_none());
        }
        assert!(
            selected_action(&menu, 2)
                .expect("enabled Copy must resolve")
                .as_any()
                .is::<Copy>()
        );
        assert!(
            selected_action(&menu, 3)
                .expect("enabled Select All must resolve")
                .as_any()
                .is::<SelectAll>()
        );
    }
}
