//! Native (AppKit) context menus for the local and remote file panes.
//!
//! The menu model is plain data ([`NativeMenu`]), so the item set is unit
//! tested without a window; only [`macsftp_native_menu::show`] touches AppKit.
//! The presenter keeps the GPUI window unborrowed while NSMenu tracks the
//! mouse and dispatches the selected item through the focused pane.

use gpui_kit::base::input::NativeMenu;
use gpui_kit::{App, Context, Pixels, Point, Window};
use macsftp_core::FileKind;
use tracing::warn;

use crate::app_actions::{
    ConnectRecent, CopyFiles, CopyPath, CopyRecentEndpoint, DeleteSelection, DownloadSelection,
    ForgetRecent, NewFolder, OpenSelectedEntry, PasteFiles, RefreshPane, RenameEntry, ShowInFinder,
    UploadSelection,
};
use crate::workspace::{PaneSide, Workspace};

/// What the user targeted inside a file pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextMenuTarget {
    /// Blank area below the listing.
    Background,
    /// A listing row; the kind picks the open-vs-edit wording.
    Entry(FileKind),
}

/// Build the pane context menu.
///
/// HIG conventions: primary action first, the destructive item in its own
/// group, an ellipsis on items that still ask for input or confirmation, and
/// commands that exist but are unavailable right now are disabled instead of
/// hidden. Only side and entry-kind differences remove items (Show in Finder
/// is local-only, Download is remote-only), because those commands never apply
/// to the other pane.
pub(crate) fn file_context_menu(
    side: PaneSide,
    target: ContextMenuTarget,
    remote_ready: bool,
    can_paste: bool,
) -> NativeMenu {
    match (side, target) {
        (_, ContextMenuTarget::Background) => {
            let enabled = side == PaneSide::Local || remote_ready;
            NativeMenu::new()
                .menu_with_disabled("New Folder…", !enabled, Box::new(NewFolder))
                .separator()
                .menu_with_disabled("Refresh", !enabled, Box::new(RefreshPane))
        }
        // Every local row opens: folders navigate, links resolve their target,
        // and files go to their default application (the Finder convention for
        // the user's own disk), so the entry kind no longer splits the menu.
        (PaneSide::Local, ContextMenuTarget::Entry(_)) => NativeMenu::new()
            .menu("Open", Box::new(OpenSelectedEntry))
            .separator()
            .menu_with_disabled("Upload", !remote_ready, Box::new(UploadSelection))
            .separator()
            .menu("Rename…", Box::new(RenameEntry))
            .menu("New Folder…", Box::new(NewFolder))
            .separator()
            .menu("Delete…", Box::new(DeleteSelection))
            .separator()
            .menu("Copy", Box::new(CopyFiles))
            .menu_with_disabled("Paste", !can_paste, Box::new(PasteFiles))
            .separator()
            .menu("Copy Path", Box::new(CopyPath))
            .menu("Show in Finder", Box::new(ShowInFinder)),
        (PaneSide::Remote, ContextMenuTarget::Entry(kind)) => {
            let open_label = match kind {
                FileKind::Directory | FileKind::Symlink => "Open",
                _ => "Open for Editing",
            };
            NativeMenu::new()
                .menu_with_disabled(open_label, !remote_ready, Box::new(OpenSelectedEntry))
                .separator()
                .menu_with_disabled("Download", !remote_ready, Box::new(DownloadSelection))
                .separator()
                .menu_with_disabled("Rename…", !remote_ready, Box::new(RenameEntry))
                .menu_with_disabled("New Folder…", !remote_ready, Box::new(NewFolder))
                .separator()
                .menu_with_disabled("Delete…", !remote_ready, Box::new(DeleteSelection))
                .separator()
                .menu("Copy", Box::new(CopyFiles))
                // Pasting into the server pane needs the session, unlike
                // copying the selection it would move.
                .menu_with_disabled("Paste", !can_paste || !remote_ready, Box::new(PasteFiles))
                .separator()
                .menu("Copy Path", Box::new(CopyPath))
        }
    }
}

impl Workspace {
    /// Select the row under the pointer (Transmit-style right-click semantics)
    /// and present the native context menu for the pane at `position`.
    pub(crate) fn show_file_context_menu(
        &mut self,
        side: PaneSide,
        entry_index: Option<usize>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Focus first: the presenter captures the focused handle and drops the
        // selected action if focus moved while the menu was open.
        self.focus_pane(side, window, cx);
        if let Some(index) = entry_index {
            self.select_index(side, index, cx);
        }
        let target = match entry_index.and_then(|index| self.entry_kind_at(side, index, cx)) {
            Some(kind) => ContextMenuTarget::Entry(kind),
            None => ContextMenuTarget::Background,
        };
        let remote_ready = self.fs_scope_for_side(PaneSide::Remote).is_some();
        let can_paste = !self.file_clipboard.is_empty();
        let menu = file_context_menu(side, target, remote_ready, can_paste);
        if let Err(error) = macsftp_native_menu::show(menu, position, window, cx) {
            warn!(%error, "could not present the file context menu");
        }
    }

    /// Present the recent-connection row menu: reconnect, copy the endpoint,
    /// or forget the entry. Pure row actions — no pane state is involved, so
    /// unlike the file menu nothing here is ever disabled.
    pub(crate) fn show_recent_context_menu(
        &mut self,
        recent_id: u64,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = NativeMenu::new()
            .menu("Connect", Box::new(ConnectRecent { id: recent_id }))
            .menu(
                "Copy user@host:port",
                Box::new(CopyRecentEndpoint { id: recent_id }),
            )
            .separator()
            .menu(
                "Remove from Recent Connections",
                Box::new(ForgetRecent { id: recent_id }),
            );
        if let Err(error) = macsftp_native_menu::show(menu, position, window, cx) {
            warn!(%error, "could not present the recent connections menu");
        }
    }

    /// Kind of the entry at a visible index, used for menu item wording.
    fn entry_kind_at(&self, side: PaneSide, visible_index: usize, cx: &App) -> Option<FileKind> {
        let real_index = self.visible_indices(side, cx).get(visible_index).copied()?;
        let tab = self.active_tab()?;
        match side {
            PaneSide::Local => tab.local.entries.get(real_index).map(|entry| entry.kind),
            PaneSide::Remote => tab.remote.entries.get(real_index).map(|entry| entry.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Action;
    use gpui_kit::base::input::NativeMenuItem;

    /// `(label, disabled)` per item; separators have no label.
    fn layout(menu: &NativeMenu) -> Vec<(Option<&str>, bool)> {
        menu.items
            .iter()
            .map(|item| match item {
                NativeMenuItem::Separator => (None, false),
                NativeMenuItem::Action {
                    label, disabled, ..
                } => (Some(label.as_ref()), *disabled),
            })
            .collect()
    }

    fn assert_action<A: Action + 'static>(menu: &NativeMenu, label: &str) {
        let item = menu
            .items
            .iter()
            .find(|item| matches!(item, NativeMenuItem::Action { label: item_label, .. } if item_label.as_ref() == label))
            .unwrap_or_else(|| panic!("menu must contain {label}"));
        let NativeMenuItem::Action { action, .. } = item else {
            panic!("{label} must be an action item");
        };
        assert!(
            action.as_any().is::<A>(),
            "{label} must dispatch {}",
            std::any::type_name::<A>()
        );
    }

    #[test]
    fn local_directory_menu_groups_open_transfer_edit_delete_and_path() {
        let menu = file_context_menu(
            PaneSide::Local,
            ContextMenuTarget::Entry(FileKind::Directory),
            true,
            true,
        );
        assert_eq!(
            layout(&menu),
            vec![
                (Some("Open"), false),
                (None, false),
                (Some("Upload"), false),
                (None, false),
                (Some("Rename…"), false),
                (Some("New Folder…"), false),
                (None, false),
                (Some("Delete…"), false),
                (None, false),
                (Some("Copy"), false),
                (Some("Paste"), false),
                (None, false),
                (Some("Copy Path"), false),
                (Some("Show in Finder"), false),
            ]
        );
        assert_action::<OpenSelectedEntry>(&menu, "Open");
        assert_action::<UploadSelection>(&menu, "Upload");
        assert_action::<RenameEntry>(&menu, "Rename…");
        assert_action::<NewFolder>(&menu, "New Folder…");
        assert_action::<DeleteSelection>(&menu, "Delete…");
        assert_action::<CopyPath>(&menu, "Copy Path");
        assert_action::<ShowInFinder>(&menu, "Show in Finder");
    }

    /// A local file opens in its default application, so the menu leads with
    /// Open exactly like a folder or link does.
    #[test]
    fn local_file_menu_leads_with_open() {
        let menu = file_context_menu(
            PaneSide::Local,
            ContextMenuTarget::Entry(FileKind::File),
            true,
            true,
        );
        assert_eq!(layout(&menu)[0], (Some("Open"), false));
        assert_action::<OpenSelectedEntry>(&menu, "Open");
        assert_action::<UploadSelection>(&menu, "Upload");
    }

    #[test]
    fn remote_file_menu_opens_for_editing_and_keeps_local_only_items_out() {
        let menu = file_context_menu(
            PaneSide::Remote,
            ContextMenuTarget::Entry(FileKind::File),
            true,
            true,
        );
        assert_eq!(
            layout(&menu),
            vec![
                (Some("Open for Editing"), false),
                (None, false),
                (Some("Download"), false),
                (None, false),
                (Some("Rename…"), false),
                (Some("New Folder…"), false),
                (None, false),
                (Some("Delete…"), false),
                (None, false),
                (Some("Copy"), false),
                (Some("Paste"), false),
                (None, false),
                (Some("Copy Path"), false),
            ]
        );
        assert_action::<OpenSelectedEntry>(&menu, "Open for Editing");
        assert_action::<DownloadSelection>(&menu, "Download");
        assert!(
            layout(&menu)
                .iter()
                .all(|(label, _)| *label != Some("Show in Finder")),
            "Show in Finder only reveals local paths"
        );
    }

    #[test]
    fn disconnected_remote_menu_disables_session_commands_but_keeps_copy_path() {
        let menu = file_context_menu(
            PaneSide::Remote,
            ContextMenuTarget::Entry(FileKind::Directory),
            false,
            true,
        );
        for (label, disabled) in layout(&menu) {
            match label {
                Some("Copy Path") => assert!(!disabled, "Copy Path works offline"),
                // Copying a selection needs no session; pasting it into the
                // server pane does.
                Some("Copy") => assert!(!disabled, "Copy works offline"),
                Some(_) => assert!(disabled, "{label:?} needs the session"),
                None => {}
            }
        }
    }

    #[test]
    fn local_upload_waits_for_a_remote_session() {
        let disconnected = file_context_menu(
            PaneSide::Local,
            ContextMenuTarget::Entry(FileKind::File),
            false,
            true,
        );
        // Open stays available offline: a local file needs no session.
        assert_eq!(layout(&disconnected)[0], (Some("Open"), false));
        assert_eq!(layout(&disconnected)[2], (Some("Upload"), true));
    }

    /// Paste is offered but disabled with nothing copied, and Copy is always
    /// available: it only needs the selection.
    #[test]
    fn paste_waits_for_a_copied_selection() {
        let menu = file_context_menu(
            PaneSide::Local,
            ContextMenuTarget::Entry(FileKind::File),
            true,
            false,
        );
        for (label, disabled) in layout(&menu) {
            match label {
                Some("Paste") => assert!(disabled, "nothing is copied yet"),
                Some("Copy") => assert!(!disabled, "Copy only needs the selection"),
                _ => {}
            }
        }
        assert_action::<CopyFiles>(&menu, "Copy");
        assert_action::<PasteFiles>(&menu, "Paste");
    }

    #[test]
    fn background_menu_offers_new_folder_then_refresh() {
        let menu = file_context_menu(PaneSide::Local, ContextMenuTarget::Background, false, true);
        assert_eq!(
            layout(&menu),
            vec![
                (Some("New Folder…"), false),
                (None, false),
                (Some("Refresh"), false),
            ]
        );
        assert_action::<NewFolder>(&menu, "New Folder…");
        assert_action::<RefreshPane>(&menu, "Refresh");

        let offline =
            file_context_menu(PaneSide::Remote, ContextMenuTarget::Background, false, true);
        assert_eq!(
            layout(&offline),
            vec![
                (Some("New Folder…"), true),
                (None, false),
                (Some("Refresh"), true),
            ],
            "a disconnected remote pane keeps the menu shape and disables both commands"
        );
    }
}
