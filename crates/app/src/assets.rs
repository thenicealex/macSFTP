use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Embedded asset source: ships the icon SVGs inside the binary so the
/// app bundle needs no external resource directory.
pub struct Assets;

macro_rules! embedded_icons {
    ($($name:literal),+ $(,)?) => {
        fn lookup(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("icons/", $name) => {
                    Some(include_bytes!(concat!("../assets/icons/", $name)).as_slice())
                })+
                _ => None,
            }
        }

        fn icon_paths() -> Vec<SharedString> {
            vec![$(concat!("icons/", $name).into()),+]
        }
    };
}

embedded_icons!(
    "plus.svg",
    "close.svg",
    "arrow_up.svg",
    "refresh.svg",
    "copy.svg",
    "upload.svg",
    "download.svg",
    "folder.svg",
    "file.svg",
    "symlink.svg",
    "trash.svg",
    "chevron_down.svg",
    "chevron_left.svg",
    "chevron_right.svg",
    "transfers.svg",
    "server.svg",
);

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(lookup(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(icon_paths()
            .into_iter()
            .filter(|icon_path| icon_path.starts_with(path))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource;
    use macsftp_ui::IconName;

    use super::Assets;

    #[test]
    fn every_icon_name_resolves_to_an_embedded_asset() {
        let icons = [
            IconName::Plus,
            IconName::Close,
            IconName::ArrowUp,
            IconName::Refresh,
            IconName::Copy,
            IconName::Upload,
            IconName::Download,
            IconName::Folder,
            IconName::File,
            IconName::Symlink,
            IconName::Trash,
            IconName::ChevronDown,
            IconName::ChevronLeft,
            IconName::ChevronRight,
            IconName::Transfers,
            IconName::Server,
        ];

        for icon in icons {
            let loaded = Assets
                .load(icon.path())
                .expect("asset lookup must not error");
            assert!(loaded.is_some(), "missing embedded asset for {icon:?}");
        }
    }
}
