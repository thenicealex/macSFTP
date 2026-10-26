use gpui_kit::{
    App, BoxShadow, Global, Hsla, Pixels, SharedString, WindowAppearance, hsla, px, rgb,
};

/// Which of the two required token sets is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

/// Apple native theme tokens: neutral surfaces, thin hairline borders, system accent,
/// and fixed-meaning semantic colors matching macOS HIG. Stored as a GPUI global so
/// every view reads the same token set.
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
    /// Text on filled accent or destructive buttons/badges (white in both appearances).
    pub text_on_accent: Hsla,
    // Preserve the Component 0.7.1 input palette while owning its presentation.
    pub input_placeholder: Hsla,
    pub input_caret: Hsla,
    pub input_selection: Hsla,
    pub element_hover: Hsla,
    pub element_active: Hsla,
    /// Selected rows and tabs; accent-tinted, low alpha.
    pub element_selected: Hsla,
    /// Knob of an on/off switch. Kept light in both themes so it reads
    /// against the accent-filled on state.
    pub switch_knob: Hsla,
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
    pub modal_radius: Pixels,
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

    /// Elevation scale for floating surfaces. Every overlay takes one of the
    /// three steps instead of assembling its own shadow.
    ///
    /// Elevation reads through the border and the scrim first; the shadow only
    /// softens the edge. §12.15 forbids heavy shadows, so all three steps stay
    /// light, and the scale is kept ordered so a lower surface can never look
    /// further from the page than a higher one.
    ///
    /// - [`Theme::shadow_chip`]: tooltips and other one-line affordances.
    /// - [`Theme::shadow_popover`]: palettes, the tab switcher, drag previews.
    /// - [`Theme::shadow_modal`]: modal and sheet cards.
    pub fn shadow_chip(&self) -> Vec<BoxShadow> {
        let color = hsla(0.0, 0.0, 0.0, 0.08);
        vec![
            BoxShadow::new(px(0.), px(1.), color)
                .blur_radius(px(3.))
                .spread_radius(px(-1.)),
        ]
    }

    /// See [`Theme::shadow_chip`] for the scale.
    pub fn shadow_popover(&self) -> Vec<BoxShadow> {
        let color = hsla(0.0, 0.0, 0.0, 0.10);
        vec![
            BoxShadow::new(px(0.), px(4.), color)
                .blur_radius(px(6.))
                .spread_radius(px(-1.)),
            BoxShadow::new(px(0.), px(2.), color)
                .blur_radius(px(4.))
                .spread_radius(px(-2.)),
        ]
    }

    /// See [`Theme::shadow_chip`] for the scale.
    pub fn shadow_modal(&self) -> Vec<BoxShadow> {
        let color = hsla(0.0, 0.0, 0.0, 0.12);
        vec![
            BoxShadow::new(px(0.), px(8.), color)
                .blur_radius(px(16.))
                .spread_radius(px(-4.)),
            BoxShadow::new(px(0.), px(2.), color)
                .blur_radius(px(6.))
                .spread_radius(px(-2.)),
        ]
    }

    /// Dimming behind a task-modal overlay: a dialog, the command palette, the
    /// tab switcher. One value for every scrim so the backdrop reads the same
    /// wherever it appears; it comes from here rather than from a literal at
    /// the call site.
    pub fn scrim(&self) -> Hsla {
        hsla(0.0, 0.0, 0.0, 0.45)
    }

    /// Hairline along the window's top edge, where chrome meets the frame.
    ///
    /// macOS windows catch the light along their top edge: a light line in
    /// dark appearance, a barely darker one in light appearance. §12.15 rules
    /// out translucent chrome, so this hairline is the only material cue the
    /// chrome gets.
    pub fn chrome_highlight(&self) -> Hsla {
        match self.appearance {
            Appearance::Dark => hsla(0.0, 0.0, 1.0, 0.08),
            Appearance::Light => hsla(0.0, 0.0, 0.0, 0.06),
        }
    }

    /// Project tokens are the single source for Base behavior and presentation.
    pub fn install(self, cx: &mut App) {
        init(cx);
        let colors = gpui_kit::base::ColorTokens {
            background: self.colors.background,
            foreground: self.colors.text,
            surface: self.colors.elevated_surface,
            surface_foreground: self.colors.text,
            primary: self.colors.accent,
            primary_foreground: self.colors.text_on_accent,
            secondary: self.colors.surface,
            secondary_foreground: self.colors.text,
            muted: self.colors.surface,
            muted_foreground: self.colors.input_placeholder,
            accent: self.colors.element_hover,
            accent_foreground: self.colors.text,
            destructive: self.colors.error,
            destructive_foreground: self.colors.text_on_accent,
            border: self.colors.border,
            input: self.colors.background,
            ring: self.colors.border_focused,
            selection: self.colors.input_selection,
        };
        let mut tokens = gpui_kit::base::SemanticThemeTokens {
            colors,
            ..Default::default()
        };
        tokens.typography.md.size = px(UI_FONT_SIZE);
        tokens.typography.sans = self.fonts.ui_family.clone();
        tokens.typography.mono = self.fonts.mono_family.clone();
        cx.set_global(gpui_kit::base::Theme {
            appearance: match self.appearance {
                Appearance::Dark => gpui_kit::base::ThemeAppearance::Dark,
                Appearance::Light => gpui_kit::base::ThemeAppearance::Light,
            },
            tokens,
            ..Default::default()
        });
        cx.set_global(self);
        cx.refresh_windows();
    }

    /// macOS Dark is the default palette for dark appearance (macOS Sonoma / Sequoia HIG).
    pub fn macos_dark() -> Self {
        Self {
            appearance: Appearance::Dark,
            colors: ThemeColors {
                background: rgb(0x1e1e1e).into(),
                surface: rgb(0x262628).into(),
                elevated_surface: rgb(0x2c2c2e).into(),
                border: rgb(0x38383a).into(),
                border_focused: rgb(0x0a84ff).into(),
                text: rgb(0xf5f5f7).into(),
                text_muted: rgb(0x8e8e93).into(),
                text_disabled: rgb(0x636366).into(),
                text_on_accent: rgb(0xffffff).into(),
                input_placeholder: rgb(0x8e8e93).into(),
                input_caret: rgb(0x0a84ff).into(),
                input_selection: hsla(214.0 / 360.0, 1.0, 0.60, 0.35),
                element_hover: hsla(0.0, 0.0, 1.0, 0.06),
                element_active: hsla(0.0, 0.0, 1.0, 0.10),
                element_selected: hsla(211.0 / 360.0, 1.0, 0.52, 0.20),
                switch_knob: rgb(0xffffff).into(),
                accent: rgb(0x0a84ff).into(),
                error: rgb(0xff453a).into(),
                warning: rgb(0xff9f0a).into(),
                success: rgb(0x30d158).into(),
                info: rgb(0x64d2ff).into(),
                scrollbar_thumb: hsla(0.0, 0.0, 1.0, 0.25),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 1.0, 0.40),
                scrollbar_thumb_active: hsla(0.0, 0.0, 1.0, 0.55),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
            },
            fonts: default_fonts(),
            sizes: default_sizes(),
        }
    }

    /// One Dark is retained as a compatibility alias for [`Theme::macos_dark`].
    pub fn one_dark() -> Self {
        Self::macos_dark()
    }

    /// macOS Light is the default palette for light appearance (macOS Sonoma / Sequoia HIG).
    pub fn macos_light() -> Self {
        Self {
            appearance: Appearance::Light,
            colors: ThemeColors {
                background: rgb(0xffffff).into(),
                surface: rgb(0xf5f5f7).into(),
                elevated_surface: rgb(0xffffff).into(),
                border: rgb(0xd1d1d6).into(),
                border_focused: rgb(0x007aff).into(),
                text: rgb(0x1d1d1f).into(),
                text_muted: rgb(0x86868b).into(),
                text_disabled: rgb(0xc7c7cc).into(),
                text_on_accent: rgb(0xffffff).into(),
                input_placeholder: rgb(0x86868b).into(),
                input_caret: rgb(0x007aff).into(),
                input_selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.25),
                element_hover: hsla(0.0, 0.0, 0.0, 0.05),
                element_active: hsla(0.0, 0.0, 0.0, 0.09),
                element_selected: hsla(211.0 / 360.0, 1.0, 0.50, 0.14),
                switch_knob: rgb(0xffffff).into(),
                accent: rgb(0x007aff).into(),
                error: rgb(0xff3b30).into(),
                warning: rgb(0xff9500).into(),
                success: rgb(0x34c759).into(),
                info: rgb(0x0071a4).into(),
                scrollbar_thumb: hsla(0.0, 0.0, 0.0, 0.28),
                scrollbar_thumb_hover: hsla(0.0, 0.0, 0.0, 0.42),
                scrollbar_thumb_active: hsla(0.0, 0.0, 0.0, 0.56),
                scrollbar_track: hsla(0.0, 0.0, 0.0, 0.0),
            },
            fonts: default_fonts(),
            sizes: default_sizes(),
        }
    }

    /// One Light is retained as a compatibility alias for [`Theme::macos_light`].
    pub fn one_light() -> Self {
        Self::macos_light()
    }

    pub fn dark() -> Self {
        Self::macos_dark()
    }

    pub fn light() -> Self {
        Self::macos_light()
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
        scrollbar_width: px(10.0),
        button_height: px(26.0),
        button_min_width: px(UI_FONT_SIZE * 1.5),
        button_text_size: px(UI_FONT_SIZE * 0.875),
        button_padding_x: px(UI_FONT_SIZE * 0.75),
        button_radius: px(5.0),
        button_focus_ring: px(3.0),
        modal_radius: px(10.0),
        // Component expands the 22px icon control to fit its padded 16px child.
        icon_button_width: px(26.0),
        icon_button_height: px(22.0),
        // Match the rendered Component Small input (1.5rem at the 13px UI font).
        input_height: px(UI_FONT_SIZE * 1.5),
        input_text_size: px(UI_FONT_SIZE * 0.875),
        input_padding_x: px(8.0),
        input_padding_y: px(2.0),
        input_radius: px(5.0),
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
    gpui_kit::base::init(cx);
    gpui_kit::base::Root::register_plugin::<WindowPresentation>(cx, |_, _| WindowPresentation);
    cx.set_global(UiInitialized);
}

struct WindowPresentation;

impl gpui_kit::Render for WindowPresentation {
    fn render(
        &mut self,
        _window: &mut gpui_kit::Window,
        _cx: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        gpui_kit::div()
    }
}

impl gpui_kit::base::RootPlugin for WindowPresentation {
    fn prepare(&mut self, window: &mut gpui_kit::Window, cx: &mut gpui_kit::Context<Self>) {
        window.set_rem_size(px(UI_FONT_SIZE));
        gpui_kit::base::TextSelection::activate_scope(Default::default(), window, cx);
    }

    fn style(
        &self,
        surface: &mut gpui_kit::Stateful<gpui_kit::Div>,
        _window: &mut gpui_kit::Window,
        cx: &mut App,
    ) {
        use gpui_kit::Styled as _;
        let theme = cx.theme();
        use gpui_kit::Refineable as _;
        surface.style().refine(
            &gpui_kit::StyleRefinement::default()
                .font_family(theme.fonts.ui_family.clone())
                .text_size(px(UI_FONT_SIZE))
                .bg(theme.colors.background)
                .text_color(theme.colors.text),
        );
    }
}

#[cfg(test)]
mod tests {
    #[gpui_kit::test]
    fn root_defaults_follow_theme_across_two_windows(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{AppContext as _, ParentElement as _};
        struct Content;
        impl gpui_kit::Render for Content {
            fn render(
                &mut self,
                _window: &mut gpui_kit::Window,
                _cx: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                gpui_kit::div().child("window content")
            }
        }
        cx.update(|cx| super::Theme::one_dark().install(cx));
        let first = cx.add_window(|window, cx| {
            let content = cx.new(|_| Content);
            gpui_kit::base::Root::new(content, window, cx)
        });
        let second = cx.add_window(|window, cx| {
            let content = cx.new(|_| Content);
            gpui_kit::base::Root::new(content, window, cx)
        });
        for theme in [super::Theme::one_light(), super::Theme::one_dark()] {
            let expected = theme.colors.background;
            cx.update(|cx| theme.install(cx));
            for handle in [first, second] {
                let handle: gpui_kit::AnyWindowHandle = handle.into();
                handle
                    .update(cx, |_, window, cx| {
                        window.draw(cx).clear(cx);
                        assert_eq!(window.rem_size(), gpui_kit::px(super::UI_FONT_SIZE));
                        let base = cx.global::<gpui_kit::base::Theme>();
                        assert_eq!(base.tokens.colors.background, expected);
                        assert_eq!(
                            base.tokens.typography.md.size,
                            gpui_kit::px(super::UI_FONT_SIZE)
                        );
                    })
                    .expect("both open Root windows must render after a theme switch");
            }
        }
    }

    #[test]
    fn elevation_shadow_token_stays_light() {
        // §12.15 forbids heavy shadows: every step may only soften an edge, in
        // both appearances, and every call site takes it from here.
        for theme in [Theme::one_dark(), Theme::one_light()] {
            let steps = [
                ("chip", theme.shadow_chip()),
                ("popover", theme.shadow_popover()),
                ("modal", theme.shadow_modal()),
            ];
            let mut previous_reach = px(0.0);
            for (name, shadows) in steps {
                assert!(
                    !shadows.is_empty(),
                    "{name} needs at least one soft edge to read as elevated"
                );
                for shadow in &shadows {
                    assert!(
                        shadow.color.a <= 0.15,
                        "{name} shadow alpha {} is heavier than §12.15 allows",
                        shadow.color.a
                    );
                    assert!(
                        shadow.blur_radius <= px(16.0),
                        "{name} shadow blur {} stops reading as an edge",
                        shadow.blur_radius
                    );
                    assert!(shadow.offset.y >= px(0.0), "overlays cast shadows downward");
                    assert!(!shadow.inset, "overlay shadows are never inset");
                }
                // The steps must stay ordered: a tooltip may never look further
                // from the page than the modal that contains it.
                let reach = shadows
                    .iter()
                    .map(|shadow| shadow.offset.y + shadow.blur_radius)
                    .fold(px(0.0), |widest, next| widest.max(next));
                assert!(
                    reach > previous_reach,
                    "{name} must sit above the step below it"
                );
                previous_reach = reach;
            }
        }
    }

    #[test]
    fn scrim_stays_a_dimming_layer() {
        // One value for every backdrop: dark enough to separate the card from
        // the page, light enough that the page stays readable behind it.
        let scrim = Theme::one_dark().scrim();
        assert_eq!(scrim, Theme::one_light().scrim());
        assert!(
            (0.3..=0.6).contains(&scrim.a),
            "scrim alpha {} is not a dimming layer",
            scrim.a
        );
    }

    #[test]
    fn chrome_highlight_is_a_hairline_in_both_appearances() {
        // One hairline of alpha; anything stronger would read as a drawn border
        // and §12.15's no-glass rule means the chrome has no other material cue.
        for theme in [Theme::one_dark(), Theme::one_light()] {
            let highlight = theme.chrome_highlight();
            assert!(
                highlight.a > 0.0 && highlight.a <= 0.1,
                "chrome highlight alpha {} is not a hairline",
                highlight.a
            );
        }
        let dark = Theme::one_dark().chrome_highlight();
        let light = Theme::one_light().chrome_highlight();
        assert!(
            dark.l > light.l,
            "dark chrome catches a light line, light chrome a darker one"
        );
    }

    use gpui_kit::{WindowAppearance, px, rgb};

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
    fn dark_appearance_defaults_to_macos_dark_tokens() {
        let theme = Theme::dark();

        assert_eq!(theme.colors.background, rgb(0x1e1e1e).into());
        assert_eq!(theme.colors.text, rgb(0xf5f5f7).into());
        assert_eq!(theme.colors.accent, rgb(0x0a84ff).into());
        assert_eq!(theme.colors.error, rgb(0xff453a).into());
        assert_eq!(theme.colors.text_on_accent, rgb(0xffffff).into());
    }

    #[test]
    fn light_appearance_defaults_to_macos_light_tokens() {
        let theme = Theme::light();

        assert_eq!(theme.colors.background, rgb(0xffffff).into());
        assert_eq!(theme.colors.text, rgb(0x1d1d1f).into());
        assert_eq!(theme.colors.accent, rgb(0x007aff).into());
        assert_eq!(theme.colors.error, rgb(0xff3b30).into());
        assert_eq!(theme.colors.text_on_accent, rgb(0xffffff).into());
    }

    #[test]
    fn system_appearance_selects_the_matching_one_theme() {
        let dark = Theme::for_appearance(WindowAppearance::Dark);
        let light = Theme::for_appearance(WindowAppearance::Light);

        assert_eq!(
            dark.colors.background,
            Theme::macos_dark().colors.background
        );
        assert_eq!(
            light.colors.background,
            Theme::macos_light().colors.background
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
