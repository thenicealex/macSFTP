use gpui::{
    App, ClickEvent, ClipboardItem, Context, KeyDownEvent, ScrollStrategy, UniformListScrollHandle,
    Window,
};
use macsftp_core::{
    AppCommand, ConnectionState, DisconnectReason, EntryPath, FileKind, FileSortField, LocalPath,
    RemotePath, SortDirection, TabId, TabState,
};
use macsftp_platform::read_local_directory;
use macsftp_ui::InputKeyResult;

use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::nav::HistoryOp;
use crate::workspace::visible_entries::{visible_local_indices, visible_remote_indices};
use crate::workspace::*;

/// Visible-list page step for PageUp / PageDown (design §3).
pub const PAGE_SIZE: usize = 10;

impl crate::workspace::Workspace {
    pub(crate) fn focus_pane(
        &mut self,
        side: PaneSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focused_side = side;
        window.focus(self.pane_focus(side));
        cx.notify();
    }

    pub(crate) fn pane_filter(&self, side: PaneSide) -> &PaneFilter {
        match side {
            PaneSide::Local => &self.local_filter,
            PaneSide::Remote => &self.remote_filter,
        }
    }

    pub(crate) fn pane_filter_mut(&mut self, side: PaneSide) -> &mut PaneFilter {
        match side {
            PaneSide::Local => &mut self.local_filter,
            PaneSide::Remote => &mut self.remote_filter,
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
                visible_local_indices(&tab.local.entries, show_hidden, "").len()
            }
            (Some(tab), PaneSide::Remote) => {
                visible_remote_indices(&tab.remote.entries, show_hidden, "").len()
            }
            (None, _) => 0,
        }
    }

    pub(crate) fn entry_count(&self, side: PaneSide, cx: &App) -> usize {
        self.visible_indices(side, cx).len()
    }

    pub(crate) fn entry_path_at(
        &self,
        side: PaneSide,
        visible_index: usize,
        cx: &App,
    ) -> Option<EntryPath> {
        let real_index = *self.visible_indices(side, cx).get(visible_index)?;
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

    /// Active end of a multi-select range (the selected visible index away from the anchor).
    fn selection_edge_visible_index(&self, side: PaneSide, cx: &App) -> Option<usize> {
        let tab = self.active_tab()?;
        let mut selected_visible = Vec::new();
        for path in &tab.selection.selected_paths {
            if let Some(index) = self.visible_index_of_path(side, path, cx) {
                selected_visible.push(index);
            }
        }
        if selected_visible.is_empty() {
            return None;
        }
        let lo = *selected_visible.iter().min()?;
        let hi = *selected_visible.iter().max()?;
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
        let anchor_path = self
            .selection_anchor
            .clone()
            .or_else(|| self.entry_path_at(side, self.selected_index(side, cx).unwrap_or(0), cx));
        let Some(anchor_path) = anchor_path else {
            return;
        };
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(anchor_path.clone());
        }
        let start = self
            .visible_index_of_path(side, &anchor_path, cx)
            .unwrap_or(end);
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let mut paths = Vec::new();
        for vi in lo..=hi {
            if let Some(path) = self.entry_path_at(side, vi, cx) {
                paths.push(path);
            }
        }
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
        let n = self.entry_count(side, cx);
        let mut paths = Vec::new();
        for i in 0..n {
            if let Some(path) = self.entry_path_at(side, i, cx) {
                paths.push(path);
            }
        }
        if let Some(first) = paths.first() {
            self.selection_anchor = Some(first.clone());
        }
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
                if entry.kind == FileKind::Directory {
                    let path = entry.path.clone();
                    self.navigate_pane_local(path, HistoryOp::Push, window, cx);
                }
            }
            PaneSide::Remote => {
                let Some(entry) = tab.remote.entries.get(real_index) else {
                    return;
                };
                if entry.kind == FileKind::Directory {
                    let path = entry.path.clone();
                    self.navigate_pane_remote(path, HistoryOp::Push, cx);
                } else {
                    let (path, size, modified_at) =
                        (entry.path.clone(), entry.size, entry.modified_at);
                    self.begin_edit(path, size, modified_at, cx);
                }
            }
        }
    }

    pub(crate) fn open_selected_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.focused_side;
        if let Some(index) = self.selected_index(side, cx) {
            self.open_entry_at(side, index, window, cx);
        }
    }

    pub(crate) fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        let next = !cx.resources().config.config().show_hidden_files;
        match cx.resources_mut().config.set_show_hidden_files(next) {
            Ok(()) => self.config_error = None,
            Err(error) => {
                warn!(error = %error, "could not save show_hidden_files");
                self.config_error =
                    Some("Could not write config.json. Check file permissions.".into());
            }
        }
        cx.notify();
    }
    pub(crate) fn set_local_path(
        &mut self,
        path: LocalPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = if let Some(tab) = self.active_tab_mut() {
            let message = Self::load_local_directory(&path, tab);
            tab.selection.selected_paths.clear();
            message
        } else {
            None
        };
        if let Some(message) = message {
            self.status_message = Some(message.into());
        }
        self.local_scroll = UniformListScrollHandle::new();
        cx.notify();
    }
    pub(crate) fn load_local_directory(path: &LocalPath, tab: &mut TabState) -> Option<String> {
        match read_local_directory(path) {
            Ok(mut entries) => {
                macsftp_core::sort_entries(&mut entries, &tab.sort);
                tab.local.entries = entries;
                tab.local.path = Some(path.clone());
                tab.local.error = None;
                None
            }
            Err(error) => {
                tab.local.entries = Vec::new();
                tab.local.path = Some(path.clone());
                warn!(error = %error, "could not read local directory {}", path.as_str());
                Some(format!("Cannot open {}: {error}", path.as_str()))
            }
        }
    }

    pub(crate) fn apply_sort_field(&mut self, field: FileSortField, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        if tab.sort.field == field {
            tab.sort.direction = match tab.sort.direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
        } else {
            tab.sort.field = field;
            tab.sort.direction = SortDirection::Ascending;
        }
        let sort = tab.sort.clone();
        macsftp_core::sort_entries(&mut tab.local.entries, &sort);
        macsftp_core::sort_entries(&mut tab.remote.entries, &sort);
        cx.notify();
    }
    /// Request a remote listing. Returns `false` if the command could not be
    /// enqueued (caller must not advance navigation history in that case).
    pub(crate) fn request_remote_directory(
        &mut self,
        tab_id: TabId,
        path: RemotePath,
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
            tab.selection.selected_paths.clear();
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

    pub(crate) fn clear_filter(&mut self, side: PaneSide) {
        self.pane_filter_mut(side).clear();
    }

    pub(crate) fn clear_filters(&mut self) {
        self.local_filter.clear();
        self.remote_filter.clear();
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
        cx.notify();
    }

    /// Surfaces that own keyboard input block type-to-filter / cmd-f.
    fn filter_input_blocked(&self) -> bool {
        self.go_to_path_open
            || self.connect_form.is_some()
            || self.delete_confirm.is_some()
            || self.inline_edit.is_some()
            || self.active_host_key_prompt().is_some()
            || self.active_transfer_conflict_prompt().is_some()
            || self.about_open
            || self.surface != WorkspaceSurface::Files
    }

    /// FilePane key path for type-to-filter and explicit filter editing.
    pub(crate) fn handle_filter_key(
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

        // Escape always clears an active filter for this pane.
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            if self.pane_filter(side).is_active() {
                cx.stop_propagation();
                self.clear_filter(side);
                window.focus(self.pane_focus(side));
                cx.notify();
            }
            return;
        }

        if self.pane_filter(side).explicit_focus {
            if keystroke.modifiers.platform && keystroke.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    let filter = self.pane_filter_mut(side);
                    filter.input.insert(&text);
                    filter.query = filter.input.value().to_string();
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            let filter = self.pane_filter_mut(side);
            if filter.input.handle_keystroke(keystroke) == InputKeyResult::Handled {
                filter.query = filter.input.value().to_string();
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        // Type-to-filter: printable chars (shift ok) append; backspace pops.
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return;
        }

        if keystroke.key == "backspace" {
            let filter = self.pane_filter_mut(side);
            if filter.query.is_empty() {
                return;
            }
            filter.query.pop();
            if filter.query.is_empty() {
                filter.clear();
            } else {
                filter.input.set_value(filter.query.clone());
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        if let Some(key_char) = &keystroke.key_char
            && !key_char.chars().any(char::is_control)
        {
            let filter = self.pane_filter_mut(side);
            filter.query.push_str(key_char);
            filter.input.set_value(filter.query.clone());
            filter.explicit_focus = false;
            cx.stop_propagation();
            cx.notify();
        }
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
        let Some(tab_id) = self.active_tab().map(|tab| tab.id) else {
            return;
        };
        let current = self.active_tab().and_then(|tab| tab.local.path.clone());

        let target = match op {
            HistoryOp::Push => {
                let nav = self.tab_nav.entry(tab_id).or_default();
                nav.local.push_navigating_from(
                    current.as_ref().map(|path| path.as_str()),
                    path.as_str(),
                );
                path
            }
            HistoryOp::Replace => path,
            HistoryOp::Back => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let nav = self.tab_nav.entry(tab_id).or_default();
                let Some(target) = nav.local.go_back(current_path.as_str()) else {
                    return;
                };
                LocalPath::new(target)
            }
            HistoryOp::Forward => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let nav = self.tab_nav.entry(tab_id).or_default();
                let Some(target) = nav.local.go_forward(current_path.as_str()) else {
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
        let Some(tab_id) = self.active_tab().map(|tab| tab.id) else {
            return;
        };
        let current = self.active_tab().and_then(|tab| tab.remote.path.clone());

        // Peek history targets for Back/Forward without mutating stacks until
        // the ReadRemoteDir command is successfully enqueued (review fix).
        let target = match op {
            HistoryOp::Push | HistoryOp::Replace => path,
            HistoryOp::Back => {
                let Some(nav) = self.tab_nav.get(&tab_id) else {
                    return;
                };
                let Some(target) = nav.remote.back.last() else {
                    return;
                };
                RemotePath::new(target.clone())
            }
            HistoryOp::Forward => {
                let Some(nav) = self.tab_nav.get(&tab_id) else {
                    return;
                };
                let Some(target) = nav.remote.forward.last() else {
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
                let nav = self.tab_nav.entry(tab_id).or_default();
                nav.remote.push_navigating_from(
                    current.as_ref().map(|path| path.as_str()),
                    target.as_str(),
                );
            }
            HistoryOp::Replace => {}
            HistoryOp::Back => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let nav = self.tab_nav.entry(tab_id).or_default();
                debug_assert_eq!(
                    nav.remote.go_back(current_path.as_str()).as_deref(),
                    Some(target.as_str()),
                    "peeked remote back target must still be current",
                );
            }
            HistoryOp::Forward => {
                let Some(current_path) = current.as_ref() else {
                    return;
                };
                let nav = self.tab_nav.entry(tab_id).or_default();
                debug_assert_eq!(
                    nav.remote.go_forward(current_path.as_str()).as_deref(),
                    Some(target.as_str()),
                    "peeked remote forward target must still be current",
                );
            }
        }

        self.clear_filter(PaneSide::Remote);
        self.selection_anchor = None;
    }

    pub(crate) fn pane_can_navigate_back(&self, side: PaneSide) -> bool {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id) else {
            return false;
        };
        let Some(nav) = self.tab_nav.get(&tab_id) else {
            return false;
        };
        match side {
            PaneSide::Local => nav.local.can_back(),
            PaneSide::Remote => nav.remote.can_back(),
        }
    }

    pub(crate) fn pane_can_navigate_forward(&self, side: PaneSide) -> bool {
        let Some(tab_id) = self.active_tab().map(|tab| tab.id) else {
            return false;
        };
        let Some(nav) = self.tab_nav.get(&tab_id) else {
            return false;
        };
        match side {
            PaneSide::Local => nav.local.can_forward(),
            PaneSide::Remote => nav.remote.can_forward(),
        }
    }
    pub(crate) fn copy_focused_path(&mut self, cx: &mut Context<Self>) {
        let side = self.focused_side;
        let Some(tab) = self.active_tab() else {
            return;
        };
        let path_text = match side {
            PaneSide::Local => tab
                .local
                .path
                .as_ref()
                .map(|path| path.as_str().to_string()),
            PaneSide::Remote => tab
                .remote
                .path
                .as_ref()
                .map(|path| path.as_str().to_string()),
        };
        if let Some(path_text) = path_text {
            cx.write_to_clipboard(ClipboardItem::new_string(path_text.clone()));
            self.status_message = Some(format!("Copied {path_text}").into());
            cx.notify();
        }
    }
    pub(crate) fn on_row_clicked(
        &mut self,
        side: PaneSide,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane(side, window, cx);
        if event.is_right_click() {
            self.open_context_menu(side, Some(index), cx);
            return;
        }
        if event.click_count() >= 2 {
            self.open_entry_at(side, index, window, cx);
        } else {
            self.select_index(side, index, cx);
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
        self.send_command(AppCommand::DisconnectTab { tab_id }, cx);
        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            tab.disconnect(DisconnectReason::UserRequested);
            tab.remote.entries.clear();
            tab.remote.path = None;
            tab.remote.is_refreshing = false;
            tab.remote.error = None;
        }
        self.state.drain_expired_modals();
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }
}
