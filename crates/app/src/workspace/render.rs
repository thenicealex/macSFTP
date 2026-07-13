use gpui::{
    AppContext, Context, FontWeight, Hsla, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*, px, uniform_list,
};
use macsftp_core::{ EntryPath, ConnectionState, TransferHistoryRecord,
    TransferHistoryStatus, TransferJob, TransferState,
};
use macsftp_ui::{ DragPreview,
    ActiveTheme, FileRowModel, IconName, empty_state, file_row, file_table_header,
    format_size, format_timestamp, icon, icon_button, tab, text_button, text_tooltip,
    transfer_row, connection_status, transfer_history_title, transfer_history_detail, transfer_title,
    section_header_static,
};

use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::helpers::*;
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
            .on_activate(cx.listener(move |workspace, _event, _window, cx| {
                workspace.activate_tab(tab_id, cx);
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
                icon_button("new-tab", IconName::Plus, "New Tab (⌘T)").on_click(cx.listener(
                    |workspace, _event, window, cx| {
                        workspace.open_new_tab(window, cx);
                    },
                )),
            ))
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
        let remote_error = (side == PaneSide::Remote)
            .then(|| tab_state.and_then(|tab| tab.remote.error.clone()))
            .flatten();

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

        let (pane_name, up_id, refresh_id, copy_id, list_container_id) = match side {
            PaneSide::Local => (
                "Local",
                "local-up",
                "local-refresh",
                "local-copy-path",
                "local-list-container",
            ),
            PaneSide::Remote => (
                "Remote",
                "remote-up",
                "remote-refresh",
                "remote-copy-path",
                "remote-list-container",
            ),
        };

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
            .child(
                icon_button(up_id, IconName::ArrowUp, "Parent Directory (⌘↑)").on_click(
                    cx.listener(move |workspace, _event, window, cx| {
                        workspace.focused_side = side;
                        workspace.go_to_parent_directory(window, cx);
                    }),
                ),
            )
            .child(
                icon_button(refresh_id, IconName::Refresh, "Refresh (⌘R)").on_click(cx.listener(
                    move |workspace, _event, window, cx| {
                        workspace.focused_side = side;
                        workspace.refresh_focused_pane(window, cx);
                    },
                )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_1()
                    .text_size(px(12.0))
                    .text_color(if pane_focused {
                        theme.colors.text
                    } else {
                        theme.colors.text_muted
                    })
                    .truncate()
                    .child(path_label.clone().unwrap_or_else(|| pane_name.to_string())),
            )
            .when(is_remote_refreshing, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(theme.colors.text_muted)
                        .child("Refreshing…"),
                )
            })
            .child(
                icon_button(copy_id, IconName::Copy, "Copy Path (⌘⇧C)").on_click(cx.listener(
                    move |workspace, _event, _window, cx| {
                        workspace.focused_side = side;
                        workspace.copy_focused_path(cx);
                    },
                )),
            )
            .when(side == PaneSide::Local, |bar| {
                bar.child(
                    icon_button("local-upload", IconName::Upload, "Upload Selection (⌘U)")
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
                        "Download Selection (⌘D)",
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
            text_button(id, "Refresh & Retry").on_click(cx.listener(
                |workspace, _event, window, cx| {
                    workspace.focused_side = PaneSide::Remote;
                    workspace.refresh_focused_pane(window, cx);
                },
            ))
        };
        let connection_placeholder: Option<gpui::AnyElement> = if side == PaneSide::Remote {
            match tab_state.map(|tab| &tab.connection) {
                Some(ConnectionState::Empty) => Some(
                    empty_state(
                        "Not connected",
                        vec![connect_button("connect-remote", "Connect… (⌘⇧R)")],
                        cx,
                    )
                    .into_any_element(),
                ),
                Some(ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. }) => {
                    Some(
                        empty_state(format!("Connecting to {target_host}…"), vec![], cx)
                            .into_any_element(),
                    )
                }
                Some(ConnectionState::AwaitingHostKey { .. }) => Some(
                    empty_state("Waiting for host key decision…", vec![], cx).into_any_element(),
                ),
                Some(ConnectionState::AwaitingCredentials { .. }) => {
                    Some(empty_state("Waiting for credentials…", vec![], cx).into_any_element())
                }
                Some(ConnectionState::Disconnected { .. }) => Some(
                    empty_state(
                        "Disconnected",
                        vec![
                            connect_button("reconnect-remote", "Reconnect (⌘⇧R)"),
                            edit_connection_button("edit-connection-disconnected"),
                        ],
                        cx,
                    )
                    .into_any_element(),
                ),
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
        } else {
            None
        };

        let entry_count = self.entry_count(side);
        let list: gpui::AnyElement = if let Some(placeholder) = connection_placeholder {
            placeholder
        } else if entry_count == 0 && is_remote_refreshing {
            empty_state("Loading directory…", vec![], cx).into_any_element()
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

                    visible_range
                        .filter_map(|index| {
                            let (model, drag_item) = match side {
                                PaneSide::Local => {
                                    let entry = tab.local.entries.get(index)?;
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
                                    let entry = tab.remote.entries.get(index)?;
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
                                file_row(("file-row", index), model, cx)
                                    .on_click(move |event, window, cx| {
                                        workspace.update(cx, |workspace, cx| {
                                            workspace
                                                .on_row_clicked(side, index, event, window, cx);
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
            .when(side == PaneSide::Local, |pane| {
                pane.border_r_1().border_color(theme.colors.border)
            })
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
            .child(file_table_header(&sort, cx))
            .child(div().flex_1().min_h_0().child(list))
    }
    pub(crate) fn render_transfer_drawer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        // Cloned (not borrowed) so the shared transfer store isn't held
        // borrowed across the `render_transfer_job(job, cx)` calls below.
        let jobs = cx.transfers().jobs.clone();
        let jobs = &jobs;

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

        let mut drawer = div()
            .id("transfer-drawer")
            .flex()
            .flex_col()
            .flex_none()
            .max_h(px(240.0))
            .overflow_y_scroll()
            .bg(theme.colors.surface)
            .border_t_1()
            .border_color(theme.colors.border);

        drawer = drawer.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h(px(28.0))
                .px_2()
                .text_size(px(11.0))
                .text_color(theme.colors.text_muted)
                .child(icon(IconName::Transfers, theme.colors.text_muted))
                .child("Transfers")
                .child(div().flex_1())
                .child(format!(
                    "{} active · {} queued · {} done · {} failed",
                    active_jobs.len(),
                    queued_jobs.len(),
                    completed_jobs.len(),
                    failed_jobs.len()
                )),
        );

        if jobs.is_empty() && cx.resources().transfer_history.records().is_empty() {
            return drawer.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(60.0))
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child("No transfers"),
            );
        }

        for (label, section_jobs) in [("Active", active_jobs), ("Queued", queued_jobs)] {
            if !section_jobs.is_empty() {
                drawer = drawer
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
            drawer = drawer.child(
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
                drawer = drawer.children(
                    section_jobs
                        .into_iter()
                        .map(|job| self.render_transfer_job(job, cx)),
                );
            }
        }

        // History (plan §18): persisted transfers from prior sessions,
        // including unfinished ones captured at the last app close. Only
        // shown when there is something to retry or review.
        let mut history_records: Vec<TransferHistoryRecord> =
            cx.resources().transfer_history.records().to_vec();
        history_records.sort_by_key(|record| std::cmp::Reverse(record.last_updated));
        if !history_records.is_empty() {
            drawer = drawer.child(section_header_static(
                "History",
                history_records.len(),
                &theme,
            ));
            for record in &history_records {
                drawer = drawer.child(self.render_history_record(record, cx));
            }
        }

        drawer
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
            } => format!(
                "{} / {} · — MB/s · ETA —",
                format_size(Some(*bytes_done)),
                bytes_total
                    .map(|total| format_size(Some(total)).to_string())
                    .unwrap_or_else(|| "?".to_string())
            )
            .into(),
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
    pub(crate) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();

        let (status_color, status_text) = match self.active_tab() {
            Some(tab) => {
                let (color, label) = connection_status(&tab.connection, &theme);
                (color, format!("{label} · {}", tab.title))
            }
            None => (theme.colors.text_disabled, "No connection".to_string()),
        };

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

        let mut transfer_summary = format!("{active_count} active");
        if failed_count > 0 {
            transfer_summary.push_str(&format!(" · {failed_count} failed"));
        }

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
                    .child(div().truncate().child(status_text))
                    .children(
                        self.status_message
                            .clone()
                            .map(|message| div().truncate().child(format!("— {message}"))),
                    ),
            )
            .child(
                div()
                    .id("status-transfers")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .rounded_sm()
                    .hover(|style| style.bg(theme.colors.element_hover))
                    .tooltip(text_tooltip("Toggle Transfers (⌘J)"))
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
                    .child(transfer_summary),
            )
    }
    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
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
                            workspace.workspace_focus.focus(window);
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
                            .bg(theme.colors.surface)
                            .border_r_1()
                            .border_color(theme.colors.border)
                            .child(
                                div()
                                    .px_2()
                                    .py_2()
                                    .rounded_sm()
                                    .bg(theme.colors.element_selected)
                                    .text_size(px(12.0))
                                    .text_color(theme.colors.text)
                                    .child("General"),
                            ),
                    )
                    .child(
                        div()
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
                                                    .child("Choose how macSFTP follows the macOS appearance."),
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
                                                    .child(text_button("open-log-folder", "Open Log Folder").on_click(
                                                        cx.listener(|workspace, _event, _window, cx| {
                                                            workspace.open_log_folder(cx);
                                                        }),
                                                    ))
                                                    .child(text_button("copy-version", "Copy Version Info").on_click(
                                                        cx.listener(|workspace, _event, _window, cx| {
                                                            workspace.copy_version_info(cx);
                                                        }),
                                                    )),
                                            ),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
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
                                    cx.listener(|workspace, _event, _window, cx| {
                                        workspace.about_open = false;
                                        cx.notify();
                                    }),
                                )),
                        ),
                )
                .into_any_element(),
        )
    }
}
