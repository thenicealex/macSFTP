mod components;
mod file_list;
mod icon;
mod input;
mod scrollbar;
mod tab;
mod theme;
mod transfer_row;
mod workspace_widgets;

pub use components::{
    IconButton, TextButton, Tooltip, empty_state, icon_button, loading_state, text_button,
    text_tooltip,
};
pub use file_list::{
    FileRowModel, MODIFIED_COLUMN_WIDTH, SIZE_COLUMN_WIDTH, file_row, file_table_header,
    format_size, format_size_label, format_timestamp,
};
pub use icon::{ICON_SIZE, IconName, icon};
pub use input::{InputKeyResult, InputState, SecretInputState, TextFieldModel, text_field};
pub use scrollbar::{
    MIN_THUMB, Scrollbar, ScrollbarState, ThumbGeometry, scroll_area, thumb_geometry,
};
pub use tab::{Tab, tab};
pub use theme::{ActiveTheme, Appearance, Theme, ThemeColors, ThemeFonts, ThemeSizes};
pub use transfer_row::{TransferRow, transfer_row};
pub use workspace_widgets::{
    DragPreview, connection_status, copy_name, section_header_static, transfer_title,
};

pub fn crate_name() -> &'static str {
    "macsftp-ui"
}
