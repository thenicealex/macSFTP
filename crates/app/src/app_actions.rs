use gpui_kit::{App, KeyBinding, actions};
use tracing::warn;

// Stable action names from the plan (§16) plus the file-list and tab
// navigation actions the workspace needs. Names are the command-palette
// contract; keystrokes may change later.
actions!(
    macsftp,
    [
        NewWindow,
        NewTab,
        CloseTab,
        ReconnectTab,
        RefreshPane,
        FocusLocalPane,
        FocusRemotePane,
        UploadSelection,
        DownloadSelection,
        OpenCommandPalette,
        ShowTransferDrawer,
        ActivateNextTab,
        ActivatePrevTab,
        TabSwitcherNext,
        TabSwitcherPrev,
        ConfirmTabSwitcher,
        SelectNextEntry,
        SelectPrevEntry,
        SelectNextEntryExtend,
        SelectPrevEntryExtend,
        PageDown,
        PageUp,
        SelectFirstEntry,
        SelectLastEntry,
        SelectAllEntries,
        OpenSelectedEntry,
        ParentDirectory,
        NavigateBack,
        NavigateForward,
        GoToPath,
        FilterPane,
        CopyPath,
        ShowInFinder,
        DeleteSelection,
        RenameEntry,
        NewFolder,
        ToggleHiddenFiles,
        CancelActiveModal,
        OpenSettings,
        OpenProfiles,
        ShowAbout,
        OpenLogFolder,
        CopyVersionInfo,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ZoomWindow,
        BringAllToFront,
        CloseWindow,
        Quit,
        // View ▸ Sort By sets field and direction explicitly; one app-wide
        // menu bar cannot mark a per-tab sort state, so the header is where the
        // active sort shows (§12.2, §12.3). The file-list header keeps its
        // click-to-toggle behavior.
        SortByNameAscending,
        SortByNameDescending,
        SortBySizeAscending,
        SortBySizeDescending,
        SortByDateModifiedAscending,
        SortByDateModifiedDescending,
    ]
);

// Scoped controls are not application commands exposed in the palette.
actions!(form, [FocusNextField, FocusPreviousField]);
pub(crate) const CONNECT_FORM_CONTEXT: &str = "ConnectForm";
pub(crate) const PROFILE_EDITOR_CONTEXT: &str = "ProfileEditor";

/// Register the default keymap and the app-level action handlers. The menu
/// bar (`app_menus`) and the command palette (`palette_commands`) mirror this
/// list; keep them in step.
pub fn init(cx: &mut App) {
    for context in [CONNECT_FORM_CONTEXT, PROFILE_EDITOR_CONTEXT] {
        for predicate in [context.to_string(), format!("{context} > Input")] {
            cx.bind_keys([
                KeyBinding::new("tab", FocusNextField, Some(&predicate)),
                KeyBinding::new("shift-tab", FocusPreviousField, Some(&predicate)),
            ]);
        }
    }
    cx.bind_keys(keybindings());

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    // "Bring All to Front": gpui has no `arrangeInFront`, and activating the
    // application is what brings every window of the app forward together.
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &CloseWindow, cx| {
        let Some(window) = cx.active_window() else {
            return;
        };
        if let Err(error) = window.update(cx, |_root, window, _cx| window.remove_window()) {
            warn!(error = ?error, "failed to close window");
        }
    });
}

/// Every keybinding the app registers. Kept separate from [`init`] so the
/// menu-bar tests can check the chord an item displays against the chord that
/// actually fires the action.
pub(crate) fn keybindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-t", NewTab, Some("Workspace")),
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("cmd-shift-]", ActivateNextTab, Some("Workspace")),
        KeyBinding::new("cmd-shift-[", ActivatePrevTab, Some("Workspace")),
        // MRU switcher only — creation-order keys above stay on ActivateNext/PrevTab.
        KeyBinding::new("ctrl-tab", TabSwitcherNext, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", TabSwitcherPrev, Some("Workspace")),
        KeyBinding::new("enter", ConfirmTabSwitcher, Some("TabSwitcher")),
        KeyBinding::new("escape", CancelActiveModal, Some("TabSwitcher")),
        KeyBinding::new("cmd-1", FocusLocalPane, Some("Workspace")),
        KeyBinding::new("cmd-2", FocusRemotePane, Some("Workspace")),
        KeyBinding::new("cmd-j", ShowTransferDrawer, Some("Workspace")),
        KeyBinding::new("cmd-r", RefreshPane, Some("Workspace")),
        KeyBinding::new("cmd-shift-r", ReconnectTab, Some("Workspace")),
        KeyBinding::new("cmd-u", UploadSelection, Some("Workspace")),
        KeyBinding::new("cmd-d", DownloadSelection, Some("Workspace")),
        KeyBinding::new("down", SelectNextEntry, Some("FilePane")),
        KeyBinding::new("up", SelectPrevEntry, Some("FilePane")),
        KeyBinding::new("shift-down", SelectNextEntryExtend, Some("FilePane")),
        KeyBinding::new("shift-up", SelectPrevEntryExtend, Some("FilePane")),
        KeyBinding::new("pagedown", PageDown, Some("FilePane")),
        KeyBinding::new("pageup", PageUp, Some("FilePane")),
        KeyBinding::new("home", SelectFirstEntry, Some("FilePane")),
        KeyBinding::new("end", SelectLastEntry, Some("FilePane")),
        KeyBinding::new("cmd-a", SelectAllEntries, Some("FilePane")),
        // ConnectionEntry in workspace/render.rs uses native button activation.
        KeyBinding::new(
            "enter",
            OpenSelectedEntry,
            Some("FilePane && !ConnectionEntry"),
        ),
        KeyBinding::new("cmd-up", ParentDirectory, Some("FilePane")),
        KeyBinding::new("cmd-[", NavigateBack, Some("Workspace")),
        KeyBinding::new("cmd-]", NavigateForward, Some("Workspace")),
        KeyBinding::new("cmd-shift-g", GoToPath, Some("Workspace")),
        KeyBinding::new("cmd-f", FilterPane, Some("FilePane")),
        KeyBinding::new("cmd-shift-c", CopyPath, Some("FilePane")),
        KeyBinding::new("cmd-backspace", DeleteSelection, Some("FilePane")),
        KeyBinding::new("f2", RenameEntry, Some("FilePane")),
        KeyBinding::new("cmd-shift-n", NewFolder, Some("FilePane")),
        KeyBinding::new("cmd-shift-.", ToggleHiddenFiles, Some("Workspace")),
        KeyBinding::new("cmd-shift-p", OpenCommandPalette, Some("Workspace")),
        KeyBinding::new("cmd-,", OpenSettings, Some("Workspace")),
        KeyBinding::new("cmd-m", MinimizeWindow, Some("Workspace")),
        KeyBinding::new("escape", CancelActiveModal, Some("HostKeyModal")),
        KeyBinding::new("escape", CancelActiveModal, Some("TransferConflictModal")),
        KeyBinding::new("escape", CancelActiveModal, Some("DeleteConfirmModal")),
        KeyBinding::new(
            "escape",
            CancelActiveModal,
            Some("ProfileDeleteConfirmModal"),
        ),
        KeyBinding::new("escape", CancelActiveModal, Some("GoToPath")),
        KeyBinding::new("escape", CancelActiveModal, Some("ConnectForm")),
        KeyBinding::new("escape", CancelActiveModal, Some("Settings")),
        KeyBinding::new("escape", CancelActiveModal, Some("About")),
        KeyBinding::new("escape", CancelActiveModal, Some("CommandPalette")),
        // App menu: the system chords for Hide/Hide Others. "Show All" has no
        // chord anywhere in macOS, so neither does its menu item.
        KeyBinding::new("cmd-h", HideApplication, None),
        KeyBinding::new("cmd-alt-h", HideOtherApplications, None),
        // File ▸ Close Window. ⌘W belongs to the tab (§12.7).
        KeyBinding::new("cmd-shift-w", CloseWindow, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]
}
