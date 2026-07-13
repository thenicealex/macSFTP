//! Explicit command palette registry (phase 4).
//! Not reflective of all `actions!` — only user-facing commands with stable ids.

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
        title: "New Tab",
        keywords: &["connection", "open", "connect"],
        keybinding: Some("⌘T"),
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "CloseTab",
        title: "Close Tab",
        keywords: &["close", "dismiss"],
        keybinding: Some("⌘W"),
        when: PaletteWhen::HasTabs,
    },
    PaletteCommand {
        id: "RefreshPane",
        title: "Refresh",
        keywords: &["reload", "update", "list"],
        keybinding: Some("⌘R"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "FocusLocalPane",
        title: "Focus Local Pane",
        keywords: &["left", "local", "side"],
        keybinding: Some("⌘1"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "FocusRemotePane",
        title: "Focus Remote Pane",
        keywords: &["right", "remote", "side", "server"],
        keybinding: Some("⌘2"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "UploadSelection",
        title: "Upload Selection",
        keywords: &["put", "send", "transfer"],
        keybinding: Some("⌘U"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "DownloadSelection",
        title: "Download Selection",
        keywords: &["get", "receive", "transfer"],
        keybinding: Some("⌘D"),
        when: PaletteWhen::ConnectedRemote,
    },
    PaletteCommand {
        id: "ShowTransferDrawer",
        title: "Toggle Transfers",
        keywords: &["queue", "jobs", "progress"],
        keybinding: Some("⌘J"),
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "OpenSettings",
        title: "Open Settings",
        keywords: &["preferences", "options", "config"],
        keybinding: Some("⌘,"),
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "OpenProfiles",
        title: "Manage Profiles",
        keywords: &["profile", "credentials", "settings"],
        keybinding: None,
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "ShowAbout",
        title: "About macSFTP",
        keywords: &["version", "info"],
        keybinding: None,
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "DeleteSelection",
        title: "Delete Selection",
        keywords: &["remove", "trash", "rm"],
        keybinding: Some("⌘⌫"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "RenameEntry",
        title: "Rename",
        keywords: &["mv", "move", "name"],
        keybinding: Some("F2"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "NewFolder",
        title: "New Folder",
        keywords: &["mkdir", "directory", "create"],
        keybinding: Some("⌘⇧N"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "FilterPane",
        title: "Filter Pane",
        keywords: &["search", "find", "narrow"],
        keybinding: Some("⌘F"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "GoToPath",
        title: "Go to Path",
        keywords: &["jump", "cd", "location"],
        keybinding: Some("⌘⇧G"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "NavigateBack",
        title: "Back",
        keywords: &["history", "previous"],
        keybinding: Some("⌘["),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "NavigateForward",
        title: "Forward",
        keywords: &["history", "next"],
        keybinding: Some("⌘]"),
        when: PaletteWhen::HasActiveTab,
    },
    // Toolbar parent-directory control (⌘↑). Display string is the source of
    // truth for path-bar tooltips via `labeled_shortcut`.
    PaletteCommand {
        id: "ParentDirectory",
        title: "Parent Directory",
        keywords: &["up", "cd..", "parent", "folder"],
        keybinding: Some("⌘↑"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "ToggleHiddenFiles",
        title: "Show Hidden Files",
        keywords: &["dotfiles", "hidden", "toggle"],
        keybinding: Some("⌘⇧."),
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "CopyPath",
        title: "Copy Path",
        keywords: &["clipboard", "path"],
        keybinding: Some("⌘⇧C"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "ReconnectTab",
        title: "Reconnect",
        keywords: &["retry", "connect", "session"],
        keybinding: Some("⌘⇧R"),
        when: PaletteWhen::HasActiveTab,
    },
    PaletteCommand {
        id: "OpenLogFolder",
        title: "Open Log Folder",
        keywords: &["logs", "diagnostics", "debug"],
        keybinding: None,
        when: PaletteWhen::Always,
    },
    PaletteCommand {
        id: "OpenCommandPalette",
        title: "Command Palette",
        keywords: &["commands", "actions", "search"],
        keybinding: Some("⌘⇧P"),
        when: PaletteWhen::Always,
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
        let hits = filter_palette_commands("new ta", &ctx);
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
            ("UploadSelection", Some("⌘U")),
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
            ("ReconnectTab", Some("⌘⇧R")),
            ("OpenLogFolder", None),
            ("OpenCommandPalette", Some("⌘⇧P")),
        ];
        for (id, keybinding) in required {
            let command = by_id.get(id).unwrap_or_else(|| panic!("missing id: {id}"));
            assert_eq!(command.keybinding, keybinding, "keybinding for {id}");
        }
    }

    #[test]
    fn labeled_shortcut_matches_registry_for_toolbar_tooltips() {
        // Path-bar / status-bar tooltips (Task 5 discoverability) must show the
        // same chords as palette rows. Keep this table in sync with render.rs.
        let cases = [
            ("New Tab", "NewTab", "New Tab (⌘T)"),
            ("Refresh", "RefreshPane", "Refresh (⌘R)"),
            (
                "Parent Directory",
                "ParentDirectory",
                "Parent Directory (⌘↑)",
            ),
            ("Back", "NavigateBack", "Back (⌘[)"),
            ("Forward", "NavigateForward", "Forward (⌘])"),
            ("New Folder", "NewFolder", "New Folder (⌘⇧N)"),
            (
                "Delete Selection",
                "DeleteSelection",
                "Delete Selection (⌘⌫)",
            ),
            (
                "Toggle Transfers",
                "ShowTransferDrawer",
                "Toggle Transfers (⌘J)",
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
            (
                "Upload Selection",
                "UploadSelection",
                "Upload Selection (⌘U)",
            ),
            (
                "Download Selection",
                "DownloadSelection",
                "Download Selection (⌘D)",
            ),
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
