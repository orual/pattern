//! Data model for conversation rendering.
//!
//! A [`RenderBatch`] represents one user-to-agent exchange. Each batch
//! contains ordered [`Section`]s representing different types of content
//! (text, thinking, tool calls, tool results, display output).
//!
//! Key design decisions:
//! - Text and Thinking sections concatenate consecutive same-type events
//!   into one section (no per-chunk section proliferation).
//! - Thinking, ToolCall, ToolResult sections are collapsed by default.
//!   Text and Display sections are never collapsed.
//! - Height caching uses `Option<u16>` — set to `None` when content
//!   changes, computed lazily during render.

use pattern_core::traits::turn_sink::DisplayKind;
use pattern_server::protocol::WireTurnEvent;
use smol_str::SmolStr;

use super::markdown;

/// Indent (in columns) applied to the body of expanded ToolCall/ToolResult
/// sections — arguments and tool output sit under their header line, shifted
/// right so the hierarchy is visible at a glance. Used by both the renderer
/// (to offset the paragraph's draw rect) and `compute_heights` (to compute
/// wrap height at the narrower content width). Must stay in sync across
/// both call sites.
pub const TOOL_BODY_INDENT: u16 = 2;

// ---------------------------------------------------------------------------
// Section types
// ---------------------------------------------------------------------------

/// The kind of content a section holds.
#[derive(Debug, Clone)]
pub enum SectionKind {
    /// Streamed LLM text (the model's answer).
    Text(String),
    /// LLM reasoning content (extended thinking).
    Thinking(String),
    /// A tool invocation requested by the model.
    ToolCall {
        /// Retained for future expand view rendering.
        #[allow(dead_code)]
        call_id: String,
        function_name: String,
        arguments: String,
    },
    /// The result of a tool invocation.
    ToolResult {
        call_id: String,
        success: bool,
        content: String,
    },
    /// Agent display output (chunk, final, or note).
    Display { kind: DisplayKind, text: String },
}

/// One logical section within a [`RenderBatch`].
#[derive(Debug, Clone)]
pub struct Section {
    /// What kind of content this section holds.
    pub kind: SectionKind,
    /// Whether the section is collapsed in the UI. Thinking, ToolCall,
    /// and ToolResult start collapsed; Text and Display never collapse.
    pub collapsed: bool,
    /// Cached rendered height in lines at a specific width. `None` means
    /// the cache is invalidated and must be recomputed during render.
    pub cached_height: Option<u16>,
}

impl Section {
    /// Create a new section with appropriate default collapsed state.
    fn new(kind: SectionKind) -> Self {
        let collapsed = matches!(
            kind,
            SectionKind::Thinking(_)
                | SectionKind::ToolCall { .. }
                | SectionKind::ToolResult { .. }
        );
        Self {
            kind,
            collapsed,
            cached_height: None,
        }
    }

    /// One-line summary for collapsed view, prefixed with `▸`.
    pub fn summary(&self) -> String {
        match &self.kind {
            SectionKind::Text(s) => {
                let preview = truncate_preview(s, 60);
                format!("▸ text: {preview}")
            }
            SectionKind::Thinking(s) => {
                let preview = truncate_preview(s, 60);
                format!("▸ thinking: {preview}")
            }
            SectionKind::ToolCall { function_name, .. } => {
                format!("▸ tool: {function_name}")
            }
            SectionKind::ToolResult {
                call_id, success, ..
            } => {
                let status = if *success { "ok" } else { "error" };
                format!("▸ result ({status}): {call_id}")
            }
            SectionKind::Display { kind, text } => {
                let label = match kind {
                    DisplayKind::Chunk => "chunk",
                    DisplayKind::Final => "final",
                    DisplayKind::Note => "note",
                };
                let preview = truncate_preview(text, 60);
                format!("▸ display ({label}): {preview}")
            }
        }
    }

    /// Height in terminal lines. Returns 1 if collapsed, otherwise
    /// the cached height or 1 as fallback if not yet computed.
    pub fn height(&self) -> u16 {
        if self.collapsed {
            return 1;
        }
        self.cached_height.unwrap_or(1)
    }

    /// Whether this section type supports collapsing.
    ///
    /// Thinking, ToolCall, and ToolResult sections are collapsible.
    /// Text and Display sections are not.
    pub fn is_collapsible(&self) -> bool {
        matches!(
            self.kind,
            SectionKind::Thinking(_)
                | SectionKind::ToolCall { .. }
                | SectionKind::ToolResult { .. }
        )
    }
}

/// Truncate a string to at most `max_chars` characters, appending `...`
/// if truncated. Replaces newlines with spaces for single-line display.
/// Render a short label for a [`pattern_core::types::origin::Author`] suitable
/// for prefixing a one-line outbound-message line in the conversation view.
fn format_sender_label(author: &pattern_core::types::origin::Author) -> String {
    use pattern_core::types::origin::Author;
    match author {
        Author::Partner(_) => "[partner]".to_string(),
        Author::Human(h) => match &h.display_name {
            Some(name) => format!("[{name}]"),
            None => "[human]".to_string(),
        },
        Author::Agent(a) => format!("[{}]", a.agent_id),
        Author::System { reason } => format!("[system:{reason:?}]"),
        // `Author` is `#[non_exhaustive]`; future variants render
        // generically until a dedicated label is added.
        _ => "[unknown]".to_string(),
    }
}

fn truncate_preview(s: &str, max_chars: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if cleaned.chars().count() <= max_chars {
        cleaned
    } else {
        let truncated: String = cleaned.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

// ---------------------------------------------------------------------------
// RenderBatch
// ---------------------------------------------------------------------------

/// One user-to-agent exchange in the conversation.
#[derive(Debug, Clone)]
pub struct RenderBatch {
    /// Unique identifier for this batch.
    pub batch_id: SmolStr,
    /// The user's message that initiated this exchange, if any.
    pub user_message: Option<String>,
    /// The agent that authored this batch's response, if known. When set, a
    /// `[name]` label is rendered inline with the first line of the
    /// agent's sections. System/notification batches leave this `None`.
    pub agent_name: Option<SmolStr>,
    /// Ordered sections of agent response content.
    pub sections: Vec<Section>,
    /// Whether the agent is still streaming content for this batch.
    pub streaming: bool,
}

impl RenderBatch {
    /// Create a new batch with the given ID and optional user message.
    pub fn new(batch_id: SmolStr, user_message: Option<String>) -> Self {
        Self {
            batch_id,
            user_message,
            agent_name: None,
            sections: Vec::new(),
            streaming: true,
        }
    }

    /// Attach an agent name to this batch. The renderer shows a `[name]`
    /// label inline with the first section's first line, mirroring the
    /// `[you]` prefix on the user message.
    pub fn with_agent(mut self, name: SmolStr) -> Self {
        self.agent_name = Some(name);
        self
    }

    /// Append a wire turn event to this batch, extending or creating sections
    /// as appropriate.
    ///
    /// Accepts [`WireTurnEvent`] (the postcard-safe wire format) rather than
    /// the internal `TurnEvent`, since the TUI receives events over the wire
    /// from the daemon.
    pub fn push_event(&mut self, event: &WireTurnEvent) {
        match event {
            WireTurnEvent::Text(chunk) => {
                // Extend the last Text section if one exists, otherwise create new.
                if let Some(section) = self.sections.last_mut()
                    && let SectionKind::Text(ref mut existing) = section.kind
                {
                    existing.push_str(chunk);
                    section.cached_height = None;
                    return;
                }
                self.sections
                    .push(Section::new(SectionKind::Text(chunk.clone())));
            }
            WireTurnEvent::Thinking(chunk) => {
                // Extend the last Thinking section if one exists, otherwise create new.
                if let Some(section) = self.sections.last_mut()
                    && let SectionKind::Thinking(ref mut existing) = section.kind
                {
                    existing.push_str(chunk);
                    section.cached_height = None;
                    return;
                }
                self.sections
                    .push(Section::new(SectionKind::Thinking(chunk.clone())));
            }
            WireTurnEvent::ToolCall {
                call_id,
                function_name,
                arguments_json,
            } => {
                self.sections.push(Section::new(SectionKind::ToolCall {
                    call_id: call_id.clone(),
                    function_name: function_name.clone(),
                    arguments: arguments_json.clone(),
                }));
            }
            WireTurnEvent::ToolResult {
                call_id,
                success,
                content_json,
            } => {
                self.sections.push(Section::new(SectionKind::ToolResult {
                    call_id: call_id.clone(),
                    success: *success,
                    content: content_json.clone(),
                }));
            }
            WireTurnEvent::Display { kind, text } => {
                self.sections.push(Section::new(SectionKind::Display {
                    kind: *kind,
                    text: text.clone(),
                }));
            }
            WireTurnEvent::MessageSent {
                recipient,
                body,
                from,
            } => {
                // Render outbound agent traffic as a Display::Note section
                // with a "→ recipient" prefix. Phase 4 introduces the
                // event; future work may dedicate a SectionKind for it
                // once the design settles. For now the existing Display
                // path keeps the rendering surface narrow.
                let label = format_sender_label(from);
                self.sections.push(Section::new(SectionKind::Display {
                    kind: pattern_core::traits::turn_sink::DisplayKind::Note,
                    text: format!("{label} → {recipient}: {body}"),
                }));
            }
            WireTurnEvent::Stop(_) => {
                self.streaming = false;
            }
            WireTurnEvent::FrontingChanged { .. } => {
                // Phase 5: fronting-state notifications. The TUI's
                // status line / fronting-status indicator is the
                // intended consumer; the conversation view does not
                // render this event as a section.
            }
            WireTurnEvent::ConstellationChanged { .. } => {
                // Phase 6 T8: registry-mutation notifications. Consumed
                // by the constellation panel (re-fetches on receipt);
                // not rendered in the conversation view.
            }
        }
    }

    /// Compute and cache heights for all sections that have `None` cached height.
    /// Uses markdown rendering for Text sections and plain line counting for others.
    pub fn compute_heights(&mut self, width: u16) {
        for section in &mut self.sections {
            if section.cached_height.is_some() {
                continue;
            }
            if section.collapsed {
                section.cached_height = Some(1);
                continue;
            }
            let height = match &section.kind {
                SectionKind::Text(s) => markdown::markdown_height(s, width),
                SectionKind::Thinking(s) => plain_text_height(s, width),
                SectionKind::ToolCall {
                    arguments,
                    function_name: _,
                    ..
                } => {
                    // Header line + indented arguments. The inner width
                    // must match the renderer's narrowed draw rect or the
                    // cached height will under-count wrapped lines.
                    let header_height = 1u16;
                    let inner_width = width.saturating_sub(TOOL_BODY_INDENT);
                    let args_height = plain_text_height(arguments, inner_width);
                    header_height.saturating_add(args_height)
                }
                SectionKind::ToolResult { content, .. } => {
                    // Header line + indented content (see ToolCall note).
                    let header_height = 1u16;
                    let inner_width = width.saturating_sub(TOOL_BODY_INDENT);
                    let content_height = plain_text_height(content, inner_width);
                    header_height.saturating_add(content_height)
                }
                SectionKind::Display { text, .. } => plain_text_height(text, width),
            };
            section.cached_height = Some(height.max(1));
        }
    }

    /// Total height of this batch in terminal lines: user message line (if
    /// any) + intra-batch gap (blank separator between user and agent, when
    /// both sides have content) + sum of section heights. The `[agent]`
    /// label is rendered inline with the first section's first line and
    /// does not occupy its own row.
    pub fn total_height(&self) -> u16 {
        let user_msg_height: u16 = if self.user_message.is_some() { 1 } else { 0 };
        let intra_gap: u16 = if self.user_message.is_some()
            && (self.agent_name.is_some() || !self.sections.is_empty())
        {
            1
        } else {
            0
        };
        let sections_height: u16 = self.sections.iter().map(|s| s.height()).sum();
        user_msg_height
            .saturating_add(intra_gap)
            .saturating_add(sections_height)
    }
}

/// Compute the height of plain text when wrapped at a given width.
/// Uses ratatui's `Paragraph::line_count` for accurate wrapping.
fn plain_text_height(text: &str, width: u16) -> u16 {
    use ratatui::text::Text;
    use ratatui_widgets::paragraph::{Paragraph, Wrap};

    if text.is_empty() || width == 0 {
        return 1;
    }
    let t = Text::from(text.to_owned());
    let paragraph = Paragraph::new(t).wrap(Wrap { trim: true });
    (paragraph.line_count(width) as u16).max(1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::turn::StopReason;

    fn make_batch() -> RenderBatch {
        RenderBatch::new("test-batch-1".into(), Some("Hello agent".into()))
    }

    #[test]
    fn text_events_concatenate_into_single_section() {
        let mut batch = make_batch();
        batch.push_event(&WireTurnEvent::Text("Hello ".into()));
        batch.push_event(&WireTurnEvent::Text("world".into()));

        assert_eq!(batch.sections.len(), 1);
        match &batch.sections[0].kind {
            SectionKind::Text(s) => assert_eq!(s, "Hello world"),
            other => panic!("expected Text section, got {other:?}"),
        }
        // Text sections are never collapsed.
        assert!(!batch.sections[0].collapsed);
    }

    #[test]
    fn thinking_sections_are_collapsed_by_default() {
        let mut batch = make_batch();
        batch.push_event(&WireTurnEvent::Thinking("Let me consider...".into()));

        assert_eq!(batch.sections.len(), 1);
        assert!(batch.sections[0].collapsed);
        match &batch.sections[0].kind {
            SectionKind::Thinking(s) => assert_eq!(s, "Let me consider..."),
            other => panic!("expected Thinking section, got {other:?}"),
        }
    }

    #[test]
    fn stop_event_marks_batch_not_streaming() {
        let mut batch = make_batch();
        assert!(batch.streaming);

        batch.push_event(&WireTurnEvent::Text("response".into()));
        batch.push_event(&WireTurnEvent::Stop(StopReason::EndTurn));

        assert!(!batch.streaming);
        // Stop does not create a section.
        assert_eq!(batch.sections.len(), 1);
    }

    #[test]
    fn display_events_create_sections() {
        let mut batch = make_batch();
        batch.push_event(&WireTurnEvent::Display {
            kind: DisplayKind::Note,
            text: "Processing...".into(),
        });
        batch.push_event(&WireTurnEvent::Display {
            kind: DisplayKind::Final,
            text: "Done!".into(),
        });

        assert_eq!(batch.sections.len(), 2);
        // Display sections are never collapsed.
        assert!(!batch.sections[0].collapsed);
        assert!(!batch.sections[1].collapsed);

        match &batch.sections[0].kind {
            SectionKind::Display { kind, text } => {
                assert_eq!(*kind, DisplayKind::Note);
                assert_eq!(text, "Processing...");
            }
            other => panic!("expected Display section, got {other:?}"),
        }
    }

    #[test]
    fn thinking_summary_has_triangle_prefix() {
        let section = Section::new(SectionKind::Thinking("deep thoughts".into()));
        let summary = section.summary();
        assert!(summary.starts_with("▸ thinking:"));
        assert!(summary.contains("deep thoughts"));
    }

    #[test]
    fn tool_call_summary_shows_function_name() {
        let section = Section::new(SectionKind::ToolCall {
            call_id: "call-1".into(),
            function_name: "search".into(),
            arguments: "{}".into(),
        });
        assert_eq!(section.summary(), "▸ tool: search");
    }

    #[test]
    fn collapsed_section_height_is_one() {
        let section = Section::new(SectionKind::Thinking("long\nthinking\ncontent".into()));
        assert!(section.collapsed);
        assert_eq!(section.height(), 1);
    }

    #[test]
    fn text_interleaved_with_thinking_creates_separate_sections() {
        let mut batch = make_batch();
        batch.push_event(&WireTurnEvent::Text("First ".into()));
        batch.push_event(&WireTurnEvent::Text("part.".into()));
        batch.push_event(&WireTurnEvent::Thinking("hmm...".into()));
        batch.push_event(&WireTurnEvent::Text("Second part.".into()));

        assert_eq!(batch.sections.len(), 3);
        assert!(matches!(&batch.sections[0].kind, SectionKind::Text(s) if s == "First part."));
        assert!(matches!(&batch.sections[1].kind, SectionKind::Thinking(s) if s == "hmm..."));
        assert!(matches!(&batch.sections[2].kind, SectionKind::Text(s) if s == "Second part."));
    }
}
