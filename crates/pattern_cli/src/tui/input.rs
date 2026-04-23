//! Input handler wrapping [`TextArea`] with submit, history, and slash command
//! detection.
//!
//! Intercepts key events before passing them to the textarea: Enter submits,
//! Shift/Ctrl+Enter inserts a newline, Up/Down cycle through history when the
//! textarea is a single empty line (or already browsing history), and Escape
//! clears the input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pattern_core::types::provider::ContentPart;
use ratatui::style::Style;
use ratatui_textarea::TextArea;

use super::commands::parse_slash_command;

// ---------------------------------------------------------------------------
// InputAction
// ---------------------------------------------------------------------------

/// Result of processing an input event.
#[derive(Debug)]
pub enum InputAction {
    /// User submitted a message (Enter with non-empty input).
    Submit(Vec<ContentPart>),
    /// User entered a slash command.
    SlashCommand { name: String, args: Vec<String> },
    /// Input changed (for autocomplete refresh).
    Changed,
    /// No action needed.
    None,
}

// ---------------------------------------------------------------------------
// InputHandler
// ---------------------------------------------------------------------------

/// Wraps a [`TextArea`] with submit semantics, slash command detection, and
/// input history cycling.
pub struct InputHandler {
    textarea: TextArea<'static>,
    history: Vec<String>,
    history_index: Option<usize>,
    max_history: usize,
    /// Current input stashed when the user starts browsing history.
    stashed_input: Option<String>,
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl InputHandler {
    /// Create a new input handler with an empty textarea and default settings.
    pub fn new() -> Self {
        let mut textarea = TextArea::new(vec!["".to_string()]);
        // The textarea widget underlines the entire cursor line by default,
        // which looks noisy against the chat history. Reset it so only the
        // cursor glyph itself signals focus.
        textarea.set_cursor_line_style(Style::default());
        Self {
            textarea,
            history: Vec::new(),
            history_index: None,
            max_history: 50,
            stashed_input: None,
        }
    }

    /// Handle a key event, returning what action the app should take.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            // Shift+Enter or Ctrl+Enter → insert newline.
            KeyCode::Enter if shift || ctrl => {
                self.textarea.insert_newline();
                InputAction::Changed
            }

            // Plain Enter → submit.
            KeyCode::Enter => self.submit(),

            // Up arrow → history if textarea is a single empty line or already
            // browsing history.
            KeyCode::Up if self.can_history_up() => {
                self.history_up();
                InputAction::Changed
            }

            // Down arrow → history forward if currently browsing history.
            KeyCode::Down if self.history_index.is_some() => {
                self.history_down();
                InputAction::Changed
            }

            // Escape → clear input and cancel history browsing.
            KeyCode::Esc => {
                self.textarea.select_all();
                self.textarea.cut();
                self.history_index = None;
                self.stashed_input = None;
                InputAction::None
            }

            // All other keys → pass to textarea.
            _ => {
                self.textarea.input(key);
                InputAction::Changed
            }
        }
    }

    /// Return the current text content of the textarea (all lines joined).
    pub fn current_text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Borrow the underlying [`TextArea`] for rendering.
    pub fn widget(&self) -> &TextArea<'static> {
        &self.textarea
    }

    /// Replace the textarea content with the given string.
    ///
    /// Used by autocomplete to replace the input with the accepted value.
    pub fn set_text(&mut self, content: &str) {
        self.set_textarea_content(content);
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Whether the Up key should trigger history browsing rather than being
    /// passed to the textarea.
    ///
    /// History is entered when the textarea has a single line (empty or with
    /// content — the current text is stashed so it can be restored on Down).
    /// Multi-line textareas let Up navigate within the text instead.
    fn can_history_up(&self) -> bool {
        // Already browsing history — always allow further cycling.
        if self.history_index.is_some() {
            return true;
        }
        // Start browsing from any single-line state. The current content is
        // stashed by `history_up()` so it can be restored on Down.
        self.textarea.lines().len() == 1
    }

    /// Submit the current input. Returns the appropriate [`InputAction`].
    fn submit(&mut self) -> InputAction {
        let text = self.textarea.lines().join("\n").trim().to_string();
        if text.is_empty() {
            return InputAction::None;
        }

        // Push to history.
        self.history.push(text.clone());
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }
        self.history_index = None;
        self.stashed_input = None;

        // Clear the textarea.
        self.textarea.select_all();
        self.textarea.cut();

        // Check for slash command.
        if let Some((name, args)) = parse_slash_command(&text) {
            return InputAction::SlashCommand {
                name: name.to_string(),
                args: args.into_iter().map(String::from).collect(),
            };
        }

        InputAction::Submit(vec![ContentPart::Text(text)])
    }

    /// Cycle backward through history (older entries).
    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            Some(i) if i > 0 => i - 1,
            Some(_) => return, // Already at oldest.
            None => {
                // Stash current input before entering history.
                self.stashed_input = Some(self.textarea.lines().join("\n"));
                self.history.len() - 1
            }
        };
        self.history_index = Some(idx);
        self.set_textarea_content(&self.history[idx].clone());
    }

    /// Cycle forward through history (newer entries).
    fn history_down(&mut self) {
        let idx = match self.history_index {
            Some(i) => i + 1,
            None => return,
        };
        if idx >= self.history.len() {
            // Restore stashed input.
            self.history_index = None;
            let stashed = self.stashed_input.take().unwrap_or_default();
            self.set_textarea_content(&stashed);
        } else {
            self.history_index = Some(idx);
            self.set_textarea_content(&self.history[idx].clone());
        }
    }

    /// Replace the textarea content with the given string.
    fn set_textarea_content(&mut self, content: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(content);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    /// Helper: create a key event for a character.
    fn char_key(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Helper: create a key event for a special key.
    fn special_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Helper: create a key event with modifiers.
    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Helper: type a string into the handler character by character.
    fn type_str(handler: &mut InputHandler, s: &str) {
        for c in s.chars() {
            handler.handle_key(char_key(c));
        }
    }

    #[test]
    fn enter_submits_text() {
        let mut handler = InputHandler::new();
        type_str(&mut handler, "hello");
        let action = handler.handle_key(special_key(KeyCode::Enter));

        match action {
            InputAction::Submit(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::Text(t) => assert_eq!(t, "hello"),
                    other => panic!("expected Text, got {other:?}"),
                }
            }
            other => panic!("expected Submit, got {other:?}"),
        }

        // Textarea should be cleared after submit.
        assert_eq!(handler.current_text(), "");
    }

    #[test]
    fn shift_enter_inserts_newline() {
        let mut handler = InputHandler::new();
        type_str(&mut handler, "line1");
        let action = handler.handle_key(modified_key(KeyCode::Enter, KeyModifiers::SHIFT));

        assert!(matches!(action, InputAction::Changed));
        // Textarea should now have two lines.
        assert_eq!(handler.textarea.lines().len(), 2);
        assert_eq!(handler.textarea.lines()[0], "line1");
    }

    #[test]
    fn slash_command_detected() {
        let mut handler = InputHandler::new();
        type_str(&mut handler, "/quit");
        let action = handler.handle_key(special_key(KeyCode::Enter));

        match action {
            InputAction::SlashCommand { name, args } => {
                assert_eq!(name, "quit");
                assert!(args.is_empty());
            }
            other => panic!("expected SlashCommand, got {other:?}"),
        }
    }

    #[test]
    fn history_up_cycles() {
        let mut handler = InputHandler::new();

        // Submit "a" and "b".
        type_str(&mut handler, "a");
        handler.handle_key(special_key(KeyCode::Enter));
        type_str(&mut handler, "b");
        handler.handle_key(special_key(KeyCode::Enter));

        // Up → should show "b" (most recent).
        handler.handle_key(special_key(KeyCode::Up));
        assert_eq!(handler.current_text(), "b");

        // Up again → should show "a".
        handler.handle_key(special_key(KeyCode::Up));
        assert_eq!(handler.current_text(), "a");
    }

    #[test]
    fn history_down_restores() {
        let mut handler = InputHandler::new();

        // Submit "a".
        type_str(&mut handler, "a");
        handler.handle_key(special_key(KeyCode::Enter));

        // Up → shows "a".
        handler.handle_key(special_key(KeyCode::Up));
        assert_eq!(handler.current_text(), "a");

        // Down → should restore empty (stashed input).
        handler.handle_key(special_key(KeyCode::Down));
        assert_eq!(handler.current_text(), "");
    }

    #[test]
    fn history_stashes_current_input() {
        let mut handler = InputHandler::new();

        // Submit "a" to have history.
        type_str(&mut handler, "a");
        handler.handle_key(special_key(KeyCode::Enter));

        // Type "draft" (don't submit).
        type_str(&mut handler, "draft");

        // Up stashes "draft" and shows the last history entry.
        handler.handle_key(special_key(KeyCode::Up));
        assert_eq!(handler.current_text(), "a");

        // Down restores the stashed "draft".
        handler.handle_key(special_key(KeyCode::Down));
        assert_eq!(handler.current_text(), "draft");
    }

    #[test]
    fn empty_enter_does_nothing() {
        let mut handler = InputHandler::new();
        let action = handler.handle_key(special_key(KeyCode::Enter));
        assert!(matches!(action, InputAction::None));
    }

    #[test]
    fn history_max_size() {
        let mut handler = InputHandler::new();

        // Push 60 entries.
        for i in 0..60 {
            type_str(&mut handler, &format!("msg{i}"));
            handler.handle_key(special_key(KeyCode::Enter));
        }

        // History should be capped at 50.
        assert_eq!(handler.history.len(), 50);

        // Oldest entries should be dropped (msg0..msg9 gone).
        assert_eq!(handler.history[0], "msg10");
        assert_eq!(handler.history[49], "msg59");
    }
}
