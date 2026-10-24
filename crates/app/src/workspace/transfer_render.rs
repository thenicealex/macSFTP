use std::collections::HashSet;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::base::motion::{Transition, transition};
use gpui_kit::{
    AppContext, Context, CursorStyle, DragMoveEvent, ElementId, Hsla, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Role, SharedString, Styled, Window, div, prelude::*, px,
};
use macsftp_core::{
    EditPhase, EntryPath, IdMatcher, TransferId, TransferJob, TransferPlanState, TransferState,
    TransferStore,
};
use macsftp_ui::{
    ActiveTheme, IconName, connection_status, format_size, icon, icon_button,
    section_header_static, text_tooltip, transfer_row, transfer_title,
};

use crate::palette_commands::labeled_shortcut;
use crate::resources::{ActiveResources, ActiveTransfers};
use crate::workspace::PaneSide;

/// How long the progress bar takes to ease toward a newly reported fraction.
/// Chunk updates arrive far faster than the eye can resolve, so the bar chases
/// the real value instead of stepping with it; the percentage label beside it
/// stays exact, so a stall is still visible immediately.
pub(crate) const PROGRESS_SMOOTHING: std::time::Duration = std::time::Duration::from_millis(200);

/// The fraction the bar paints for one job: the reported fraction, eased.
/// The id is keyed per job so concurrent transfers each chase their own value,
/// and the text beside the bar keeps printing the reported one.
pub(crate) fn painted_progress(
    job_id: TransferId,
    reported: f32,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> f32 {
    transition(
        ElementId::from(("transfer-progress", job_id.0)),
        reported,
        Transition::new(PROGRESS_SMOOTHING),
        window,
        cx,
    )
}

pub(crate) fn visible_transfer_jobs(store: &TransferStore) -> Vec<&TransferJob> {
    let mut hidden_roots = HashSet::new();
    let mut hidden_matchers = Vec::new();

    for plan in &store.plans {
        let show_root = if plan.child_jobs.is_empty()
            || matches!(plan.state, TransferPlanState::Planning)
            || matches!(plan.state, TransferPlanState::Cancelled)
        {
            true
        } else if matches!(plan.state, TransferPlanState::Failed { .. }) {
            let children_are_terminal = plan.child_jobs.iter().all(|child_id| {
                store.find_job(*child_id).is_some_and(|job| {
                    matches!(
                        job.state,
                        TransferState::Completed
                            | TransferState::Skipped
                            | TransferState::Failed { .. }
                    )
                })
            });
            !children_are_terminal
        } else {
            false
        };

        if show_root {
            hidden_matchers.push(IdMatcher::new(&plan.child_jobs));
        } else {
            hidden_roots.insert(plan.root_job_id);
        }
    }

    store
        .jobs
        .iter()
        .filter(|job| {
            !hidden_roots.contains(&job.id)
                && !hidden_matchers
                    .iter()
                    .any(|matcher| matcher.contains(job.id))
        })
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
    /// `animated_height` is the open/close spring sampled by `Render::render`.
    /// Content decisions still read the stored height, so nothing changes
    /// while the drawer is in flight.
    pub(crate) fn render_transfer_drawer(
        &mut self,
        animated_height: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        use crate::workspace::drawer_height::{
            RESIZE_HANDLE_HEIGHT, ResizeDragGhost, TransferDrawerResize,
        };

        // Reclamp after window shrink so stored height never exceeds max.
        self.reclamp_drawer_height(window.viewport_size().height);

        use crate::workspace::view_state::TransferSection;

        let theme = cx.theme().clone();
        let workspace_entity = cx.entity();

        struct DrawerSection {
            total: usize,
            rendered: Vec<TransferJob>,
        }

        let max_in_progress = self
            .transfer_drawer
            .visible_rows(TransferSection::InProgress, usize::MAX)
            .0;
        let max_waiting = self
            .transfer_drawer
            .visible_rows(TransferSection::Waiting, usize::MAX)
            .0;
        let max_completed = self
            .transfer_drawer
            .visible_rows(TransferSection::Completed, usize::MAX)
            .0;
        let max_failed = self
            .transfer_drawer
            .visible_rows(TransferSection::Failed, usize::MAX)
            .0;

        let mut in_progress = DrawerSection {
            total: 0,
            rendered: Vec::new(),
        };
        let mut waiting = DrawerSection {
            total: 0,
            rendered: Vec::new(),
        };
        let mut completed = DrawerSection {
            total: 0,
            rendered: Vec::new(),
        };
        let mut failed = DrawerSection {
            total: 0,
            rendered: Vec::new(),
        };
        let mut running = Vec::new();

        let visible_jobs = visible_transfer_jobs(cx.transfers());
        let total_visible_jobs = visible_jobs.len();

        for job in visible_jobs {
            match &job.state {
                TransferState::Running {
                    bytes_done,
                    bytes_total,
                    ..
                } => {
                    running.push((job.id, *bytes_done, *bytes_total));
                    in_progress.total += 1;
                    if in_progress.rendered.len() < max_in_progress {
                        in_progress.rendered.push(job.clone());
                    }
                }
                TransferState::Planning
                | TransferState::Cancelling
                | TransferState::WaitingForConflictDecision { .. } => {
                    in_progress.total += 1;
                    if in_progress.rendered.len() < max_in_progress {
                        in_progress.rendered.push(job.clone());
                    }
                }
                TransferState::Queued => {
                    waiting.total += 1;
                    if waiting.rendered.len() < max_waiting {
                        waiting.rendered.push(job.clone());
                    }
                }
                TransferState::Completed | TransferState::Skipped => {
                    completed.total += 1;
                    if completed.rendered.len() < max_completed {
                        completed.rendered.push(job.clone());
                    }
                }
                TransferState::Failed { .. } => {
                    failed.total += 1;
                    if failed.rendered.len() < max_failed {
                        failed.rendered.push(job.clone());
                    }
                }
            }
        }

        let now = std::time::Instant::now();
        let agg = cx.rates().aggregate(&running, now);

        // Drawer chrome: counts plus aggregate speed/ETA for running jobs.
        let agg_label = {
            let mut s = format!(
                "{} active · {} queued · {} done · {} failed",
                in_progress.total, waiting.total, completed.total, failed.total
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

        // The stored height is clamped to the minimum at every write, so the
        // container carries no min of its own: it must be free to reach zero,
        // or a closing drawer would unmount from 40px with a visible jump.
        let mut drawer = div()
            .id("transfer-drawer")
            .debug_selector(|| "transfer-drawer".into())
            .flex()
            .flex_col()
            .flex_none()
            .h(animated_height)
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
                .flex()
                .items_center()
                .justify_center()
                .w_full()
                .h(RESIZE_HANDLE_HEIGHT)
                .cursor_row_resize()
                .bg(theme.colors.surface)
                .hover(|style| style.bg(hover_background))
                .tooltip(text_tooltip("Drag to resize. Double-click to reset."))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|workspace, event: &MouseDownEvent, window, cx| {
                        // Prefer mouse_down double-click so drag threshold
                        // does not steal the reset gesture.
                        if event.click_count >= 2 {
                            workspace.reset_drawer_height(window.viewport_size().height);
                            workspace.transfer_drawer.resize = None;
                            cx.notify();
                        }
                    }),
                )
                .on_drag(
                    TransferDrawerResize {
                        start_height: self.transfer_drawer.height,
                        start_y: px(0.0),
                    },
                    {
                        let workspace_entity = workspace_entity.clone();
                        move |_value, _cursor_offset, window, cx| {
                            let start_y = window.mouse_position().y;
                            workspace_entity.update(cx, |workspace, cx| {
                                workspace.transfer_drawer.resize = Some(TransferDrawerResize {
                                    start_height: workspace.transfer_drawer.height,
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
                        let Some(start) = workspace.transfer_drawer.resize.clone() else {
                            return;
                        };
                        let current_y = event.event.position.y;
                        let new_height = start.start_height + (start.start_y - current_y);
                        workspace.set_drawer_height(new_height, window.viewport_size().height);
                        cx.set_active_drag_cursor_style(CursorStyle::ResizeRow, window);
                        cx.notify();
                    },
                ))
                .child(
                    div()
                        .w(px(36.0))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(theme.colors.text_disabled),
                ),
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
                .when(in_progress.total + waiting.total > 0, |header| {
                    let workspace = workspace_entity.clone();
                    header.child(
                        icon_button(
                            "cancel-all-transfers",
                            IconName::Close,
                            labeled_shortcut("Cancel All Transfers", "CancelAllTransfers"),
                        )
                        .icon_color(theme.colors.warning)
                        .on_click(move |_event, _window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.cancel_all_transfers(cx);
                            });
                        }),
                    )
                })
                .when(completed.total + failed.total > 0, |header| {
                    let workspace = workspace_entity.clone();
                    header.child(
                        icon_button("clear-transfer-records", IconName::Trash, "Clear History")
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
                )
                .child(
                    div()
                        .debug_selector(|| "hide-transfer-drawer".into())
                        .child(
                            icon_button(
                                "hide-transfer-drawer",
                                IconName::ChevronDown,
                                labeled_shortcut("Hide Transfers", "ShowTransferDrawer"),
                            )
                            .on_click(cx.listener(
                                |workspace, _, _, cx| {
                                    workspace.transfer_drawer.open = false;
                                    workspace.transfer_drawer.resize = None;
                                    cx.notify();
                                },
                            )),
                        ),
                ),
        );

        let mut content = div().flex().flex_col();

        if total_visible_jobs == 0 {
            return drawer.child(
                div()
                    .debug_selector(|| "empty-transfer-queue".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .text_size(px(12.0))
                    .text_color(theme.colors.text_muted)
                    .child("No transfers yet")
                    .when(self.transfer_drawer.height >= px(90.0), |empty| {
                        empty.child(
                            div()
                                .text_size(px(11.0))
                                .child("Select files to upload or download."),
                        )
                    }),
            );
        }

        // One row per transfer, and a mass download or upload has one per file.
        // The drawer is not virtualized, so each row is laid out on every
        // frame: a section therefore renders a bounded page of rows and offers
        // the rest behind a control rather than building hundreds of rows that
        // are mostly scrolled out of sight.
        for section in TransferSection::ALL {
            let (label, section_data, collapsible, toggle_id) = match section {
                TransferSection::InProgress => {
                    ("In Progress", &mut in_progress, false, "toggle-in-progress")
                }
                TransferSection::Waiting => ("Waiting", &mut waiting, false, "toggle-waiting"),
                TransferSection::Completed => {
                    ("Complete", &mut completed, true, "toggle-completed")
                }
                TransferSection::Failed => ("Failed", &mut failed, true, "toggle-failed"),
            };
            if section_data.total == 0 {
                continue;
            }
            let expanded = self.transfer_drawer.is_expanded(section);
            if collapsible {
                content = content.child(
                    div()
                        .id(toggle_id)
                        .debug_selector(move || toggle_id.into())
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
                            let next = !workspace.transfer_drawer.is_expanded(section);
                            workspace.transfer_drawer.set_expanded(section, next);
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
                        .child(format!("{label} ({})", section_data.total)),
                );
                if !expanded {
                    continue;
                }
            } else {
                content = content.child(section_header_static(label, section_data.total, &theme));
            }

            let (visible, hidden) = self
                .transfer_drawer
                .visible_rows(section, section_data.total);
            let rendered = std::mem::take(&mut section_data.rendered);
            content = content.children(rendered.into_iter().take(visible).map(|job| {
                // An id per row lets a test see what the section actually
                // built, which is the point of the page size.
                div()
                    .id(("transfer-row", job.id.0))
                    .test_support()
                    .child(self.render_transfer_job(&job, window, cx))
            }));
            if hidden > 0 {
                content = content.child(
                    div()
                        .id(toggle_id)
                        .debug_selector(move || format!("{toggle_id}-more"))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .h(px(24.0))
                        .px_2()
                        .text_size(px(11.0))
                        .text_color(theme.colors.accent)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.colors.element_hover))
                        .on_click(cx.listener(move |workspace, _event, _window, cx| {
                            workspace.transfer_drawer.set_showing_all(section, true);
                            cx.notify();
                        }))
                        .child(format!("Show {hidden} more")),
                );
            }
        }

        let body = macsftp_ui::scroll_area(
            "transfer-drawer-body",
            content,
            self.transfer_scroll(),
            self.transfer_scrollbar(),
            window,
            cx,
        );
        drawer.child(body)
    }
    pub(crate) fn render_transfer_job(
        &self,
        job: &TransferJob,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> macsftp_ui::TransferRow {
        let theme = cx.theme().clone();
        let job_id = job.id;
        let title = transfer_title(job);

        let (state_label, state_color): (SharedString, Hsla) = match &job.state {
            TransferState::Queued => ("Waiting".into(), theme.colors.text_muted),
            TransferState::Planning => ("Preparing…".into(), theme.colors.info),
            TransferState::WaitingForConflictDecision { .. } => {
                ("Needs Your Choice".into(), theme.colors.warning)
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
                    _ => "Transferring…".to_string(),
                };
                (label.into(), theme.colors.accent)
            }
            TransferState::Cancelling => ("Cancelling…".into(), theme.colors.warning),
            TransferState::Completed => ("Complete".into(), theme.colors.success),
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
                        "{} items found · {}",
                        plan.planned_count,
                        format_size(plan.total_bytes)
                    )
                })
                .unwrap_or_else(|| "Preparing…".to_string())
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
                .unwrap_or_else(|| "Complete with a warning".into()),
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
                let shown = painted_progress(job_id, fraction, window, cx);
                row.progress(shown, theme.colors.accent)
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
            let workspace = workspace.clone();
            row = row.on_retry(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.retry_transfer(job_id, cx);
                });
            });
        }
        // Reveal needs a local destination to point at. A completed upload's
        // destination lives on the server, where Finder cannot go, so the
        // control is absent instead of reporting that it cannot help; a failed
        // or cancelled row may have no file at all.
        let revealable = matches!(job.state, TransferState::Completed)
            && matches!(job.destination, macsftp_core::TransferEndpoint::Local(_));
        if revealable {
            row = row.on_reveal(move |_event, _window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.reveal_transfer_destination(job_id, cx);
                });
            });
        }

        row
    }

    /// Reveal a finished download's destination in Finder.
    ///
    /// Only local destinations are revealable, and the row only offers the
    /// control for those (see `render_transfer_job`); a remote path is reported
    /// rather than opened, so a caller that gets here with one is told why
    /// nothing happened.
    pub(crate) fn reveal_transfer_destination(
        &mut self,
        transfer_id: macsftp_core::TransferId,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = cx.transfers().find_job(transfer_id) else {
            return;
        };
        match &job.destination {
            macsftp_core::TransferEndpoint::Local(path) => {
                crate::workspace::reveal_local_path(cx, std::path::Path::new(path.as_str()));
            }
            macsftp_core::TransferEndpoint::Remote(_) => {
                self.show_status(
                    crate::workspace::view_state::StatusLine::from("This file is on the server."),
                    cx,
                );
            }
        }
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
            Some(tab) => connection_status(&tab.connection, &theme),
            None => (
                theme.colors.text_disabled,
                SharedString::from("Not Connected"),
            ),
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
        let selected_edit = self.selected_edit_session(cx);

        // One spoken summary for the whole bar: the dot is decorative, and the
        // trailing counts would otherwise read as separate fragments.
        let mut spoken = status_text.to_string();
        if selected_count > 0 {
            spoken.push_str(&format!(", {selected_count} selected"));
        }
        if let Some(line) = &self.status_message {
            spoken.push_str(&format!(", {}", line.message));
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
            .id("status-bar")
            .role(Role::Status)
            .aria_label(spoken)
            .test_support()
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
                    .children(self.status_message.clone().map(|line| {
                        div()
                            .min_w_0()
                            .truncate()
                            .child(format!("— {}", line.message))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .when_some(selected_edit, |row, (session_id, phase)| {
                        row.child(match phase {
                            EditPhase::Editing => {
                                let is_modified = cx
                                    .resources()
                                    .edit_sessions
                                    .get(session_id)
                                    .is_some_and(|s| s.local_modified);
                                div()
                                    .id("upload-modified-file")
                                    .flex()
                                    .items_center()
                                    .h(px(18.0))
                                    .px_2()
                                    .rounded_sm()
                                    .border_1()
                                    .border_color(if is_modified {
                                        theme.colors.accent
                                    } else {
                                        theme.colors.border
                                    })
                                    .bg(if is_modified {
                                        theme.colors.element_selected
                                    } else {
                                        theme.colors.surface
                                    })
                                    .text_size(px(11.0))
                                    .text_color(if is_modified {
                                        theme.colors.accent
                                    } else {
                                        theme.colors.text
                                    })
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme.colors.element_hover))
                                    .active(|style| style.bg(theme.colors.element_active))
                                    .on_click(cx.listener(|workspace, _event, _window, cx| {
                                        workspace.upload_selected_edit(cx);
                                    }))
                                    .child("Upload Changes")
                                    .into_any_element()
                            }
                            EditPhase::Downloading => div()
                                .text_color(theme.colors.text_muted)
                                .child("Downloading Copy…")
                                .into_any_element(),
                            EditPhase::CheckingRemote => div()
                                .text_color(theme.colors.text_muted)
                                .child("Checking Server File…")
                                .into_any_element(),
                            EditPhase::UploadingBack => div()
                                .text_color(theme.colors.text_muted)
                                .child("Uploading Changes…")
                                .into_any_element(),
                            EditPhase::RemoteConflict => div()
                                .text_color(theme.colors.error)
                                .child("Review Changes")
                                .into_any_element(),
                        })
                    })
                    .child(
                        div()
                            .id("status-transfers")
                            .debug_selector(|| "status-transfers".into())
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .px_1()
                            .rounded_sm()
                            .hover(|style| style.bg(theme.colors.element_hover))
                            .tooltip(text_tooltip(labeled_shortcut(
                                if self.transfer_drawer.open {
                                    "Hide Transfers"
                                } else {
                                    "Show Transfers"
                                },
                                "ShowTransferDrawer",
                            )))
                            .on_click(cx.listener(|workspace, _event, _window, cx| {
                                workspace.transfer_drawer.open = !workspace.transfer_drawer.open;
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
                            .child("Transfers"),
                    ),
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
            just_finished_plan: None,
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
            "Preparation Failed",
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
            "Transfer Failed",
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
    fn large_plan_visible_jobs_is_linear() {
        let child_count = 5_000;
        let child_ids: Vec<TransferId> = (2..=child_count + 1).map(TransferId).collect();
        let mut jobs = Vec::with_capacity(child_count as usize + 1);
        jobs.push(job(1, TransferState::Completed));
        for &id in &child_ids {
            jobs.push(job(id.0, TransferState::Completed));
        }

        let store = TransferStore {
            just_finished_plan: None,
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(1),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/dir")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv/dir")),
                state: TransferPlanState::Completed,
                planned_count: child_count as usize,
                total_bytes: Some(100_000),
                child_jobs: child_ids.clone(),
                conflict_policy: ConflictPolicy::default(),
            }],
            jobs,
            pending_conflicts: Vec::new(),
        };

        let visible = visible_transfer_jobs(&store);
        assert_eq!(visible.len(), child_count as usize);
        assert_eq!(visible[0].id, TransferId(2));
    }

    #[test]
    fn large_planning_plan_visible_jobs_hides_children_with_id_matcher() {
        let child_count = 10_000;
        let child_ids: Vec<TransferId> = (2..=child_count + 1).map(TransferId).collect();
        let mut jobs = Vec::with_capacity(child_count as usize + 1);
        jobs.push(job(1, TransferState::Planning));
        for &id in &child_ids {
            jobs.push(job(id.0, TransferState::Queued));
        }

        let store = TransferStore {
            just_finished_plan: None,
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(1),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp/dir")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv/dir")),
                state: TransferPlanState::Planning,
                planned_count: child_count as usize,
                total_bytes: Some(100_000),
                child_jobs: child_ids,
                conflict_policy: ConflictPolicy::default(),
            }],
            jobs,
            pending_conflicts: Vec::new(),
        };

        let start = std::time::Instant::now();
        let visible = visible_transfer_jobs(&store);
        let elapsed = start.elapsed();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, TransferId(1));
        assert!(
            elapsed.as_millis() < 50,
            "visible_transfer_jobs took too long: {elapsed:?}"
        );
    }

    #[test]
    fn retry_action_is_shown_for_retryable_failure() {
        let error = UserFacingError::new(
            ErrorCode::Unknown,
            "Transfer Failed",
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
            "Transfer Couldn’t Start",
            "The connection closed. Reconnect and try again.",
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
