// AppKit target/tracking pattern adapted from gpui-component 0.7.1
// src/native_menu/macos.rs (Apache-2.0; see ../LICENSE-APACHE).
// This adapter retains NSView and guards window/focus lifetime; it supports
// only Base ordinary-input actions, without icons or nested menus.

use std::{cell::Cell, io};

use gpui::{App, Pixels, Point, Window};
use gpui_base::input::{NativeMenu, NativeMenuItem};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel,
};
use objc2_app_kit::{NSMenu, NSMenuItem, NSView};
use objc2_foundation::{NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

struct MenuTargetIvars {
    selected: Cell<isize>,
}

// SAFETY: this NSObject subclass is created and invoked only on the main thread.
// The selector has the AppKit action signature and records only the item's tag.
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MacSFTPNativeInputMenuTarget"]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;
    impl MenuTarget {
        #[unsafe(method(menuItemClicked:))]
        fn menu_item_clicked(&self, sender: &NSMenuItem) {
            self.ivars().selected.set(sender.tag());
        }
    }
);

impl MenuTarget {
    fn new() -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(MenuTargetIvars {
            selected: Cell::new(-1),
        });
        // SAFETY: allocated is our initialized NSObject subclass; init returns
        // ownership of that same class with the ivars established above.
        unsafe { msg_send![super(allocated), init] }
    }
}

pub fn show(
    menu: NativeMenu,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> io::Result<()> {
    if menu.items.is_empty() {
        return Ok(());
    }
    let marker = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("native menu must run on the main thread"))?;
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err(io::Error::other("native menu requires an AppKit window"));
    };
    // SAFETY: GPUI's borrowed AppKit handle guarantees a live NSView. Retain it
    // before releasing the Window borrow so closing a window cannot leave the
    // scheduled tracking task with a dangling pointer. Both uses stay on GPUI's
    // foreground/main-thread executor.
    let view = unsafe { Retained::<NSView>::retain(handle.ns_view.as_ptr().cast()) }
        .ok_or_else(|| io::Error::other("could not retain native menu view"))?;
    let window_handle = Window::window_handle(window);
    let owner_focus = window.focused(cx);
    super::schedule_menu(window_handle, owner_focus, cx, move || {
        run_menu(&menu, &view, position, marker)
    });
    Ok(())
}

fn run_menu(
    menu: &NativeMenu,
    view: &NSView,
    position: Point<Pixels>,
    marker: MainThreadMarker,
) -> Option<Box<dyn gpui::Action>> {
    let target = MenuTarget::new();
    let native = NSMenu::new(marker);
    native.setAutoenablesItems(false);
    for (index, item) in menu.items.iter().enumerate() {
        match item {
            NativeMenuItem::Separator => native.addItem(&NSMenuItem::separatorItem(marker)),
            NativeMenuItem::Action {
                label, disabled, ..
            } => {
                let tag = isize::try_from(index).ok()?;
                let native_item = NSMenuItem::new(marker);
                native_item.setTitle(&NSString::from_str(label));
                native_item.setEnabled(!*disabled);
                native_item.setTag(tag);
                if !disabled {
                    // SAFETY: target remains retained for the whole tracking
                    // call, and implements the exact one-argument action selector.
                    unsafe {
                        native_item.setTarget(Some(&target as &AnyObject));
                        native_item.setAction(Some(sel!(menuItemClicked:)));
                    }
                }
                native.addItem(&native_item);
            }
        }
    }
    let y = if view.isFlipped() {
        f32::from(position.y) as f64
    } else {
        view.bounds().size.height - f32::from(position.y) as f64
    };
    native.popUpMenuPositioningItem_atLocation_inView(
        None,
        NSPoint::new(f32::from(position.x) as f64, y),
        Some(view),
    );
    super::selected_action(menu, target.ivars().selected.get())
}
