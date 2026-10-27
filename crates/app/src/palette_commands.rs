//! Explicit command palette registry (phase 4).
//! Not reflective of all `actions!` — only user-facing commands with stable ids.
//! Each row carries the action it runs; the palette never calls a handler itself.

use gpui_kit::Action;

use crate::app_actions::{
    CancelAllTransfers, CloseTab, CopyPath, DeleteSelection, DownloadSelection, FilterPane,
    FocusLocalPane, FocusRemotePane, GoToPath, NavigateBack, NavigateForward, NewFolder, NewTab,
    OpenCommandPalette, OpenLogFolder, OpenProfiles, OpenSelectedEntry, OpenSettings,
    ParentDirectory, ReconnectTab, RefreshPane, RenameEntry, ShowAbout, ShowInFinder,
    ShowTransferDrawer, SortByDateModifiedAscending, SortByDateModifiedDescending,
    SortByNameAscending, SortByNameDescending, SortBySizeAscending, SortBySizeDescending,
    ToggleHiddenFiles, UploadEditedFile, UploadSelection,
};

/// Coarse availability predicate for a palette entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteWhen {
    Always,
    HasTabs,
    HasActiveTab,
    ConnectedRemote,
}

/// One command exposed in the command palette.
#[derive(Debug, Clone, Copy)]
pub struct PaletteCommand {
    pub id: &'static str,
    pub title: &'static str,
    pub keywords: &'static [&'static str],
    pub keybinding: Option<&'static str>,
    pub when: PaletteWhen,
    /// The action this row runs — the same one the menu bar and keymap use.
    pub action: fn() -> Box<dyn Action>,
}

/// View-side facts used to evaluate [`PaletteWhen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteContext {
    pub has_tabs: bool,
    pub has_active_tab: bool,
    pub remote_connected: bool,
}

static PALETTE_COMMANDS: &[PaletteCommand] = &[
    PaletteCommand {
        id: "NewTab",
        title: "New Connection",
        keywords: &["connection", "open", "connect", "tab", "new tab"],
        keybinding: Some("⌘T"),
        when: PaletteWhen::Always,
        action: || Box::new(NewTab),
    },
    PaletteCommand {
        id: "CloseTab",
        title: "Close Tab",
        keywords: &["close", "dismiss"],
        keybinding: Some("⌘W"),
        when: PaletteWhen::HasTabs,
        action: || Box::new(CloseTab),
    },
    PaletteCommand {
        id: "RefreshPane",
        title: "Refresh",
        keywords: &["reload", "update", "list"],
        keybinding: Some("⌘R"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(RefreshPane),
    },
    PaletteCommand {
        id: "FocusLocalPane",
        title: "Select Local Files",
        keywords: &["left", "local", "side", "focus local pane"],
        keybinding: Some("⌘1"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(FocusLocalPane),
    },
    PaletteCommand {
        id: "FocusRemotePane",
        title: "Select Server Files",
        keywords: &["right", "remote", "side", "server", "focus remote pane"],
        keybinding: Some("⌘2"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(FocusRemotePane),
    },
    PaletteCommand {
        id: "OpenSelectedEntry",
        title: "Open",
        keywords: &["enter", "open selected", "open file", "launch"],
        keybinding: Some("↩"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(OpenSelectedEntry),
    },
    PaletteCommand {
        id: "UploadSelection",
        title: "Upload",
        keywords: &["put", "send", "transfer", "upload selection"],
        keybinding: Some("⌘U"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(UploadSelection),
    },
    PaletteCommand {
        id: "UploadEditedFile",
        title: "Upload Edited File",
        keywords: &["upload", "edit", "save", "remote edit", "changes"],
        keybinding: Some("⌘⇧U"),
        when: PaletteWhen::ConnectedRemote,
        action: || Box::new(UploadEditedFile),
    },
    PaletteCommand {
        id: "DownloadSelection",
        title: "Download",
        keywords: &["get", "receive", "transfer", "download selection"],
        keybinding: Some("⌘D"),
        when: PaletteWhen::ConnectedRemote,
        action: || Box::new(DownloadSelection),
    },
    PaletteCommand {
        id: "CancelAllTransfers",
        title: "Cancel All Transfers",
        keywords: &["stop", "abort", "cancel", "transfers", "queue"],
        keybinding: Some("⌘."),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(CancelAllTransfers),
    },
    PaletteCommand {
        id: "ShowTransferDrawer",
        title: "Transfers",
        keywords: &[
            "queue",
            "jobs",
            "progress",
            "show transfers",
            "hide transfers",
            "toggle transfers",
        ],
        keybinding: Some("⌘J"),
        when: PaletteWhen::Always,
        action: || Box::new(ShowTransferDrawer),
    },
    PaletteCommand {
        id: "OpenSettings",
        title: "Settings…",
        keywords: &["preferences", "options", "config", "open settings"],
        keybinding: Some("⌘,"),
        when: PaletteWhen::Always,
        action: || Box::new(OpenSettings),
    },
    PaletteCommand {
        id: "OpenProfiles",
        title: "Manage Connections…",
        keywords: &["profile", "credentials", "settings", "manage profiles"],
        keybinding: None,
        when: PaletteWhen::Always,
        action: || Box::new(OpenProfiles),
    },
    PaletteCommand {
        id: "ShowAbout",
        title: "About macSFTP",
        keywords: &["version", "info"],
        keybinding: None,
        when: PaletteWhen::Always,
        action: || Box::new(ShowAbout),
    },
    PaletteCommand {
        id: "DeleteSelection",
        title: "Delete",
        keywords: &["remove", "trash", "rm", "delete selection"],
        keybinding: Some("⌘⌫"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(DeleteSelection),
    },
    PaletteCommand {
        id: "RenameEntry",
        title: "Rename",
        keywords: &["mv", "move", "name"],
        keybinding: Some("F2"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(RenameEntry),
    },
    PaletteCommand {
        id: "NewFolder",
        title: "New Folder",
        keywords: &["mkdir", "directory", "create"],
        keybinding: Some("⌘⇧N"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(NewFolder),
    },
    PaletteCommand {
        id: "FilterPane",
        title: "Filter Files",
        keywords: &["search", "find", "narrow", "filter pane"],
        keybinding: Some("⌘F"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(FilterPane),
    },
    PaletteCommand {
        id: "GoToPath",
        title: "Go to Folder",
        keywords: &["jump", "cd", "location", "go to path"],
        keybinding: Some("⌘⇧G"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(GoToPath),
    },
    // Sort sets field and direction explicitly, mirroring View ▸ Sort By: the
    // palette row and the menu item read the same way, and neither has to
    // show which sort is active (§12.2).
    PaletteCommand {
        id: "SortByNameAscending",
        title: "Sort by Name (A→Z)",
        keywords: &["sort", "order", "name", "ascending"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(SortByNameAscending),
    },
    PaletteCommand {
        id: "SortByNameDescending",
        title: "Sort by Name (Z→A)",
        keywords: &["sort", "order", "name", "descending", "reverse"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(SortByNameDescending),
    },
    PaletteCommand {
        id: "SortBySizeAscending",
        title: "Sort by Size (Smallest First)",
        keywords: &["sort", "order", "size", "ascending"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(SortBySizeAscending),
    },
    PaletteCommand {
        id: "SortBySizeDescending",
        title: "Sort by Size (Largest First)",
        keywords: &["sort", "order", "size", "descending", "reverse"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(SortBySizeDescending),
    },
    PaletteCommand {
        id: "SortByDateModifiedAscending",
        title: "Sort by Last Modified (Oldest First)",
        keywords: &["sort", "order", "modified", "date", "ascending"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(SortByDateModifiedAscending),
    },
    PaletteCommand {
        id: "SortByDateModifiedDescending",
        title: "Sort by Last Modified (Newest First)",
        keywords: &["sort", "order", "modified", "date", "descending", "reverse"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(SortByDateModifiedDescending),
    },
    PaletteCommand {
        id: "NavigateBack",
        title: "Back",
        keywords: &["history", "previous"],
        keybinding: Some("⌘["),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(NavigateBack),
    },
    PaletteCommand {
        id: "NavigateForward",
        title: "Forward",
        keywords: &["history", "next"],
        keybinding: Some("⌘]"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(NavigateForward),
    },
    // Toolbar parent-directory control (⌘↑). Display string is the source of
    // truth for path-bar tooltips via `labeled_shortcut`.
    PaletteCommand {
        id: "ParentDirectory",
        title: "Up One Level",
        keywords: &["up", "cd..", "parent", "folder", "parent directory"],
        keybinding: Some("⌘↑"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(ParentDirectory),
    },
    PaletteCommand {
        id: "ToggleHiddenFiles",
        title: "Show Hidden Files",
        keywords: &["dotfiles", "hidden", "toggle"],
        keybinding: Some("⌘⇧."),
        when: PaletteWhen::Always,
        action: || Box::new(ToggleHiddenFiles),
    },
    PaletteCommand {
        id: "CopyPath",
        title: "Copy Path",
        keywords: &["clipboard", "path"],
        keybinding: Some("⌘⇧C"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(CopyPath),
    },
    PaletteCommand {
        id: "ShowInFinder",
        title: "Show in Finder",
        keywords: &["reveal", "finder", "locate", "show in finder"],
        keybinding: None,
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(ShowInFinder),
    },
    PaletteCommand {
        id: "ReconnectTab",
        title: "Reconnect",
        keywords: &["retry", "connect", "session"],
        keybinding: Some("⌘⇧R"),
        when: PaletteWhen::HasActiveTab,
        action: || Box::new(ReconnectTab),
    },
    PaletteCommand {
        id: "OpenLogFolder",
        title: "Show Logs",
        keywords: &["logs", "diagnostics", "debug", "open log folder"],
        keybinding: None,
        when: PaletteWhen::Always,
        action: || Box::new(OpenLogFolder),
    },
    PaletteCommand {
        id: "OpenCommandPalette",
        title: "Find an Action",
        keywords: &["commands", "actions", "search", "command palette"],
        keybinding: Some("⌘⇧P"),
        when: PaletteWhen::Always,
        action: || Box::new(OpenCommandPalette),
    },
];

pub fn all_palette_commands() -> &'static [PaletteCommand] {
    PALETTE_COMMANDS
}

/// Display key chord for a registry id (e.g. `"RefreshPane"` → `Some("⌘R")`).
/// Tooltips should use this (or [`labeled_shortcut`]) so path-bar strings cannot
/// drift from palette rows. Actual key bindings live in `app_actions`; these
/// strings are display-only.
pub fn keybinding_for(command_id: &str) -> Option<&'static str> {
    all_palette_commands()
        .iter()
        .find(|command| command.id == command_id)
        .and_then(|command| command.keybinding)
}

/// `"Refresh"` + `"RefreshPane"` → `"Refresh (⌘R)"`. Falls back to `label` alone
/// when the id is missing or has no keybinding.
pub fn labeled_shortcut(label: &str, command_id: &str) -> String {
    match keybinding_for(command_id) {
        Some(key) => format!("{label} ({key})"),
        None => label.to_string(),
    }
}

/// Filter the registry by `when` and case-insensitive title/keyword substring.
/// Empty query returns every command that passes the context predicate.
pub fn filter_palette_commands(query: &str, ctx: &PaletteContext) -> Vec<&'static PaletteCommand> {
    let needle = query.trim().to_lowercase();
    all_palette_commands()
        .iter()
        .filter(|command| when_matches(command.when, ctx))
        .filter(|command| {
            if needle.is_empty() {
                return true;
            }
            title_or_keywords_match(command, &needle)
        })
        .collect()
}

fn when_matches(when: PaletteWhen, ctx: &PaletteContext) -> bool {
    match when {
        PaletteWhen::Always => true,
        PaletteWhen::HasTabs => ctx.has_tabs,
        PaletteWhen::HasActiveTab => ctx.has_active_tab,
        PaletteWhen::ConnectedRemote => ctx.remote_connected,
    }
}

fn title_or_keywords_match(command: &PaletteCommand, needle: &str) -> bool {
    if command.title.to_lowercase().contains(needle) {
        return true;
    }
    command
        .keywords
        .iter()
        .any(|keyword| keyword.to_lowercase().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_title_case_insensitive() {
        let ctx = PaletteContext {
            has_tabs: true,
            has_active_tab: true,
            remote_connected: false,
        };
        let hits = filter_palette_commands("new con", &ctx);
        assert!(hits.iter().any(|c| c.id == "NewTab"));
    }

    #[test]
    fn filter_hides_when_predicate_fails() {
        let ctx = PaletteContext {
            has_tabs: false,
            has_active_tab: false,
            remote_connected: false,
        };
        let hits = filter_palette_commands("download", &ctx);
        assert!(!hits.iter().any(|c| c.id == "DownloadSelection"));
    }

    #[test]
    fn empty_query_returns_only_when_matching() {
        let ctx = PaletteContext {
            has_tabs: true,
            has_active_tab: false,
            remote_connected: false,
        };
        let hits = filter_palette_commands("", &ctx);
        assert!(hits.iter().any(|c| c.id == "NewTab"));
        assert!(hits.iter().any(|c| c.id == "CloseTab"));
        assert!(!hits.iter().any(|c| c.id == "RefreshPane"));
        assert!(!hits.iter().any(|c| c.id == "DownloadSelection"));
    }

    #[test]
    fn filter_matches_keywords() {
        let ctx = PaletteContext {
            has_tabs: true,
            has_active_tab: true,
            remote_connected: true,
        };
        let hits = filter_palette_commands("mkdir", &ctx);
        assert!(hits.iter().any(|c| c.id == "NewFolder"));
        let profile_hits = filter_palette_commands("credentials", &ctx);
        assert!(profile_hits.iter().any(|c| c.id == "OpenProfiles"));
    }

    #[test]
    fn registry_includes_required_mvp_ids() {
        let by_id: std::collections::HashMap<&str, &PaletteCommand> = all_palette_commands()
            .iter()
            .map(|command| (command.id, command))
            .collect();
        let required = [
            ("NewTab", Some("⌘T")),
            ("CloseTab", Some("⌘W")),
            ("RefreshPane", Some("⌘R")),
            ("FocusLocalPane", Some("⌘1")),
            ("FocusRemotePane", Some("⌘2")),
            ("OpenSelectedEntry", Some("↩")),
            ("UploadSelection", Some("⌘U")),
            ("UploadEditedFile", Some("⌘⇧U")),
            ("DownloadSelection", Some("⌘D")),
            ("ShowTransferDrawer", Some("⌘J")),
            ("OpenSettings", Some("⌘,")),
            ("OpenProfiles", None),
            ("ShowAbout", None),
            ("DeleteSelection", Some("⌘⌫")),
            ("RenameEntry", Some("F2")),
            ("NewFolder", Some("⌘⇧N")),
            ("FilterPane", Some("⌘F")),
            ("GoToPath", Some("⌘⇧G")),
            ("NavigateBack", Some("⌘[")),
            ("NavigateForward", Some("⌘]")),
            ("ParentDirectory", Some("⌘↑")),
            ("ToggleHiddenFiles", Some("⌘⇧.")),
            ("CopyPath", Some("⌘⇧C")),
            ("ShowInFinder", None),
            ("ReconnectTab", Some("⌘⇧R")),
            ("OpenLogFolder", None),
            ("OpenCommandPalette", Some("⌘⇧P")),
            ("SortByNameAscending", None),
            ("SortByNameDescending", None),
            ("SortBySizeAscending", None),
            ("SortBySizeDescending", None),
            ("SortByDateModifiedAscending", None),
            ("SortByDateModifiedDescending", None),
        ];
        for (id, keybinding) in required {
            let command = by_id.get(id).unwrap_or_else(|| panic!("missing id: {id}"));
            assert_eq!(command.keybinding, keybinding, "keybinding for {id}");
        }
    }

    #[test]
    fn every_row_dispatches_its_registered_action() {
        for command in all_palette_commands() {
            assert_eq!(
                (command.action)().name(),
                format!("macsftp::{}", command.id),
                "row {} must run its own action",
                command.id
            );
        }
    }

    #[test]
    fn labeled_shortcut_matches_registry_for_toolbar_tooltips() {
        // Path-bar / status-bar tooltips (Task 5 discoverability) must show the
        // same chords as palette rows. Keep this table in sync with render.rs.
        let cases = [
            ("New Connection", "NewTab", "New Connection (⌘T)"),
            ("Refresh", "RefreshPane", "Refresh (⌘R)"),
            ("Up One Level", "ParentDirectory", "Up One Level (⌘↑)"),
            ("Back", "NavigateBack", "Back (⌘[)"),
            ("Forward", "NavigateForward", "Forward (⌘])"),
            ("New Folder", "NewFolder", "New Folder (⌘⇧N)"),
            ("Delete", "DeleteSelection", "Delete (⌘⌫)"),
            (
                "Show Transfers",
                "ShowTransferDrawer",
                "Show Transfers (⌘J)",
            ),
            (
                "Show Hidden Files",
                "ToggleHiddenFiles",
                "Show Hidden Files (⌘⇧.)",
            ),
            (
                "Hide Hidden Files",
                "ToggleHiddenFiles",
                "Hide Hidden Files (⌘⇧.)",
            ),
            ("Upload", "UploadSelection", "Upload (⌘U)"),
            ("Download", "DownloadSelection", "Download (⌘D)"),
            ("Copy Path", "CopyPath", "Copy Path (⌘⇧C)"),
            ("Reconnect", "ReconnectTab", "Reconnect (⌘⇧R)"),
        ];
        for (label, id, expected) in cases {
            assert_eq!(labeled_shortcut(label, id), expected, "tooltip for {id}");
            assert!(keybinding_for(id).is_some(), "key present for {id}");
        }
    }

    #[test]
    fn labeled_shortcut_falls_back_without_key() {
        assert_eq!(
            labeled_shortcut("About macSFTP", "ShowAbout"),
            "About macSFTP"
        );
        assert_eq!(labeled_shortcut("Unknown", "NotACommand"), "Unknown");
    }
}
