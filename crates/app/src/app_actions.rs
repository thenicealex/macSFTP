use gpui::{App, KeyBinding, actions};

// Stable action names from the plan (§16) plus the file-list and tab
// navigation actions the M1 shell needs. Names are the command-palette
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
        CancelSelectedTransfer,
        OpenCommandPalette,
        ShowTransferDrawer,
        ActivateNextTab,
        ActivatePrevTab,
        SelectNextEntry,
        SelectPrevEntry,
        OpenSelectedEntry,
        ParentDirectory,
        CopyPath,
        DeleteSelection,
        RenameEntry,
        NewFolder,
        CancelActiveModal,
        OpenSettings,
        ShowAbout,
        OpenLogFolder,
        CopyVersionInfo,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ZoomWindow,
        Quit,
    ]
);

/// Register the default keymap. Bindings live in one place so tests and
/// `main` agree on them.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-t", NewTab, Some("Workspace")),
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("cmd-shift-]", ActivateNextTab, Some("Workspace")),
        KeyBinding::new("cmd-shift-[", ActivatePrevTab, Some("Workspace")),
        KeyBinding::new("cmd-1", FocusLocalPane, Some("Workspace")),
        KeyBinding::new("cmd-2", FocusRemotePane, Some("Workspace")),
        KeyBinding::new("cmd-j", ShowTransferDrawer, Some("Workspace")),
        KeyBinding::new("cmd-r", RefreshPane, Some("Workspace")),
        KeyBinding::new("cmd-shift-r", ReconnectTab, Some("Workspace")),
        KeyBinding::new("cmd-u", UploadSelection, Some("Workspace")),
        KeyBinding::new("cmd-d", DownloadSelection, Some("Workspace")),
        KeyBinding::new("down", SelectNextEntry, Some("FilePane")),
        KeyBinding::new("up", SelectPrevEntry, Some("FilePane")),
        KeyBinding::new("enter", OpenSelectedEntry, Some("FilePane")),
        KeyBinding::new("cmd-up", ParentDirectory, Some("FilePane")),
        KeyBinding::new("cmd-shift-c", CopyPath, Some("FilePane")),
        KeyBinding::new("cmd-backspace", DeleteSelection, Some("FilePane")),
        KeyBinding::new("f2", RenameEntry, Some("FilePane")),
        KeyBinding::new("cmd-shift-n", NewFolder, Some("FilePane")),
        KeyBinding::new("cmd-,", OpenSettings, Some("Workspace")),
        KeyBinding::new("cmd-m", MinimizeWindow, Some("Workspace")),
        KeyBinding::new("escape", CancelActiveModal, Some("HostKeyModal")),
        KeyBinding::new("escape", CancelActiveModal, Some("TransferConflictModal")),
        KeyBinding::new("escape", CancelActiveModal, Some("DeleteConfirmModal")),
        KeyBinding::new("escape", CancelActiveModal, Some("ConnectForm")),
        KeyBinding::new("escape", CancelActiveModal, Some("Settings")),
        KeyBinding::new("escape", CancelActiveModal, Some("About")),
        KeyBinding::new("cmd-q", Quit, None),
    ]);

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
}
