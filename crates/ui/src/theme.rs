use gpui::{App, Global, Hsla, Pixels, SharedString, WindowAppearance, hsla, px, rgb};

/// Which of the two required token sets is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

/// Zed-style theme tokens: neutral surfaces, thin borders, one accent,
/// and fixed-meaning semantic colors. Stored as a GPUI global so every
/// view reads the same token set.
#[derive(Debug, Clone)]
pub struct Theme {
    pub appearance: Appearance,
    pub colors: ThemeColors,
    pub fonts: ThemeFonts,
    pub sizes: ThemeSizes,
}

impl Global for Theme {}

#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    /// Main working area behind file lists.
    pub background: Hsla,
    /// Chrome surfaces: tab bar, path bars, drawer, status bar.
    pub surface: Hsla,
    /// Modal, popover, and tooltip surfaces.
    pub elevated_surface: Hsla,
    pub border: Hsla,
    /// Focused pane / focused control indicator.
    pub border_focused: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_disabled: Hsla,
    pub element_hover: Hsla,
    pub element_active: Hsla,
    /// Selected rows and tabs; accent-tinted, low alpha.
    pub element_selected: Hsla,
    pub accent: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    pub success: Hsla,
    pub info: Hsla,
}

#[derive(Debug, Clone)]
pub struct ThemeFonts {
    pub ui_family: SharedString,
    pub mono_family: SharedString,
}

/// Fixed heights so hover/loading/progress never cause layout jitter.
#[derive(Debug, Clone, Copy)]
pub struct ThemeSizes {
    pub tab_bar_height: Pixels,
    pub path_bar_height: Pixels,
    pub table_header_height: Pixels,
    pub file_row_height: Pixels,
    pub transfer_row_height: Pixels,
    pub status_bar_height: Pixels,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            appearance: Appearance::Dark,
            colors: ThemeColors {
                background: rgb(0x1f2127).into(),
                surface: rgb(0x191b1f).into(),
                elevated_surface: rgb(0x25272d).into(),
                border: rgb(0x2e3138).into(),
                border_focused: rgb(0x5c7fb8).into(),
                text: rgb(0xc8ccd4).into(),
                text_muted: rgb(0x8b909b).into(),
                text_disabled: rgb(0x5c6067).into(),
                element_hover: hsla(0.0, 0.0, 1.0, 0.04),
                element_active: hsla(0.0, 0.0, 1.0, 0.08),
                element_selected: hsla(214.0 / 360.0, 0.6, 0.6, 0.16),
                accent: rgb(0x74ade8).into(),
                error: rgb(0xd07277).into(),
                warning: rgb(0xdec184).into(),
                success: rgb(0xa1c181).into(),
                info: rgb(0x6eb4bf).into(),
            },
            fonts: default_fonts(),
            sizes: default_sizes(),
        }
    }

    pub fn light() -> Self {
        Self {
            appearance: Appearance::Light,
            colors: ThemeColors {
                background: rgb(0xfafafa).into(),
                surface: rgb(0xefeff1).into(),
                elevated_surface: rgb(0xffffff).into(),
                border: rgb(0xd9dade).into(),
                border_focused: rgb(0x3b79c7).into(),
                text: rgb(0x33373e).into(),
                text_muted: rgb(0x71757d).into(),
                text_disabled: rgb(0xa5a8ae).into(),
                element_hover: hsla(0.0, 0.0, 0.0, 0.04),
                element_active: hsla(0.0, 0.0, 0.0, 0.08),
                element_selected: hsla(214.0 / 360.0, 0.55, 0.5, 0.14),
                accent: rgb(0x3b79c7).into(),
                error: rgb(0xb3423f).into(),
                warning: rgb(0x9a7b27).into(),
                success: rgb(0x5e8a46).into(),
                info: rgb(0x3a7e8a).into(),
            },
            fonts: default_fonts(),
            sizes: default_sizes(),
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }
}

fn default_fonts() -> ThemeFonts {
    ThemeFonts {
        // ".SystemUIFont" resolves to the macOS system font in GPUI.
        ui_family: ".SystemUIFont".into(),
        mono_family: "Menlo".into(),
    }
}

fn default_sizes() -> ThemeSizes {
    ThemeSizes {
        tab_bar_height: px(34.0),
        path_bar_height: px(32.0),
        table_header_height: px(26.0),
        file_row_height: px(26.0),
        transfer_row_height: px(44.0),
        status_bar_height: px(26.0),
    }
}

/// Read the active theme anywhere a `&App` is reachable.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, Theme};

    #[test]
    fn dark_and_light_token_sets_are_both_defined_and_distinct() {
        let dark = Theme::dark();
        let light = Theme::light();

        assert_eq!(dark.appearance, Appearance::Dark);
        assert_eq!(light.appearance, Appearance::Light);
        assert_ne!(dark.colors.background, light.colors.background);
        assert_ne!(dark.colors.text, light.colors.text);
    }

    #[test]
    fn ui_and_mono_font_families_are_separate_tokens() {
        let theme = Theme::dark();

        assert_ne!(theme.fonts.ui_family, theme.fonts.mono_family);
    }
}
