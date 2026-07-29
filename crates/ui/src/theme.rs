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
    /// Custom scrollbar thumb (resting).
    pub scrollbar_thumb: Hsla,
    /// Custom scrollbar thumb on hover.
    pub scrollbar_thumb_hover: Hsla,
    /// Custom scrollbar thumb while being dragged.
    pub scrollbar_thumb_active: Hsla,
    /// Custom scrollbar track background (transparent by default).
    pub scrollbar_track: Hsla,
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
    /// Width of the custom scrollbar (track + thumb).
    pub scrollbar_width: Pixels,
}

impl Theme {
    /// One Dark is the default palette for every dark appearance mode.
    pub fn one_dark() -> Self {
        Self {
            appearance: Appearance::Dark,
            colors: ThemeColors {
                background: rgb(0x282c34).into(),
                surface: rgb(0x21252b).into(),
                elevated_surface: rgb(0x2c313a).into(),
                border: rgb(0x3e4451).into(),
                border_focused: rgb(0x61afef).into(),
                text: rgb(0xabb2bf).into(),
                text_muted: rgb(0x828997).into(),
                text_disabled: rgb(0x5c6370).into(),
                element_hover: hsla(0.0, 0.0, 1.0, 0.04),
                element_active: hsla(0.0, 0.0, 1.0, 0.08),
                element_selected: hsla(214.0 / 360.0, 0.6, 0.6, 0.16),
                accent: rgb(0x61afef).into(),
                error: rgb(0xe06c75).into(),
                warning: rgb(0xe5c07b).into(),
                success: rgb(0x98c379).into(),
                info: rgb(0x56b6c2).into(),
                scrollbar_thumb: hsla(0.0, 0.0, 1.0, 0.22),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 1.0, 0.36),
                scrollbar_thumb_active: hsla(0.0, 0.0, 1.0, 0.50),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
            },
            fonts: default_fonts(),
            sizes: default_sizes(),
        }
    }

    /// One Light is the default palette for every light appearance mode.
    pub fn one_light() -> Self {
        Self {
            appearance: Appearance::Light,
            colors: ThemeColors {
                background: rgb(0xfafafa).into(),
                surface: rgb(0xf0f0f0).into(),
                elevated_surface: rgb(0xffffff).into(),
                border: rgb(0xd3d4d5).into(),
                border_focused: rgb(0x4078f2).into(),
                text: rgb(0x383a42).into(),
                text_muted: rgb(0x696c77).into(),
                text_disabled: rgb(0xa0a1a7).into(),
                element_hover: hsla(0.0, 0.0, 0.0, 0.04),
                element_active: hsla(0.0, 0.0, 0.0, 0.08),
                element_selected: hsla(214.0 / 360.0, 0.55, 0.5, 0.14),
                accent: rgb(0x4078f2).into(),
                error: rgb(0xe45649).into(),
                warning: rgb(0xc18401).into(),
                success: rgb(0x50a14f).into(),
                info: rgb(0x0184bc).into(),
                scrollbar_thumb: hsla(0.0, 0.0, 0.0, 0.30),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 0.0, 0.45),
                scrollbar_thumb_active: hsla(0.0, 0.0, 0.0, 0.55),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
            },
            fonts: default_fonts(),
            sizes: default_sizes(),
        }
    }

    pub fn dark() -> Self {
        Self::one_dark()
    }

    pub fn light() -> Self {
        Self::one_light()
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::one_dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::one_light(),
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
        scrollbar_width: px(10.0),
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
    use gpui::{WindowAppearance, px, rgb};

    use super::{Appearance, Theme};

    #[test]
    fn dark_and_light_token_sets_are_both_defined_and_distinct() {
        let dark = Theme::one_dark();
        let light = Theme::one_light();

        assert_eq!(dark.appearance, Appearance::Dark);
        assert_eq!(light.appearance, Appearance::Light);
        assert_ne!(dark.colors.background, light.colors.background);
        assert_ne!(dark.colors.text, light.colors.text);
    }

    #[test]
    fn dark_appearance_defaults_to_one_dark_tokens() {
        let theme = Theme::dark();

        assert_eq!(theme.colors.background, rgb(0x282c34).into());
        assert_eq!(theme.colors.text, rgb(0xabb2bf).into());
        assert_eq!(theme.colors.accent, rgb(0x61afef).into());
        assert_eq!(theme.colors.error, rgb(0xe06c75).into());
    }

    #[test]
    fn light_appearance_defaults_to_one_light_tokens() {
        let theme = Theme::light();

        assert_eq!(theme.colors.background, rgb(0xfafafa).into());
        assert_eq!(theme.colors.text, rgb(0x383a42).into());
        assert_eq!(theme.colors.accent, rgb(0x4078f2).into());
        assert_eq!(theme.colors.error, rgb(0xe45649).into());
    }

    #[test]
    fn system_appearance_selects_the_matching_one_theme() {
        let dark = Theme::for_appearance(WindowAppearance::Dark);
        let light = Theme::for_appearance(WindowAppearance::Light);

        assert_eq!(dark.colors.background, Theme::one_dark().colors.background);
        assert_eq!(
            light.colors.background,
            Theme::one_light().colors.background
        );
    }

    #[test]
    fn ui_and_mono_font_families_are_separate_tokens() {
        let theme = Theme::dark();

        assert_ne!(theme.fonts.ui_family, theme.fonts.mono_family);
    }

    #[test]
    fn scrollbar_tokens_are_defined_and_distinct_per_appearance() {
        let dark = Theme::one_dark();
        let light = Theme::one_light();

        // Both appearances define all four scrollbar color tokens.
        assert_ne!(dark.colors.scrollbar_thumb, light.colors.scrollbar_thumb);
        assert_ne!(
            dark.colors.scrollbar_thumb_hover,
            light.colors.scrollbar_thumb_hover
        );
        assert_ne!(
            dark.colors.scrollbar_thumb_active,
            light.colors.scrollbar_thumb_active
        );
        // Track is transparent in both, but the field must exist.
        assert_eq!(dark.colors.scrollbar_track, light.colors.scrollbar_track);

        // A scrollbar width token exists and is positive.
        assert!(dark.sizes.scrollbar_width > px(0.0));
        assert_eq!(dark.sizes.scrollbar_width, light.sizes.scrollbar_width);
    }
}
