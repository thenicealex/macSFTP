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

/// Filter the registry by `when` and case-insensitive title/keyword substring.
/// Empty query returns every command that passes the context predicate.
pub fn filter_palette_commands(
    query: &str,
    ctx: &PaletteContext,
) -> Vec<&'static PaletteCommand> {
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
            ("ShowAbout", None),
            ("DeleteSelection", Some("⌘⌫")),
            ("RenameEntry", Some("F2")),
            ("NewFolder", Some("⌘⇧N")),
            ("FilterPane", Some("⌘F")),
            ("GoToPath", Some("⌘⇧G")),
            ("NavigateBack", Some("⌘[")),
            ("NavigateForward", Some("⌘]")),
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
}
