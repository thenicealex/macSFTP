use gpui::prelude::FluentBuilder as _;
use gpui::{
    AccessibleAction, App, AppContext as _, Context, ElementId, Entity, EntityInputHandler as _,
    Focusable as _, InteractiveElement, IntoElement, Keystroke, MouseButton, ParentElement,
    RenderOnce, Role, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    px, relative,
};
use gpui_base::{
    InputBase,
    input::{InputEditorStyle, InputState as BaseInputState},
};
use std::rc::Rc;
use zeroize::Zeroize;

use crate::theme::ActiveTheme;

/// Retains Base editing state while the caller owns the authoritative draft.
/// Programmatic draft changes synchronize only when text differs, preserving
/// selection, undo history, and IME composition through unrelated redraws.
pub struct PlainInput {
    state: Entity<gpui_base::input::InputState>,
    _subscription: Subscription,
}

impl PlainInput {
    pub fn new<V: 'static>(
        value: &str,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<V>,
        on_event: impl Fn(
            &mut V,
            &Entity<gpui_base::input::InputState>,
            &gpui_base::input::InputEvent,
            &mut Window,
            &mut Context<V>,
        ) + 'static,
    ) -> Self {
        let state = cx.new(|cx| {
            gpui_base::input::InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value.to_string())
        });
        let subscription = cx.subscribe_in(&state, window, on_event);
        Self {
            state,
            _subscription: subscription,
        }
    }

    pub fn state(&self) -> &Entity<gpui_base::input::InputState> {
        &self.state
    }

    pub fn sync(&self, value: &str, window: &mut Window, cx: &mut App) {
        if self.state.read(cx).value().as_ref() != value {
            self.state.update(cx, |state, cx| {
                // IME preedit may not have emitted Change yet; a passive
                // parent redraw must not replace the marked text with the draft.
                if state.marked_text_range(window, cx).is_none() {
                    state.set_value(value.to_string(), window, cx);
                }
            });
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle(cx).is_focused(window)
    }
}

pub fn plain_text_field(
    id: impl Into<ElementId>,
    input: &PlainInput,
    _cx: &App,
) -> impl IntoElement {
    ordinary_text_field(id, input.state())
}

/// Project-owned presentation for a retained Base editing entity.
#[derive(IntoElement)]
pub struct OrdinaryTextField {
    id: ElementId,
    state: Entity<BaseInputState>,
}

pub fn ordinary_text_field(
    id: impl Into<ElementId>,
    state: &Entity<BaseInputState>,
) -> OrdinaryTextField {
    OrdinaryTextField {
        id: id.into(),
        state: state.clone(),
    }
}

impl RenderOnce for OrdinaryTextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.state.update(cx, |state, _| {
            state.set_editor_style(InputEditorStyle {
                foreground: theme.colors.text,
                muted_foreground: theme.colors.input_placeholder,
                background: theme.input_background(state.presentation().is_disabled()),
                border: theme.colors.border,
                selection: theme.colors.input_selection,
                caret: theme.colors.input_caret,
                ..Default::default()
            });
            state.on_context_menu(Rc::new(show_input_context_menu));
        });
        let state = self.state.read(cx);
        let disabled = state.presentation().is_disabled();
        let editable = state.is_editable();
        let focused = state.focus_handle(cx).is_focused(window) && !disabled;
        let focus = state.focus_handle(cx);
        let placeholder = state.presentation().placeholder().clone();
        let value = window.is_a11y_active().then(|| state.value());
        let mouse_state = self.state.clone();
        let focus_state = self.state.clone();
        let value_state = self.state.clone();
        InputBase::new(self.id)
            .focused(focused)
            .disabled(disabled)
            .when(disabled, |field| {
                field.capture_any_mouse_down(|_, _, cx| cx.stop_propagation())
            })
            .role(Role::TextInput)
            .track_focus(&focus)
            .when(!placeholder.is_empty(), |field| {
                field
                    .aria_label(placeholder.clone())
                    .aria_placeholder(placeholder)
            })
            .when_some(value, |field, value| field.aria_value(value))
            .on_a11y_action(AccessibleAction::Focus, move |_, window, cx| {
                focus_state.update(cx, |state, cx| {
                    if !state.presentation().is_disabled() {
                        state.focus(window, cx);
                    }
                });
            })
            .when(editable, |field| {
                field.on_a11y_action(AccessibleAction::SetValue, move |data, window, cx| {
                    if let Some(gpui::accesskit::ActionData::Value(value)) = data {
                        value_state.update(cx, |state, cx| {
                            if state.is_editable() {
                                let length = state.value().encode_utf16().count();
                                state.replace_text_in_range(Some(0..length), value, window, cx);
                            }
                        });
                    }
                })
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                mouse_state.update(cx, |state, cx| {
                    if !state.presentation().is_disabled() {
                        state.focus(window, cx);
                    }
                });
            })
            .relative()
            .flex()
            .w_full()
            .min_w_0()
            .items_center()
            .h(theme.sizes.input_height)
            .px(theme.sizes.input_padding_x)
            .py(theme.sizes.input_padding_y)
            .text_size(theme.sizes.input_text_size)
            .line_height(relative(1.25))
            .font_family(theme.fonts.ui_family.clone())
            .bg(theme.input_background(disabled))
            .rounded(theme.sizes.input_radius)
            .border_1()
            .border_color(if focused {
                theme.colors.border_focused
            } else {
                theme.colors.border
            })
            .when(focused, |field| {
                field.child(
                    div()
                        .absolute()
                        .top(-theme.sizes.input_focus_ring - px(1.0))
                        .left(-theme.sizes.input_focus_ring - px(1.0))
                        .right(-theme.sizes.input_focus_ring - px(1.0))
                        .bottom(-theme.sizes.input_focus_ring - px(1.0))
                        .border(theme.sizes.input_focus_ring)
                        .rounded(theme.sizes.input_radius + theme.sizes.input_focus_ring)
                        .border_color(gpui::Hsla {
                            a: 0.5,
                            ..theme.colors.border_focused
                        }),
                )
            })
            .child(gpui_base::Input::new(&self.state))
    }
}

// Base owns the menu model; the isolated macOS adapter owns only presentation.
fn show_input_context_menu(
    menu: gpui_base::input::NativeMenu,
    capabilities: gpui_base::input::InputContextMenuCapabilities,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_base::input::{Copy, Cut, Paste, SelectAll};
    let menu = menu
        .menu_with_disabled(
            "Cut",
            !(capabilities.is_editable() && capabilities.is_copyable()),
            Box::new(Cut),
        )
        .menu_with_disabled("Copy", !capabilities.is_copyable(), Box::new(Copy))
        .menu_with_disabled("Paste", !capabilities.is_editable(), Box::new(Paste))
        .separator()
        .menu("Select All", Box::new(SelectAll));
    if let Err(error) = macsftp_native_menu::show(menu, position, window, cx) {
        tracing::warn!(%error, "could not show native input menu");
    }
}

/// Pure single-line draft state. Ordinary fields render through `PlainInput`;
/// this editing path remains for sensitive fields and file-list type-to-filter.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct InputState {
    value: String,
    /// Byte offset into `value`, always on a char boundary.
    cursor: usize,
}

impl std::fmt::Debug for InputState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputState")
            .field("value", &"[REDACTED]")
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl Drop for InputState {
    fn drop(&mut self) {
        self.value.zeroize();
        self.cursor = 0;
    }
}

/// Non-cloneable marker for password/passphrase fields. It reuses the same
/// editing behavior as [`InputState`] while preventing accidental secret
/// duplication in form snapshots.
#[derive(Default, PartialEq, Eq)]
pub struct SecretInputState(InputState);

impl SecretInputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(value: impl Into<String>) -> Self {
        Self(InputState::with_value(value))
    }

    pub fn value(&self) -> &str {
        self.0.value()
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.0.set_value(value);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn chars_before_cursor(&self) -> usize {
        self.0.chars_before_cursor()
    }

    pub fn as_input_state(&self) -> &InputState {
        &self.0
    }

    pub fn as_input_state_mut(&mut self) -> &mut InputState {
        &mut self.0
    }
}

impl std::fmt::Debug for SecretInputState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretInputState")
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// What `handle_keystroke` did with the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKeyResult {
    /// The key edited or moved within the field.
    Handled,
    /// Not an editing key — let bindings and other handlers see it.
    Ignored,
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self { value, cursor }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        let value = value.into();
        self.value.zeroize();
        self.value = value;
        self.cursor = self.value.len();
    }

    pub fn clear(&mut self) {
        self.value.zeroize();
        self.cursor = 0;
    }

    /// Character count before the cursor — used by the renderer to
    /// split the text around the caret.
    pub fn chars_before_cursor(&self) -> usize {
        self.value[..self.cursor].chars().count()
    }

    pub fn insert(&mut self, text: &str) {
        // Single-line field: strip newlines from pasted text.
        let mut sanitized: String = text.chars().filter(|ch| !ch.is_control()).collect();
        self.replace_range(self.cursor..self.cursor, &sanitized);
        self.cursor += sanitized.len();
        sanitized.zeroize();
    }

    /// Apply one keystroke. Modifier chords (except shift) are ignored
    /// so command bindings keep working while a field is focused.
    pub fn handle_keystroke(&mut self, keystroke: &Keystroke) -> InputKeyResult {
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return InputKeyResult::Ignored;
        }

        match keystroke.key.as_str() {
            "backspace" => {
                if let Some(previous) = self.previous_boundary() {
                    self.replace_range(previous..self.cursor, "");
                    self.cursor = previous;
                }
                InputKeyResult::Handled
            }
            "delete" => {
                if let Some(next) = self.next_boundary() {
                    self.replace_range(self.cursor..next, "");
                }
                InputKeyResult::Handled
            }
            "left" => {
                if let Some(previous) = self.previous_boundary() {
                    self.cursor = previous;
                }
                InputKeyResult::Handled
            }
            "right" => {
                if let Some(next) = self.next_boundary() {
                    self.cursor = next;
                }
                InputKeyResult::Handled
            }
            "home" => {
                self.cursor = 0;
                InputKeyResult::Handled
            }
            "end" => {
                self.cursor = self.value.len();
                InputKeyResult::Handled
            }
            _ => match &keystroke.key_char {
                Some(key_char) if !key_char.chars().any(char::is_control) => {
                    self.insert(key_char.clone().as_str());
                    InputKeyResult::Handled
                }
                _ => InputKeyResult::Ignored,
            },
        }
    }

    fn previous_boundary(&self) -> Option<usize> {
        self.value[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
    }

    fn next_boundary(&self) -> Option<usize> {
        self.value[self.cursor..]
            .chars()
            .next()
            .map(|ch| self.cursor + ch.len_utf8())
    }

    fn replace_range(&mut self, range: std::ops::Range<usize>, replacement: &str) {
        let mut next =
            String::with_capacity(self.value.len() - (range.end - range.start) + replacement.len());
        next.push_str(&self.value[..range.start]);
        next.push_str(replacement);
        next.push_str(&self.value[range.end..]);
        self.value.zeroize();
        self.value = next;
    }
}

/// Render one single-line text field. The caller owns focus routing;
/// `focused` controls the border and caret. `masked` renders bullets
/// (passwords/passphrases must never appear on screen).
pub struct TextFieldModel<'a> {
    pub state: &'a InputState,
    pub placeholder: &'a str,
    pub focused: bool,
    pub masked: bool,
}

pub fn text_field(
    id: impl Into<ElementId>,
    model: TextFieldModel<'_>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let display_value: String = if model.masked {
        model.state.value().chars().map(|_| '•').collect()
    } else {
        model.state.value().to_string()
    };

    let char_split = model.chars_before_cursor_display();
    let (before_cursor, after_cursor): (String, String) = {
        let mut characters = display_value.chars();
        let before: String = characters.by_ref().take(char_split).collect();
        let after: String = characters.collect();
        (before, after)
    };
    let is_empty = display_value.is_empty();

    div()
        .id(id.into())
        .flex()
        .items_center()
        .w_full()
        .h(px(26.0))
        .px_2()
        .rounded_sm()
        .border_1()
        .border_color(if model.focused {
            theme.colors.border_focused
        } else {
            theme.colors.border
        })
        .bg(theme.colors.background)
        .text_size(px(12.0))
        .font_family(theme.fonts.mono_family.clone())
        .child(
            div()
                .flex()
                .items_center()
                .min_w_0()
                .overflow_hidden()
                .when(is_empty && !model.focused, |field| {
                    field.child(
                        div()
                            .text_color(theme.colors.text_disabled)
                            .child(SharedString::from(model.placeholder.to_string())),
                    )
                })
                .when(!is_empty || model.focused, |field| {
                    field
                        .child(
                            div()
                                .text_color(theme.colors.text)
                                .child(SharedString::from(before_cursor)),
                        )
                        .when(model.focused, |field| {
                            field.child(
                                div()
                                    .w(px(1.0))
                                    .h(px(15.0))
                                    .flex_none()
                                    .bg(theme.colors.accent),
                            )
                        })
                        .child(
                            div()
                                .text_color(theme.colors.text)
                                .child(SharedString::from(after_cursor)),
                        )
                }),
        )
}

impl TextFieldModel<'_> {
    fn chars_before_cursor_display(&self) -> usize {
        // Bullets are one char per source char, so the count carries over.
        self.state.chars_before_cursor()
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use gpui::Keystroke;

    use super::{InputKeyResult, InputState, SecretInputState};

    fn key(name: &str) -> Keystroke {
        Keystroke::parse(name).expect("test keystroke must parse")
    }

    fn typed(character: char) -> Keystroke {
        let mut keystroke = key(&character.to_string());
        keystroke.key_char = Some(character.to_string());
        keystroke
    }

    struct InputHarness {
        base: gpui::Entity<gpui_base::input::InputState>,
    }

    impl gpui::Render for InputHarness {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use crate::theme::ActiveTheme as _;
            use gpui::{InteractiveElement as _, ParentElement as _, Styled as _};
            window.set_rem_size(cx.theme().sizes.input_text_size / 0.875);
            gpui::div().flex().flex_col().w_full().child(
                gpui::div()
                    .debug_selector(|| "base-input".into())
                    .child(super::ordinary_text_field("base-field", &self.base)),
            )
        }
    }

    #[gpui::test]
    fn ordinary_input_preserves_frozen_component_bounds_in_narrow_windows(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| crate::theme::Theme::one_dark().install(cx));
        let (view, cx) = cx.add_window_view(|window, cx| InputHarness {
            base: cx.new(|cx| gpui_base::input::InputState::new(window, cx)),
        });
        for theme in [
            crate::theme::Theme::one_dark(),
            crate::theme::Theme::one_light(),
        ] {
            cx.update(|_window, cx| {
                theme.install(cx);
                view.update(cx, |_view, cx| cx.notify());
            });
            for width in [160.0, 480.0] {
                cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(160.0)));
                cx.run_until_parked();
                let base = cx
                    .debug_bounds("base-input")
                    .expect("Base input must be rendered");
                // Stage-2 comparison measured this actual compact height.
                assert_eq!(
                    base.size,
                    gpui::size(gpui::px(width), gpui::px(19.5)),
                    "migration must preserve compact input bounds"
                );
            }
        }
    }

    #[test]
    fn typing_inserts_at_cursor() {
        let mut input = InputState::new();
        for character in "host".chars() {
            assert_eq!(
                input.handle_keystroke(&typed(character)),
                InputKeyResult::Handled
            );
        }
        assert_eq!(input.value(), "host");

        // Move left twice, insert in the middle.
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&typed('X'));
        assert_eq!(input.value(), "hoXst");
    }

    #[test]
    fn debug_output_redacts_input_values() {
        let input = InputState::with_value("do-not-log-this");
        let secret = SecretInputState::with_value("do-not-log-this-either");

        assert!(!format!("{input:?}").contains("do-not-log-this"));
        assert!(!format!("{secret:?}").contains("do-not-log-this-either"));
    }

    #[test]
    fn clearing_secret_input_removes_the_visible_value() {
        let mut secret = SecretInputState::with_value("temporary-secret");

        secret.clear();

        assert!(secret.value().is_empty());
        assert_eq!(secret.chars_before_cursor(), 0);
    }

    #[test]
    fn backspace_and_delete_edit_around_cursor() {
        let mut input = InputState::with_value("abc");
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&key("backspace")); // removes 'b'
        assert_eq!(input.value(), "ac");
        input.handle_keystroke(&key("delete")); // removes 'c'
        assert_eq!(input.value(), "a");
        // At end: delete is a no-op.
        input.handle_keystroke(&key("delete"));
        assert_eq!(input.value(), "a");
    }

    #[test]
    fn home_end_move_cursor() {
        let mut input = InputState::with_value("abc");
        input.handle_keystroke(&key("home"));
        input.handle_keystroke(&typed('0'));
        assert_eq!(input.value(), "0abc");
        input.handle_keystroke(&key("end"));
        input.handle_keystroke(&typed('9'));
        assert_eq!(input.value(), "0abc9");
    }

    #[test]
    fn command_chords_are_ignored() {
        let mut input = InputState::with_value("abc");
        let mut chord = key("cmd-a");
        chord.key_char = Some("a".to_string());
        assert_eq!(input.handle_keystroke(&chord), InputKeyResult::Ignored);
        assert_eq!(input.value(), "abc");
    }

    #[test]
    fn paste_strips_control_characters() {
        let mut input = InputState::new();
        input.insert("host\n.example\t.com");
        assert_eq!(input.value(), "host.example.com");
    }

    #[test]
    fn multibyte_characters_edit_cleanly() {
        let mut input = InputState::new();
        input.insert("héllo");
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&key("left"));
        input.handle_keystroke(&key("backspace")); // removes 'h'
        assert_eq!(input.value(), "éllo");
        assert_eq!(input.chars_before_cursor(), 0);
    }
}
