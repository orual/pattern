//! ConversationView widget with virtual scrolling.
//!
//! [`ConversationView`] is a [`StatefulWidget`] that renders a scrollable
//! conversation composed of [`RenderBatch`]es. Only batches intersecting
//! the viewport are rendered per frame (virtual scrolling).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{StatefulWidget, Widget};
use ratatui_widgets::paragraph::{Paragraph, Wrap};

use super::markdown;
use super::model::{RenderBatch, Section, SectionKind};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Mutable state for the conversation view. Owned by the application,
/// passed as `&mut` during render.
#[derive(Debug, Default)]
pub struct ConversationState {
    /// All batches in the conversation.
    pub batches: Vec<RenderBatch>,
    /// Current scroll offset in lines from the top.
    pub scroll_offset: usize,
    /// Whether to auto-scroll to the bottom when new content arrives.
    pub auto_scroll: bool,
    /// Currently focused (batch_idx, section_idx) for expand/collapse.
    pub focused_section: Option<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

/// Lightweight view struct for the conversation. All mutable data lives
/// in [`ConversationState`].
pub struct ConversationView;

impl StatefulWidget for ConversationView {
    type State = ConversationState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Step 1: compute any uncached heights.
        for batch in &mut state.batches {
            batch.compute_heights(area.width);
        }

        // Step 2: calculate total content height.
        let total_height: usize = state
            .batches
            .iter()
            .map(|b| b.total_height() as usize)
            .sum();

        // Step 3: auto-scroll to bottom if enabled.
        let viewport_height = area.height as usize;
        if state.auto_scroll {
            state.scroll_offset = total_height.saturating_sub(viewport_height);
        }

        // Step 4: virtual scrolling — find first visible batch.
        let mut accumulated: usize = 0;
        let mut current_y = area.y;
        let viewport_bottom = area.y + area.height;

        for batch in &state.batches {
            let batch_height = batch.total_height() as usize;

            // Skip batches entirely above the viewport.
            if accumulated + batch_height <= state.scroll_offset {
                accumulated += batch_height;
                continue;
            }

            // How many lines of this batch are above the viewport?
            let skip_lines = state.scroll_offset.saturating_sub(accumulated);

            // Step 5: render this batch.
            current_y = render_batch(batch, area, buf, current_y, viewport_bottom, skip_lines);

            accumulated += batch_height;

            // Stop when below viewport.
            if current_y >= viewport_bottom {
                break;
            }
        }

        // Step 6: streaming indicator on last batch.
        if let Some(last_batch) = state.batches.last()
            && last_batch.streaming
            && current_y < viewport_bottom
        {
            let cursor_span = Span::styled("▍", Style::default().fg(Color::Cyan));
            let cursor_line = Line::from(vec![cursor_span]);
            buf.set_line(area.x, current_y, &cursor_line, area.width);
        }
    }
}

// ---------------------------------------------------------------------------
// Batch rendering
// ---------------------------------------------------------------------------

/// Render a single batch into the buffer, starting at `start_y`, skipping
/// `skip_lines` from the top of the batch. Returns the next Y position.
fn render_batch(
    batch: &RenderBatch,
    area: Rect,
    buf: &mut Buffer,
    mut current_y: u16,
    viewport_bottom: u16,
    mut skip_lines: usize,
) -> u16 {
    // Render user message line.
    if let Some(ref msg) = batch.user_message {
        if skip_lines > 0 {
            skip_lines -= 1;
        } else if current_y < viewport_bottom {
            let user_line = Line::from(vec![
                Span::styled(
                    "[you] ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(msg.as_str()),
            ]);
            buf.set_line(area.x, current_y, &user_line, area.width);
            current_y += 1;
        }
    }

    // Render each section.
    for section in &batch.sections {
        if current_y >= viewport_bottom {
            break;
        }

        let section_height = section.height() as usize;

        // Skip lines within this section if needed.
        if skip_lines >= section_height {
            skip_lines -= section_height;
            continue;
        }

        let lines_to_skip_in_section = skip_lines;
        skip_lines = 0;

        current_y = render_section(
            section,
            area,
            buf,
            current_y,
            viewport_bottom,
            lines_to_skip_in_section,
        );
    }

    current_y
}

/// Render a single section into the buffer.
fn render_section(
    section: &Section,
    area: Rect,
    buf: &mut Buffer,
    current_y: u16,
    viewport_bottom: u16,
    skip_lines: usize,
) -> u16 {
    if section.collapsed {
        // Collapsed: render the one-line summary.
        if skip_lines == 0 && current_y < viewport_bottom {
            let summary = section.summary();
            let style = Style::default().fg(Color::DarkGray);
            let line = Line::from(vec![Span::styled(summary, style)]);
            buf.set_line(area.x, current_y, &line, area.width);
            return current_y + 1;
        }
        return current_y;
    }

    // Expanded rendering based on section kind.
    match &section.kind {
        SectionKind::Text(content) => {
            let text = markdown::render_markdown(content);
            let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
            render_paragraph_lines(
                &paragraph,
                area,
                buf,
                current_y,
                viewport_bottom,
                skip_lines,
            )
        }
        SectionKind::Thinking(content) => {
            let style = Style::default().fg(Color::DarkGray);
            let text = ratatui::text::Text::styled(content.clone(), style);
            let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
            render_paragraph_lines(
                &paragraph,
                area,
                buf,
                current_y,
                viewport_bottom,
                skip_lines,
            )
        }
        SectionKind::ToolCall {
            function_name,
            arguments,
            ..
        } => {
            let mut y = current_y;
            let mut remaining_skip = skip_lines;

            // Header line.
            if remaining_skip > 0 {
                remaining_skip -= 1;
            } else if y < viewport_bottom {
                let header = Line::from(vec![
                    Span::styled(
                        "tool: ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(function_name.as_str()),
                ]);
                buf.set_line(area.x, y, &header, area.width);
                y += 1;
            }

            // Arguments.
            if y < viewport_bottom {
                let style = Style::default().fg(Color::DarkGray);
                let text = ratatui::text::Text::styled(arguments.clone(), style);
                let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
                y = render_paragraph_lines(
                    &paragraph,
                    area,
                    buf,
                    y,
                    viewport_bottom,
                    remaining_skip,
                );
            }
            y
        }
        SectionKind::ToolResult {
            success, content, ..
        } => {
            let mut y = current_y;
            let mut remaining_skip = skip_lines;

            // Header line.
            if remaining_skip > 0 {
                remaining_skip -= 1;
            } else if y < viewport_bottom {
                let status_color = if *success { Color::Green } else { Color::Red };
                let status_text = if *success {
                    "result: ok"
                } else {
                    "result: error"
                };
                let header = Line::from(vec![Span::styled(
                    status_text,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                )]);
                buf.set_line(area.x, y, &header, area.width);
                y += 1;
            }

            // Content.
            if y < viewport_bottom {
                let text = ratatui::text::Text::from(content.clone());
                let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
                y = render_paragraph_lines(
                    &paragraph,
                    area,
                    buf,
                    y,
                    viewport_bottom,
                    remaining_skip,
                );
            }
            y
        }
        SectionKind::Display { text, kind } => {
            let style = match kind {
                pattern_core::traits::turn_sink::DisplayKind::Note => {
                    Style::default().fg(Color::DarkGray)
                }
                _ => Style::default().fg(Color::Cyan),
            };
            let t = ratatui::text::Text::styled(text.clone(), style);
            let paragraph = Paragraph::new(t).wrap(Wrap { trim: true });
            render_paragraph_lines(
                &paragraph,
                area,
                buf,
                current_y,
                viewport_bottom,
                skip_lines,
            )
        }
    }
}

/// Render a paragraph's lines into the buffer, skipping `skip_lines`
/// from the top. Returns the next Y position.
fn render_paragraph_lines(
    paragraph: &Paragraph<'_>,
    area: Rect,
    buf: &mut Buffer,
    start_y: u16,
    viewport_bottom: u16,
    skip_lines: usize,
) -> u16 {
    // Render the paragraph into a temporary buffer to get individual lines,
    // then copy the visible ones. This is simpler and more correct than
    // trying to manually split wrapped lines.
    let total_lines = paragraph.line_count(area.width) as u16;
    let render_height =
        total_lines.min(viewport_bottom.saturating_sub(start_y) + skip_lines as u16);

    if render_height == 0 {
        return start_y;
    }

    // Create a temporary buffer large enough for the full paragraph.
    let temp_area = Rect {
        x: 0,
        y: 0,
        width: area.width,
        height: total_lines,
    };

    if temp_area.width == 0 || temp_area.height == 0 {
        return start_y;
    }

    let mut temp_buf = Buffer::empty(temp_area);
    paragraph.clone().render(temp_area, &mut temp_buf);

    // Copy visible lines from temp buffer to real buffer.
    let mut y = start_y;
    for line_idx in skip_lines..(total_lines as usize) {
        if y >= viewport_bottom {
            break;
        }
        for x in 0..area.width {
            let cell = &temp_buf[(x, line_idx as u16)];
            buf[(area.x + x, y)] = cell.clone();
        }
        y += 1;
    }

    y
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::model::RenderBatch;
    use pattern_core::traits::turn_sink::TurnEvent;
    use pattern_core::types::turn::StopReason;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Helper: render the ConversationView into a TestBackend and return
    /// the buffer content as a string for snapshot comparison.
    fn render_to_string(state: &mut ConversationState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_stateful_widget(ConversationView, f.area(), state);
            })
            .unwrap();

        buffer_to_string(terminal.backend().buffer())
    }

    /// Convert a Buffer to a trimmed-right string representation,
    /// one line per row. This gives us a clean snapshot target.
    fn buffer_to_string(buf: &Buffer) -> String {
        let mut lines = Vec::new();
        for y in 0..buf.area.height {
            let mut line = String::new();
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                line.push_str(cell.symbol());
            }
            // Trim trailing spaces for cleaner snapshots.
            lines.push(line.trim_end().to_string());
        }
        // Join with newlines, but trim trailing empty lines.
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    fn make_text_batch() -> RenderBatch {
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello agent".into()));
        batch.push_event(&TurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&TurnEvent::Stop(StopReason::EndTurn));
        batch
    }

    fn make_thinking_batch(collapsed: bool) -> RenderBatch {
        let mut batch = RenderBatch::new("batch-2".into(), Some("Think about this".into()));
        batch.push_event(&TurnEvent::Thinking(
            "Let me consider the options carefully...".into(),
        ));
        batch.push_event(&TurnEvent::Text("I have thought about it.".into()));
        batch.push_event(&TurnEvent::Stop(StopReason::EndTurn));
        if !collapsed {
            // Expand the thinking section (index 0).
            batch.sections[0].collapsed = false;
        }
        batch
    }

    fn make_tool_call_batch() -> RenderBatch {
        use pattern_core::types::provider::ToolCall as ProviderToolCall;
        let mut batch = RenderBatch::new("batch-3".into(), Some("Search for info".into()));
        batch.push_event(&TurnEvent::ToolCall(ProviderToolCall {
            call_id: "call-123".into(),
            fn_name: "search".into(),
            fn_arguments: serde_json::json!({"query": "pattern"}),
            thought_signatures: None,
            thought_signatures_provenance: None,
        }));
        batch.push_event(&TurnEvent::Text("Found results.".into()));
        batch.push_event(&TurnEvent::Stop(StopReason::EndTurn));
        batch
    }

    #[test]
    fn renders_text_batch() {
        let mut state = ConversationState {
            batches: vec![make_text_batch()],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
        };
        let output = render_to_string(&mut state, 50, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn thinking_collapsed_shows_summary() {
        let mut state = ConversationState {
            batches: vec![make_thinking_batch(true)],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
        };
        let output = render_to_string(&mut state, 60, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn thinking_expanded_shows_content() {
        let mut state = ConversationState {
            batches: vec![make_thinking_batch(false)],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
        };
        let output = render_to_string(&mut state, 60, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn tool_call_collapsed_shows_name() {
        let mut state = ConversationState {
            batches: vec![make_tool_call_batch()],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
        };
        let output = render_to_string(&mut state, 50, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn scroll_offset_skips_first_batch() {
        let batch1 = make_text_batch();
        let mut batch2 = RenderBatch::new("batch-2".into(), Some("Second question".into()));
        batch2.push_event(&TurnEvent::Text("Second answer.".into()));
        batch2.push_event(&TurnEvent::Stop(StopReason::EndTurn));

        let mut state = ConversationState {
            batches: vec![batch1, batch2],
            auto_scroll: false,
            // Offset past the first batch (user_message + text = 2 lines).
            scroll_offset: 2,
            focused_section: None,
        };
        let output = render_to_string(&mut state, 50, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn auto_scroll_follows_new_content() {
        // Create enough batches to exceed viewport.
        let mut batches = Vec::new();
        for i in 0..10 {
            let mut batch =
                RenderBatch::new(format!("batch-{i}").into(), Some(format!("Question {i}")));
            batch.push_event(&TurnEvent::Text(format!("Answer {i}.")));
            batch.push_event(&TurnEvent::Stop(StopReason::EndTurn));
            batches.push(batch);
        }

        let mut state = ConversationState {
            batches,
            auto_scroll: true,
            scroll_offset: 0,
            focused_section: None,
        };

        // Render with a small viewport.
        let output = render_to_string(&mut state, 50, 6);

        // After render, scroll_offset should have been adjusted.
        assert!(
            state.scroll_offset > 0,
            "auto_scroll should have adjusted offset"
        );
        insta::assert_snapshot!(output);
    }
}
