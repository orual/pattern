// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Fuzzy autocomplete widget powered by nucleo.
//!
//! Provides a [`CompletionSource`] trait for pluggable candidate providers,
//! [`AutocompleteState`] for tracking selection and filtered results, and
//! [`AutocompleteWidget`] for rendering the popup above the input area.

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Matcher, Utf32Str};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Widget};

// ---------------------------------------------------------------------------
// Completion item
// ---------------------------------------------------------------------------

/// A single scored completion candidate.
#[derive(Debug, Clone)]
pub struct CompletionItem {
    /// The value to insert on accept (e.g. command name).
    pub value: String,
    /// Human-readable description for display.
    pub description: String,
    /// Fuzzy match score from nucleo (higher = better match).
    pub score: u32,
}

// ---------------------------------------------------------------------------
// Nucleo filtering
// ---------------------------------------------------------------------------

/// Filter and score candidates against a fuzzy pattern using nucleo.
///
/// Returns matching items sorted by score descending (best match first).
/// An empty pattern returns all candidates (for bare `/` command listing).
pub fn filter_candidates(pattern: &str, candidates: &[(String, String)]) -> Vec<CompletionItem> {
    if pattern.is_empty() {
        return candidates
            .iter()
            .map(|(value, desc)| CompletionItem {
                value: value.clone(),
                description: desc.clone(),
                score: 0,
            })
            .collect();
    }

    let mut matcher = Matcher::new(nucleo::Config::DEFAULT);
    let pat = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);

    let mut results: Vec<CompletionItem> = candidates
        .iter()
        .filter_map(|(value, desc)| {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(value, &mut buf);
            let score = pat.score(haystack, &mut matcher)?;
            Some(CompletionItem {
                value: value.clone(),
                description: desc.clone(),
                score,
            })
        })
        .collect();

    results.sort_by_key(|item| std::cmp::Reverse(item.score));
    results
}

// ---------------------------------------------------------------------------
// AutocompleteState
// ---------------------------------------------------------------------------

/// What kind of completion the popup is currently driving.
/// Drives the accept-time replacement strategy: slash-command
/// completions replace the whole input with `/<value> `; mention
/// completions replace just the trailing `@<partial>` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletionMode {
    #[default]
    Slash,
    Mention,
}

/// Tracks the state of the autocomplete popup.
pub struct AutocompleteState {
    /// Whether the popup is currently visible.
    visible: bool,
    /// Filtered and scored completion items.
    items: Vec<CompletionItem>,
    /// Index of the currently selected item.
    selected: usize,
    /// The current pattern being matched against.
    pattern: String,
    /// What kind of completion is active.
    mode: CompletionMode,
}

impl Default for AutocompleteState {
    fn default() -> Self {
        Self::new()
    }
}

impl AutocompleteState {
    /// Create a new hidden autocomplete state.
    pub fn new() -> Self {
        Self {
            visible: false,
            items: Vec::new(),
            selected: 0,
            pattern: String::new(),
            mode: CompletionMode::Slash,
        }
    }

    /// Update the autocomplete with a new pattern, candidate list, and mode.
    ///
    /// Shows the popup if there are matches; hides it otherwise.
    /// If exactly one match and it equals the pattern, auto-dismisses
    /// (the user already typed the full command name).
    pub fn update(
        &mut self,
        pattern: &str,
        candidates: &[(String, String)],
        mode: CompletionMode,
    ) {
        self.pattern = pattern.to_string();
        self.mode = mode;
        self.items = filter_candidates(pattern, candidates);

        // Auto-dismiss when the only match is an exact match.
        if self.items.len() == 1 && self.items[0].value == pattern {
            self.hide();
            return;
        }

        self.visible = !self.items.is_empty();
        // Clamp selection to valid range.
        if self.selected >= self.items.len() {
            self.selected = 0;
        }
    }

    /// Active completion mode (set by [`Self::update`]).
    pub fn mode(&self) -> CompletionMode {
        self.mode
    }

    /// Hide the autocomplete popup.
    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = 0;
        self.pattern.clear();
    }

    /// Move selection to the next item, wrapping around.
    pub fn next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    /// Move selection to the previous item, wrapping around.
    pub fn prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.items.len() - 1
        } else {
            self.selected - 1
        };
    }

    /// Return the value of the currently selected item, if any.
    pub fn accept(&self) -> Option<&str> {
        if !self.visible {
            return None;
        }
        self.items
            .get(self.selected)
            .map(|item| item.value.as_str())
    }

    /// Whether the popup is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }
}

// ---------------------------------------------------------------------------
// AutocompleteWidget
// ---------------------------------------------------------------------------

/// Maximum number of visible items in the popup.
const MAX_POPUP_HEIGHT: usize = 8;

/// Renders the autocomplete popup above the input area.
///
/// Call with the input area rect — the widget computes its own position
/// above that area.
pub struct AutocompleteWidget<'a> {
    state: &'a AutocompleteState,
}

impl<'a> AutocompleteWidget<'a> {
    /// Create a new autocomplete widget for the given state.
    pub fn new(state: &'a AutocompleteState) -> Self {
        Self { state }
    }

    /// Render the popup into the buffer, positioned above `input_area`.
    ///
    /// The popup overlays the conversation area, so it is rendered after
    /// the main frame content.
    pub fn render_above(&self, input_area: Rect, buf: &mut Buffer) {
        if !self.state.visible || self.state.items.is_empty() {
            return;
        }

        let item_count = self.state.items.len().min(MAX_POPUP_HEIGHT);
        let popup_height = item_count as u16;

        // Position directly above the input area.
        if input_area.y < popup_height {
            // Not enough room above input — skip rendering.
            return;
        }

        let popup_area = Rect {
            x: input_area.x,
            y: input_area.y.saturating_sub(popup_height),
            width: input_area.width,
            height: popup_height,
        };

        // Clear the background behind the popup.
        Clear.render(popup_area, buf);

        // Build list items: "command_name  description" with description dimmed.
        let list_items: Vec<ListItem> = self
            .state
            .items
            .iter()
            .take(MAX_POPUP_HEIGHT)
            .enumerate()
            .map(|(i, item)| {
                let is_selected = i == self.state.selected;
                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let desc_style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                // Pad the name to align descriptions.
                let padded_name = format!("{:<16}", item.value);
                let line = Line::from(vec![
                    Span::styled(padded_name, name_style),
                    Span::styled(&item.description, desc_style),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(list_items).style(Style::default().bg(Color::Black));
        Widget::render(list, popup_area, buf);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_utils::buffer_to_string;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Standard test candidates (subset of builtin commands).
    fn test_candidates() -> Vec<(String, String)> {
        vec![
            ("clear".into(), "Clear conversation view".into()),
            ("quit".into(), "Exit the TUI".into()),
            ("front".into(), "Switch fronting persona".into()),
            ("agents".into(), "List active agents".into()),
            ("status".into(), "Show runtime status".into()),
            ("shutdown".into(), "Stop the daemon".into()),
            ("cancel".into(), "Cancel the current batch".into()),
            ("panel".into(), "Toggle side panel".into()),
        ]
    }

    #[test]
    fn filter_matches_prefix() {
        let results = filter_candidates("cl", &test_candidates());
        assert!(!results.is_empty(), "should match at least 'clear'");
        assert_eq!(results[0].value, "clear");
    }

    #[test]
    fn filter_fuzzy_matches() {
        let results = filter_candidates("sht", &test_candidates());
        let values: Vec<&str> = results.iter().map(|r| r.value.as_str()).collect();
        assert!(
            values.contains(&"shutdown"),
            "fuzzy 'sht' should match 'shutdown', got: {values:?}"
        );
    }

    #[test]
    fn filter_no_match() {
        let results = filter_candidates("xyz", &test_candidates());
        assert!(results.is_empty(), "should have no matches for 'xyz'");
    }

    #[test]
    fn filter_sorts_by_score() {
        let results = filter_candidates("s", &test_candidates());
        // All results should be sorted by score descending.
        for window in results.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "results should be sorted by score descending: {} (score {}) came before {} (score {})",
                window[0].value,
                window[0].score,
                window[1].value,
                window[1].score,
            );
        }
    }

    #[test]
    fn accept_returns_selected_value() {
        let mut state = AutocompleteState::new();
        state.update("cl", &test_candidates(), CompletionMode::Slash);
        assert!(state.is_visible());

        let accepted = state.accept();
        assert_eq!(accepted, Some("clear"));
    }

    #[test]
    fn escape_dismisses() {
        let mut state = AutocompleteState::new();
        state.update("cl", &test_candidates(), CompletionMode::Slash);
        assert!(state.is_visible());

        state.hide();
        assert!(!state.is_visible());
        assert_eq!(state.accept(), None);
    }

    #[test]
    fn popup_snapshot() {
        // Render the autocomplete popup above a simulated input area.
        let mut state = AutocompleteState::new();
        state.update("s", &test_candidates(), CompletionMode::Slash);
        assert!(state.is_visible());

        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                // Simulate: input area is the last 2 rows.
                let input_area = Rect {
                    x: area.x,
                    y: area.height.saturating_sub(2),
                    width: area.width,
                    height: 2,
                };
                let widget = AutocompleteWidget::new(&state);
                widget.render_above(input_area, f.buffer_mut());
            })
            .unwrap();

        let output = buffer_to_string(terminal.backend().buffer());
        insta::assert_snapshot!(output);
    }
}
