use gpui::{Context, Hsla, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use macsftp_core::{ConnectionState, TransferEndpoint, TransferJob};

use crate::{ActiveTheme, Theme};

/// Static section header used in the transfer drawer.
pub fn section_header_static(label: &str, count: usize, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(24.0))
        .px_2()
        .text_size(px(11.0))
        .text_color(theme.colors.text_muted)
        .child(format!("{label} ({count})"))
}

/// Returns the color and label to display for a tab's connection state.
pub fn connection_status(connection: &ConnectionState, theme: &Theme) -> (Hsla, SharedString) {
    match connection {
        ConnectionState::Empty => (theme.colors.text_disabled, "Not connected".into()),
        ConnectionState::Connecting { .. } => (theme.colors.warning, "Connecting…".into()),
        ConnectionState::AwaitingHostKey { .. } => {
            (theme.colors.warning, "Awaiting host key".into())
        }
        ConnectionState::AwaitingCredentials { .. } => {
            (theme.colors.warning, "Awaiting credentials".into())
        }
        ConnectionState::Connected { .. } => (theme.colors.success, "Connected".into()),
        ConnectionState::Reconnecting { .. } => (theme.colors.warning, "Reconnecting…".into()),
        ConnectionState::Disconnected { .. } => (theme.colors.text_disabled, "Disconnected".into()),
        ConnectionState::Failed { error } => (
            theme.colors.error,
            format!("Failed — {}", error.title).into(),
        ),
    }
}

/// Builds a human-readable title for a transfer job.
pub fn transfer_title(job: &TransferJob) -> String {
    let endpoint_text = |endpoint: &TransferEndpoint| match endpoint {
        TransferEndpoint::Local(path) => path.as_str().to_string(),
        TransferEndpoint::Remote(path) => path.as_str().to_string(),
    };
    let source = endpoint_text(&job.source);
    let destination = endpoint_text(&job.destination);
    let name = source.rsplit('/').next().unwrap_or(&source).to_string();
    format!("{name} · {source} → {destination}")
}

/// Generates a default "copy" name for a conflicting destination.
pub fn copy_name(destination: &TransferEndpoint) -> String {
    let path = match destination {
        TransferEndpoint::Local(path) => path.as_str(),
        TransferEndpoint::Remote(path) => path.as_str(),
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} (copy).{extension}"),
        _ => format!("{name} (copy)"),
    }
}

/// Lightweight ghost rendered under the cursor while a file row is being
/// dragged between the local and remote panes.
pub struct DragPreview {
    pub label: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.colors.element_selected)
            .text_color(theme.colors.text)
            .shadow_md()
            .text_size(px(12.0))
            .child(self.label.clone())
    }
}
