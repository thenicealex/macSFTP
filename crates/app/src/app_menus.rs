//! The application menu bar.
//!
//! One place defines it. The items reference the same actions as the keymap
//! (`app_actions`) and the command palette (`palette_commands`), and the tests
//! below keep the three aligned: a label, an action, and the chord an item
//! displays cannot drift apart unnoticed.
//!
//! Structure and copy follow macOS convention — application menu, File, Edit,
//! View, Window, Help. The Edit menu uses [`MenuItem::os_action`] so AppKit
//! gets the standard selectors (`cut:`, `copy:`, `undo:` …) while the items
//! still dispatch the `gpui-base` input actions that edit our text fields
//! (§12.14). The Window menu is named exactly `"Window"`: the platform turns
//! that menu into the system window list (Mission Control shows the same
//! titles, §12.7).

use gpui_kit::base::input;
use gpui_kit::{Menu, MenuItem, OsAction, SystemMenuType};

use crate::APP_NAME;
use crate::app_actions::{
    BringAllToFront, CloseTab, CloseWindow, CopyVersionInfo, DeleteSelection, DownloadSelection,
    FilterPane, FocusLocalPane, FocusRemotePane, GoToPath, HideApplication, HideOtherApplications,
    MinimizeWindow, NewFolder, NewTab, NewWindow, OpenLogFolder, OpenSettings, Quit, ReconnectTab,
    RefreshPane, RenameEntry, ShowAbout, ShowAllApplications, ShowTransferDrawer,
    SortByDateModifiedAscending, SortByDateModifiedDescending, SortByNameAscending,
    SortByNameDescending, SortBySizeAscending, SortBySizeDescending, ToggleHiddenFiles,
    UploadEditedFile, UploadSelection, ZoomWindow,
};

pub(crate) fn menu_bar() -> Vec<Menu> {
    vec![
        application_menu(),
        file_menu(),
        edit_menu(),
        view_menu(),
        window_menu(),
        help_menu(),
    ]
}

fn application_menu() -> Menu {
    Menu {
        disabled: false,
        name: APP_NAME.into(),
        items: vec![
            MenuItem::action(format!("About {APP_NAME}"), ShowAbout),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {APP_NAME}"), HideApplication),
            MenuItem::action("Hide Others", HideOtherApplications),
            MenuItem::action("Show All", ShowAllApplications),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ],
    }
}

fn file_menu() -> Menu {
    Menu {
        disabled: false,
        name: "File".into(),
        // Commands that open a panel asking for input or confirmation carry the
        // HIG ellipsis — the same rule the pane context menu documents
        // (workspace::context_menu).
        items: vec![
            MenuItem::action("New Window", NewWindow),
            MenuItem::action("New Connection", NewTab),
            MenuItem::separator(),
            MenuItem::action("New Folder…", NewFolder),
            MenuItem::action("Rename…", RenameEntry),
            MenuItem::action("Delete…", DeleteSelection),
            MenuItem::separator(),
            MenuItem::action("Upload", UploadSelection),
            MenuItem::action("Upload Edited File", UploadEditedFile),
            MenuItem::action("Download", DownloadSelection),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
        ],
    }
}

fn edit_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Edit".into(),
        items: vec![
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        ],
    }
}

fn sort_by_submenu() -> Menu {
    Menu {
        disabled: false,
        name: "Sort By".into(),
        items: vec![
            MenuItem::action("Name (A→Z)", SortByNameAscending),
            MenuItem::action("Name (Z→A)", SortByNameDescending),
            MenuItem::action("Size (Smallest First)", SortBySizeAscending),
            MenuItem::action("Size (Largest First)", SortBySizeDescending),
            MenuItem::action("Last Modified (Oldest First)", SortByDateModifiedAscending),
            MenuItem::action("Last Modified (Newest First)", SortByDateModifiedDescending),
        ],
    }
}

fn view_menu() -> Menu {
    Menu {
        disabled: false,
        name: "View".into(),
        items: vec![
            MenuItem::action("Select Local Files", FocusLocalPane),
            MenuItem::action("Select Server Files", FocusRemotePane),
            MenuItem::separator(),
            MenuItem::action("Show Hidden Files", ToggleHiddenFiles),
            MenuItem::submenu(sort_by_submenu()),
            MenuItem::action("Filter Files", FilterPane),
            MenuItem::separator(),
            MenuItem::action("Go to Folder…", GoToPath),
            MenuItem::separator(),
            MenuItem::action("Refresh", RefreshPane),
            MenuItem::action("Reconnect", ReconnectTab),
            MenuItem::separator(),
            MenuItem::action("Transfers", ShowTransferDrawer),
        ],
    }
}

fn window_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Window".into(),
        items: vec![
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Zoom", ZoomWindow),
            MenuItem::separator(),
            MenuItem::action("Bring All to Front", BringAllToFront),
        ],
    }
}

fn help_menu() -> Menu {
    Menu {
        disabled: false,
        name: "Help".into(),
        items: vec![
            MenuItem::action("Show Logs", OpenLogFolder),
            MenuItem::action("Copy Version Info", CopyVersionInfo),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_actions::keybindings;
    use crate::palette_commands::all_palette_commands;
    use gpui_kit::{Action, Keystroke};

    /// One command the menu bar must offer: its full path, its action, the
    /// chord macOS displays next to the item (derived from the keymap), and
    /// the command-palette row that mirrors the same action (§12.2). A row
    /// with no chord is a command macOS leaves unshortcut, not an oversight.
    type DocumentedCommand = (
        String,
        Box<dyn Action>,
        Option<&'static str>,
        Option<&'static str>,
    );

    fn documented_commands() -> Vec<DocumentedCommand> {
        let app_menu = APP_NAME;
        vec![
            (
                format!("{app_menu} ▸ About {app_menu}"),
                Box::new(ShowAbout),
                None,
                Some("ShowAbout"),
            ),
            (
                format!("{app_menu} ▸ Settings…"),
                Box::new(OpenSettings),
                Some("cmd-,"),
                Some("OpenSettings"),
            ),
            (
                format!("{app_menu} ▸ Hide {app_menu}"),
                Box::new(HideApplication),
                Some("cmd-h"),
                None,
            ),
            (
                format!("{app_menu} ▸ Hide Others"),
                Box::new(HideOtherApplications),
                Some("cmd-alt-h"),
                None,
            ),
            (
                format!("{app_menu} ▸ Show All"),
                Box::new(ShowAllApplications),
                None,
                None,
            ),
            (
                format!("{app_menu} ▸ Quit {app_menu}"),
                Box::new(Quit),
                Some("cmd-q"),
                None,
            ),
            (
                "File ▸ New Window".to_string(),
                Box::new(NewWindow),
                Some("cmd-n"),
                None,
            ),
            (
                "File ▸ New Connection".to_string(),
                Box::new(NewTab),
                Some("cmd-t"),
                Some("NewTab"),
            ),
            (
                "File ▸ New Folder…".to_string(),
                Box::new(NewFolder),
                Some("cmd-shift-n"),
                Some("NewFolder"),
            ),
            (
                "File ▸ Rename…".to_string(),
                Box::new(RenameEntry),
                Some("f2"),
                Some("RenameEntry"),
            ),
            (
                "File ▸ Delete…".to_string(),
                Box::new(DeleteSelection),
                Some("cmd-backspace"),
                Some("DeleteSelection"),
            ),
            (
                "File ▸ Upload".to_string(),
                Box::new(UploadSelection),
                Some("cmd-u"),
                Some("UploadSelection"),
            ),
            (
                "File ▸ Upload Edited File".to_string(),
                Box::new(UploadEditedFile),
                Some("cmd-u"),
                Some("UploadEditedFile"),
            ),
            (
                "File ▸ Download".to_string(),
                Box::new(DownloadSelection),
                Some("cmd-d"),
                Some("DownloadSelection"),
            ),
            (
                "File ▸ Close Tab".to_string(),
                Box::new(CloseTab),
                Some("cmd-w"),
                Some("CloseTab"),
            ),
            (
                "File ▸ Close Window".to_string(),
                Box::new(CloseWindow),
                Some("cmd-shift-w"),
                None,
            ),
            (
                "View ▸ Select Local Files".to_string(),
                Box::new(FocusLocalPane),
                Some("cmd-1"),
                Some("FocusLocalPane"),
            ),
            (
                "View ▸ Select Server Files".to_string(),
                Box::new(FocusRemotePane),
                Some("cmd-2"),
                Some("FocusRemotePane"),
            ),
            (
                "View ▸ Show Hidden Files".to_string(),
                Box::new(ToggleHiddenFiles),
                Some("cmd-shift-."),
                Some("ToggleHiddenFiles"),
            ),
            (
                "View ▸ Sort By ▸ Name (A→Z)".to_string(),
                Box::new(SortByNameAscending),
                None,
                Some("SortByNameAscending"),
            ),
            (
                "View ▸ Sort By ▸ Name (Z→A)".to_string(),
                Box::new(SortByNameDescending),
                None,
                Some("SortByNameDescending"),
            ),
            (
                "View ▸ Sort By ▸ Size (Smallest First)".to_string(),
                Box::new(SortBySizeAscending),
                None,
                Some("SortBySizeAscending"),
            ),
            (
                "View ▸ Sort By ▸ Size (Largest First)".to_string(),
                Box::new(SortBySizeDescending),
                None,
                Some("SortBySizeDescending"),
            ),
            (
                "View ▸ Sort By ▸ Last Modified (Oldest First)".to_string(),
                Box::new(SortByDateModifiedAscending),
                None,
                Some("SortByDateModifiedAscending"),
            ),
            (
                "View ▸ Sort By ▸ Last Modified (Newest First)".to_string(),
                Box::new(SortByDateModifiedDescending),
                None,
                Some("SortByDateModifiedDescending"),
            ),
            (
                "View ▸ Filter Files".to_string(),
                Box::new(FilterPane),
                Some("cmd-f"),
                Some("FilterPane"),
            ),
            (
                "View ▸ Go to Folder…".to_string(),
                Box::new(GoToPath),
                Some("cmd-shift-g"),
                Some("GoToPath"),
            ),
            (
                "View ▸ Refresh".to_string(),
                Box::new(RefreshPane),
                Some("cmd-r"),
                Some("RefreshPane"),
            ),
            (
                "View ▸ Reconnect".to_string(),
                Box::new(ReconnectTab),
                Some("cmd-shift-r"),
                Some("ReconnectTab"),
            ),
            (
                "View ▸ Transfers".to_string(),
                Box::new(ShowTransferDrawer),
                Some("cmd-j"),
                Some("ShowTransferDrawer"),
            ),
            (
                "Window ▸ Minimize".to_string(),
                Box::new(MinimizeWindow),
                Some("cmd-m"),
                None,
            ),
            (
                "Window ▸ Zoom".to_string(),
                Box::new(ZoomWindow),
                None,
                None,
            ),
            (
                "Window ▸ Bring All to Front".to_string(),
                Box::new(BringAllToFront),
                None,
                None,
            ),
            (
                "Help ▸ Show Logs".to_string(),
                Box::new(OpenLogFolder),
                None,
                Some("OpenLogFolder"),
            ),
            (
                "Help ▸ Copy Version Info".to_string(),
                Box::new(CopyVersionInfo),
                None,
                None,
            ),
        ]
    }

    /// Every `MenuItem::Action` in the bar, flattened, as
    /// `(path, action, os_action)`.
    fn action_items(menus: &[Menu]) -> Vec<(String, &dyn Action, Option<OsAction>)> {
        fn walk<'a>(
            menu: &'a Menu,
            path: &str,
            out: &mut Vec<(String, &'a dyn Action, Option<OsAction>)>,
        ) {
            for item in &menu.items {
                match item {
                    MenuItem::Action {
                        name,
                        action,
                        os_action,
                        ..
                    } => out.push((format!("{path} ▸ {name}"), action.as_ref(), *os_action)),
                    MenuItem::Submenu(submenu) => {
                        walk(submenu, &format!("{path} ▸ {}", submenu.name), out)
                    }
                    MenuItem::Separator | MenuItem::SystemMenu(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        for menu in menus {
            walk(menu, &menu.name, &mut out);
        }
        out
    }

    fn item_names(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .map(|item| match item {
                MenuItem::Separator => "—".to_string(),
                MenuItem::Action { name, .. } => name.to_string(),
                MenuItem::Submenu(submenu) => format!("[{}]", submenu.name),
                MenuItem::SystemMenu(os_menu) => format!("[{}]", os_menu.name),
            })
            .collect()
    }

    #[test]
    fn menu_bar_follows_macos_structure() {
        let menus = menu_bar();
        let names: Vec<String> = menus.iter().map(|menu| menu.name.to_string()).collect();
        // The Window menu's name is load-bearing: the platform hands AppKit
        // the menu with exactly this name as the system window list.
        assert_eq!(names, [APP_NAME, "File", "Edit", "View", "Window", "Help"]);

        let edit = &menus[2];
        assert_eq!(
            item_names(edit),
            ["Undo", "Redo", "—", "Cut", "Copy", "Paste", "Select All"]
        );
        // AppKit standard selectors, so system services (save panels, the
        // Services menu) and the input actions agree on what Edit means.
        let expected = [
            (
                "Undo",
                OsAction::Undo,
                Box::new(input::Undo) as Box<dyn Action>,
            ),
            ("Redo", OsAction::Redo, Box::new(input::Redo)),
            ("Cut", OsAction::Cut, Box::new(input::Cut)),
            ("Copy", OsAction::Copy, Box::new(input::Copy)),
            ("Paste", OsAction::Paste, Box::new(input::Paste)),
            (
                "Select All",
                OsAction::SelectAll,
                Box::new(input::SelectAll),
            ),
        ];
        let edit_actions: Vec<&dyn Action> = edit
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(action.as_ref()),
                _ => None,
            })
            .collect();
        assert_eq!(edit_actions.len(), expected.len());
        for (name, os_action, action) in expected {
            let item = edit
                .items
                .iter()
                .find(|item| match item {
                    MenuItem::Action {
                        name: item_name, ..
                    } => item_name.as_ref() == name,
                    _ => false,
                })
                .unwrap_or_else(|| panic!("Edit menu is missing {name}"));
            match item {
                MenuItem::Action {
                    action: item_action,
                    os_action: item_os_action,
                    ..
                } => {
                    assert!(
                        *item_os_action == Some(os_action),
                        "{name} must use the standard AppKit selector"
                    );
                    assert!(
                        item_action.partial_eq(&*action),
                        "{name} must dispatch {}",
                        action.name()
                    );
                }
                _ => unreachable!("filtered to actions above"),
            }
        }
    }

    #[test]
    fn menu_commands_match_keymap_and_palette() {
        let menu_bar = menu_bar();
        let items = action_items(&menu_bar);
        for (path, action, chord, palette_id) in documented_commands() {
            let found = items
                .iter()
                .find(|(_, item_action, _)| item_action.partial_eq(&*action))
                .unwrap_or_else(|| panic!("menu bar is missing {}", action.name()));
            assert_eq!(found.0, path, "menu path for {}", action.name());

            if let Some(chord) = chord {
                let expected: Vec<Keystroke> = chord
                    .split_whitespace()
                    .map(|stroke| {
                        Keystroke::parse(stroke)
                            .unwrap_or_else(|_| panic!("{stroke} is a valid keystroke"))
                    })
                    .collect();
                let binding = keybindings()
                    .into_iter()
                    .find(|binding| {
                        binding.action().partial_eq(&*action)
                            && binding
                                .keystrokes()
                                .iter()
                                .map(|stroke| stroke.inner().clone())
                                .eq(expected.iter().cloned())
                    })
                    .unwrap_or_else(|| {
                        panic!("{} displays {chord} but nothing binds it", action.name())
                    });
                assert_eq!(binding.keystrokes().len(), expected.len());
            }

            if let Some(palette_id) = palette_id {
                let command = all_palette_commands()
                    .iter()
                    .find(|command| command.id == palette_id)
                    .unwrap_or_else(|| panic!("{path} must have a palette row ({palette_id})"));
                assert_eq!(
                    command.keybinding.is_some(),
                    chord.is_some(),
                    "palette row {palette_id} shows a chord only when the item does"
                );
            }
        }

        // Nothing rides along undocumented: every item is either in the table
        // above or an Edit-menu input action (whose chords gpui-base owns).
        let documented = documented_commands();
        let edit_actions: [Box<dyn Action>; 6] = [
            Box::new(input::Undo),
            Box::new(input::Redo),
            Box::new(input::Cut),
            Box::new(input::Copy),
            Box::new(input::Paste),
            Box::new(input::SelectAll),
        ];
        for (path, action, _) in &items {
            let is_documented = documented
                .iter()
                .any(|(_, documented, _, _)| documented.partial_eq(*action));
            let is_edit = edit_actions
                .iter()
                .any(|edit_action| edit_action.partial_eq(*action));
            assert!(
                is_documented || is_edit,
                "{path} ({}) is not in the documented menu table",
                action.name()
            );
        }
    }
}
