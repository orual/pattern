//! Side panel widget with display event routing.
//!
//! The panel has three content modes (Status, Thinking, Context) and a
//! notification area at the top for `Display::Note` messages. Display
//! chunk/final events route to `display_content` in the main body.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, StatefulWidget, Widget, Wrap};

// ---------------------------------------------------------------------------
// Panel content mode
// ---------------------------------------------------------------------------

/// What the panel is currently showing in its main content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelContent {
    /// Default status view: connection state, agent info.
    #[default]
    Status,
    /// Expanded thinking block content (AC4.5).
    Thinking,
    /// Placeholder for future memory/context view.
    Context,
}

// ---------------------------------------------------------------------------
// Panel state
// ---------------------------------------------------------------------------

/// Mutable state for the side panel. Owned by the application.
pub struct PanelState {
    /// Which content mode is active.
    pub content: PanelContent,
    /// `Display::Note` messages rendered in the notification area at the top.
    pub notes: Vec<String>,
    /// `Display::Chunk/Final` content rendered in the main panel body.
    pub display_content: String,
    /// Thinking block content shown in the panel (AC4.5). Set when the user
    /// expands a thinking section into the panel.
    pub expanded_thinking: Option<String>,
    /// Maximum notes to keep before oldest are dropped.
    pub max_notes: usize,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            content: PanelContent::default(),
            notes: Vec::new(),
            display_content: String::new(),
            expanded_thinking: None,
            max_notes: 3,
        }
    }
}

impl PanelState {
    /// Push a note, dropping the oldest if over `max_notes`.
    pub fn push_note(&mut self, text: String) {
        self.notes.push(text);
        while self.notes.len() > self.max_notes {
            self.notes.remove(0);
        }
    }

    /// Append a chunk to the display content.
    pub fn push_chunk(&mut self, text: &str) {
        self.display_content.push_str(text);
    }

    /// Replace display content with a final message.
    pub fn set_final(&mut self, text: String) {
        self.display_content = text;
    }
}

// ---------------------------------------------------------------------------
// Side panel widget
// ---------------------------------------------------------------------------

/// Stateless view struct for the side panel. All mutable data lives
/// in [`PanelState`].
pub struct SidePanel;

impl StatefulWidget for SidePanel {
    type State = PanelState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Split into notification area (up to 3 lines) and content area.
        let note_lines = state.notes.len().min(state.max_notes) as u16;
        let note_height = note_lines.min(area.height.saturating_sub(1));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(note_height), // notification area
                Constraint::Min(1),              // content area
            ])
            .split(area);

        let note_area = chunks[0];
        let content_area = chunks[1];

        // Render notification area (Display::Note messages).
        render_notes(note_area, buf, &state.notes);

        // Render content area based on mode.
        match state.content {
            PanelContent::Status => {
                render_status_content(content_area, buf, &state.display_content);
            }
            PanelContent::Thinking => {
                render_thinking_content(content_area, buf, state.expanded_thinking.as_deref());
            }
            PanelContent::Context => {
                render_context_placeholder(content_area, buf);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Render Display::Note messages in the notification area.
fn render_notes(area: Rect, buf: &mut Buffer, notes: &[String]) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let style = Style::default().fg(Color::DarkGray);

    for (i, note) in notes.iter().rev().take(area.height as usize).enumerate() {
        let y = area.y + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let line = Line::from(vec![Span::styled(
            truncate_to_width(note, area.width as usize),
            style,
        )]);
        buf.set_line(area.x, y, &line, area.width);
    }
}

/// Render the status content view.
fn render_status_content(area: Rect, buf: &mut Buffer, display_content: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " panel ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    block.render(area, buf);

    if !display_content.is_empty() {
        let text = ratatui::text::Text::from(display_content.to_owned());
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
        paragraph.render(inner, buf);
    }
}

/// Render expanded thinking content in the panel.
fn render_thinking_content(area: Rect, buf: &mut Buffer, thinking: Option<&str>) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " thinking ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    block.render(area, buf);

    let content = thinking.unwrap_or("(no thinking block selected)");
    let style = Style::default().fg(Color::DarkGray);
    let text = ratatui::text::Text::styled(content.to_owned(), style);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
    paragraph.render(inner, buf);
}

/// Render the context placeholder.
fn render_context_placeholder(area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " context ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    block.render(area, buf);

    let text = ratatui::text::Text::styled(
        "context info coming soon".to_owned(),
        Style::default().fg(Color::DarkGray),
    );
    let paragraph = Paragraph::new(text);
    paragraph.render(inner, buf);
}

/// Truncate a string to fit within a given column width.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if s.len() <= max_width {
        s.to_owned()
    } else {
        let mut truncated: String = s.chars().take(max_width.saturating_sub(1)).collect();
        truncated.push('…');
        truncated
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

    /// Helper: render the SidePanel into a TestBackend and return the buffer
    /// content as a string.
    fn render_panel(state: &mut PanelState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_stateful_widget(SidePanel, f.area(), state);
            })
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    // -----------------------------------------------------------------------
    // Unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn note_events_accumulate() {
        let mut state = PanelState {
            max_notes: 3,
            ..Default::default()
        };
        for i in 1..=5 {
            state.push_note(format!("note {i}"));
        }
        // Only the last 3 should remain.
        assert_eq!(state.notes.len(), 3);
        assert_eq!(state.notes[0], "note 3");
        assert_eq!(state.notes[1], "note 4");
        assert_eq!(state.notes[2], "note 5");
    }

    #[test]
    fn chunk_events_concatenate() {
        let mut state = PanelState::default();
        state.push_chunk("hello ");
        state.push_chunk("world");
        assert_eq!(state.display_content, "hello world");
    }

    #[test]
    fn final_event_replaces() {
        let mut state = PanelState::default();
        state.push_chunk("partial data");
        assert_eq!(state.display_content, "partial data");
        state.set_final("final result".to_owned());
        assert_eq!(state.display_content, "final result");
    }

    // -----------------------------------------------------------------------
    // Snapshot tests
    // -----------------------------------------------------------------------

    #[test]
    fn panel_renders_notes() {
        let mut state = PanelState {
            notes: vec!["agent started".into(), "processing query".into()],
            ..Default::default()
        };
        let output = render_panel(&mut state, 30, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn panel_renders_thinking() {
        let mut state = PanelState {
            content: PanelContent::Thinking,
            expanded_thinking: Some(
                "Let me consider the options carefully...\nOption A is good.\nOption B is better."
                    .into(),
            ),
            ..Default::default()
        };
        let output = render_panel(&mut state, 30, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn panel_status_mode() {
        let mut state = PanelState {
            content: PanelContent::Status,
            display_content: "supervisor: active\npattern-nd: idle".into(),
            ..Default::default()
        };
        let output = render_panel(&mut state, 30, 10);
        insta::assert_snapshot!(output);
    }
}
