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

const UI_FONT_SIZE: f32 = 13.0;

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
    // Preserve the Component 0.7.1 input palette while owning its presentation.
    pub input_placeholder: Hsla,
    pub input_caret: Hsla,
    pub input_selection: Hsla,
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
    pub button_height: Pixels,
    pub button_min_width: Pixels,
    pub button_text_size: Pixels,
    pub button_padding_x: Pixels,
    pub button_radius: Pixels,
    pub button_focus_ring: Pixels,
    pub icon_button_width: Pixels,
    pub icon_button_height: Pixels,
    pub input_height: Pixels,
    pub input_text_size: Pixels,
    pub input_padding_x: Pixels,
    pub input_padding_y: Pixels,
    pub input_radius: Pixels,
    pub input_focus_ring: Pixels,
}

impl Theme {
    pub fn input_background(&self, disabled: bool) -> Hsla {
        let mut background = self.colors.background;
        if disabled {
            background.a *= 0.4;
        } else if self.appearance == Appearance::Dark {
            background.a *= 0.3;
        }
        background
    }

    /// Project tokens are the single source for Base behavior and presentation.
    pub fn install(self, cx: &mut App) {
        init(cx);
        let colors = gpui_base::ColorTokens {
            background: self.colors.background,
            foreground: self.colors.text,
            surface: self.colors.elevated_surface,
            surface_foreground: self.colors.text,
            primary: self.colors.accent,
            primary_foreground: self.colors.background,
            secondary: self.colors.surface,
            secondary_foreground: self.colors.text,
            muted: self.colors.surface,
            muted_foreground: self.colors.input_placeholder,
            accent: self.colors.element_hover,
            accent_foreground: self.colors.text,
            destructive: self.colors.error,
            destructive_foreground: self.colors.background,
            border: self.colors.border,
            input: self.colors.background,
            ring: self.colors.border_focused,
            selection: self.colors.input_selection,
        };
        let mut tokens = gpui_base::SemanticThemeTokens {
            colors,
            ..Default::default()
        };
        tokens.typography.md.size = px(UI_FONT_SIZE);
        tokens.typography.sans = self.fonts.ui_family.clone();
        tokens.typography.mono = self.fonts.mono_family.clone();
        cx.set_global(gpui_base::Theme {
            appearance: match self.appearance {
                Appearance::Dark => gpui_base::ThemeAppearance::Dark,
                Appearance::Light => gpui_base::ThemeAppearance::Light,
            },
            tokens,
            ..Default::default()
        });
        cx.set_global(self);
        cx.refresh_windows();
    }

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
                input_placeholder: rgb(0xa3a3a3).into(),
                input_caret: rgb(0xfafafa).into(),
                input_selection: rgb(0x1d4ed8).into(),
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
                input_placeholder: rgb(0x737373).into(),
                input_caret: rgb(0x0a0a0a).into(),
                input_selection: rgb(0x55a0fc).into(),
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
        button_height: px(26.0),
        button_min_width: px(UI_FONT_SIZE * 1.5),
        button_text_size: px(UI_FONT_SIZE * 0.875),
        button_padding_x: px(UI_FONT_SIZE * 0.75),
        button_radius: px(UI_FONT_SIZE * 0.25),
        button_focus_ring: px(3.0),
        // Component expands the 22px icon control to fit its padded 16px child.
        icon_button_width: px(26.0),
        icon_button_height: px(22.0),
        // Match the rendered Component Small input (1.5rem at the 13px UI font).
        input_height: px(UI_FONT_SIZE * 1.5),
        input_text_size: px(UI_FONT_SIZE * 0.875),
        input_padding_x: px(8.0),
        input_padding_y: px(2.0),
        input_radius: px(3.0),
        input_focus_ring: px(3.0),
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

struct UiInitialized;
impl Global for UiInitialized {}

/// Register window defaults once, before constructing any Base Root.
pub fn init(cx: &mut App) {
    if cx.has_global::<UiInitialized>() {
        return;
    }
    gpui_base::init(cx);
    gpui_base::Root::register_plugin::<WindowPresentation>(cx, |_, _| WindowPresentation);
    cx.set_global(UiInitialized);
}

struct WindowPresentation;

impl gpui::Render for WindowPresentation {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
    }
}

impl gpui_base::RootPlugin for WindowPresentation {
    fn prepare(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        window.set_rem_size(px(UI_FONT_SIZE));
        gpui_base::TextSelection::activate_scope(Default::default(), window, cx);
    }

    fn style(
        &self,
        surface: &mut gpui::Stateful<gpui::Div>,
        _window: &mut gpui::Window,
        cx: &mut App,
    ) {
        use gpui::Styled as _;
        let theme = cx.theme();
        use gpui::Refineable as _;
        surface.style().refine(
            &gpui::StyleRefinement::default()
                .font_family(theme.fonts.ui_family.clone())
                .text_size(px(UI_FONT_SIZE))
                .bg(theme.colors.background)
                .text_color(theme.colors.text),
        );
    }
}

#[cfg(test)]
mod tests {
    #[gpui::test]
    fn root_defaults_follow_theme_across_two_windows(cx: &mut gpui::TestAppContext) {
        use gpui::{AppContext as _, ParentElement as _};
        struct Content;
        impl gpui::Render for Content {
            fn render(
                &mut self,
                _window: &mut gpui::Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                gpui::div().child("window content")
            }
        }
        cx.update(|cx| super::Theme::one_dark().install(cx));
        let first = cx.add_window(|window, cx| {
            let content = cx.new(|_| Content);
            gpui_base::Root::new(content, window, cx)
        });
        let second = cx.add_window(|window, cx| {
            let content = cx.new(|_| Content);
            gpui_base::Root::new(content, window, cx)
        });
        for theme in [super::Theme::one_light(), super::Theme::one_dark()] {
            let expected = theme.colors.background;
            cx.update(|cx| theme.install(cx));
            for handle in [first, second] {
                let handle: gpui::AnyWindowHandle = handle.into();
                handle
                    .update(cx, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        assert_eq!(window.rem_size(), gpui::px(super::UI_FONT_SIZE));
                        let base = cx.global::<gpui_base::Theme>();
                        assert_eq!(base.tokens.colors.background, expected);
                        assert_eq!(
                            base.tokens.typography.md.size,
                            gpui::px(super::UI_FONT_SIZE)
                        );
                    })
                    .expect("both open Root windows must render after a theme switch");
            }
        }
    }

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
