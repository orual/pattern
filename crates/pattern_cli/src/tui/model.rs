// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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

use pattern_core::{traits::turn_sink::DisplayKind, types::message::SnapshotKind};
use pattern_provider::compose::{
    render::{render_file_conflict_body, render_file_edit_body, render_shell_output_body},
    render_block_write_body,
};
use pattern_server::protocol::{WireMessageAttachment, WireTurnEvent};
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
#[allow(dead_code)]
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
        content: Vec<pattern_core::types::provider::ContentPart>,
    },
    /// Agent display output (chunk, final, or note).
    Display {
        kind: DisplayKind,
        text: String,
    },
    Attachments(Vec<String>),
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
                | SectionKind::Attachments(_)
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
                let preview = truncate_preview(s, 100);
                format!("▸ text: {preview}")
            }
            SectionKind::Thinking(s) => {
                let preview = truncate_preview(s, 100);
                format!("▸ thinking: {preview}")
            }
            SectionKind::ToolCall {
                function_name,
                arguments,
                ..
            } => {
                // For the code tool, show first line of code. For others, show function name.
                let preview = if function_name == "code" {
                    extract_code_preview(arguments, 100)
                } else {
                    function_name.clone()
                };
                format!("▸ {function_name}: {preview}")
            }
            SectionKind::ToolResult {
                success, content, ..
            } => {
                let status = if *success { "ok" } else { "err" };
                let preview = extract_result_preview(content, 100);
                format!("▸ result ({status}): {preview}")
            }

            SectionKind::Display { kind, text } => {
                let label = match kind {
                    DisplayKind::Chunk => "chunk",
                    DisplayKind::Final => "final",
                    DisplayKind::Note => "note",
                };
                let preview = truncate_preview(text, 100);
                format!("▸ display ({label}): {preview}")
            }
            SectionKind::Attachments(a) => {
                let s = a.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                let preview = truncate_preview(&s, 100);
                format!("▸ attachments: {preview}")
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
                | SectionKind::Attachments(_)
        )
    }
}


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

/// Truncate a string to at most `max_chars` characters, appending `...`
/// if truncated. Replaces newlines with spaces for single-line display.
fn truncate_preview(s: &str, max_chars: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if cleaned.chars().count() <= max_chars {
        cleaned
    } else {
        let truncated: String = cleaned.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}

/// Extract the first meaningful line of code from tool arguments JSON.
/// Parses the "code" field and returns its first non-empty line.
pub(super) fn extract_code_preview(arguments_json: &str, max_chars: usize) -> String {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(arguments_json) {
        if let Some(code) = parsed.get("code").and_then(|v| v.as_str()) {
            let first_line = code
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("(empty)");
            return truncate_preview(first_line, max_chars);
        }
    }
    truncate_preview(arguments_json, max_chars)
}

/// Extract a readable preview from tool result content.
/// Tries to parse as JSON and show a meaningful summary;
/// falls back to truncated raw text.
pub(super) fn extract_result_preview(
    content: &[pattern_core::types::provider::ContentPart],
    max_chars: usize,
) -> String {
    use pattern_core::types::provider::ContentPart;
    // Build a single-line preview: concat Text parts, substitute Binary parts
    // with `[image:name]` or `[binary:name]` placeholders so the agent can see
    // attachments exist without dumping base64.
    let mut buf = String::new();
    for part in content {
        match part {
            ContentPart::Text(s) => {
                // Try JSON-pretty unwrap for nested-JSON strings (e.g. shell envelopes).
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                    let unwrapped = unwrap_nested_json_strings(parsed);
                    if let Ok(compact) = serde_json::to_string(&unwrapped) {
                        buf.push_str(&compact);
                        continue;
                    }
                }
                buf.push_str(s);
            }
            ContentPart::Binary(b) => {
                let kind = if b.content_type.starts_with("image/") {
                    "image"
                } else if b.content_type == "application/pdf" {
                    "document"
                } else {
                    "binary"
                };
                let name = b.name.as_deref().unwrap_or("<unnamed>");
                buf.push_str(&format!("[{kind}: {name}, {}]", b.content_type));
            }
            _ => {}
        }
    }
    truncate_preview(&buf, max_chars)
}

/// Format tool result content for display. Unescapes the wire JSON encoding,
/// tries to pretty-print nested JSON, and unescapes \n in string values.
pub(super) fn format_result_content(
    content: &[pattern_core::types::provider::ContentPart],
) -> String {
    use pattern_core::types::provider::ContentPart;
    let mut buf = String::new();
    for (i, part) in content.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        match part {
            ContentPart::Text(s) => {
                // Pretty-print if it's nested JSON; otherwise show raw text.
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                    let unwrapped = unwrap_nested_json_strings(parsed);
                    match &unwrapped {
                        serde_json::Value::String(inner) => {
                            buf.push_str(&inner.replace("\\n", "\n"));
                        }
                        other => {
                            let pretty = serde_json::to_string_pretty(other)
                                .unwrap_or_else(|_| s.clone())
                                .replace("\\n", "\n");
                            buf.push_str(&pretty);
                        }
                    }
                } else {
                    buf.push_str(s);
                }
            }
            ContentPart::Binary(b) => {
                let kind = if b.content_type.starts_with("image/") {
                    "image"
                } else if b.content_type == "application/pdf" {
                    "document"
                } else {
                    "binary"
                };
                let name = b.name.as_deref().unwrap_or("<unnamed>");
                buf.push_str(&format!("[{kind}: {name}, {}]", b.content_type));
            }
            _ => {}
        }
    }
    buf
}

/// Walk a Value tree; for every String leaf that itself parses as JSON,
/// replace it with the parsed form. Recurses into arrays + objects.
fn unwrap_nested_json_strings(v: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::String(s) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(&s) {
                // Only unwrap if the parse produced something STRUCTURED — bare
                // strings/numbers/bools that happen to parse as JSON shouldn't be
                // replaced (avoid `"true"` becoming bool, etc).
                if parsed.is_object() || parsed.is_array() {
                    return unwrap_nested_json_strings(parsed);
                }
            }
            Value::String(s)
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(unwrap_nested_json_strings).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k, unwrap_nested_json_strings(v));
            }
            Value::Object(out)
        }
        other => other,
    }
}

/// Extract the display-ready code text from a code tool's arguments JSON.
/// Returns the code (and optional helpers/imports) as a markdown fenced block.
// ---------------------------------------------------------------------------
// TUI render-cost guardrails
//
// Conversation sections store WireTurnEvent payloads as Vec<ContentPart>
// (for tool results) and strings (for attachments). Without bounds, a
// large item — a 2.2MB PDF binary, a full memory-snapshot attachment —
// makes the visible-section render path slow per-frame. We truncate at
// the wire→model boundary so the Section model stores cheap-to-render
// content. The daemon retains canonical full-fidelity copies.
// ---------------------------------------------------------------------------

/// Max base64-encoded byte length of a Binary part before we substitute a
/// short text placeholder in the TUI's Section model. Above this size,
/// rendering pressure shows up even though the placeholder text itself is
/// short — clones and selection paths still iterate the bytes.
const TUI_BINARY_MAX_ENCODED: usize = 32 * 1024;

/// Max character count of an attachment string before truncation in the
/// TUI's Section model. ~half a 200x80 terminal's worth — generous enough
/// that normal memory-snapshots survive, small enough that the post-
/// compaction full-dump doesn't lag the renderer when expanded.
const TUI_ATTACHMENT_MAX_BYTES: usize = 16 * 1024;

/// Walk a Vec<ContentPart>, substituting any Binary parts above
/// `TUI_BINARY_MAX_ENCODED` with a short Text placeholder describing the
/// binary (name, mime, encoded size). Returns a fresh vec sized to be
/// cheap for the TUI to clone/render.
pub(super) fn tui_truncate_parts(
    parts: &[pattern_core::types::provider::ContentPart],
) -> Vec<pattern_core::types::provider::ContentPart> {
    use pattern_core::types::provider::{Binary, BinarySource, ContentPart};
    parts
        .iter()
        .map(|p| match p {
            ContentPart::Binary(b) => {
                let encoded_len = match &b.source {
                    BinarySource::Base64(s) => s.len(),
                    _ => 0,
                };
                if encoded_len > TUI_BINARY_MAX_ENCODED {
                    let name = b.name.as_deref().unwrap_or("<unnamed>");
                    let mime = b.content_type.as_str();
                    let placeholder = format!(
                        "[binary: {name}, {mime}, {encoded_len} encoded bytes — truncated for TUI display, canonical copy retained in daemon]"
                    );
                    ContentPart::Text(placeholder)
                } else {
                    ContentPart::Binary(b.clone())
                }
            }
            ContentPart::Text(s) => {
                // Also guard against giant Text parts. Most commonly this is a
                // legacy back-compat deserializer path where an old tool result
                // stored as a single JSON-stringified blob (with embedded base64)
                // comes back as `Text(stringified_json)` rather than
                // `[Text(marker), Binary(bytes)]`. Truncate at the same byte
                // threshold using byte-bounded char-boundary-safe truncation.
                if s.len() <= TUI_BINARY_MAX_ENCODED {
                    ContentPart::Text(s.clone())
                } else {
                    let total_bytes = s.len();
                    let mut cutoff = TUI_BINARY_MAX_ENCODED;
                    while cutoff > 0 && !s.is_char_boundary(cutoff) {
                        cutoff -= 1;
                    }
                    let kept = &s[..cutoff];
                    let omitted_bytes = total_bytes - cutoff;
                    ContentPart::Text(format!(
                        "{kept}\n\n[... truncated for TUI display: {omitted_bytes} more bytes omitted of {total_bytes} total. Canonical copy retained in daemon.]"
                    ))
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// Truncate an attachment string above `TUI_ATTACHMENT_MAX_BYTES`,
/// appending a brief note that explains the truncation. Uses byte-bounded
/// truncation with a char-boundary walk-back, so total cost is O(1) plus
/// at most 3 bytes for the boundary — no full-string char iteration.
pub(super) fn tui_truncate_attachment_text(text: String) -> String {
    let total_bytes = text.len();
    if total_bytes <= TUI_ATTACHMENT_MAX_BYTES {
        return text;
    }
    // Walk back from the byte cap to the nearest char boundary so we don't
    // split a multi-byte UTF-8 sequence. At most 3 bytes back.
    let mut cutoff = TUI_ATTACHMENT_MAX_BYTES;
    while cutoff > 0 && !text.is_char_boundary(cutoff) {
        cutoff -= 1;
    }
    let kept = &text[..cutoff];
    let omitted_bytes = total_bytes - cutoff;
    format!(
        "{kept}\n\n[... truncated for TUI display: {omitted_bytes} more bytes omitted of {total_bytes} total. Canonical copy retained in daemon.]"
    )
}

pub(super) fn render_code_tool_body(arguments: &str) -> String {

    let code_str = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(arguments) {
        let mut parts = Vec::new();
        if let Some(code) = parsed.get("code").and_then(|v| v.as_str()) {
            parts.push(code.to_string());
        }
        if let Some(helpers) = parsed.get("helpers").and_then(|v| v.as_str()) {
            if !helpers.is_empty() {
                parts.push(format!("-- helpers:\n{helpers}"));
            }
        }
        if let Some(imports) = parsed.get("imports").and_then(|v| v.as_str()) {
            if !imports.is_empty() {
                parts.push(format!("-- imports:\n{imports}"));
            }
        }
        parts.join("\n\n")
    } else {
        arguments.to_string()
    };
    format!("```haskell\n{code_str}\n```")
}

/// Format a non-code tool's arguments for display as a markdown JSON block.
pub(super) fn render_generic_tool_body(arguments: &str) -> String {
    let json_str = serde_json::from_str::<serde_json::Value>(arguments)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| arguments.to_string());
    format!("```json\n{json_str}\n```")
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
    pub message_cached_height: Option<u16>,
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
            message_cached_height: None,
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
                content,
            } => {
                self.sections.push(Section::new(SectionKind::ToolResult {
                    call_id: call_id.clone(),
                    success: *success,
                    content: tui_truncate_parts(content),
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
            WireTurnEvent::Stop(reason) => {
                self.sections.push(Section::new(SectionKind::Display {
                    kind: pattern_core::traits::turn_sink::DisplayKind::Note,
                    text: format!("Stop: {reason:?}"),
                }));
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
            WireTurnEvent::Attachments(a) => {
                self.sections.push(Section::new(SectionKind::Attachments(
                    a.into_iter()
                        .map(|attachment| match attachment {
                            WireMessageAttachment::BatchOpeningSnapshot {
                                kind,
                                block_names,
                                blocks,
                                edited_blocks,
                            } => {
                                let mut parts = Vec::new();
                                parts.push("[memory:current_state]".to_string());

                                match kind {
                                    SnapshotKind::Full => {
                                        parts.push("(full snapshot)".to_string());
                                    }
                                    SnapshotKind::Delta { since_batch } => {
                                        parts.push(format!("(delta since batch {since_batch})"));
                                        if !edited_blocks.is_empty() {
                                            let names: Vec<&str> =
                                                edited_blocks.iter().map(|s| s.as_str()).collect();
                                            parts.push(format!(
                                                "[memory:updated] blocks changed: {}",
                                                names.join(", ")
                                            ));
                                        }
                                    }
                                }

                                if block_names.is_empty() {
                                    parts.push("(no blocks loaded)".to_string());
                                } else {
                                    let names: Vec<&str> =
                                        block_names.iter().map(|s| s.as_str()).collect();
                                    parts.push(format!("Available blocks: {}", names.join(", ")));
                                }

                                for block in blocks {
                                    if let Some(ref rendered) = block.rendered {
                                        parts.push(rendered.to_string());
                                    }
                                }

                                parts.join("\n\n")
                            }
                            WireMessageAttachment::SkillAvailable {
                                handle: _,
                                name,
                                trust_tier,
                                description,
                                keywords,
                            } => {
                                let tier_str = serde_json::to_string(trust_tier)
                                    .unwrap_or_else(|_| "\"unknown\"".to_string());
                                let tier_kebab = tier_str.trim_matches('"');
                                let mut header = format!(
                                    "[skill:available] name=\"{name}\" trust_tier=\"{tier_kebab}\""
                                );
                                if let Some(desc) = description.as_deref().filter(|s| !s.is_empty())
                                {
                                    header.push_str(&format!(" description=\"{desc}\""));
                                }
                                let mut parts = vec![header];
                                if !keywords.is_empty() {
                                    parts.push(format!("keywords: [{}]", keywords.join(", ")));
                                }
                                parts.push("[skill:available:end]".to_string());
                                parts.join("\n")
                            }
                            WireMessageAttachment::Custom { content } => content.clone(),
                            WireMessageAttachment::FileEdit {
                                path,
                                kind,
                                at,
                                diff,
                            } => render_file_edit_body(path, *kind, *at, diff.as_deref()),
                            WireMessageAttachment::FileConflict { path, at } => {
                                render_file_conflict_body(path, *at)
                            }
                            WireMessageAttachment::BlockWriteNotifications { writes } => {
                                if writes.is_empty() {
                                    return String::new();
                                }
                                let bodies: Vec<String> =
                                    writes.iter().map(render_block_write_body).collect();
                                bodies.join("\n\n")
                            }
                            WireMessageAttachment::ShellOutput { task_id, kind, at } => {
                                render_shell_output_body(task_id, kind, *at)
                            }
                            WireMessageAttachment::PortEvent {
                                port_id,
                                payload,
                                at,
                            } => {
                                let port_id: &str = port_id;
                                let at = *at;
                                format!("[port:event] port=\"{port_id}\" at={at}\n{payload}")
                            }
                            // Future variants — skip gracefully.
                            _ => String::new(),
                        })
                        .map(tui_truncate_attachment_text)
                        .collect(),
                )));
            }
        }
    }

    /// Compute and cache heights for all sections that have `None` cached height.
    /// Uses markdown rendering for Text sections and plain line counting for others.
    pub fn compute_heights(&mut self, width: u16) {
        if let Some(user_message) = &self.user_message
            && self.message_cached_height.is_none()
        {
            self.message_cached_height = Some(plain_text_height(user_message, width));
        }
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
                    function_name,
                    ..
                } => {
                    let header_height = 1u16;
                    let inner_width = width.saturating_sub(TOOL_BODY_INDENT);
                    let body_height = if function_name == "code" {
                        let md = render_code_tool_body(arguments);
                        markdown::markdown_height(&md, inner_width)
                    } else {
                        let md = render_generic_tool_body(arguments);
                        markdown::markdown_height(&md, inner_width)
                    };
                    header_height.saturating_add(body_height)
                }
                SectionKind::ToolResult { content, .. } => {
                    let header_height = 1u16;
                    let inner_width = width.saturating_sub(TOOL_BODY_INDENT);
                    let rendered = format_result_content(content);
                    let content_height = plain_text_height(&rendered, inner_width);
                    header_height.saturating_add(content_height)
                }
                SectionKind::Display { text, .. } => plain_text_height(text, width),
                SectionKind::Attachments(a) => {
                    a.iter().map(|s| plain_text_height(s, width)).sum::<u16>()
                }
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
        let user_msg_height: u16 = self.message_cached_height.unwrap_or(1);
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
        // Stop now creates a Display(Note) section rendering the stop
        // reason inline ("Stop: EndTurn"), so we get the original text
        // section plus the stop-note section.
        assert_eq!(batch.sections.len(), 2);
        assert!(matches!(
            &batch.sections[1].kind,
            SectionKind::Display { kind: DisplayKind::Note, text }
                if text.contains("EndTurn")
        ));
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
        // Format: "▸ {function_name}: {preview}". For non-code tools the
        // preview falls back to function_name, so it shows up twice.
        // The test asserts the function_name appears (not the exact string)
        // so future label-format tweaks don't keep breaking this.
        let summary = section.summary();
        assert!(summary.starts_with("▸ "), "summary missing prefix: {summary}");
        assert!(summary.contains("search"), "summary missing function name: {summary}");
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
