use gpui::{Hsla, Pixels, Styled, Svg, px, svg};

/// Icon names resolved against the application's `AssetSource`.
/// The app crate embeds the SVG bytes; `ui` only knows the paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Plus,
    Close,
    ArrowUp,
    Refresh,
    Copy,
    Upload,
    Download,
    Folder,
    File,
    Symlink,
    Trash,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Transfers,
    Server,
}

impl IconName {
    pub fn path(self) -> &'static str {
        match self {
            Self::Plus => "icons/plus.svg",
            Self::Close => "icons/close.svg",
            Self::ArrowUp => "icons/arrow_up.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::Copy => "icons/copy.svg",
            Self::Upload => "icons/upload.svg",
            Self::Download => "icons/download.svg",
            Self::Folder => "icons/folder.svg",
            Self::File => "icons/file.svg",
            Self::Symlink => "icons/symlink.svg",
            Self::Trash => "icons/trash.svg",
            Self::ChevronDown => "icons/chevron_down.svg",
            Self::ChevronLeft => "icons/chevron_left.svg",
            Self::ChevronRight => "icons/chevron_right.svg",
            Self::Transfers => "icons/transfers.svg",
            Self::Server => "icons/server.svg",
        }
    }
}

pub const ICON_SIZE: Pixels = px(14.0);

pub fn icon(name: IconName, color: Hsla) -> Svg {
    icon_with_size(name, color, ICON_SIZE)
}

pub(crate) fn icon_with_size(name: IconName, color: Hsla, size: Pixels) -> Svg {
    svg()
        .path(name.path())
        .w(size)
        .h(size)
        .flex_none()
        .text_color(color)
}
