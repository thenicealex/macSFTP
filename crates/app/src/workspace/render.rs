use gpui::{
    AppContext, ClickEvent, Context, CursorStyle, DragMoveEvent, FontWeight, Hsla, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, SharedString, Styled, Window, div, prelude::*, px,
    uniform_list,
};
use macsftp_core::{
    AuthMethodKind, ConnectionState, EntryPath, LocalPath, RemotePath, TransferHistoryRecord,
    TransferHistoryStatus, TransferId, TransferJob, TransferState,
};
use macsftp_ui::{
    ActiveTheme, DragPreview, FileRowModel, IconName, InputState, TextFieldModel, connection_status,
    empty_state, file_row, file_table_header, format_size, format_timestamp, icon, icon_button,
    loading_state, section_header_static, tab, text_button, text_field, text_tooltip,
    transfer_history_detail, transfer_history_title, transfer_row, transfer_title,
};

use crate::palette_commands::labeled_shortcut;
use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::helpers::*;
use crate::workspace::nav::{HistoryOp, breadcrumb_display_indices, breadcrumb_segments};
use crate::workspace::profiles::{
    ProfileEditorField, SettingsSection, profile_list_label,
};
use crate::workspace::{PaneSide, WorkspaceSurface};
use macsftp_storage::AppearancePreference;

impl crate::workspace::Workspace {
    pub(crate) fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let active_tab_id = self.state.tabs.active_tab_id;

        let tabs = self.state.tabs.tabs.iter().map(|tab_state| {
            let tab_id = tab_state.id;
            let (status_color, status_label) = connection_status(&tab_state.connection, &theme);
            tab(
                ("tab", tab_id.0),
                tab_state.title.clone(),
                status_color,
                status_label,
            )
            .active(active_tab_id == Some(tab_id))
            .on_activate(cx.listener(move |workspace, _event, window, cx| {
                workspace.activate_tab(tab_id, window, cx);
            }))
            .on_close(cx.listener(move |workspace, _event, window, cx| {
                workspace.close_tab_by_id(tab_id, window, cx);
            }))
        });

        div()
            .flex()
            .flex_none()
            .items_center()
            .h(theme.sizes.tab_bar_height)
            .bg(theme.colors.surface)
            .border_b_1()
            .border_color(theme.colors.border)
            .child(
                div()
                    .id("tab-strip")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .child(div().px_1().child(
                icon_button(
                    "new-tab",
                    IconName::Plus,
                    labeled_shortcut("New Tab", "NewTab"),
                )
                .on_click(cx.listener(|workspace, _event, window, cx| {
                    workspace.open_new_tab(window, cx);
                })),
            ))
    }

    /// Elevated MRU list for ctrl-tab. Confirm with Enter; Esc cancels (no key-up required).
    pub(crate) fn render_tab_switcher(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.tab_switcher_open || self.tab_mru.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let selected = self.tab_switcher_index.min(self.tab_mru.len() - 1);

        let rows = self.tab_mru.iter().enumerate().filter_map(|(index, tab_id)| {
            let tab_state = self.state.tabs.find_tab(*tab_id)?;
            let (status_color, status_label) = connection_status(&tab_state.connection, &theme);
            let title: SharedString = tab_state.title.clone().into();
            let is_selected = index == selected;
            let background = if is_selected {
                theme.colors.element_selected
            } else {
                theme.colors.elevated_surface
            };
            let hover_background = theme.colors.element_hover;
            let tab_id = *tab_id;

            Some(
                div()
                    .id(("tab-switcher-row", tab_id.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(background)
                    .when(!is_selected, |row| {
                        row.hover(move |style| style.bg(hover_background))
                    })
                    .on_click(cx.listener(move |workspace, _event, window, cx| {
                        // Click selects that tab immediately (same as Enter on that row).
                        workspace.tab_switcher_index = index;
                        workspace.confirm_tab_switcher(window, cx);
                    }))
                    .child(
                        div()
                            .size(px(8.0))
                            .rounded_full()
                            .bg(status_color)
                            .flex_none(),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(13.0))
                            .text_color(theme.colors.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child(status_label),
                    ),
            )
        });

        Some(
            div()
                .id("tab-switcher-scrim")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                .child(
                    div()
                        .key_context("TabSwitcher")
                        .track_focus(&self.modal_focus)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .w(px(360.0))
                        .max_h(px(360.0))
                        .p_3()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .rounded_md()
                        .font_family(theme.fonts.ui_family.clone())
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child("Switch Tab · Enter confirm · Esc cancel"),
                        )
                        .child(
                            div()
                                .id("tab-switcher-list")
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .children(rows),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn render_pane(
        &self,
        side: PaneSide,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let pane_focused = self.pane_focus(side).contains_focused(window, cx);
        let tab_state = self.active_tab();
        let is_remote_refreshing =
            side == PaneSide::Remote && tab_state.is_some_and(|tab| tab.remote.is_refreshing);
        let show_hidden_files = cx.resources().config.config().show_hidden_files;
        let entry_count = self.entry_count(side, cx);
        let remote_error = (side == PaneSide::Remote)
            .then(|| tab_state.and_then(|tab| tab.remote.error.clone()))
            .flatten();
        let local_error = (side == PaneSide::Local)
            .then(|| tab_state.and_then(|tab| tab.local.error.clone()))
            .flatten();
        let inline_edit_active = self
            .inline_edit
            .as_ref()
            .is_some_and(|edit| edit.side == side);

        let (path_label, sort) = match (tab_state, side) {
            (Some(tab), PaneSide::Local) => (
                tab.local
                    .path
                    .as_ref()
                    .map(|path| path.as_str().to_string()),
                tab.sort.clone(),
            ),
            (Some(tab), PaneSide::Remote) => (
                tab.remote
                    .path
                    .as_ref()
                    .map(|path| path.as_str().to_string()),
                tab.sort.clone(),
            ),
            (None, _) => (None, Default::default()),
        };

        let (pane_name, back_id, forward_id, up_id, refresh_id, copy_id, list_container_id) =
            match side {
                PaneSide::Local => (
                    "Local",
                    "local-back",
                    "local-forward",
                    "local-up",
                    "local-refresh",
                    "local-copy-path",
                    "local-list-container",
                ),
                PaneSide::Remote => (
                    "Remote",
                    "remote-back",
                    "remote-forward",
                    "remote-up",
                    "remote-refresh",
                    "remote-copy-path",
                    "remote-list-container",
                ),
            };
        let can_navigate_back = self.pane_can_navigate_back(side);
        let can_navigate_forward = self.pane_can_navigate_forward(side);

        // Transfer affordances: enabled only when the tab is connected and
        // the relevant pane has a selection. This surfaces the previously
        // menu-only Upload/Download actions as visible, discoverable buttons.
        let connected = tab_state
            .is_some_and(|tab| matches!(tab.connection, ConnectionState::Connected { .. }));
        let local_selected = tab_state
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter(|path| matches!(path, EntryPath::Local(_)))
                    .count()
            })
            .unwrap_or(0);
        let remote_selected = tab_state
            .map(|tab| {
                tab.selection
                    .selected_paths
                    .iter()
                    .filter(|path| matches!(path, EntryPath::Remote(_)))
                    .count()
            })
            .unwrap_or(0);
        let can_upload = connected && local_selected > 0;
        let can_download = connected && remote_selected > 0;

        let path_bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(theme.sizes.path_bar_height)
            .px_2()
            .bg(theme.colors.surface)
            .border_b_1()
            .border_color(if pane_focused {
                theme.colors.border_focused
            } else {
                theme.colors.border
            })
            .child({
                let hover_background = theme.colors.element_hover;
                let active_background = theme.colors.element_active;
                let label_color = if can_navigate_back {
                    theme.colors.text_muted
                } else {
                    theme.colors.text_disabled
                };
                div()
                    .id(back_id)
                    .size(px(22.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .tooltip(text_tooltip(labeled_shortcut("Back", "NavigateBack")))
                    .when(can_navigate_back, |button| {
                        button
                            .hover(|style| style.bg(hover_background))
                            .active(|style| style.bg(active_background))
                            .on_click(cx.listener(move |workspace, _event, window, cx| {
                                workspace.focused_side = side;
                                workspace.navigate_focused(HistoryOp::Back, window, cx);
                            }))
                    })
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(label_color)
                            .child("◀"),
                    )
            })
            .child({
                let hover_background = theme.colors.element_hover;
                let active_background = theme.colors.element_active;
                let label_color = if can_navigate_forward {
                    theme.colors.text_muted
                } else {
                    theme.colors.text_disabled
                };
                div()
                    .id(forward_id)
                    .size(px(22.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .tooltip(text_tooltip(labeled_shortcut(
                        "Forward",
                        "NavigateForward",
                    )))
                    .when(can_navigate_forward, |button| {
                        button
                            .hover(|style| style.bg(hover_background))
                            .active(|style| style.bg(active_background))
                            .on_click(cx.listener(move |workspace, _event, window, cx| {
                                workspace.focused_side = side;
                                workspace.navigate_focused(HistoryOp::Forward, window, cx);
                            }))
                    })
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(label_color)
                            .child("▶"),
                    )
            })
            .child(
                icon_button(
                    up_id,
                    IconName::ArrowUp,
                    labeled_shortcut("Parent Directory", "ParentDirectory"),
                )
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.focused_side = side;
                    workspace.go_to_parent_directory(window, cx);
                })),
            )
            .child(
                icon_button(
                    refresh_id,
                    IconName::Refresh,
                    labeled_shortcut("Refresh", "RefreshPane"),
                )
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.focused_side = side;
                    workspace.refresh_focused_pane(window, cx);
                })),
            )
            .child({
                let text_color = if pane_focused {
                    theme.colors.text
                } else {
                    theme.colors.text_muted
                };
                let muted_color = theme.colors.text_muted;
                let hover_background = theme.colors.element_hover;
                let active_background = theme.colors.element_active;
                let side_tag: u32 = match side {
                    PaneSide::Local => 0,
                    PaneSide::Remote => 1,
                };

                let mut trail = div()
                    .id(("path-breadcrumb", side_tag as usize))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .overflow_x_hidden();

                match path_label.as_deref() {
                    None => {
                        trail = trail.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(text_color)
                                .truncate()
                                .child(pane_name.to_string()),
                        );
                    }
                    Some(path) => {
                        let segments = breadcrumb_segments(path);
                        let display = breadcrumb_display_indices(segments.len());
                        // Root label is already "/"; omit the separator after it so we
                        // never render "//Users".
                        let mut skip_next_separator = false;
                        let mut first = true;
                        for (display_i, index_opt) in display.into_iter().enumerate() {
                            if !first && !skip_next_separator {
                                trail = trail.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(11.0))
                                        .text_color(muted_color)
                                        .child("/"),
                                );
                            }
                            first = false;
                            skip_next_separator = false;

                            match index_opt {
                                None => {
                                    trail = trail.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(12.0))
                                            .text_color(muted_color)
                                            .child("…"),
                                    );
                                }
                                Some(seg_i) => {
                                    let (label, absolute) = &segments[seg_i];
                                    let is_root = label == "/";
                                    if is_root {
                                        skip_next_separator = true;
                                    }
                                    let label = label.clone();
                                    let absolute = absolute.clone();
                                    let element_id = (
                                        "breadcrumb-seg",
                                        side_tag as usize * 1000 + display_i,
                                    );
                                    trail = trail.child(
                                        div()
                                            .id(element_id)
                                            .flex_none()
                                            .px_1()
                                            .rounded_sm()
                                            .text_size(px(12.0))
                                            .text_color(text_color)
                                            .cursor_pointer()
                                            .hover(|style| style.bg(hover_background))
                                            .active(|style| style.bg(active_background))
                                            .tooltip(text_tooltip(absolute.clone()))
                                            .on_click(cx.listener(
                                                move |workspace, _event, window, cx| {
                                                    workspace.focused_side = side;
                                                    match side {
                                                        PaneSide::Local => {
                                                            workspace.navigate_pane_local(
                                                                LocalPath::new(absolute.clone()),
                                                                HistoryOp::Push,
                                                                window,
                                                                cx,
                                                            );
                                                        }
                                                        PaneSide::Remote => {
                                                            workspace.navigate_pane_remote(
                                                                RemotePath::new(absolute.clone()),
                                                                HistoryOp::Push,
                                                                cx,
                                                            );
                                                        }
                                                    }
                                                },
                                            ))
                                            .child(label),
                                    );
                                }
                            }
                        }
                    }
                }
                trail
            })
            .when(is_remote_refreshing && entry_count > 0, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child("Refreshing…"),
                )
            })
            .child(
                icon_button(
                    copy_id,
                    IconName::Copy,
                    labeled_shortcut("Copy Path", "CopyPath"),
                )
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    workspace.focused_side = side;
                    workspace.copy_focused_path(cx);
                })),
            )
            .child({
                // Labels differ by toggle state; chord always from registry.
                let hidden_tooltip = if show_hidden_files {
                    labeled_shortcut("Hide Hidden Files", "ToggleHiddenFiles")
                } else {
                    labeled_shortcut("Show Hidden Files", "ToggleHiddenFiles")
                };
                let mut button = icon_button(
                    if side == PaneSide::Local {
                        "local-toggle-hidden"
                    } else {
                        "remote-toggle-hidden"
                    },
                    IconName::File,
                    hidden_tooltip,
                )
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    workspace.toggle_hidden_files(cx);
                }));
                if show_hidden_files {
                    button = button.icon_color(theme.colors.accent);
                }
                button
            })
            .child(
                icon_button(
                    if side == PaneSide::Local {
                        "local-new-folder"
                    } else {
                        "remote-new-folder"
                    },
                    IconName::Plus,
                    labeled_shortcut("New Folder", "NewFolder"),
                )
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.focused_side = side;
                    workspace.begin_new_folder(window, cx);
                })),
            )
            .child(
                icon_button(
                    if side == PaneSide::Local {
                        "local-delete"
                    } else {
                        "remote-delete"
                    },
                    IconName::Close,
                    labeled_shortcut("Delete Selection", "DeleteSelection"),
                )
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.focused_side = side;
                    workspace.request_delete_selection(window, cx);
                })),
            )
            .when(side == PaneSide::Local, |bar| {
                bar.child(
                    icon_button(
                        "local-upload",
                        IconName::Upload,
                        labeled_shortcut("Upload Selection", "UploadSelection"),
                    )
                    .disabled(!can_upload)
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        workspace.focused_side = PaneSide::Local;
                        workspace.upload_selection(cx);
                    })),
                )
            })
            .when(side == PaneSide::Remote, |bar| {
                bar.child(
                    icon_button(
                        "remote-download",
                        IconName::Download,
                        labeled_shortcut("Download Selection", "DownloadSelection"),
                    )
                    .disabled(!can_download)
                    .on_click(cx.listener(
                        move |workspace, _event, _window, cx| {
                            workspace.focused_side = PaneSide::Remote;
                            workspace.download_selection(cx);
                        },
                    )),
                )
            });

        // Remote pane visualizes the connection lifecycle; the local
        // pane is always browsable. States per ui-ux-guidelines §6.3.
        let target_host = tab_state.map(|tab| tab.title.clone()).unwrap_or_default();
        let connect_button = |id: &'static str, label: &'static str| {
            text_button(id, label).on_click(cx.listener(|workspace, _event, window, cx| {
                workspace.request_connect(window, cx);
            }))
        };
        let edit_connection_button = |id: &'static str| {
            text_button(id, "Edit Connection…").on_click(cx.listener(
                |workspace, _event, window, cx| {
                    workspace.open_connect_form(window, cx);
                },
            ))
        };
        let retry_directory_button = |id: &'static str| {
            text_button(id, "Retry").on_click(cx.listener(|workspace, _event, window, cx| {
                let Some(tab) = workspace.active_tab() else {
                    return;
                };
                let tab_id = tab.id;
                if let Some(path) = tab.remote.path.clone() {
                    workspace.request_remote_directory(tab_id, path, cx);
                } else {
                    workspace.focused_side = PaneSide::Remote;
                    workspace.refresh_focused_pane(window, cx);
                }
            }))
        };
        let recent_rows: Vec<(u64, SharedString)> = if side == PaneSide::Remote {
            cx.resources()
                .recents
                .entries()
                .iter()
                .map(|entry| {
                    (
                        entry.id,
                        SharedString::from(format_recent_label(entry)),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let remote_empty_with_recents =
            |message: SharedString, actions: Vec<macsftp_ui::TextButton>, cx: &mut Context<Self>| {
                let theme = cx.theme();
                let rows = recent_rows.clone();
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(empty_state(message, actions, cx))
                    .when(!rows.is_empty(), |container| {
                        container
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.colors.text_muted)
                                    .font_family(theme.fonts.ui_family.clone())
                                    .child("Recent connections"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap_2()
                                    .children(rows.into_iter().map(|(id, label)| {
                                        text_button(
                                            SharedString::from(format!("recent-{id}")),
                                            label,
                                        )
                                        .on_click(cx.listener(move |workspace, _event, window, cx| {
                                            workspace.open_recent_connection(id, window, cx);
                                        }))
                                    })),
                            )
                    })
                    .into_any_element()
            };
        let connection_placeholder: Option<gpui::AnyElement> = if side == PaneSide::Remote {
            match tab_state.map(|tab| &tab.connection) {
                Some(ConnectionState::Empty) => Some(remote_empty_with_recents(
                    "Not connected".into(),
                    vec![connect_button("connect-remote", "Connect… (⌘⇧R)")],
                    cx,
                )),
                Some(ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. }) => {
                    Some(
                        empty_state(
                            format!("Connecting to {target_host}…"),
                            vec![text_button("cancel-connect", "Cancel").on_click(cx.listener(
                                |workspace, _event, window, cx| {
                                    workspace.cancel_connect(window, cx);
                                },
                            ))],
                            cx,
                        )
                        .into_any_element(),
                    )
                }
                Some(ConnectionState::AwaitingHostKey { .. }) => Some(
                    empty_state(
                        format!("Waiting for host key · {target_host}"),
                        vec![text_button("cancel-host-key", "Cancel").on_click(cx.listener(
                            |workspace, _event, window, cx| {
                                workspace.cancel_connect(window, cx);
                            },
                        ))],
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(ConnectionState::AwaitingCredentials { .. }) => {
                    Some(empty_state("Waiting for credentials…", vec![], cx).into_any_element())
                }
                Some(ConnectionState::Disconnected { .. }) => Some(remote_empty_with_recents(
                    "Disconnected".into(),
                    vec![
                        connect_button("reconnect-remote", "Reconnect (⌘⇧R)"),
                        edit_connection_button("edit-connection-disconnected"),
                    ],
                    cx,
                )),
                Some(ConnectionState::Failed { error }) => Some(
                    empty_state(
                        format!("{} — {}", error.title, error.message),
                        vec![
                            connect_button("retry-connect-remote", "Retry"),
                            edit_connection_button("edit-connection-failed"),
                        ],
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(ConnectionState::Connected { .. }) => remote_error.as_ref().map(|error| {
                    empty_state(
                        format!("{} — {}", error.title, error.message),
                        vec![retry_directory_button("retry-remote-directory")],
                        cx,
                    )
                    .into_any_element()
                }),
                None => None,
            }
        } else if let Some(error) = local_error.as_ref() {
            Some(
                empty_state(
                    format!("{} — {}", error.title, error.message),
                    vec![],
                    cx,
                )
                .into_any_element(),
            )
        } else {
            None
        };

        let list: gpui::AnyElement = if let Some(placeholder) = connection_placeholder {
            placeholder
        } else if entry_count == 0 && is_remote_refreshing {
            loading_state("Loading…", cx).into_any_element()
        } else if entry_count == 0 && !self.filter_query(side).is_empty() {
            empty_state("No matches", vec![], cx).into_any_element()
        } else if entry_count == 0 {
            empty_state("Empty directory", vec![], cx).into_any_element()
        } else {
            let workspace = cx.entity();
            uniform_list(
                list_container_id,
                entry_count,
                move |visible_range, _window, cx| {
                    let workspace_view = workspace.read(cx);
                    let Some(tab) = workspace_view.state.tabs.active_tab() else {
                        return Vec::new();
                    };
                    let selected_paths = &tab.selection.selected_paths;
                    let real_indices = workspace_view.visible_indices(side, cx);

                    visible_range
                        .filter_map(|visible_index| {
                            let real_index = *real_indices.get(visible_index)?;
                            let (model, drag_item) = match side {
                                PaneSide::Local => {
                                    let entry = tab.local.entries.get(real_index)?;
                                    (
                                        FileRowModel {
                                            name: entry.name.clone().into(),
                                            kind: entry.kind,
                                            is_hidden: entry.name.starts_with('.'),
                                            selected: selected_paths
                                                .contains(&EntryPath::Local(entry.path.clone())),
                                            size_label: format_size(entry.size),
                                            modified_label: format_timestamp(entry.modified_at),
                                        },
                                        EntryPath::Local(entry.path.clone()),
                                    )
                                }
                                PaneSide::Remote => {
                                    let entry = tab.remote.entries.get(real_index)?;
                                    (
                                        FileRowModel {
                                            name: entry.name.clone().into(),
                                            kind: entry.kind,
                                            is_hidden: entry.name.starts_with('.'),
                                            selected: selected_paths
                                                .contains(&EntryPath::Remote(entry.path.clone())),
                                            size_label: format_size(entry.size),
                                            modified_label: format_timestamp(entry.modified_at),
                                        },
                                        EntryPath::Remote(entry.path.clone()),
                                    )
                                }
                            };
                            let workspace = workspace.clone();
                            Some(
                                file_row(("file-row", visible_index), model, cx)
                                    .on_click(move |event, window, cx| {
                                        workspace.update(cx, |workspace, cx| {
                                            workspace.on_row_clicked(
                                                side,
                                                visible_index,
                                                event,
                                                window,
                                                cx,
                                            );
                                        });
                                    })
                                    .on_drag(
                                        drag_item,
                                        |value: &EntryPath, _position, _window, cx| {
                                            let label = match value {
                                                EntryPath::Local(p) => std::path::Path::new(p.as_str())
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().to_string()),
                                                EntryPath::Remote(p) => std::path::Path::new(p.as_str())
                                                    .file_name()
                                                    .map(|n| n.to_string_lossy().to_string()),
                                            }
                                            .unwrap_or_else(|| "file".to_string());
                                            cx.new(|_| DragPreview {
                                                label: label.into(),
                                            })
                                        },
                                    )
                                    .into_any_element(),
                            )
                        })
                        .collect()
                },
            )
            .track_scroll(self.scroll_handle(side).clone())
            .h_full()
            .w_full()
            .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .key_context("FilePane")
            .track_focus(self.pane_focus(side))
            .on_key_down(cx.listener(move |workspace, event, window, cx| {
                if workspace
                    .inline_edit
                    .as_ref()
                    .is_some_and(|edit| edit.side == side)
                {
                    workspace.handle_inline_edit_key(event, window, cx);
                    return;
                }
                workspace.handle_filter_key(side, event, window, cx);
            }))
            .when(side == PaneSide::Local, |pane| {
                pane.border_r_1().border_color(theme.colors.border)
            })
            .id(if side == PaneSide::Local {
                "local-file-pane"
            } else {
                "remote-file-pane"
            })
            .on_click(cx.listener(move |workspace, event: &ClickEvent, window, cx| {
                if event.is_right_click() {
                    workspace.focus_pane(side, window, cx);
                    workspace.open_context_menu(side, None, cx);
                }
            }))
            .on_drop(
                cx.listener(
                    move |workspace, path: &EntryPath, _window, cx| match (side, path) {
                        (PaneSide::Remote, EntryPath::Local(local)) => {
                            workspace.begin_upload(vec![local.clone()], cx)
                        }
                        (PaneSide::Local, EntryPath::Remote(remote)) => {
                            workspace.begin_download(vec![remote.clone()], cx)
                        }
                        _ => {}
                    },
                ),
            )
            .child(path_bar)
            .child(file_table_header(&sort, cx, {
                let entity = cx.entity();
                move |field, _window, cx| {
                    entity.update(cx, |workspace, cx| {
                        workspace.apply_sort_field(field, cx);
                    });
                }
            }))
            .when(self.pane_filter(side).is_active(), |pane| {
                let filter = self.pane_filter(side);
                let matched = entry_count;
                let total_after_hidden = self.count_after_hidden(side, cx);
                let explicit_focus = filter.explicit_focus;
                let query_display = filter.query.clone();
                let filter_field_id = if side == PaneSide::Local {
                    "local-filter-input"
                } else {
                    "remote-filter-input"
                };
                let clear_id = if side == PaneSide::Local {
                    "local-filter-clear"
                } else {
                    "remote-filter-clear"
                };
                pane.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(theme.colors.border_focused)
                        .bg(theme.colors.surface)
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child("Filter:"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .when(explicit_focus, |row| {
                                    row.child(text_field(
                                        filter_field_id,
                                        TextFieldModel {
                                            state: &self.pane_filter(side).input,
                                            placeholder: "Filter by name…",
                                            focused: true,
                                            masked: false,
                                        },
                                        cx,
                                    ))
                                })
                                .when(!explicit_focus, |row| {
                                    row.child(
                                        div()
                                            .text_size(px(12.0))
                                            .font_family(theme.fonts.mono_family.clone())
                                            .text_color(theme.colors.text)
                                            .truncate()
                                            .child(query_display),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(theme.colors.text_muted)
                                .child(format!("· {matched}/{total_after_hidden}")),
                        )
                        .child(
                            icon_button(clear_id, IconName::Close, "Clear Filter (Esc)").on_click(
                                cx.listener(move |workspace, _event, window, cx| {
                                    workspace.clear_filter(side);
                                    workspace.focus_pane(side, window, cx);
                                    cx.notify();
                                }),
                            ),
                        ),
                )
            })
            .when(inline_edit_active, |pane| {
                let edit = self.inline_edit.as_ref().expect("checked active");
                let label = match &edit.kind {
                    crate::workspace::file_ops::InlineEditKind::Rename { .. } => "Rename",
                    crate::workspace::file_ops::InlineEditKind::NewFolder { .. } => "New Folder",
                };
                let mut banner = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.colors.border_focused)
                    .bg(theme.colors.surface)
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child(label),
                    )
                    .child(div().flex_1().min_w_0().child(text_field(
                        if side == PaneSide::Local {
                            "local-inline-edit"
                        } else {
                            "remote-inline-edit"
                        },
                        TextFieldModel {
                            state: &edit.input,
                            placeholder: "name",
                            focused: true,
                            masked: false,
                        },
                        cx,
                    )))
                    .child(
                        text_button(
                            if side == PaneSide::Local {
                                "local-inline-ok"
                            } else {
                                "remote-inline-ok"
                            },
                            "OK",
                        )
                        .on_click(cx.listener(|workspace, _event, window, cx| {
                            workspace.submit_inline_edit(window, cx);
                        })),
                    )
                    .child(
                        text_button(
                            if side == PaneSide::Local {
                                "local-inline-cancel"
                            } else {
                                "remote-inline-cancel"
                            },
                            "Cancel",
                        )
                        .on_click(cx.listener(|workspace, _event, _window, cx| {
                            workspace.cancel_inline_edit(cx);
                        })),
                    );
                if let Some(error) = &edit.error {
                    banner = banner.child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.colors.error)
                            .child(error.clone()),
                    );
                }
                pane.child(banner)
            })
            .child(div().flex_1().min_h_0().child(list))
    }
    pub(crate) fn render_transfer_drawer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        use crate::workspace::drawer_height::{
            MIN_DRAWER_HEIGHT, RESIZE_HANDLE_HEIGHT, ResizeDragGhost, TransferDrawerResize,
        };

        // Reclamp after window shrink so stored height never exceeds max.
        self.reclamp_drawer_height(window.viewport_size().height);
        let height = self.drawer_height;

        let theme = cx.theme().clone();
        let workspace_entity = cx.entity();
        // Cloned (not borrowed) so the shared transfer store isn't held
        // borrowed across the `render_transfer_job(job, cx)` calls below.
        let jobs = cx.transfers().jobs.clone();
        let jobs = &jobs;

        let now = std::time::Instant::now();
        let running: Vec<(TransferId, u64, Option<u64>)> = jobs
            .iter()
            .filter_map(|job| match &job.state {
                TransferState::Running {
                    bytes_done,
                    bytes_total,
                    ..
                } => Some((job.id, *bytes_done, *bytes_total)),
                _ => None,
            })
            .collect();
        let agg = cx.rates().aggregate(&running, now);

        let active_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    TransferState::Running { .. }
                        | TransferState::Planning
                        | TransferState::Cancelling
                        | TransferState::WaitingForConflictDecision { .. }
                )
            })
            .collect();
        let queued_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Queued))
            .collect();
        let completed_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Completed | TransferState::Skipped))
            .collect();
        let failed_jobs: Vec<&TransferJob> = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Failed { .. }))
            .collect();

        // Drawer chrome: counts plus aggregate speed/ETA for running jobs.
        let agg_label = {
            let mut s = format!(
                "{} active · {} queued · {} done · {} failed",
                active_jobs.len(),
                queued_jobs.len(),
                completed_jobs.len(),
                failed_jobs.len()
            );
            if let Some(bps) = agg.speed_bps {
                s.push_str(&format!(
                    " · {}",
                    crate::rate_sampler::format_speed(Some(bps))
                ));
            }
            if let Some(eta) = agg.eta_secs {
                s.push_str(&format!(
                    " · ETA {}",
                    crate::rate_sampler::format_eta(Some(eta))
                ));
            }
            s
        };

        let mut drawer = div()
            .id("transfer-drawer")
            .flex()
            .flex_col()
            .flex_none()
            .h(height)
            .min_h(MIN_DRAWER_HEIGHT)
            .bg(theme.colors.surface)
            .border_t_1()
            .border_color(theme.colors.border);

        // Resize handle: drag to change height; double-click resets to default.
        // Never auto-closes the drawer when clamped to min height.
        let hover_background = theme.colors.element_hover;
        drawer = drawer.child(
            div()
                .id("transfer-drawer-resize-handle")
                .flex_none()
                .w_full()
                .h(RESIZE_HANDLE_HEIGHT)
                .cursor_row_resize()
                .bg(theme.colors.border)
                .hover(|style| style.bg(hover_background))
                .tooltip(text_tooltip("Drag to resize · Double-click to reset"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|workspace, event: &MouseDownEvent, window, cx| {
                        // Prefer mouse_down double-click so drag threshold
                        // does not steal the reset gesture.
                        if event.click_count >= 2 {
                            workspace
                                .reset_drawer_height(window.viewport_size().height);
                            workspace.drawer_resize = None;
                            cx.notify();
                        }
                    }),
                )
                .on_drag(
                    TransferDrawerResize {
                        start_height: self.drawer_height,
                        start_y: px(0.0),
                    },
                    move |_value, _cursor_offset, window, cx| {
                        let start_y = window.mouse_position().y;
                        // Use live drawer_height so double-click reset is not
                        // overwritten by a stale payload built at element time.
                        workspace_entity.update(cx, |workspace, cx| {
                            workspace.drawer_resize = Some(TransferDrawerResize {
                                start_height: workspace.drawer_height,
                                start_y,
                            });
                            cx.notify();
                        });
                        cx.new(|_| ResizeDragGhost)
                    },
                )
                .on_drag_move(cx.listener(
                    |workspace, event: &DragMoveEvent<TransferDrawerResize>, window, cx| {
                        let Some(start) = workspace.drawer_resize.clone() else {
                            return;
                        };
                        let current_y = event.event.position.y;
                        let new_height =
                            start.start_height + (start.start_y - current_y);
                        workspace.set_drawer_height(
                            new_height,
                            window.viewport_size().height,
                        );
                        cx.set_active_drag_cursor_style(CursorStyle::ResizeRow, window);
                        cx.notify();
                    },
                )),
        );

        drawer = drawer.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h(px(28.0))
                .px_2()
                .min_w_0()
                .text_size(px(11.0))
                .text_color(theme.colors.text_muted)
                .child(icon(IconName::Transfers, theme.colors.text_muted))
                .child(div().flex_none().child("Transfers"))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .child(agg_label),
                ),
        );

        let mut body = div()
            .id("transfer-drawer-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();

        if jobs.is_empty() && cx.resources().transfer_history.records().is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child("No transfers"),
            );
            return drawer.child(body);
        }

        for (label, section_jobs) in [("Active", active_jobs), ("Queued", queued_jobs)] {
            if !section_jobs.is_empty() {
                body = body
                    .child(section_header_static(label, section_jobs.len(), &theme))
                    .children(
                        section_jobs
                            .into_iter()
                            .map(|job| self.render_transfer_job(job, cx)),
                    );
            }
        }

        for (label, section_jobs, expanded, toggle_id) in [
            (
                "Completed",
                completed_jobs,
                self.completed_section_expanded,
                "toggle-completed",
            ),
            (
                "Failed",
                failed_jobs,
                self.failed_section_expanded,
                "toggle-failed",
            ),
        ] {
            if section_jobs.is_empty() {
                continue;
            }
            let is_completed_section = label == "Completed";
            body = body.child(
                div()
                    .id(toggle_id)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .h(px(24.0))
                    .px_2()
                    .text_size(px(11.0))
                    .text_color(theme.colors.text_muted)
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        if is_completed_section {
                            workspace.completed_section_expanded =
                                !workspace.completed_section_expanded;
                        } else {
                            workspace.failed_section_expanded = !workspace.failed_section_expanded;
                        }
                        cx.notify();
                    }))
                    .child(icon(
                        if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        theme.colors.text_muted,
                    ))
                    .child(format!("{label} ({})", section_jobs.len())),
            );
            if expanded {
                body = body.children(
                    section_jobs
                        .into_iter()
                        .map(|job| self.render_transfer_job(job, cx)),
                );
            }
        }

        // History: session-scoped store (cleared on quit / launch residual
        // wipe). Normally empty; kept for retry paths that inject records.
        let mut history_records: Vec<TransferHistoryRecord> =
            cx.resources().transfer_history.records().to_vec();
        history_records.sort_by_key(|record| std::cmp::Reverse(record.last_updated));
        if !history_records.is_empty() {
            body = body.child(section_header_static(
                "History",
                history_records.len(),
                &theme,
            ));
            for record in &history_records {
                body = body.child(self.render_history_record(record, cx));
            }
        }

        drawer.child(body)
    }
    pub(crate) fn render_history_record(
        &self,
        record: &TransferHistoryRecord,
        cx: &mut Context<Self>,
    ) -> macsftp_ui::TransferRow {
        let theme = cx.theme().clone();
        let title = transfer_history_title(record);
        let (state_label, state_color): (SharedString, Hsla) = match &record.status {
            TransferHistoryStatus::Unfinished => ("Unfinished".into(), theme.colors.warning),
            TransferHistoryStatus::Completed => ("Completed".into(), theme.colors.success),
            TransferHistoryStatus::Cancelled => ("Cancelled".into(), theme.colors.text_muted),
            TransferHistoryStatus::Failed { .. } => ("Failed".into(), theme.colors.error),
        };
        let connected = self
            .active_tab()
            .and_then(connected_transfer_session)
            .is_some();
        let mut row = transfer_row(
            ("history", record.id.0),
            record.direction,
            title,
            state_label,
            state_color,
        )
        .detail(transfer_history_detail(record));
        // Only offer retry when a live connection exists — the retry
        // rebuilds a transfer against the current connected tab.
        if record.is_retryable() && connected {
            let workspace = cx.entity();
            let record_id = record.id;
            row = row.on_retry(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.retry_history_transfer(record_id, cx);
                });
            });
        }
        row
    }
    pub(crate) fn render_transfer_job(&self, job: &TransferJob, cx: &mut Context<Self>) -> macsftp_ui::TransferRow {
        let theme = cx.theme().clone();
        let job_id = job.id;
        let title = transfer_title(job);

        let (state_label, state_color): (SharedString, Hsla) = match &job.state {
            TransferState::Queued => ("Queued".into(), theme.colors.text_muted),
            TransferState::Planning => ("Planning…".into(), theme.colors.info),
            TransferState::WaitingForConflictDecision { .. } => {
                ("Waiting for decision".into(), theme.colors.warning)
            }
            TransferState::Running {
                bytes_done,
                bytes_total,
                ..
            } => {
                let label = match bytes_total {
                    Some(total) if *total > 0 => {
                        format!("{}%", bytes_done * 100 / total)
                    }
                    _ => "Running".to_string(),
                };
                (label.into(), theme.colors.accent)
            }
            TransferState::Cancelling => ("Cancelling…".into(), theme.colors.warning),
            TransferState::Completed => ("Completed".into(), theme.colors.success),
            TransferState::Skipped => ("Skipped".into(), theme.colors.text_muted),
            TransferState::Failed { .. } => ("Failed".into(), theme.colors.error),
        };

        let detail: SharedString = match &job.state {
            TransferState::Planning => cx
                .transfers()
                .plans
                .iter()
                .find(|plan| plan.root_job_id == job.id)
                .map(|plan| {
                    format!(
                        "{} items · {}",
                        plan.planned_count,
                        format_size(plan.total_bytes)
                    )
                })
                .unwrap_or_else(|| "Planning…".to_string())
                .into(),
            TransferState::Running {
                bytes_done,
                bytes_total,
                ..
            } => {
                let snap = cx.rates().snapshot(
                    job_id,
                    *bytes_done,
                    *bytes_total,
                    std::time::Instant::now(),
                );
                crate::rate_sampler::format_running_detail(*bytes_done, *bytes_total, &snap).into()
            }
            TransferState::Completed if !job.warnings.is_empty() => job
                .warnings
                .last()
                .map(|warning| warning.message.clone().into())
                .unwrap_or_else(|| "Completed with warning".into()),
            TransferState::Failed { error, .. } if !job.warnings.is_empty() => {
                let warning = job
                    .warnings
                    .last()
                    .map(|warning| warning.message.as_str())
                    .unwrap_or("Transfer warning");
                format!("{} · {warning}", error.message).into()
            }
            TransferState::Failed { error, .. } => error.message.clone().into(),
            _ => "".into(),
        };

        let mut row = transfer_row(
            ("transfer", job_id.0),
            job.direction,
            title,
            state_label,
            state_color,
        )
        .detail(detail);

        row = match &job.state {
            TransferState::Running {
                bytes_done,
                bytes_total,
                ..
            } => {
                let fraction = bytes_total
                    .filter(|total| *total > 0)
                    .map(|total| *bytes_done as f32 / total as f32)
                    .unwrap_or(0.0);
                row.progress(fraction, theme.colors.accent)
            }
            TransferState::Completed => row.progress(1.0, theme.colors.success),
            _ => row,
        };

        let is_plan_root = cx
            .transfers()
            .plans
            .iter()
            .any(|plan| plan.root_job_id == job_id);
        let workspace = cx.entity();
        if matches!(job.state, TransferState::Planning)
            || (!is_plan_root
                && matches!(
                    job.state,
                    TransferState::Queued | TransferState::Running { .. }
                ))
        {
            let workspace = workspace.clone();
            row = row.on_cancel(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.cancel_transfer(job_id, cx);
                });
            });
        }
        if matches!(job.state, TransferState::Failed { .. }) {
            row = row.on_retry(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.retry_transfer(job_id, cx);
                });
            });
        }

        row
    }
    /// Count of selected paths on the focused pane (local or remote).
    pub(crate) fn focused_selection_count(&self) -> usize {
        let Some(tab) = self.active_tab() else {
            return 0;
        };
        tab.selection
            .selected_paths
            .iter()
            .filter(|path| match (self.focused_side, path) {
                (PaneSide::Local, EntryPath::Local(_)) => true,
                (PaneSide::Remote, EntryPath::Remote(_)) => true,
                _ => false,
            })
            .count()
    }

    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();

        let (status_color, status_text) = match self.active_tab() {
            Some(tab) => {
                let (color, label) = connection_status(&tab.connection, &theme);
                (color, format!("{label} · {}", tab.title))
            }
            None => (theme.colors.text_disabled, "No connection".to_string()),
        };

        let selected_count = self.focused_selection_count();

        let transfers = cx.transfers();
        let jobs = &transfers.jobs;
        let active_count = jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    TransferState::Running { .. }
                        | TransferState::Planning
                        | TransferState::Cancelling
                        | TransferState::WaitingForConflictDecision { .. }
                )
            })
            .count();
        let failed_count = jobs
            .iter()
            .filter(|job| matches!(job.state, TransferState::Failed { .. }))
            .count();

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(theme.sizes.status_bar_height)
            .px_2()
            .bg(theme.colors.surface)
            .border_t_1()
            .border_color(theme.colors.border)
            .text_size(px(11.0))
            .text_color(theme.colors.text_muted)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .size(px(7.0))
                            .flex_none()
                            .rounded_full()
                            .bg(status_color),
                    )
                    .child(div().min_w_0().truncate().child(status_text))
                    .when(selected_count > 0, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .child(format!("{selected_count} selected")),
                        )
                    })
                    .children(self.status_message.clone().map(|message| {
                        div()
                            .min_w_0()
                            .truncate()
                            .child(format!("— {message}"))
                    })),
            )
            .child(
                div()
                    .id("status-transfers")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .rounded_sm()
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .tooltip(text_tooltip(labeled_shortcut(
                        "Toggle Transfers",
                        "ShowTransferDrawer",
                    )))
                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                        workspace.drawer_open = !workspace.drawer_open;
                        cx.notify();
                    }))
                    .child(icon(
                        IconName::Transfers,
                        if failed_count > 0 {
                            theme.colors.error
                        } else if active_count > 0 {
                            theme.colors.accent
                        } else {
                            theme.colors.text_muted
                        },
                    ))
                    .child(div().child(format!("{active_count} active")))
                    .when(failed_count > 0, |row| {
                        row.child(
                            div()
                                .text_color(theme.colors.error)
                                .child(format!("· {failed_count} failed")),
                        )
                    }),
            )
    }
    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let selected_section = self.settings_section;
        let selected_appearance = cx.resources().config.config().appearance;
        let appearance_button = |id: &'static str,
                                 label: &'static str,
                                 appearance: AppearancePreference,
                                 cx: &mut Context<Self>| {
            let selected = selected_appearance == appearance;
            div()
                .id(id)
                .flex_1()
                .px_3()
                .py_2()
                .text_center()
                .text_size(px(12.0))
                .rounded_sm()
                .border_1()
                .border_color(if selected {
                    theme.colors.accent
                } else {
                    theme.colors.border
                })
                .bg(if selected {
                    theme.colors.element_selected
                } else {
                    theme.colors.background
                })
                .text_color(if selected {
                    theme.colors.accent
                } else {
                    theme.colors.text
                })
                .hover(|style| style.bg(theme.colors.element_hover))
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.set_appearance(appearance, window, cx);
                }))
                .child(label)
        };

        let sidebar_item = |id: &'static str,
                            label: &'static str,
                            section: SettingsSection,
                            cx: &mut Context<Self>| {
            let selected = selected_section == section;
            div()
                .id(id)
                .px_2()
                .py_2()
                .rounded_sm()
                .bg(if selected {
                    theme.colors.element_selected
                } else {
                    theme.colors.surface
                })
                .text_size(px(12.0))
                .text_color(if selected {
                    theme.colors.text
                } else {
                    theme.colors.text_muted
                })
                .hover(|style| style.bg(theme.colors.element_hover))
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    workspace.set_settings_section(section, cx);
                }))
                .child(label)
        };

        let body = match self.settings_section {
            SettingsSection::General => div()
                .flex_1()
                .min_w_0()
                .p_6()
                .child(
                    div()
                        .max_w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap_5()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(16.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Appearance"),
                                )
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(theme.colors.text_muted)
                                        .child(
                                            "Choose how macSFTP follows the macOS appearance.",
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(appearance_button(
                                    "appearance-system",
                                    "System",
                                    AppearancePreference::System,
                                    cx,
                                ))
                                .child(appearance_button(
                                    "appearance-light",
                                    "Light",
                                    AppearancePreference::Light,
                                    cx,
                                ))
                                .child(appearance_button(
                                    "appearance-dark",
                                    "Dark",
                                    AppearancePreference::Dark,
                                    cx,
                                )),
                        )
                        .children(self.config_error.clone().map(|error| {
                            div()
                                .p_3()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.colors.error)
                                .text_size(px(12.0))
                                .text_color(theme.colors.error)
                                .child(error)
                        }))
                        .child(div().h(px(1.0)).bg(theme.colors.border))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Diagnostics"),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            text_button("open-log-folder", "Open Log Folder")
                                                .on_click(cx.listener(
                                                    |workspace, _event, _window, cx| {
                                                        workspace.open_log_folder(cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            text_button("copy-version", "Copy Version Info")
                                                .on_click(cx.listener(
                                                    |workspace, _event, _window, cx| {
                                                        workspace.copy_version_info(cx);
                                                    },
                                                )),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
            SettingsSection::Profiles => self.render_settings_profiles(cx),
        };

        div()
            .key_context("Settings")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(theme.colors.background)
            .child(
                div()
                    .h(theme.sizes.tab_bar_height)
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .bg(theme.colors.surface)
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Settings"),
                    )
                    .child(text_button("settings-done", "Done").on_click(cx.listener(
                        |workspace, _event, window, cx| {
                            workspace.surface = WorkspaceSurface::Files;
                            workspace.focus_pane(workspace.focused_side, window, cx);
                            cx.notify();
                        },
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(180.0))
                            .flex_none()
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .bg(theme.colors.surface)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(sidebar_item(
                                "settings-section-general",
                                "General",
                                SettingsSection::General,
                                cx,
                            ))
                            .child(sidebar_item(
                                "settings-section-profiles",
                                "Profiles",
                                SettingsSection::Profiles,
                                cx,
                            )),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    /// Settings → Profiles: list on the left, editor form on the right.
    pub(crate) fn render_settings_profiles(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let profiles = cx.resources().profiles.profiles().to_vec();
        let total_count = profiles.len();
        let filtered = self.filtered_profiles(&profiles);
        let filtered_count = filtered.len();
        let filter_active = !self.profile_filter.value().trim().is_empty();
        let selected_id = self.selected_profile_id;
        let editing_new = self
            .profile_editor
            .as_ref()
            .is_some_and(|editor| editor.is_new);
        let filter_focused = self.profile_filter_focused;

        // Materialize rows before the editor borrow of `cx` (listeners capture).
        let list_rows: Vec<_> = filtered
            .into_iter()
            .map(|profile| {
                let profile_id = profile.id;
                let selected = selected_id == Some(profile_id) && !editing_new;
                let label = profile_list_label(profile);
                div()
                    .id(("settings-profile-row", profile_id.0))
                    .px_2()
                    .py_2()
                    .rounded_sm()
                    .bg(if selected {
                        theme.colors.element_selected
                    } else {
                        theme.colors.background
                    })
                    .text_size(px(12.0))
                    .text_color(theme.colors.text)
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .on_click(cx.listener(move |workspace, _event, _window, cx| {
                        workspace.select_profile_in_settings(profile_id, cx);
                    }))
                    .child(label)
            })
            .collect();

        let list_body: gpui::AnyElement = if total_count == 0 {
            empty_state(
                "No saved profiles",
                vec![
                    text_button("settings-empty-new-profile", "New Profile").on_click(
                        cx.listener(|workspace, _event, _window, cx| {
                            workspace.start_new_profile(cx);
                        }),
                    ),
                ],
                cx,
            )
            .into_any_element()
        } else if filtered_count == 0 && filter_active {
            empty_state("No matches", vec![], cx).into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .min_h_0()
                .children(list_rows)
                .into_any_element()
        };

        let detail = if self.profile_editor.is_some() {
            self.render_profile_editor(cx)
        } else if total_count == 0 {
            div()
                .text_size(px(13.0))
                .text_color(theme.colors.text_muted)
                .child("Create a profile to get started")
                .into_any_element()
        } else {
            div()
                .text_size(px(13.0))
                .text_color(theme.colors.text_muted)
                .child("Select a profile or create a new one")
                .into_any_element()
        };

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                div()
                    .w(px(220.0))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .border_r_1()
                    .border_color(theme.colors.border)
                    .track_focus(&self.workspace_focus)
                    .on_key_down(cx.listener(Self::handle_profile_filter_key))
                    .child(
                        text_button("settings-new-profile", "New Profile").on_click(cx.listener(
                            |workspace, _event, _window, cx| {
                                workspace.start_new_profile(cx);
                            },
                        )),
                    )
                    .child(
                        div()
                            .id("settings-profile-filter")
                            .on_click(cx.listener(|workspace, _event, window, cx| {
                                workspace.focus_profile_filter(window, cx);
                            }))
                            .child(text_field(
                                "settings-profile-filter-input",
                                TextFieldModel {
                                    state: &self.profile_filter,
                                    placeholder: "Filter profiles…",
                                    focused: filter_focused,
                                    masked: false,
                                },
                                cx,
                            )),
                    )
                    .child(list_body),
            )
            .child(div().flex_1().min_w_0().p_6().child(detail))
            .into_any_element()
    }

    pub(crate) fn render_profile_editor(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(editor) = self.profile_editor.as_ref() else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();

        let field_row = |label: &'static str,
                         field: ProfileEditorField,
                         state: &InputState,
                         placeholder: &'static str,
                         masked: bool,
                         focused: ProfileEditorField,
                         cx: &mut Context<Self>| {
            div()
                .id(("profile-editor-field", field as usize))
                .flex()
                .items_center()
                .gap_2()
                .on_click(cx.listener(move |workspace, _event, window, cx| {
                    workspace.profile_filter_focused = false;
                    if let Some(editor) = workspace.profile_editor.as_mut() {
                        editor.focused_field = field;
                    }
                    window.focus(&workspace.modal_focus);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(120.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(text_field(
                    ("profile-editor-input", field as usize),
                    TextFieldModel {
                        state,
                        placeholder,
                        focused: focused == field,
                        masked,
                    },
                    cx,
                )))
        };

        let auth_toggle = |label: &'static str,
                           method: AuthMethodKind,
                           id: &'static str,
                           active: AuthMethodKind,
                           cx: &mut Context<Self>| {
            text_button(id, label)
                .primary(active == method)
                .on_click(cx.listener(move |workspace, _event, _window, cx| {
                    if let Some(editor) = workspace.profile_editor.as_mut() {
                        editor.set_auth_method(method);
                        cx.notify();
                    }
                }))
        };

        let focused = editor.focused_field;
        let auth_method = editor.auth_method;
        let title = if editor.is_new {
            "New Profile"
        } else {
            "Edit Profile"
        };

        let mut form = div()
            .id("profile-editor")
            .key_context("ProfileEditor")
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(Self::handle_profile_editor_key))
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .max_w(px(560.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.colors.text)
                    .child(title),
            );

        if let Some(error) = &editor.error {
            form = form.child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme.colors.error)
                    .child(error.clone()),
            );
        }

        form = form
            .child(field_row(
                "Name",
                ProfileEditorField::Name,
                &editor.name,
                "optional",
                false,
                focused,
                cx,
            ))
            .child(field_row(
                "Host",
                ProfileEditorField::Host,
                &editor.host,
                "example.com",
                false,
                focused,
                cx,
            ))
            .child(field_row(
                "Port",
                ProfileEditorField::Port,
                &editor.port,
                "22",
                false,
                focused,
                cx,
            ))
            .child(field_row(
                "Username",
                ProfileEditorField::Username,
                &editor.username,
                "user",
                false,
                focused,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(120.0))
                            .flex_none()
                            .text_size(px(11.0))
                            .text_color(theme.colors.text_muted)
                            .child("Auth"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(auth_toggle(
                                "Password",
                                AuthMethodKind::Password,
                                "profile-auth-password",
                                auth_method,
                                cx,
                            ))
                            .child(auth_toggle(
                                "Private Key",
                                AuthMethodKind::PrivateKey,
                                "profile-auth-private-key",
                                auth_method,
                                cx,
                            )),
                    ),
            );

        form = match auth_method {
            AuthMethodKind::Password => form.child(field_row(
                "Password",
                ProfileEditorField::Password,
                &editor.password,
                if editor.secret_present_hint {
                    "leave blank to keep"
                } else {
                    ""
                },
                true,
                focused,
                cx,
            )),
            AuthMethodKind::PrivateKey => form
                .child(field_row(
                    "Key path",
                    ProfileEditorField::KeyPath,
                    &editor.key_path,
                    "~/.ssh/id_ed25519",
                    false,
                    focused,
                    cx,
                ))
                .child(field_row(
                    "Passphrase",
                    ProfileEditorField::Passphrase,
                    &editor.passphrase,
                    if editor.secret_present_hint {
                        "leave blank to keep"
                    } else {
                        ""
                    },
                    true,
                    focused,
                    cx,
                )),
        };

        form = form.child(field_row(
            "Remote path",
            ProfileEditorField::DefaultRemotePath,
            &editor.default_remote_path,
            "/home/user",
            false,
            focused,
            cx,
        ));

        if editor.secret_present_hint {
            form = form.child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme.colors.text_muted)
                    .child("Credentials are stored in the Keychain. Leave blank to keep."),
            );
        }

        form = form.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    text_button("settings-save-profile", "Save")
                        .primary(true)
                        .on_click(cx.listener(|workspace, _event, _window, cx| {
                            workspace.save_profile_editor(cx);
                        })),
                )
                .when(!editor.is_new, |row| {
                    let profile_id = editor.profile_id;
                    row.child(
                        text_button("settings-delete-profile", "Delete…")
                            .danger(true)
                            .on_click(cx.listener(move |workspace, _event, window, cx| {
                                if let Some(id) = profile_id {
                                    workspace.request_delete_profile(id, window, cx);
                                } else if let Some(id) = workspace.selected_profile_id {
                                    workspace.request_delete_profile(id, window, cx);
                                }
                            })),
                    )
                }),
        );

        form.into_any_element()
    }
    pub(crate) fn render_about(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.about_open {
            return None;
        }
        let theme = cx.theme().clone();

        let mark = div()
            .size(px(72.0))
            .rounded_md()
            .bg(theme.colors.surface)
            .border_1()
            .border_color(theme.colors.border)
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(22.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.colors.accent)
                    .child("⇄"),
            );

        Some(
            div()
                .id("about-scrim")
                .key_context("About")
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
                .child(
                    div()
                        .w(px(340.0))
                        .p_5()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_3()
                        .rounded_md()
                        .bg(theme.colors.elevated_surface)
                        .border_1()
                        .border_color(theme.colors.border)
                        .child(mark)
                        .child(
                            div()
                                .text_size(px(18.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child("macSFTP"),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme.colors.text_muted)
                                .child("A fast, native SFTP client for macOS"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    text_button("about-copy-version", "Copy Version Info")
                                        .on_click(cx.listener(|workspace, _event, _window, cx| {
                                            workspace.copy_version_info(cx);
                                        })),
                                )
                                .child(text_button("about-close", "Close").primary(true).on_click(
                                    cx.listener(|workspace, _event, window, cx| {
                                        workspace.close_about(window, cx);
                                    }),
                                )),
                        ),
                )
                .into_any_element(),
        )
    }
}
