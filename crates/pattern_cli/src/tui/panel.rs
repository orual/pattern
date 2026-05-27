// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use unicode_width::UnicodeWidthStr;

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
    #[allow(dead_code)]
    Context,
    /// Constellation panel: fronting state + persona registry +
    /// relationships + groups (Phase 6 T8).
    Constellation,
    /// Spawn feed: per-spawn event groupings for ephemeral / sibling /
    /// fork output. Auto-switched on first non-Main event so the user
    /// sees sub-spawn activity instead of having it merge into the main
    /// agent's transcript.
    SpawnFeed,
}

/// One entry per spawn child the panel knows about.
///
/// Each entry holds a [`crate::tui::model::RenderBatch`] so the spawn's
/// events render with the same fidelity as the main conversation view —
/// proper ToolCall / ToolResult / Thinking sections, markdown text, etc.
/// — instead of a bespoke summary format. The kind discriminator lives
/// on the entry for header coloring; the actual rendering goes through
/// the conversation crate's `render_batch`.
#[derive(Debug, Clone)]
pub struct SpawnEntry {
    /// Stable identifier for the spawn (spawn_id for ephemerals, fork_id
    /// for forks, persona_id for siblings). Used to key incoming events
    /// to the right entry.
    pub key: String,

    /// Wire events accumulated as a renderable batch. Mirrors the way
    /// main-conversation batches collect events; the SpawnFeed renderer
    /// hands this to `conversation::render_batch` to get the same look.
    pub batch: crate::tui::model::RenderBatch,
    /// `false` once a `Stop` event arrives.
    pub active: bool,
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
    /// Pre-rendered constellation panel content. Computed by App before
    /// each frame when `content == Constellation`. Owned text avoids
    /// lifetime gymnastics inside the widget impl. Phase 6 T8.
    pub constellation_text: ratatui::text::Text<'static>,
    /// Per-spawn event groupings rendered in `SpawnFeed` mode. Most
    /// recently active spawn surfaces first; events within a spawn
    /// render newest-first.
    pub spawns: Vec<SpawnEntry>,
    /// Maximum spawn entries to retain before evicting the oldest.
    /// Stops accumulating panel state from long-running sessions.
    pub max_spawn_entries: usize,
    /// Maximum events kept per spawn entry. Older events get dropped
    /// Previous panel content saved when auto-switching to SpawnFeed.
    /// Restored when the user explicitly leaves SpawnFeed via the
    /// existing toggle keybinding (Ctrl-P / panel cycle).
    pub prev_content_before_spawn_feed: Option<PanelContent>,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            content: PanelContent::default(),
            notes: Vec::new(),
            display_content: String::new(),
            expanded_thinking: None,
            max_notes: 3,
            constellation_text: ratatui::text::Text::default(),
            spawns: Vec::new(),
            max_spawn_entries: 16,
            prev_content_before_spawn_feed: None,
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

    /// Clear accumulated display content. Call when a new batch starts so
    /// stale chunk data from a previous batch does not persist if no Final
    /// event arrives (e.g., connection dropped mid-stream).
    pub fn clear_display(&mut self) {
        self.display_content.clear();
    }

    /// Append a wire event to a spawn entry, creating the entry if it
    /// doesn't exist yet. Returns whether this is the first event for this
    /// spawn (caller uses that signal to auto-switch panel content).
    ///
    /// The event is pushed into the entry's `RenderBatch` so it renders
    /// identically to the main conversation view via `render_batch`.
    pub fn push_spawn_event(
        &mut self,
        key: &str,
        label: &str,
        event: &pattern_server::protocol::WireTurnEvent,
    ) -> bool {
        let stop = matches!(event, pattern_server::protocol::WireTurnEvent::Stop(_));
        let is_new;
        if let Some(idx) = self.spawns.iter().position(|s| s.key == key) {
            is_new = false;
            let mut entry = self.spawns.remove(idx);
            entry.batch.push_event(event);
            if stop {
                entry.active = false;
            }
            self.spawns.insert(0, entry);
        } else {
            is_new = true;
            // Stamp the spawn label as the agent_name so render_batch
            // shows `[ephemeral 37dd2fad]` as the attribution prefix on
            // the first section. This is the same mechanism the main
            // view uses for agent attribution.
            let mut batch = crate::tui::model::RenderBatch::new(smol_str::SmolStr::from(key), None)
                .with_agent(smol_str::SmolStr::from(label));
            batch.push_event(event);
            let entry = SpawnEntry {
                key: key.to_string(),
                batch,
                active: !stop,
            };
            self.spawns.insert(0, entry);
            while self.spawns.len() > self.max_spawn_entries {
                self.spawns.pop();
            }
        }
        is_new
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
            PanelContent::Constellation => {
                render_constellation_text(content_area, buf, &state.constellation_text);
            }
            PanelContent::SpawnFeed => {
                render_spawn_feed(content_area, buf, &mut state.spawns);
            }
        }
    }
}

/// Render pre-built constellation panel text into the content area.
fn render_constellation_text(
    area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    text: &ratatui::text::Text<'static>,
) {
    use ratatui::widgets::Paragraph;
    Paragraph::new(text.clone())
        .wrap(ratatui::widgets::Wrap { trim: false })
        .render(area, buf);
}

/// Render the spawn feed: per-spawn entries rendered via the same
/// `conversation::render_batch` path the main conversation view uses,
/// so ToolCall / ToolResult / Thinking / Display all keep their full
/// fidelity. Each spawn's batch carries the spawn label as its
/// `agent_name`, so the conversation renderer's existing agent-prefix
/// logic stamps `[ephemeral <id>]` on the first section automatically.
///
/// Newest spawn surfaces first (ordering maintained in `push_spawn_event`).
/// Active/done status surfaces as a small line above each batch.
fn render_spawn_feed(area: Rect, buf: &mut Buffer, spawns: &mut [SpawnEntry]) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            " spawns ",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);
    block.render(area, buf);

    if spawns.is_empty() {
        let line = Line::from(vec![Span::styled(
            "(no spawn activity)",
            Style::default().fg(Color::DarkGray),
        )]);
        if inner.height > 0 {
            buf.set_line(inner.x, inner.y, &line, inner.width);
        }
        return;
    }

    // Delegate each spawn's batch render to conversation::render_batch so the
    // events get the same fidelity as the main conversation (proper ToolCall
    // sections, thinking blocks, markdown text, etc.). The batch carries the
    // spawn label as its agent_name; the conversation renderer's existing
    // agent-prefix logic stamps `[ephemeral 37dd2fad]` on the first section
    // automatically.
    //
    // Click targets are local-only (a scratch Vec) — interactive expand /
    // collapse for spawn-feed entries is a sub-task for later; for now
    // sections render in their default state.
    let viewport_bottom = inner.y.saturating_add(inner.height);
    let mut current_y = inner.y;
    let mut click_targets_scratch: Vec<(usize, usize, u16)> = Vec::new();
    for (idx, entry) in spawns.iter_mut().enumerate() {
        if current_y >= viewport_bottom {
            break;
        }
        // Compute heights so render_batch lays out correctly.
        entry.batch.compute_heights(inner.width);
        let next_y = crate::tui::conversation::render_batch(
            &entry.batch,
            idx,
            inner,
            buf,
            current_y,
            viewport_bottom,
            0,
            &mut click_targets_scratch,
        );
        // Active/done indicator on a single line below the batch.
        if next_y < viewport_bottom {
            let (status_label, status_color) = if entry.active {
                ("● active", Color::Green)
            } else {
                ("○ done", Color::DarkGray)
            };
            let status_line = Line::from(vec![Span::styled(
                format!("  {status_label}"),
                Style::default().fg(status_color),
            )]);
            buf.set_line(inner.x, next_y, &status_line, inner.width);
            current_y = next_y.saturating_add(2);
        } else {
            current_y = next_y;
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

/// Truncate a string to fit within a given display column width.
///
/// Uses Unicode display width rather than byte or codepoint count so that
/// double-width characters (CJK, emoji) are measured correctly.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        s.to_owned()
    } else {
        // Walk codepoints accumulating display width until we exceed the budget.
        let ellipsis_width = '…'.len_utf8(); // 3 bytes, 1 display column
        let budget = max_width.saturating_sub(1); // reserve one column for '…'
        let mut cols = 0usize;
        let mut end = 0usize;
        for ch in s.chars() {
            let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if cols + w > budget {
                break;
            }
            cols += w;
            end += ch.len_utf8();
        }
        let _ = ellipsis_width; // used implicitly via '…' push
        let mut truncated = s[..end].to_owned();
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
