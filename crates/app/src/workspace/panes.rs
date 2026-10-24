use std::path::Path;
use std::time::Instant;

use gpui_kit::{
    App, ClickEvent, ClipboardItem, Context, KeyDownEvent, ScrollStrategy, UniformListScrollHandle,
    Window, div,
};
use macsftp_core::{
    AppCommand, ConnectionState, DisconnectReason, EntryPath, FileKind, FileSortField, LocalPath,
    RemotePath, SortDirection, TabId,
};
use macsftp_platform::read_local_directory;

use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::helpers::connected_transfer_session;
use crate::workspace::visible_entries::{
    first_prefix_match, is_dotfile, visible_local_indices, visible_remote_indices,
};
use crate::workspace::*;
use macsftp_core::HistoryOp;

/// Visible-list page step for PageUp / PageDown (design §3).
pub const PAGE_SIZE: usize = 10;

/// The status line a local directory read announces while in flight; the
/// completed read retires exactly this line, nothing else.
const LOCAL_LOADING_STATUS: &str = "Loading files on your Mac…";

impl crate::workspace::Workspace {
    /// Flex weight of a pane in the shared content area.
    ///
    /// The weights are the split ratio and its complement, so the divider's
    /// position is the ratio of the total: dragging to 30% gives the local
    /// pane 0.3 and the remote pane 0.7.
    pub(crate) fn pane_split_weight(&self, side: PaneSide, content_width: gpui_kit::Pixels) -> f32 {
        let ratio = pane_split::clamp_split_ratio_for_width(self.pane_split_ratio, content_width);
        match side {
            PaneSide::Local => ratio,
            PaneSide::Remote => 1.0 - ratio,
        }
    }

    /// Where the divider sits in the window, in window coordinates.
    ///
    /// The drag anchors on the divider's own position rather than on the
    /// pointer, so grabbing it off-centre does not make it jump sideways.
    /// The panes fill the content area, which starts at the window's left
    /// edge, so the local pane's width is the distance from there to the
    /// divider.
    pub(crate) fn divider_position(
        &self,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui_kit::Pixels> {
        let content_width = window.viewport_size().width;
        if content_width <= gpui_kit::px(0.0) {
            return None;
        }
        let ratio = pane_split::clamp_split_ratio_for_width(self.pane_split_ratio, content_width);
        let usable = (content_width - pane_split::DIVIDER_WIDTH).max(gpui_kit::px(0.0));
        Some(usable * ratio + pane_split::DIVIDER_WIDTH / 2.0)
    }

    pub(crate) fn focus_pane(
        &mut self,
        side: PaneSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_side = side;
        window.focus(self.pane_focus(side), cx);
        cx.notify();
    }

    pub(crate) fn pane_filter(&self, side: PaneSide) -> &PaneFilter {
        match side {
            PaneSide::Local => &self.local.filter,
            PaneSide::Remote => &self.remote.filter,
        }
    }

    pub(crate) fn pane_filter_mut(&mut self, side: PaneSide) -> &mut PaneFilter {
        match side {
            PaneSide::Local => &mut self.local.filter,
            PaneSide::Remote => &mut self.remote.filter,
        }
    }

    pub(crate) fn pane_type_select(&self, side: PaneSide) -> &TypeSelect {
        match side {
            PaneSide::Local => &self.local.type_select,
            PaneSide::Remote => &self.remote.type_select,
        }
    }

    pub(crate) fn pane_type_select_mut(&mut self, side: PaneSide) -> &mut TypeSelect {
        match side {
            PaneSide::Local => &mut self.local.type_select,
            PaneSide::Remote => &mut self.remote.type_select,
        }
    }

    pub(crate) fn filter_query(&self, side: PaneSide) -> &str {
        self.pane_filter(side).query.as_str()
    }

    /// Indices of stored entries that currently appear in the file list.
    /// Index space used by selection, keyboard nav, and `uniform_list`.
    pub(crate) fn visible_indices(&self, side: PaneSide, cx: &App) -> Vec<usize> {
        let show_hidden = cx.resources().config.config().show_hidden_files;
        let query = self.filter_query(side);
        match (self.active_tab(), side) {
            (Some(tab), PaneSide::Local) => {
                visible_local_indices(&tab.local.entries, show_hidden, query)
            }
            (Some(tab), PaneSide::Remote) => {
                visible_remote_indices(&tab.remote.entries, show_hidden, query)
            }
            (None, _) => Vec::new(),
        }
    }

    /// Visible count after hidden-files filter only (ignores name query).
    pub(crate) fn count_after_hidden(&self, side: PaneSide, cx: &App) -> usize {
        let show_hidden = cx.resources().config.config().show_hidden_files;
        match (self.active_tab(), side) {
            (Some(tab), PaneSide::Local) => {
                if show_hidden {
                    tab.local.entries.len()
                } else {
                    tab.local
                        .entries
                        .iter()
                        .filter(|entry| !is_dotfile(&entry.name))
                        .count()
                }
            }
            (Some(tab), PaneSide::Remote) => {
                if show_hidden {
                    tab.remote.entries.len()
                } else {
                    tab.remote
                        .entries
                        .iter()
                        .filter(|entry| !is_dotfile(&entry.name))
                        .count()
                }
            }
            (None, _) => 0,
        }
    }

    pub(crate) fn entry_count(&self, side: PaneSide, cx: &App) -> usize {
        let query = self.filter_query(side);
        if query.is_empty() {
            return self.count_after_hidden(side, cx);
        }
        self.visible_indices(side, cx).len()
    }

    pub(crate) fn entry_path_at(
        &self,
        side: PaneSide,
        visible_index: usize,
        cx: &App,
    ) -> Option<EntryPath> {
        let real_index = *self.visible_indices(side, cx).get(visible_index)?;
        self.entry_path_at_stored_index(side, real_index)
    }

    fn entry_path_at_stored_index(&self, side: PaneSide, real_index: usize) -> Option<EntryPath> {
        let tab = self.active_tab()?;
        match side {
            PaneSide::Local => tab
                .local
                .entries
                .get(real_index)
                .map(|entry| EntryPath::Local(entry.path.clone())),
            PaneSide::Remote => tab
                .remote
                .entries
                .get(real_index)
                .map(|entry| EntryPath::Remote(entry.path.clone())),
        }
    }

    /// Visible-list index of the first selected path on `side`, if any.
    pub(crate) fn selected_index(&self, side: PaneSide, cx: &App) -> Option<usize> {
        let tab = self.active_tab()?;
        let selected = tab.selection.selected_paths.first()?;
        self.visible_index_of_path(side, selected, cx)
    }

    /// Visible-list index of `path` on `side`, if currently shown.
    pub(crate) fn visible_index_of_path(
        &self,
        side: PaneSide,
        path: &EntryPath,
        cx: &App,
    ) -> Option<usize> {
        let tab = self.active_tab()?;
        let visible = self.visible_indices(side, cx);
        match (side, path) {
            (PaneSide::Local, EntryPath::Local(local_path)) => {
                visible.iter().position(|&real_index| {
                    tab.local
                        .entries
                        .get(real_index)
                        .is_some_and(|entry| &entry.path == local_path)
                })
            }
            (PaneSide::Remote, EntryPath::Remote(remote_path)) => {
                visible.iter().position(|&real_index| {
                    tab.remote
                        .entries
                        .get(real_index)
                        .is_some_and(|entry| &entry.path == remote_path)
                })
            }
            _ => None,
        }
    }

    fn selection_visible_bounds(&self, side: PaneSide, cx: &App) -> Option<(usize, usize)> {
        let tab = self.active_tab()?;
        let selected: std::collections::HashSet<_> = tab.selection.selected_paths.iter().collect();
        let visible = self.visible_indices(side, cx);
        let mut selected_visible = visible
            .iter()
            .enumerate()
            .filter_map(|(index, &real_index)| {
                let path = self.entry_path_at_stored_index(side, real_index)?;
                selected.contains(&path).then_some(index)
            });
        let lo = selected_visible.next()?;
        let hi = selected_visible.next_back().unwrap_or(lo);
        Some((lo, hi))
    }

    /// Active end of a multi-select range (the selected visible index away from the anchor).
    fn selection_edge_visible_index(&self, side: PaneSide, cx: &App) -> Option<usize> {
        let (lo, hi) = self.selection_visible_bounds(side, cx)?;
        let anchor_index = self
            .selection_anchor
            .as_ref()
            .and_then(|path| self.visible_index_of_path(side, path, cx));
        // Keep the end opposite the anchor so shift+arrow continues from there.
        Some(match anchor_index {
            Some(anchor) if anchor == lo => hi,
            Some(anchor) if anchor == hi => lo,
            _ => hi,
        })
    }

    pub(crate) fn select_index(
        &mut self,
        side: PaneSide,
        visible_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.entry_path_at(side, visible_index, cx) else {
            return;
        };
        self.selection_anchor = Some(path.clone());
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.selected_paths = vec![path];
        }
        self.scroll_handle(side)
            .scroll_to_item(visible_index, ScrollStrategy::Top);
        cx.notify();
    }

    pub(crate) fn move_selection(&mut self, side: PaneSide, offset: isize, cx: &mut Context<Self>) {
        let entry_count = self.entry_count(side, cx);
        if entry_count == 0 {
            return;
        }
        let next_index = match self.selected_index(side, cx) {
            Some(current) => {
                (current as isize + offset).clamp(0, entry_count as isize - 1) as usize
            }
            None => 0,
        };
        self.select_index(side, next_index, cx);
    }

    pub(crate) fn extend_selection_to(
        &mut self,
        side: PaneSide,
        visible_index: usize,
        cx: &mut Context<Self>,
    ) {
        let visible = self.visible_indices(side, cx);
        if visible.is_empty() {
            return;
        }
        let end = visible_index.min(visible.len() - 1);
        let start = self
            .selection_anchor
            .as_ref()
            .and_then(|path| self.visible_index_of_path(side, path, cx))
            .or_else(|| {
                self.selection_visible_bounds(side, cx)
                    .map(|(first, _)| first)
            })
            .unwrap_or(if self.selection_anchor.is_none() {
                0
            } else {
                end
            });
        self.selection_anchor = self.entry_path_at_stored_index(side, visible[start]);
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let paths = visible[lo..=hi]
            .iter()
            .filter_map(|&real_index| self.entry_path_at_stored_index(side, real_index))
            .collect();
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.selected_paths = paths;
        }
        self.scroll_handle(side)
            .scroll_to_item(end, ScrollStrategy::Top);
        cx.notify();
    }

    pub(crate) fn move_selection_extend(
        &mut self,
        side: PaneSide,
        offset: isize,
        cx: &mut Context<Self>,
    ) {
        let entry_count = self.entry_count(side, cx);
        if entry_count == 0 {
            return;
        }
        let current = self.selected_index(side, cx).unwrap_or(0);
        let edge = self
            .selection_edge_visible_index(side, cx)
            .unwrap_or(current);
        let next = (edge as isize + offset).clamp(0, entry_count as isize - 1) as usize;
        self.extend_selection_to(side, next, cx);
    }

    pub(crate) fn select_all_visible(&mut self, side: PaneSide, cx: &mut Context<Self>) {
        let paths: Vec<_> = self
            .visible_indices(side, cx)
            .into_iter()
            .filter_map(|real_index| self.entry_path_at_stored_index(side, real_index))
            .collect();
        self.selection_anchor = paths.first().cloned();
        if let Some(tab) = self.active_tab_mut() {
            tab.selection.selected_paths = paths;
        }
        cx.notify();
    }

    pub(crate) fn page_selection(
        &mut self,
        side: PaneSide,
        direction: isize,
        cx: &mut Context<Self>,
    ) {
        let entry_count = self.entry_count(side, cx);
        if entry_count == 0 {
            return;
        }
        let current = self.selected_index(side, cx).unwrap_or(0);
        let next = (current as isize + direction * PAGE_SIZE as isize)
            .clamp(0, entry_count as isize - 1) as usize;
        self.select_index(side, next, cx);
    }

    pub(crate) fn open_entry_at(
        &mut self,
        side: PaneSide,
        visible_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let real_index = match self.visible_indices(side, cx).get(visible_index).copied() {
            Some(index) => index,
            None => return,
        };
        let Some(tab) = self.active_tab() else {
            return;
        };
        match side {
            PaneSide::Local => {
                let Some(entry) = tab.local.entries.get(real_index) else {
                    return;
                };
                match entry.kind {
                    FileKind::Directory => {
                        let path = entry.path.clone();
                        self.navigate_pane_local(path, HistoryOp::Push, window, cx);
                    }
                    FileKind::Symlink => {
                        let path = entry.path.clone();
                        self.open_local_symlink(path, window, cx);
                    }
                    // A local file is the user's own data on their own disk, so
                    // the Finder convention applies: opening it hands it to its
                    // default application. Remote files never take this path —
                    // they are edited in a temp copy instead (see below), because
                    // a download preserves the remote exec bit and opening it
                    // would run attacker-controlled content.
                    FileKind::File => {
                        let path = entry.path.clone();
                        open_local_path(cx, Path::new(path.as_str()));
                    }
                    // A socket, FIFO, device or an entry whose metadata could
                    // not be read has no application to open it with, and
                    // handing it to LaunchServices can block on a FIFO. Say so
                    // instead of trying.
                    FileKind::Other | FileKind::Unknown => {
                        self.show_status("This item can’t be opened.".into(), cx);
                    }
                }
            }
            PaneSide::Remote => {
                let Some(entry) = tab.remote.entries.get(real_index) else {
                    return;
                };
                match entry.kind {
                    FileKind::Directory => {
                        let path = entry.path.clone();
                        self.navigate_pane_remote(path, HistoryOp::Push, cx);
                    }
                    // The listing never stats links, so the actor decides at
                    // open time whether this navigates or edits the target.
                    FileKind::Symlink => {
                        let path = entry.path.clone();
                        self.resolve_remote_entry(path, cx);
                    }
                    _ => {
                        let (path, size, modified_at) =
                            (entry.path.clone(), entry.size, entry.modified_at);
                        self.begin_edit(path, size, modified_at, cx);
                    }
                }
            }
        }
    }

    /// Open a local symlink by following it. `read_dir`/`DirEntry::file_type`
    /// deliberately do not follow links, so the target kind is resolved here on
    /// the background executor; a dangling link or a non-directory target is
    /// reported in the status bar without moving the pane.
    ///
    /// The stat can take seconds on a network volume, so the result is bound to
    /// the tab and directory that requested it: a user who switched tabs or
    /// navigated elsewhere in the meantime must not have another listing
    /// replaced by this late answer.
    fn open_local_symlink(&mut self, path: LocalPath, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id;
        let parent = path.parent();
        let entry = EntryPath::Local(path.clone());
        self.begin_link_check(tab_id, entry.clone(), None, cx);
        let probe = path.clone();
        let stat_task = cx
            .background_executor()
            .spawn(async move { std::fs::metadata(probe.as_str()) });
        cx.spawn_in(window, async move |workspace, cx| {
            let result = stat_task.await;
            let _ = workspace.update_in(cx, |workspace, window, cx| {
                let still_applicable = workspace.state.tabs.active_tab_id == Some(tab_id)
                    && workspace
                        .state
                        .tabs
                        .find_tab(tab_id)
                        .and_then(|tab| tab.local.path.clone())
                        == parent;
                // Clear the pending status even when the answer is dropped, so
                // switching tabs or directories cannot leave "Checking the
                // link…" behind. A newer message is left alone.
                workspace.finish_link_check(tab_id, &entry, None, cx);
                if !still_applicable {
                    return;
                }
                match result {
                    Ok(metadata) if metadata.is_dir() => {
                        workspace.navigate_pane_local(path, HistoryOp::Push, window, cx);
                    }
                    Ok(_) => {
                        // The link points at a file: open it like any other
                        // local file instead of refusing it as a folder.
                        open_local_path(cx, Path::new(path.as_str()));
                    }
                    Err(error) => {
                        workspace.status_message =
                            Some(format!("The link could not be followed: {error}").into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Ask the live actor what a remote listing entry resolves to. The answer
    /// arrives as `RemoteEntryResolved`, scoped to the tab's session so a
    /// reconnect supersedes it.
    fn resolve_remote_entry(&mut self, path: RemotePath, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        let tab_id = tab.id;
        let Some((session_epoch, _)) = connected_transfer_session(tab) else {
            self.show_status("Connect to the server before opening this item.".into(), cx);
            cx.notify();
            return false;
        };
        if !self.send_command(
            AppCommand::ResolveRemoteEntry {
                tab_id,
                session_epoch,
                path: path.clone(),
            },
            cx,
        ) {
            return false;
        }
        self.begin_link_check(tab_id, EntryPath::Remote(path), Some(session_epoch), cx);
        true
    }

    pub(crate) fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        if let Some(index) = self.selected_index(side, cx) {
            self.open_entry_at(side, index, window, cx);
        }
    }

    pub(crate) fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        let next = !cx.resources().config.config().show_hidden_files;
        let result = cx.resources_mut().config.set_show_hidden_files(next);
        self.finish_config_change(result, "show_hidden_files", cx);
        cx.notify();
    }
    pub(crate) fn set_local_path(
        &mut self,
        path: LocalPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active_tab_mut() {
            let tab_id = tab.id;
            tab.local.path = Some(path.clone());
            tab.local.error = None;
            tab.selection.selected_paths.clear();
            self.request_local_directory(tab_id, path, cx);
        }
        self.local.scroll = UniformListScrollHandle::new();
        cx.notify();
    }

    pub(crate) fn request_local_directory(
        &mut self,
        tab_id: TabId,
        path: LocalPath,
        cx: &mut Context<Self>,
    ) {
        self.read_local_directory(tab_id, path, false, cx);
    }

    /// Re-read a local folder without disturbing what the user is looking at.
    ///
    /// Used after a transfer finishes: the listing must gain the new files but
    /// the scroll position and selection belong to the user, and the pane must
    /// not announce a navigation it did not make.
    fn reload_local_directory(&mut self, tab_id: TabId, path: LocalPath, cx: &mut Context<Self>) {
        self.read_local_directory(tab_id, path, true, cx);
    }

    fn read_local_directory(
        &mut self,
        tab_id: TabId,
        path: LocalPath,
        preserve_view: bool,
        cx: &mut Context<Self>,
    ) {
        // The read generation lives on the tab itself, so requesting for a
        // missing tab cannot create phantom side-table state.
        let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
            return;
        };
        let request_epoch = tab.local.begin_local_read();
        if !preserve_view && self.state.tabs.active_tab_id == Some(tab_id) {
            self.show_status(LOCAL_LOADING_STATUS.into(), cx);
        }
        let read_path = path.clone();
        let read_task = cx
            .background_executor()
            .spawn(async move { read_local_directory(&read_path) });
        cx.spawn(async move |workspace, cx| {
            let result = read_task.await;
            let _ = workspace.update(cx, |workspace, cx| {
                let is_active = workspace.state.tabs.active_tab_id == Some(tab_id);
                let Some(tab) = workspace.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                if !tab.local.accepts_local_result(request_epoch, &path) {
                    return;
                }
                match result {
                    Ok(mut entries) => {
                        macsftp_core::sort_entries(&mut entries, &tab.sort);
                        tab.local.entries = entries;
                        tab.local.error = None;
                        // Retire only the loading line this read put up. A
                        // message the user earned while the folder was loading
                        // (a path copy, an error) must survive the listing
                        // landing, and a background reload never announced
                        // itself, so it must not take down a navigation's line.
                        if is_active
                            && !preserve_view
                            && workspace
                                .status_message
                                .as_ref()
                                .is_some_and(|line| line.message == LOCAL_LOADING_STATUS)
                        {
                            workspace.status_message = None;
                        }
                    }
                    Err(error) => {
                        tab.local.entries.clear();
                        let failure = macsftp_platform::local_fs_error(
                            "The folder on your Mac couldn’t be loaded.",
                            &error,
                        );
                        if is_active {
                            workspace.status_message =
                                Some(format!("Cannot open {}: {error}", path.as_str()).into());
                        }
                        tab.local.error = Some(failure);
                        warn!(
                            error = %error,
                            "could not read local directory {}",
                            path.as_str()
                        );
                    }
                }
                if is_active && !preserve_view {
                    workspace.local.scroll = UniformListScrollHandle::new();
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Sort the active tab by `field` in `direction`: where the menu bar and
    /// the command palette land. The active sort shows in the file-list header
    /// (§12.3), because one app-wide menu bar cannot mark a per-tab state
    /// (§12.2).
    pub(crate) fn set_sort(
        &mut self,
        field: FileSortField,
        direction: SortDirection,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.sort.field == field && tab.sort.direction == direction {
            return;
        }
        tab.sort.field = field;
        tab.sort.direction = direction;
        let sort = tab.sort.clone();
        macsftp_core::sort_entries(&mut tab.local.entries, &sort);
        macsftp_core::sort_entries(&mut tab.remote.entries, &sort);
        cx.notify();
    }
    /// File-list header click: the same column toggles direction, a new
    /// column starts ascending.
    pub(crate) fn apply_sort_field(&mut self, field: FileSortField, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let direction = if tab.sort.field == field {
            match tab.sort.direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            }
        } else {
            SortDirection::Ascending
        };
        self.set_sort(field, direction, cx);
    }
    /// Request a remote listing. Returns `false` if the command could not be
    /// enqueued (caller must not advance navigation history in that case).
    pub(crate) fn request_remote_directory(
        &mut self,
        tab_id: TabId,
        path: RemotePath,
        cx: &mut Context<Self>,
    ) -> bool {
        self.read_remote_directory(tab_id, path, false, cx)
    }

    /// Remote counterpart of [`Self::reload_local_directory`]: re-read the
    /// folder a finished transfer wrote into while keeping the user's
    /// selection, scroll position and status text.
    fn reload_remote_directory(&mut self, tab_id: TabId, path: RemotePath, cx: &mut Context<Self>) {
        self.read_remote_directory(tab_id, path, true, cx);
    }

    fn read_remote_directory(
        &mut self,
        tab_id: TabId,
        path: RemotePath,
        preserve_view: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.send_command(
            AppCommand::ReadRemoteDir {
                tab_id,
                path: path.clone(),
            },
            cx,
        ) {
            return false;
        }
        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            // Keep the last listing visible while the actor reads the new
            // path. This supplies immediate feedback before the actor emits
            // RemoteDirLoading and prevents a refresh flash.
            tab.remote.path = Some(path);
            tab.remote.is_refreshing = true;
            tab.remote.error = None;
            if !preserve_view {
                tab.selection.selected_paths.clear();
            }
        }
        cx.notify();
        true
    }
    pub(crate) fn go_to_parent_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        match side {
            PaneSide::Local => {
                if let Some(parent) = tab.local.path.as_ref().and_then(LocalPath::parent) {
                    self.navigate_pane_local(parent, HistoryOp::Push, window, cx);
                }
            }
            PaneSide::Remote => {
                if let Some(parent) = tab.remote.path.as_ref().and_then(RemotePath::parent) {
                    self.navigate_pane_remote(parent, HistoryOp::Push, cx);
                }
            }
        }
    }

    /// Refresh reloads the current path without pushing history.
    pub(crate) fn refresh_focused_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        match side {
            PaneSide::Local => {
                if let Some(path) = tab.local.path.clone() {
                    self.set_local_path(path, window, cx);
                }
            }
            PaneSide::Remote => {
                if let Some(path) = tab.remote.path.clone() {
                    self.request_remote_directory(tab.id, path, cx);
                }
            }
        }
    }

    /// Re-read the folders a finished transfer wrote into.
    ///
    /// Only panes that are already showing one of those folders are re-read, so
    /// finishing an upload never yanks a pane back to where the files landed,
    /// and a tab showing something else costs nothing but a path comparison.
    pub(crate) fn reload_visible_directories(
        &mut self,
        local_directories: &[LocalPath],
        remote_directories: &[RemotePath],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every tab, not just the active one: a tab in the background can be
        // showing the folder that received the files, and returning to a stale
        // listing after a transfer finished is exactly the confusion this
        // reload exists to prevent. The comparison is two path reads per tab,
        // and a tab that does not match costs nothing else.
        let matches: Vec<(TabId, Option<LocalPath>, Option<RemotePath>, bool)> = self
            .state
            .tabs
            .tabs
            .iter()
            .map(|tab| {
                let local = tab
                    .local
                    .path
                    .clone()
                    .filter(|path| local_directories.contains(path));
                let remote = tab
                    .remote
                    .path
                    .clone()
                    .filter(|path| remote_directories.contains(path));
                let connected = matches!(tab.connection, ConnectionState::Connected { .. });
                (tab.id, local, remote, connected)
            })
            .collect();
        for (tab_id, local, remote, connected) in matches {
            if let Some(path) = local {
                self.reload_local_directory(tab_id, path, cx);
            }
            if let Some(path) = remote
                && connected
            {
                self.reload_remote_directory(tab_id, path, cx);
            }
        }
    }

    pub(crate) fn clear_filter(&mut self, side: PaneSide) {
        self.pane_filter_mut(side).clear();
        // Navigation, tab switches and Escape all funnel through here, so a
        // type-select session never survives the list it matched against.
        self.pane_type_select_mut(side).clear();
        self.text_inputs.remove(&if side == PaneSide::Local {
            TextInputTarget::LocalFilter
        } else {
            TextInputTarget::RemoteFilter
        });
    }

    pub(crate) fn clear_filters(&mut self) {
        self.clear_filter(PaneSide::Local);
        self.clear_filter(PaneSide::Remote);
    }

    /// `cmd-f`: show the filter bar and route keys into the filter input.
    pub(crate) fn open_filter_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_input_blocked() {
            return;
        }
        let side = self.focused_side;
        let filter = self.pane_filter_mut(side);
        filter.explicit_focus = true;
        filter.input.set_value(filter.query.clone());
        self.focus_pane(side, window, cx);
        self.focus_text_input(
            if side == PaneSide::Local {
                TextInputTarget::LocalFilter
            } else {
                TextInputTarget::RemoteFilter
            },
            window,
            cx,
        );
        cx.notify();
    }

    /// Surfaces that own keyboard input block type-to-filter / cmd-f.
    fn filter_input_blocked(&self) -> bool {
        self.go_to_path.open
            || self.connect_form_ui.form.is_some()
            || self.modal_inputs.delete_confirm.is_some()
            || self.modal_inputs.inline_edit.is_some()
            || self.active_host_key_prompt().is_some()
            || self.active_keyboard_interactive_prompt().is_some()
            || self.active_transfer_conflict_prompt().is_some()
            || self.modal_inputs.about_open
            || self.surface != WorkspaceSurface::Files
    }

    /// FilePane key path for Finder-style type-select and explicit filter
    /// editing.
    pub(crate) fn handle_type_select_key(
        &mut self,
        side: PaneSide,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filter_input_blocked() {
            return;
        }

        let keystroke = &event.keystroke;

        // Escape clears the explicit filter first; with no filter running it
        // ends the type-select session.
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            if self.pane_filter(side).is_active() {
                cx.stop_propagation();
                self.clear_filter(side);
                window.focus(self.pane_focus(side), cx);
                cx.notify();
            } else if !self.pane_type_select(side).prefix().is_empty() {
                cx.stop_propagation();
                self.pane_type_select_mut(side).clear();
                cx.notify();
            }
            return;
        }

        if self.pane_filter(side).explicit_focus {
            return;
        }

        // Modifier chords belong to menu commands, not to type-select.
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return;
        }

        if keystroke.key == "backspace" {
            if self.pane_type_select(side).prefix().is_empty() {
                return;
            }
            cx.stop_propagation();
            let prefix = self.pane_type_select_mut(side).pop().to_string();
            self.jump_to_type_select_match(side, &prefix, cx);
            return;
        }

        if let Some(key_char) = &keystroke.key_char
            && !key_char.chars().any(char::is_control)
        {
            cx.stop_propagation();
            self.extend_type_select(side, key_char, Instant::now(), cx);
        }
    }

    /// Extend the type-select prefix at `now` and move the selection to the
    /// first visible name that matches it; the list itself never narrows.
    pub(crate) fn extend_type_select(
        &mut self,
        side: PaneSide,
        typed: &str,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        self.pane_type_select_mut(side).extend(typed, now);
        let mut prefix = self.pane_type_select(side).prefix().to_string();
        let mut target = self.first_type_select_match(side, &prefix, cx);
        // A longer prefix that matches nothing falls back to the character
        // that was just typed, so one stray key never strands the session.
        if target.is_none() && prefix.chars().count() > 1 {
            self.pane_type_select_mut(side).restart(typed, now);
            prefix = typed.to_string();
            target = self.first_type_select_match(side, &prefix, cx);
        }
        match target {
            Some(index) => self.select_index(side, index, cx),
            None => cx.notify(),
        }
    }

    /// Re-run an existing prefix (backspace) without changing it.
    fn jump_to_type_select_match(&mut self, side: PaneSide, prefix: &str, cx: &mut Context<Self>) {
        match self.first_type_select_match(side, prefix, cx) {
            Some(index) => self.select_index(side, index, cx),
            None => cx.notify(),
        }
    }

    /// First visible index after the current selection (wrapping around)
    /// whose name starts with `prefix`. Scanning from just past the selection
    /// makes repeated letters cycle through matches, Finder-style.
    fn first_type_select_match(&self, side: PaneSide, prefix: &str, cx: &App) -> Option<usize> {
        let names = self.visible_names(side, cx);
        let from = self
            .selected_index(side, cx)
            .map(|index| index + 1)
            .unwrap_or(0);
        first_prefix_match(&names, from, prefix)
    }

    fn visible_names(&self, side: PaneSide, cx: &App) -> Vec<&str> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        self.visible_indices(side, cx)
            .into_iter()
            .filter_map(|real_index| match side {
                PaneSide::Local => tab
                    .local
                    .entries
                    .get(real_index)
                    .map(|entry| entry.name.as_str()),
                PaneSide::Remote => tab
                    .remote
                    .entries
                    .get(real_index)
                    .map(|entry| entry.name.as_str()),
            })
            .collect()
    }

    pub(crate) fn navigate_focused(
        &mut self,
        op: HistoryOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.focused_side {
            PaneSide::Local => {
                // Path is ignored for Back/Forward; required for Push/Replace.
                self.navigate_pane_local(LocalPath::new(String::new()), op, window, cx);
            }
            PaneSide::Remote => {
                self.navigate_pane_remote(RemotePath::new(String::new()), op, cx);
            }
        }
    }

    pub(crate) fn navigate_pane_local(
        &mut self,
        path: LocalPath,
        op: HistoryOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id;
        let current = tab.local.path.clone();

        let target = match op {
            HistoryOp::Push => {
                let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                tab.nav
                    .local
                    .push_navigating_from(current.as_ref().map(|p| p.as_str()), path.as_str());
                path
            }
            HistoryOp::Replace => path,
            HistoryOp::Back => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                let Some(target) = tab.nav.local.go_back(current_path.as_str()) else {
                    return;
                };
                LocalPath::new(target)
            }
            HistoryOp::Forward => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                let Some(target) = tab.nav.local.go_forward(current_path.as_str()) else {
                    return;
                };
                LocalPath::new(target)
            }
        };

        self.set_local_path(target, window, cx);
        self.clear_filter(PaneSide::Local);
        self.selection_anchor = None;
    }

    pub(crate) fn navigate_pane_remote(
        &mut self,
        path: RemotePath,
        op: HistoryOp,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id;
        let current = tab.remote.path.clone();

        // Peek history targets for Back/Forward without mutating stacks until
        // the ReadRemoteDir command is successfully enqueued (review fix).
        let target = match op {
            HistoryOp::Push | HistoryOp::Replace => path,
            HistoryOp::Back => {
                let Some(tab) = self.state.tabs.find_tab(tab_id) else {
                    return;
                };
                let Some(target) = tab.nav.remote.back.last() else {
                    return;
                };
                RemotePath::new(target.clone())
            }
            HistoryOp::Forward => {
                let Some(tab) = self.state.tabs.find_tab(tab_id) else {
                    return;
                };
                let Some(target) = tab.nav.remote.forward.last() else {
                    return;
                };
                RemotePath::new(target.clone())
            }
        };

        if !self.request_remote_directory(tab_id, target.clone(), cx) {
            return;
        }

        match op {
            HistoryOp::Push => {
                let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                tab.nav.remote.push_navigating_from(
                    current.as_ref().map(|path| path.as_str()),
                    target.as_str(),
                );
            }
            HistoryOp::Replace => {}
            HistoryOp::Back => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                debug_assert_eq!(
                    tab.nav.remote.go_back(current_path.as_str()).as_deref(),
                    Some(target.as_str()),
                    "peeked remote back target must still be current",
                );
            }
            HistoryOp::Forward => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let Some(tab) = self.state.tabs.find_tab_mut(tab_id) else {
                    return;
                };
                debug_assert_eq!(
                    tab.nav.remote.go_forward(current_path.as_str()).as_deref(),
                    Some(target.as_str()),
                    "peeked remote forward target must still be current",
                );
            }
        }

        self.clear_filter(PaneSide::Remote);
        self.selection_anchor = None;
    }

    pub(crate) fn pane_can_navigate_back(&self, side: PaneSide) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        match side {
            PaneSide::Local => tab.nav.local.can_back(),
            PaneSide::Remote => tab.nav.remote.can_back(),
        }
    }

    pub(crate) fn pane_can_navigate_forward(&self, side: PaneSide) -> bool {
        let Some(tab) = self.active_tab() else {
            return false;
        };
        match side {
            PaneSide::Local => tab.nav.local.can_forward(),
            PaneSide::Remote => tab.nav.remote.can_forward(),
        }
    }
    /// Copy Path copies the selected entries, matching Finder: the context
    /// menu selects the row under the pointer before it is presented, so the
    /// item's own path is what lands on the clipboard. Only an empty selection
    /// (toolbar button or shortcut with no rows picked) falls back to the pane's
    /// current folder. Several selected rows are copied one path per line.
    pub(crate) fn copy_focused_path(&mut self, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let mut paths: Vec<String> = self
            .selected_fs_entries(side)
            .iter()
            .map(|entry| entry.path.as_str().to_string())
            .collect();
        if paths.is_empty() {
            let folder = self.active_tab().and_then(|tab| {
                let path = match side {
                    PaneSide::Local => tab.local.path.as_ref().map(|path| path.as_str()),
                    PaneSide::Remote => tab.remote.path.as_ref().map(|path| path.as_str()),
                };
                path.map(str::to_string)
            });
            paths.extend(folder);
        }
        let Some(first_path) = paths.first().cloned() else {
            return;
        };
        let message = if paths.len() == 1 {
            format!("Path copied: {first_path}")
        } else {
            format!("{} paths copied", paths.len())
        };
        cx.write_to_clipboard(ClipboardItem::new_string(paths.join("\n")));
        self.show_status(message.into(), cx);
        cx.notify();
    }
    /// Copy the focused pane's selection into the file clipboard.
    ///
    /// Finder's Cmd-C for files: the selection is remembered, and pasting it in
    /// the other pane becomes the transfer that moves it there. The paths also
    /// go on the system clipboard as text, so the same keystroke still gives a
    /// terminal paste something useful.
    pub(crate) fn copy_files(&mut self, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let paths = self
            .selected_fs_entries(side)
            .into_iter()
            .map(|entry| match entry.path {
                macsftp_core::FsPath::Local(path) => EntryPath::Local(path),
                macsftp_core::FsPath::Remote(path) => EntryPath::Remote(path),
            })
            .collect::<Vec<_>>();
        if paths.is_empty() {
            self.show_status("Select files or folders to copy.".into(), cx);
            return;
        }
        let text = paths
            .iter()
            .map(|path| match path {
                EntryPath::Local(path) => path.as_str().to_string(),
                EntryPath::Remote(path) => path.as_str().to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let count = paths.len();
        self.file_clipboard.set(paths);
        self.show_status(
            if count == 1 {
                "Copied 1 item.".into()
            } else {
                format!("Copied {count} items.").into()
            },
            cx,
        );
    }

    /// Paste the file clipboard into the focused pane.
    ///
    /// The two panes are the two locations, so a paste that crosses them is the
    /// upload or download that moves the files. A paste into the pane the files
    /// already live in is reported instead of silently doing nothing: copying
    /// within one folder needs a server-side copy this client does not have.
    pub(crate) fn paste_files(&mut self, cx: &mut Context<Self>) {
        let side = self.focused_side;
        if self.file_clipboard.is_empty() {
            self.show_status("Nothing to paste.".into(), cx);
            return;
        }
        let copied = self.file_clipboard.paths().to_vec();
        match side {
            PaneSide::Remote => {
                let sources: Vec<LocalPath> = copied
                    .iter()
                    .filter_map(|path| match path {
                        EntryPath::Local(local) => Some(local.clone()),
                        EntryPath::Remote(_) => None,
                    })
                    .collect();
                if sources.is_empty() {
                    self.show_status(
                        "Those items are already in this folder. Copying within one folder is not supported yet."
                            .into(),
                        cx,
                    );
                    return;
                }
                self.begin_upload(sources, cx);
            }
            PaneSide::Local => {
                let sources: Vec<RemotePath> = copied
                    .iter()
                    .filter_map(|path| match path {
                        EntryPath::Remote(remote) => Some(remote.clone()),
                        EntryPath::Local(_) => None,
                    })
                    .collect();
                if sources.is_empty() {
                    self.show_status(
                        "Those items are already on your Mac. Copying within one folder is not supported yet."
                            .into(),
                        cx,
                    );
                    return;
                }
                self.begin_download(sources, cx);
            }
        }
    }

    pub(crate) fn toggle_selection_at(
        &mut self,
        side: PaneSide,
        visible_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.entry_path_at(side, visible_index, cx) else {
            return;
        };
        self.selection_anchor = Some(path.clone());
        if let Some(tab) = self.active_tab_mut() {
            if let Some(position) = tab.selection.selected_paths.iter().position(|p| p == &path) {
                tab.selection.selected_paths.remove(position);
            } else {
                tab.selection.selected_paths.push(path);
            }
        }
        cx.notify();
    }

    pub(crate) fn on_row_clicked(
        &mut self,
        side: PaneSide,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Right clicks arrive as mouse-up events and open the native context
        // menu (see `show_file_context_menu`); clicks here are left clicks.
        self.focus_pane(side, window, cx);
        if event.click_count() >= 2 {
            self.open_entry_at(side, index, window, cx);
        } else {
            let modifiers = event.modifiers();
            if modifiers.shift {
                self.extend_selection_to(side, index, cx);
            } else if modifiers.platform || modifiers.secondary() {
                self.toggle_selection_at(side, index, cx);
            } else {
                self.select_index(side, index, cx);
            }
        }
    }

    /// Cancel an in-flight connect/reconnect (or host-key wait) from the
    /// remote empty-state Cancel button.
    pub(crate) fn cancel_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let tab_id = tab.id;
        match &tab.connection {
            ConnectionState::AwaitingHostKey { request_id, .. } => {
                let request_id = *request_id;
                self.reject_host_key(request_id, window, cx);
                return;
            }
            ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. } => {}
            _ => return,
        }
        if !self.send_command(AppCommand::DisconnectTab { tab_id }, cx) {
            return;
        }
        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            tab.disconnect(DisconnectReason::UserRequested);
            tab.remote.entries.clear();
            tab.remote.path = None;
            tab.remote.is_refreshing = false;
            tab.remote.error = None;
        }
        self.drain_expired_modals();
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
}

impl crate::workspace::Workspace {
    /// Copy a recent connection's `user@host:port` — the string a terminal or
    /// another client needs — without opening the connection.
    pub(crate) fn copy_recent_endpoint(&mut self, recent_id: u64, cx: &mut Context<Self>) {
        let Some(endpoint) = cx
            .resources()
            .recents
            .entries()
            .iter()
            .find(|entry| entry.id == recent_id)
            .map(|entry| format!("{}@{}:{}", entry.username, entry.host, entry.port))
        else {
            return;
        };
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(endpoint.clone()));
        self.show_status(format!("Endpoint copied: {endpoint}").into(), cx);
    }

    /// Remove one recent connection. The row disappears immediately; a stale
    /// host the user mis-typed once does not follow them around.
    pub(crate) fn forget_recent_connection(&mut self, recent_id: u64, cx: &mut Context<Self>) {
        match cx.resources_mut().recents.forget(recent_id) {
            Ok(true) => {
                self.show_status("Removed from Recent Connections.".into(), cx);
                // The recents store is process-wide, so every window's Recent
                // Connections list is stale now, not just this one.
                cx.refresh_windows();
            }
            Ok(false) => {}
            Err(error) => {
                warn!(%error, "could not forget a recent connection");
                self.show_status(
                    "The recent connection couldn’t be removed. Try again.".into(),
                    cx,
                );
            }
        }
    }
}

/// The divider between the local and remote panes.
///
/// The box is a few points wide — a wider gutter would cost a narrow window
/// real space — and the drawn rule sits in the middle of it. The rest of the
/// box is transparent but still grabbable, which is what makes a one-pixel
/// line practical to hit.
pub(crate) fn render_pane_divider(theme: &macsftp_ui::Theme) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id("pane-divider")
        .debug_selector(|| "pane-divider".into())
        .flex_none()
        .w(pane_split::DIVIDER_WIDTH)
        .h_full()
        .relative()
        .cursor_col_resize()
        // The rule sits in the middle of the divider's box; the rest of the
        // box is transparent but still grabbable.
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(pane_split::DIVIDER_WIDTH / 2.0 - pane_split::DIVIDER_RULE_WIDTH / 2.0)
                .w(pane_split::DIVIDER_RULE_WIDTH)
                .bg(theme.colors.border),
        )
}
