// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use super::model::{RenderBatch, Section, SectionKind, TOOL_BODY_INDENT};

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
    /// Click targets populated during render: `(batch_idx, section_idx, y_position)`.
    /// Used by mouse click handlers to map a row to a collapsible section.
    /// Cleared and repopulated on every frame.
    pub click_targets: Vec<(usize, usize, u16)>,
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

        // Clear click targets from the previous frame.
        state.click_targets.clear();

        // Step 1: compute any uncached heights.
        for batch in &mut state.batches {
            batch.compute_heights(area.width);
        }

        // Step 2: calculate total content height. Each batch after the first
        // is preceded by a one-line separator for visual breathing room
        // between exchanges.
        let total_height: usize = state
            .batches
            .iter()
            .enumerate()
            .map(|(i, b)| b.total_height() as usize + if i == 0 { 0 } else { 1 })
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

        for (batch_idx, batch) in state.batches.iter().enumerate() {
            let separator_height = if batch_idx == 0 { 0 } else { 1 };
            let batch_height = batch.total_height() as usize;
            let block_height = separator_height + batch_height;

            // Skip blocks entirely above the viewport.
            if accumulated + block_height <= state.scroll_offset {
                accumulated += block_height;
                continue;
            }

            // How many lines of this (separator + batch) block are above the
            // viewport?
            let mut skip_lines = state.scroll_offset.saturating_sub(accumulated);

            // Render the inter-batch separator as a blank line (if applicable
            // and visible).
            if separator_height > 0 {
                if skip_lines > 0 {
                    skip_lines -= 1;
                } else if current_y < viewport_bottom {
                    current_y += 1;
                }
            }

            if current_y >= viewport_bottom {
                break;
            }

            // Step 5: render this batch, collecting click targets for collapsed sections.
            current_y = render_batch(
                batch,
                batch_idx,
                area,
                buf,
                current_y,
                viewport_bottom,
                skip_lines,
                &mut state.click_targets,
            );

            accumulated += block_height;

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
            let cursor_span = Span::styled(" ▍", Style::default().fg(Color::Cyan));
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
///
/// Records click targets for collapsible sections (collapsed or expandable)
/// into `click_targets` as `(batch_idx, section_idx, y_position)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_batch(
    batch: &RenderBatch,
    batch_idx: usize,
    area: Rect,
    buf: &mut Buffer,
    mut current_y: u16,
    viewport_bottom: u16,
    mut skip_lines: usize,
    click_targets: &mut Vec<(usize, usize, u16)>,
) -> u16 {
    // Render user message line.
    if let Some(ref msg) = batch.user_message {
        let prefix = Span::styled(
            "[you] ",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
        let mut text = ratatui::text::Text::raw(msg.as_str());
        // Prepend the [you] prefix to the first line.
        if let Some(first_line) = text.lines.first_mut() {
            first_line.spans.insert(0, prefix);
        }
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
        let msg_height = paragraph.line_count(area.width) as u16;
        if skip_lines >= msg_height as usize {
            skip_lines -= msg_height as usize;
        } else {
            current_y = render_paragraph_lines(
                &paragraph,
                area,
                buf,
                current_y,
                viewport_bottom,
                skip_lines,
            );
            skip_lines = 0;
        }
    }

    // Intra-batch gap: a blank line between the user message and the agent's
    // response, for visual breathing room within a batch.
    if batch.user_message.is_some() && (batch.agent_name.is_some() || !batch.sections.is_empty()) {
        if skip_lines > 0 {
            skip_lines -= 1;
        } else if current_y < viewport_bottom {
            current_y += 1;
        }
    }

    // The agent label is prepended inline to the first visible section. We
    // build the span once and pass it via `section_prefix`; after the first
    // section consumes it, subsequent sections render without a prefix.
    let mut section_prefix: Option<Span<'static>> = batch.agent_name.as_ref().map(|name| {
        Span::styled(
            format!("[{name}] "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    });

    // Render each section.
    for (section_idx, section) in batch.sections.iter().enumerate() {
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

        // Record click target for collapsible sections. The section's first
        // visible line (current_y) is the click target row. Sections that can
        // be collapsed (thinking, tool call/result) are always clickable —
        // whether currently collapsed or expanded, clicking toggles the state.
        if section.is_collapsible() && lines_to_skip_in_section == 0 {
            click_targets.push((batch_idx, section_idx, current_y));
        }

        // Consume the agent prefix on the first section we actually render.
        let prefix = if lines_to_skip_in_section == 0 {
            section_prefix.take()
        } else {
            None
        };

        current_y = render_section(
            section,
            area,
            buf,
            current_y,
            viewport_bottom,
            lines_to_skip_in_section,
            prefix,
        );
    }

    current_y
}

/// Prepend a styled prefix span to the first line of a ratatui [`Text`] in
/// place. Used by [`render_section`] to inject the `[agent]` label inline
/// with the first line of the first section in a batch.
fn prepend_span_to_text(text: &mut ratatui::text::Text<'static>, span: Span<'static>) {
    if let Some(first_line) = text.lines.first_mut() {
        first_line.spans.insert(0, span);
    } else {
        text.lines.push(Line::from(vec![span]));
    }
}

/// Render a single section into the buffer.
///
/// When `prefix` is `Some`, the given span is prepended to the first visible
/// line of this section — used to inline the `[agent]` label on the first
/// section of a batch (mirroring how `[you]` is inline with the user line).
fn render_section(
    section: &Section,
    area: Rect,
    buf: &mut Buffer,
    current_y: u16,
    viewport_bottom: u16,
    skip_lines: usize,
    prefix: Option<Span<'static>>,
) -> u16 {
    // ToolCall and ToolResult render their own styled headers (matching the
    // expanded arrow `▾` to the collapsed `▸`) so users can see at a glance
    // that an expanded block is a tool section rather than free text. They
    // fall through the generic-collapsed short-circuit below.
    let use_tool_header = matches!(
        section.kind,
        SectionKind::ToolCall { .. } | SectionKind::ToolResult { .. }
    );

    if section.collapsed && !use_tool_header {
        // Collapsed (non-tool): render the one-line summary.
        if skip_lines == 0 && current_y < viewport_bottom {
            let summary = section.summary();
            let summary_span = Span::styled(summary, Style::default().fg(Color::DarkGray));
            let spans = match prefix {
                Some(p) => vec![p, summary_span],
                None => vec![summary_span],
            };
            let line = Line::from(spans);
            buf.set_line(area.x, current_y, &line, area.width);
            return current_y + 1;
        }
        return current_y;
    }

    // Expanded rendering based on section kind.
    match &section.kind {
        SectionKind::Text(content) => {
            let mut text = markdown::render_markdown(content);
            if let Some(p) = prefix {
                prepend_span_to_text(&mut text, p);
            }
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
            let mut text = ratatui::text::Text::styled(content.clone(), style);
            if let Some(p) = prefix {
                prepend_span_to_text(&mut text, p);
            }
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

            // Header line — same format for collapsed (▸) and expanded (▾),
            // rendered in a muted DarkGray so it reads as metadata rather
            // than content.
            if remaining_skip > 0 {
                remaining_skip -= 1;
            } else if y < viewport_bottom {
                let arrow = if section.collapsed { "▸" } else { "▾" };
                let header_style = Style::default().fg(Color::DarkGray);
                let mut spans = Vec::with_capacity(3);
                if let Some(p) = prefix.clone() {
                    spans.push(p);
                }
                // For code tool, show first line of code in header
                let preview = if function_name == "code" {
                    super::model::extract_code_preview(arguments, 60)
                } else {
                    function_name.clone()
                };
                spans.push(Span::styled(
                    format!(" {arrow} {function_name}: "),
                    header_style,
                ));
                spans.push(Span::styled(
                    preview,
                    Style::default().fg(Color::Rgb(130, 130, 180)),
                ));
                let header = Line::from(spans);
                buf.set_line(area.x, y, &header, area.width);
                y += 1;
            }

            // Body (expanded only): for code tool, wrap in a fenced code
            // block and render through the markdown renderer (gets syntax
            // highlighting). For other tools, pretty-print JSON.
            if !section.collapsed && y < viewport_bottom {
                let text = if function_name == "code" {
                    let md = super::model::render_code_tool_body(arguments);
                    markdown::render_markdown(&md)
                } else {
                    let md = super::model::render_generic_tool_body(arguments);
                    markdown::render_markdown(&md)
                };
                let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
                let inner = indented_area(area);
                y = render_paragraph_lines(
                    &paragraph,
                    inner,
                    buf,
                    y,
                    viewport_bottom,
                    remaining_skip,
                );
            }
            y
        }
        SectionKind::ToolResult {
            call_id: _,
            success,
            content,
        } => {
            let mut y = current_y;
            let mut remaining_skip = skip_lines;

            // Header line — same format for collapsed (▸) and expanded (▾).
            // The surrounding text is muted DarkGray; the status token (ok /
            // error) keeps its status colour so the outcome stands out at a
            // glance.
            if remaining_skip > 0 {
                remaining_skip -= 1;
            } else if y < viewport_bottom {
                let arrow = if section.collapsed { "▸" } else { "▾" };
                let status_color = if *success { Color::Green } else { Color::Red };
                let status = if *success { "ok" } else { "err" };
                let muted = Style::default().fg(Color::DarkGray);
                let preview = super::model::extract_result_preview(content, 55);
                let mut spans = Vec::with_capacity(5);
                if let Some(p) = prefix.clone() {
                    spans.push(p);
                }
                spans.push(Span::styled(format!(" {arrow} result ("), muted));
                spans.push(Span::styled(status, Style::default().fg(status_color)));
                spans.push(Span::styled(format!("): {preview}"), muted));
                let header = Line::from(spans);
                buf.set_line(area.x, y, &header, area.width);
                y += 1;
            }

            // Body (expanded only): pretty-print JSON, unescape strings.
            if !section.collapsed && y < viewport_bottom {
                let display_text = super::model::format_result_content(content);
                let style = if *success {
                    Style::default().fg(Color::Rgb(150, 180, 150))
                } else {
                    Style::default().fg(Color::Rgb(200, 130, 130))
                };
                let text = ratatui::text::Text::styled(display_text, style);
                let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
                let inner = indented_area(area);

                y = render_paragraph_lines(
                    &paragraph,
                    inner,
                    buf,
                    y,
                    viewport_bottom,
                    remaining_skip,
                );
            }
            y
        }
        SectionKind::Attachments(a) => {
            let mut y = current_y;
            let style = Style::default().fg(Color::DarkGray);
            let arrow = if section.collapsed { "▸" } else { "▾" };

            let header = Line::from(format!(" {arrow} attachments"));
            buf.set_line(area.x, y, &header, area.width);
            y += 1;
            // Only render attachment bodies when the section is expanded.
            // Each attachment is a (potentially 16KB) string that goes through
            // Paragraph wrap iteration every frame; doing this for a collapsed
            // section is pure overhead, and post-compaction memory-dump
            // snapshots are big enough to make scrolling laggy when they're
            // anywhere near the viewport.
            if !section.collapsed {
                for attachment in a {
                    let text = ratatui::text::Text::styled(attachment, style);
                    let paragraph = Paragraph::new(text).wrap(Wrap { trim: true });
                    let inner = indented_area(area);
                    y = render_paragraph_lines(&paragraph, inner, buf, y, viewport_bottom, skip_lines);
                }
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
            let mut t = ratatui::text::Text::styled(text.clone(), style);
            if let Some(p) = prefix {
                prepend_span_to_text(&mut t, p);
            }
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

/// Format tool result content for expanded display.
/// Parses JSON, pretty-prints objects/arrays, unescapes strings,
/// and renders newlines as actual line breaks.
/// Return a sub-Rect shifted right by [`TOOL_BODY_INDENT`] columns, with
/// `width` reduced by the same amount. Used for expanded tool call/result
/// bodies so their content sits under the header and wraps at the visual
/// right edge. Height is left alone — callers clip using their own y-bound.
fn indented_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(TOOL_BODY_INDENT),
        y: area.y,
        width: area.width.saturating_sub(TOOL_BODY_INDENT),
        height: area.height,
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

    // Only allocate enough rows to cover the visible window: the lines we
    // skip plus the lines we actually need to paint. Allocating the full
    // paragraph height for every section every frame can OOM on large sections.
    let needed_lines = (skip_lines as u16).saturating_add(viewport_bottom.saturating_sub(start_y));
    let render_height = total_lines.min(needed_lines);

    if render_height == 0 {
        return start_y;
    }

    // Create a temporary buffer sized to just what we need.
    let temp_area = Rect {
        x: 0,
        y: 0,
        width: area.width,
        height: render_height,
    };

    if temp_area.width == 0 || temp_area.height == 0 {
        return start_y;
    }

    let mut temp_buf = Buffer::empty(temp_area);
    paragraph.clone().render(temp_area, &mut temp_buf);

    // Copy visible lines from temp buffer to real buffer.
    // The temp buffer only contains `render_height` rows, so cap the loop.
    let mut y = start_y;
    for line_idx in skip_lines..(render_height as usize) {
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
    use crate::tui::test_utils::buffer_to_string;
    use pattern_core::types::turn::StopReason;
    use pattern_server::protocol::WireTurnEvent;
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

    fn make_text_batch() -> RenderBatch {
        let mut batch = RenderBatch::new("batch-1".into(), Some("Hello agent".into()));
        batch.push_event(&WireTurnEvent::Text("The answer is **42**.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        batch
    }

    fn make_thinking_batch(collapsed: bool) -> RenderBatch {
        let mut batch = RenderBatch::new("batch-2".into(), Some("Think about this".into()));
        batch.push_event(&WireTurnEvent::Thinking(
            "Let me consider the options carefully...".into(),
        ));
        batch.push_event(&WireTurnEvent::Text("I have thought about it.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        if !collapsed {
            // Expand the thinking section (index 0).
            batch.sections[0].collapsed = false;
        }
        batch
    }

    fn make_tool_call_batch() -> RenderBatch {
        let mut batch = RenderBatch::new("batch-3".into(), Some("Search for info".into()));
        batch.push_event(&WireTurnEvent::ToolCall {
            call_id: "call-123".into(),
            function_name: "search".into(),
            arguments_json: serde_json::json!({"query": "pattern"}).to_string(),
        });
        batch.push_event(&WireTurnEvent::Text("Found results.".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        batch
    }

    #[test]
    fn renders_text_batch() {
        let mut state = ConversationState {
            batches: vec![make_text_batch()],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
            click_targets: Vec::new(),
        };
        let output = render_to_string(&mut state, 50, 10);
        insta::assert_snapshot!(output);
    }

    /// A batch with `agent_name` renders `[name] ` inline with the first
    /// line of the agent's first section, after the user message.
    #[test]
    fn agent_header_renders_after_user_line() {
        let batch = make_text_batch().with_agent("supervisor".into());
        let mut state = ConversationState {
            batches: vec![batch],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
            click_targets: Vec::new(),
        };
        let output = render_to_string(&mut state, 50, 10);
        assert!(
            output.contains("[supervisor]"),
            "expected agent header in output, got:\n{output}"
        );
        // Ensure the agent header falls on a line after the user message.
        let lines: Vec<&str> = output.lines().collect();
        let user_idx = lines
            .iter()
            .position(|l| l.contains("[you]"))
            .expect("user line present");
        let agent_idx = lines
            .iter()
            .position(|l| l.contains("[supervisor]"))
            .expect("agent header present");
        assert!(
            agent_idx > user_idx,
            "agent header must come after user line, user={user_idx} agent={agent_idx}"
        );
    }

    /// Two batches render with a blank separator line between them.
    #[test]
    fn blank_line_separates_consecutive_batches() {
        let batch_a = RenderBatch::new("batch-a".into(), Some("first question".into()))
            .with_agent("a".into());
        let batch_b = RenderBatch::new("batch-b".into(), Some("second question".into()))
            .with_agent("b".into());
        let mut state = ConversationState {
            batches: vec![batch_a, batch_b],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
            click_targets: Vec::new(),
        };
        let output = render_to_string(&mut state, 50, 10);
        let lines: Vec<&str> = output.lines().collect();
        // Find the two user lines — the gap between them must contain a blank
        // line (only whitespace).
        let first_user = lines
            .iter()
            .position(|l| l.contains("first question"))
            .expect("first user line");
        let second_user = lines
            .iter()
            .position(|l| l.contains("second question"))
            .expect("second user line");
        let gap_range = first_user + 1..second_user;
        assert!(
            gap_range.clone().any(|i| lines[i].trim().is_empty()),
            "expected a blank separator line between batches, got:\n{output}"
        );
    }

    #[test]
    fn thinking_collapsed_shows_summary() {
        let mut state = ConversationState {
            batches: vec![make_thinking_batch(true)],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
            click_targets: Vec::new(),
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
            click_targets: Vec::new(),
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
            click_targets: Vec::new(),
        };
        let output = render_to_string(&mut state, 50, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn scroll_offset_skips_first_batch() {
        let batch1 = make_text_batch();
        let mut batch2 = RenderBatch::new("batch-2".into(), Some("Second question".into()));
        batch2.push_event(&WireTurnEvent::Text("Second answer.".into()));
        batch2.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));

        let mut state = ConversationState {
            batches: vec![batch1, batch2],
            auto_scroll: false,
            // Skip the first batch's user_message + intra-gap (2 lines), so
            // the text line of batch 1 and then batch 2 are visible. Note
            // that the text line here is single-line, so total_height of
            // batch 1 is 3 (user + gap + text).
            scroll_offset: 2,
            focused_section: None,
            click_targets: Vec::new(),
        };
        let output = render_to_string(&mut state, 50, 10);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn user_message_has_bold_green_prefix() {
        // Render a batch with a user message and verify the style of the
        // `[you] ` prefix cells: they must be bold and green.
        let batch = make_text_batch();
        let mut state = ConversationState {
            batches: vec![batch],
            auto_scroll: false,
            scroll_offset: 0,
            focused_section: None,
            click_targets: Vec::new(),
        };

        let backend = TestBackend::new(50, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                f.render_stateful_widget(ConversationView, f.area(), &mut state);
            })
            .unwrap();

        // The user message is the first row. The `[you] ` prefix is at x=0, y=0.
        // Check that the first cell carries bold + green styling.
        let buf = terminal.backend().buffer();
        let cell = &buf[(0u16, 0u16)];
        assert!(
            cell.style()
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD),
            "user prefix must be bold, got style: {:?}",
            cell.style()
        );
        assert_eq!(
            cell.style().fg,
            Some(ratatui::style::Color::Green),
            "user prefix must be green, got style: {:?}",
            cell.style()
        );
    }

    #[test]
    fn scroll_mid_section_shows_correct_lines() {
        // A thinking section with multiple lines, used because Thinking uses
        // plain_text_height which correctly counts newlines as separate lines.
        // (Text sections use markdown rendering where single newlines collapse.)
        let mut batch = RenderBatch::new("batch-scroll".into(), Some("user question".into()));
        // Five distinct lines in the thinking section. The section starts collapsed,
        // so expand it so the content is visible.
        batch.push_event(&WireTurnEvent::Thinking(
            "line one\nline two\nline three\nline four\nline five".into(),
        ));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
        // Expand the thinking section so it contributes full height.
        batch.sections[0].collapsed = false;

        // Total content: 1 user_msg + 1 intra-batch gap + 5 thinking lines
        // = 7 lines. scroll_offset=3 skips the user message line, the blank
        // gap, and "line one", so the viewport should start at "line two".
        let mut state = ConversationState {
            batches: vec![batch],
            auto_scroll: false,
            scroll_offset: 3,
            focused_section: None,
            click_targets: Vec::new(),
        };

        let output = render_to_string(&mut state, 50, 4);
        assert!(
            output.contains("line two"),
            "partial scroll should show lines starting at the correct offset; got: {output:?}"
        );
        assert!(
            !output.contains("user question"),
            "user message should be scrolled off-screen; got: {output:?}"
        );
        assert!(
            !output.contains("line one"),
            "first thinking line should be scrolled off-screen; got: {output:?}"
        );
        insta::assert_snapshot!(output);
    }

    #[test]
    fn auto_scroll_follows_new_content() {
        // Create enough batches to exceed viewport.
        let mut batches = Vec::new();
        for i in 0..10 {
            let mut batch =
                RenderBatch::new(format!("batch-{i}").into(), Some(format!("Question {i}")));
            batch.push_event(&WireTurnEvent::Text(format!("Answer {i}.")));
            batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));
            batches.push(batch);
        }

        let mut state = ConversationState {
            batches,
            auto_scroll: true,
            scroll_offset: 0,
            focused_section: None,
            click_targets: Vec::new(),
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
