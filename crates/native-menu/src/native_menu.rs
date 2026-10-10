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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_base::input::{Copy, Cut, NativeMenu, SelectAll};

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
