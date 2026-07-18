use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, InteractiveElement, IntoElement, Keystroke, ParentElement, SharedString,
    Styled, div, px,
};
use zeroize::Zeroize;

use crate::theme::ActiveTheme;

/// Minimal single-line text editing state: value + cursor. Pure logic,
/// no GPUI entities, so the form owner keeps one per field and routes
/// key events to the focused one.
///
/// MVP scope (M3): character input via `key_char`, backspace/delete,
/// arrow/home/end movement, insert-at-cursor paste. No selection, no
/// IME composition — those come with a real input component later.
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
