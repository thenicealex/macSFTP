use std::collections::HashSet;

use gpui::{
    AppContext, Context, CursorStyle, DragMoveEvent, Hsla, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Styled, Window, div, prelude::*, px,
};
use macsftp_core::{
    EntryPath, TransferId, TransferJob, TransferPlanState, TransferState, TransferStore,
};
use macsftp_ui::{
    ActiveTheme, IconName, connection_status, format_size, icon, icon_button,
    section_header_static, text_tooltip, transfer_row, transfer_title,
};

use crate::palette_commands::labeled_shortcut;
use crate::resources::ActiveTransfers;
use crate::workspace::PaneSide;

pub(crate) fn visible_transfer_jobs(store: &TransferStore) -> Vec<&TransferJob> {
    let mut hidden_job_ids = HashSet::new();

    for plan in &store.plans {
        let children_are_terminal = !plan.child_jobs.is_empty()
            && plan.child_jobs.iter().all(|child_id| {
                store.find_job(*child_id).is_some_and(|job| {
                    matches!(
                        job.state,
                        TransferState::Completed
                            | TransferState::Skipped
                            | TransferState::Failed { .. }
                    )
                })
            });
        let show_root = plan.child_jobs.is_empty()
            || matches!(plan.state, TransferPlanState::Planning)
            || matches!(plan.state, TransferPlanState::Cancelled)
            || (matches!(plan.state, TransferPlanState::Failed { .. }) && !children_are_terminal);

        if show_root {
            hidden_job_ids.extend(plan.child_jobs.iter().copied());
        } else {
            hidden_job_ids.insert(plan.root_job_id);
        }
    }

    store
        .jobs
        .iter()
        .filter(|job| !hidden_job_ids.contains(&job.id))
        .collect()
}

/// A failed transfer offers a Retry control only when it is genuinely
/// retryable. Handoff failures (no connection available, connection lost,
/// transfer manager gone) mark their planned children `retryable: false`
/// because those jobs never entered the transfer manager and therefore have
/// no retry route — surfacing a Retry button there would be a dead control.
fn can_retry_transfer(job: &TransferJob) -> bool {
    matches!(
        job.state,
        TransferState::Failed {
            retryable: true,
            ..
        }
    )
}

impl crate::workspace::Workspace {
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
        let jobs: Vec<_> = visible_transfer_jobs(cx.transfers())
            .into_iter()
            .cloned()
            .collect();
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
                            workspace.reset_drawer_height(window.viewport_size().height);
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
                    {
                        let workspace_entity = workspace_entity.clone();
                        move |_value, _cursor_offset, window, cx| {
                            let start_y = window.mouse_position().y;
                            workspace_entity.update(cx, |workspace, cx| {
                                workspace.drawer_resize = Some(TransferDrawerResize {
                                    start_height: workspace.drawer_height,
                                    start_y,
                                });
                                cx.notify();
                            });
                            cx.new(|_| ResizeDragGhost)
                        }
                    },
                )
                .on_drag_move(cx.listener(
                    |workspace, event: &DragMoveEvent<TransferDrawerResize>, window, cx| {
                        let Some(start) = workspace.drawer_resize.clone() else {
                            return;
                        };
                        let current_y = event.event.position.y;
                        let new_height = start.start_height + (start.start_y - current_y);
                        workspace.set_drawer_height(new_height, window.viewport_size().height);
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
                .when(active_jobs.len() + queued_jobs.len() > 0, |header| {
                    let workspace = workspace_entity.clone();
                    header.child(
                        icon_button(
                            "cancel-all-transfers",
                            IconName::Close,
                            "Cancel All Transfers",
                        )
                        .icon_color(theme.colors.warning)
                        .on_click(move |_event, _window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.cancel_all_transfers(cx);
                            });
                        }),
                    )
                })
                .when(completed_jobs.len() + failed_jobs.len() > 0, |header| {
                    let workspace = workspace_entity.clone();
                    header.child(
                        icon_button(
                            "clear-transfer-records",
                            IconName::Trash,
                            "Clear Transfer History",
                        )
                        .on_click(move |_event, _window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.clear_transfer_records(cx);
                            });
                        }),
                    )
                })
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

        if jobs.is_empty() {
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

        drawer.child(body)
    }
    pub(crate) fn render_transfer_job(
        &self,
        job: &TransferJob,
        cx: &mut Context<Self>,
    ) -> macsftp_ui::TransferRow {
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
        if can_retry_transfer(job) {
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
            .filter(|path| {
                matches!(
                    (self.focused_side, path),
                    (PaneSide::Local, EntryPath::Local(_))
                        | (PaneSide::Remote, EntryPath::Remote(_))
                )
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

        let jobs = visible_transfer_jobs(cx.transfers());
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
                    .children(
                        self.status_message.clone().map(|message| {
                            div().min_w_0().truncate().child(format!("— {message}"))
                        }),
                    ),
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
}

#[cfg(test)]
mod tests {
    use macsftp_core::{
        ConflictPolicy, ErrorCode, LocalPath, MetadataPolicy, RemotePath, Timestamp,
        TransferDirection, TransferEndpoint, TransferId, TransferJob, TransferPlan, TransferPlanId,
        TransferPlanState, TransferState, TransferStore, UserFacingError,
    };

    use super::can_retry_transfer;
    use super::visible_transfer_jobs;

    fn job(id: u64, state: TransferState) -> TransferJob {
        TransferJob {
            id: TransferId(id),
            direction: TransferDirection::Upload,
            source: TransferEndpoint::Local(LocalPath::new("/tmp/file.txt")),
            destination: TransferEndpoint::Remote(RemotePath::new("/srv/file.txt")),
            state,
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
            warnings: Vec::new(),
            created_at: Timestamp::from_secs_since_epoch(1),
        }
    }

    fn store_with_child(
        plan_state: TransferPlanState,
        root_state: TransferState,
        child_state: TransferState,
    ) -> TransferStore {
        TransferStore {
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(1),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/file.txt")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv/file.txt")),
                state: plan_state,
                planned_count: 1,
                total_bytes: Some(10),
                child_jobs: vec![TransferId(2)],
                conflict_policy: ConflictPolicy::default(),
            }],
            jobs: vec![job(1, root_state), job(2, child_state)],
            pending_conflicts: Vec::new(),
        }
    }

    fn visible_ids(store: &TransferStore) -> Vec<TransferId> {
        visible_transfer_jobs(store)
            .into_iter()
            .map(|job| job.id)
            .collect()
    }

    #[test]
    fn planning_plan_shows_only_root_placeholder() {
        let store = store_with_child(
            TransferPlanState::Planning,
            TransferState::Planning,
            TransferState::Queued,
        );

        assert_eq!(visible_ids(&store), vec![TransferId(1)]);
    }

    #[test]
    fn planned_single_file_shows_only_executable_child() {
        let store = store_with_child(
            TransferPlanState::Queued,
            TransferState::Queued,
            TransferState::Queued,
        );

        assert_eq!(visible_ids(&store), vec![TransferId(2)]);
    }

    #[test]
    fn partial_planning_failure_shows_only_root_error() {
        let error = UserFacingError::new(
            ErrorCode::Unknown,
            "Planning failed",
            "Try the transfer again.",
        );
        let store = store_with_child(
            TransferPlanState::Failed {
                error: error.clone(),
            },
            TransferState::Failed {
                error,
                retryable: true,
            },
            TransferState::Queued,
        );

        assert_eq!(visible_ids(&store), vec![TransferId(1)]);
    }

    #[test]
    fn execution_failure_shows_only_terminal_child() {
        let error = UserFacingError::new(
            ErrorCode::Unknown,
            "Transfer failed",
            "Try the transfer again.",
        );
        let store = store_with_child(
            TransferPlanState::Failed {
                error: error.clone(),
            },
            TransferState::Failed {
                error: error.clone(),
                retryable: true,
            },
            TransferState::Failed {
                error,
                retryable: true,
            },
        );

        assert_eq!(visible_ids(&store), vec![TransferId(2)]);
    }

    #[test]
    fn retry_action_is_shown_for_retryable_failure() {
        let error = UserFacingError::new(
            ErrorCode::Unknown,
            "Transfer failed",
            "Try the transfer again.",
        );
        let failed = job(
            1,
            TransferState::Failed {
                error,
                retryable: true,
            },
        );

        assert!(
            can_retry_transfer(&failed),
            "a retryable failure must offer the Retry control"
        );
    }

    #[test]
    fn retry_action_is_hidden_for_non_retryable_failure() {
        // Handoff failures (no connection, dropped connection, manager gone)
        // mark planned children non-retryable: they never entered the
        // transfer manager, so a Retry button would be a dead control.
        let error = UserFacingError::new(
            ErrorCode::ChannelClosed,
            "Could not start transfer",
            "The connected session closed before the transfer acquired its connection.",
        );
        let failed = job(
            2,
            TransferState::Failed {
                error,
                retryable: false,
            },
        );

        assert!(
            !can_retry_transfer(&failed),
            "a non-retryable handoff failure must not offer the Retry control"
        );
    }
}
