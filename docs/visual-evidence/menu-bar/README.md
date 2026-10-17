# Menu bar visual evidence

`menu-bar.jpg` is a native macOS capture taken with macSFTP frontmost
(2026-10-17, after the menu bar work): the six top-level menus in macOS order —
app menu, File, Edit, View, Window, Help — with the Edit menu that was missing
before, sitting between File and View.

Only the menu bar strip (`screencapture -x -R0,0,820,26`) is in frame; no other
part of the desktop is included. The capture came from a debug build copied
into a minimal `macSFTP.app` bundle — AppKit takes the app menu title from the
bundle display name, so a raw `cargo build` binary shows the lowercase process
name instead — launched with `HOME` pointed at a temporary directory so it read
and wrote no real session state.

Menu **contents** cannot be captured without granting the capturing process
accessibility permission for synthetic clicks, so they are covered two ways:

- `cargo test -p macsftp-app --bin macsftp app_menus` — menu order and items,
  the standard AppKit Edit selectors, and the table that keeps every menu item,
  its keybinding chord, and its command-palette id aligned (`app_menus.rs`).
- `cargo test -p macsftp-app --bin macsftp sort_actions window_title_follows` —
  the View ▸ Sort By commands re-sort the active tab, and the window title
  follows the active tab.

The window title is deliberately invisible in a capture: the titlebar is
transparent and hides it. Mission Control and the Window menu use the active
tab's title, which is why the title is set at all.
