use std::rc::Rc;
use std::time::UNIX_EPOCH;

use gpui::{
    App, Div, ElementId, IntoElement, ParentElement, Pixels, SharedString, Stateful, Styled,
    Window, div, prelude::*, px,
};
use macsftp_core::{FileKind, FileSort, FileSortField, SortDirection, Timestamp};

use crate::icon::{IconName, icon};
use crate::theme::ActiveTheme;

/// Fixed trailing column widths shared by the header and every row so
/// the table stays aligned. The name column takes the remaining space.
pub const SIZE_COLUMN_WIDTH: Pixels = px(80.0);
pub const MODIFIED_COLUMN_WIDTH: Pixels = px(130.0);

/// Presentational data for one file row. Derived from a
/// `LocalEntry`/`RemoteEntry` snapshot by the caller.
pub struct FileRowModel {
    pub name: SharedString,
    pub kind: FileKind,
    pub is_hidden: bool,
    pub selected: bool,
    pub size_label: SharedString,
    pub modified_label: SharedString,
}

/// Table header with the current sort state on the matching column.
/// Column labels are clickable; `on_field` receives the field that was clicked.
pub fn file_table_header(
    sort: &FileSort,
    cx: &App,
    on_field: impl Fn(FileSortField, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let theme = cx.theme();
    let sort_marker = match sort.direction {
        SortDirection::Ascending => " ↑",
        SortDirection::Descending => " ↓",
    };
    let active_field = sort.field;
    let column_label = |label: &str, field: FileSortField| {
        let mut text = String::from(label);
        if active_field == field {
            text.push_str(sort_marker);
        }
        text
    };
    let on_field = Rc::new(on_field);
    let hover_background = theme.colors.element_hover;

    let name_handler = on_field.clone();
    let size_handler = on_field.clone();
    let modified_handler = on_field;

    div()
        .flex()
        .items_center()
        .flex_none()
        .h(theme.sizes.table_header_height)
        .px_2()
        .gap_2()
        .bg(theme.colors.surface)
        .border_b_1()
        .border_color(theme.colors.border)
        .text_size(px(11.0))
        .text_color(theme.colors.text_muted)
        .font_family(theme.fonts.ui_family.clone())
        .child(
            div()
                .id("sort-name")
                .flex_1()
                .cursor_pointer()
                .hover(move |style| style.bg(hover_background))
                .on_click(move |_event, window, cx| {
                    name_handler(FileSortField::Name, window, cx);
                })
                .child(column_label("Name", FileSortField::Name)),
        )
        .child(
            div()
                .id("sort-size")
                .w(SIZE_COLUMN_WIDTH)
                .flex_none()
                .text_right()
                .cursor_pointer()
                .hover(move |style| style.bg(hover_background))
                .on_click(move |_event, window, cx| {
                    size_handler(FileSortField::Size, window, cx);
                })
                .child(column_label("Size", FileSortField::Size)),
        )
        .child(
            div()
                .id("sort-modified")
                .w(MODIFIED_COLUMN_WIDTH)
                .flex_none()
                .cursor_pointer()
                .hover(move |style| style.bg(hover_background))
                .on_click(move |_event, window, cx| {
                    modified_handler(FileSortField::ModifiedAt, window, cx);
                })
                .child(column_label("Modified", FileSortField::ModifiedAt)),
        )
}

/// One virtualized file row. The caller attaches click handlers.
pub fn file_row(id: impl Into<ElementId>, model: FileRowModel, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (kind_icon, icon_color) = match model.kind {
        FileKind::Directory => (IconName::Folder, theme.colors.accent),
        FileKind::Symlink => (IconName::Symlink, theme.colors.info),
        FileKind::File | FileKind::Other | FileKind::Unknown => {
            (IconName::File, theme.colors.text_muted)
        }
    };
    let name_color = if model.is_hidden {
        theme.colors.text_muted
    } else {
        theme.colors.text
    };
    let hover_background = theme.colors.element_hover;

    div()
        .id(id.into())
        .flex()
        .items_center()
        .w_full()
        .h(theme.sizes.file_row_height)
        .px_2()
        .gap_2()
        .text_size(px(13.0))
        .font_family(theme.fonts.ui_family.clone())
        .when(model.selected, |row| row.bg(theme.colors.element_selected))
        .when(!model.selected, |row| {
            row.hover(move |style| style.bg(hover_background))
        })
        .child(icon(kind_icon, icon_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(name_color)
                .child(model.name),
        )
        .child(
            div()
                .w(SIZE_COLUMN_WIDTH)
                .flex_none()
                .text_right()
                .text_size(px(11.0))
                .text_color(theme.colors.text_muted)
                .font_family(theme.fonts.mono_family.clone())
                .child(model.size_label),
        )
        .child(
            div()
                .w(MODIFIED_COLUMN_WIDTH)
                .flex_none()
                .text_size(px(11.0))
                .text_color(theme.colors.text_muted)
                .font_family(theme.fonts.mono_family.clone())
                .child(model.modified_label),
        )
}

/// Human-readable size, Finder-style decimal units. `None` renders as
/// an em dash (directories, unknown sizes).
pub fn format_size(size: Option<u64>) -> SharedString {
    let Some(size) = size else {
        return "—".into();
    };
    if size < 1000 {
        return format!("{size} B").into();
    }

    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut value = size as f64 / 1000.0;
    let mut unit_index = 0;
    while value >= 1000.0 && unit_index < units.len() - 1 {
        value /= 1000.0;
        unit_index += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", units[unit_index]).into()
    } else {
        format!("{value:.0} {}", units[unit_index]).into()
    }
}

/// Size column text for a file-list row.
///
/// Directories render blank: their aggregate size is meaningless at browse
/// time, and a full column of dashes reads as visual noise. Only files with
/// an unknown size show the em dash.
pub fn format_size_label(kind: FileKind, size: Option<u64>) -> SharedString {
    match kind {
        FileKind::Directory => SharedString::default(),
        _ => format_size(size),
    }
}

/// UTC `YYYY-MM-DD HH:MM` label. `None` renders as an em dash.
pub fn format_timestamp(timestamp: Option<Timestamp>) -> SharedString {
    let Some(timestamp) = timestamp else {
        return "—".into();
    };
    let Ok(elapsed) = timestamp.0.duration_since(UNIX_EPOCH) else {
        return "—".into();
    };

    let total_seconds = elapsed.as_secs();
    let days = (total_seconds / 86_400) as i64;
    let seconds_of_day = total_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}").into()
}

/// Days-since-epoch to civil date (Howard Hinnant's algorithm).
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use macsftp_core::Timestamp;

    use super::{format_size, format_size_label, format_timestamp};
    use macsftp_core::FileKind;

    #[test]
    fn size_labels_use_decimal_units() {
        assert_eq!(format_size(None).as_ref(), "—");

        // Directories render blank; the em dash is reserved for unknown file sizes.
        assert_eq!(format_size_label(FileKind::Directory, None).as_ref(), "");
        assert_eq!(
            format_size_label(FileKind::Directory, Some(4096)).as_ref(),
            ""
        );
        assert_eq!(format_size_label(FileKind::File, None).as_ref(), "—");
        assert_eq!(
            format_size_label(FileKind::File, Some(999)).as_ref(),
            "999 B"
        );
        assert_eq!(format_size_label(FileKind::Symlink, None).as_ref(), "—");
        assert_eq!(format_size(Some(0)).as_ref(), "0 B");
        assert_eq!(format_size(Some(999)).as_ref(), "999 B");
        assert_eq!(format_size(Some(1_500)).as_ref(), "1.5 KB");
        assert_eq!(format_size(Some(45_000_000)).as_ref(), "45 MB");
        assert_eq!(format_size(Some(2_300_000_000)).as_ref(), "2.3 GB");
    }

    #[test]
    fn timestamp_labels_render_utc_civil_dates() {
        assert_eq!(format_timestamp(None).as_ref(), "—");
        assert_eq!(
            format_timestamp(Some(Timestamp::from_secs_since_epoch(0))).as_ref(),
            "1970-01-01 00:00"
        );
        // 2026-07-01 12:30:00 UTC
        assert_eq!(
            format_timestamp(Some(Timestamp::from_secs_since_epoch(1_782_909_000))).as_ref(),
            "2026-07-01 12:30"
        );
    }
}
