//! Attachment rendering, block-write body formatting, skill-loaded text
//! rendering, and message splicing for the compose pipeline.
//!
//! ALL system-reminder-style rendering goes through this module. No
//! standalone pseudo-message ChatMessages are produced anywhere.
//!
//! # Public surface
//!
//! Per-variant attachment renderers:
//! - [`render_file_edit_attachment`] — FileEdit -> `<system-reminder>` string.
//! - [`render_file_conflict_attachment`] — FileConflict -> `<system-reminder>` string.
//! - [`render_block_write_attachment`] — BlockWriteNotifications -> `<system-reminder>` string.
//! - [`render_port_event_attachment`] — PortEvent -> `<system-reminder>` string.
//!
//! Composite renderers:
//! - [`render_attachment_content`] — single attachment -> raw text (no wrapper).
//! - [`render_attachments_for_message`] — all attachments on a message -> wrapped text.
//!
//! Splice helper:
//! - [`splice_text_onto_message`] — splice rendered text onto a `ChatMessage`.
//!
//! Skill rendering (tool_result content, not system-reminder):
//! - [`render_skill_loaded_text`] — `[skill:loaded]` marker text for tool_result.
//!
//! Block-write body formatting (used internally by the attachment renderer):
//! - [`render_block_write_body`] — single BlockWrite -> raw body text (no wrapper).

use genai::chat::ChatMessage;
use pattern_core::types::block::{BlockWrite, BlockWriteKind};
use pattern_core::types::memory_types::SkillTrustTier;
use pattern_core::types::message::{
    FileEditKind, MessageAttachment, ShellOutputKind, SnapshotKind,
};
use pattern_core::types::origin::Author;

use crate::shaper::wrap_system_reminder;

/// Maximum number of characters to show in a content preview before
/// eliding the remainder with a suffix message.
const PREVIEW_MAX_CHARS: usize = 240;

// ---- Per-variant attachment renderers --------------------------------------

/// Render a `MessageAttachment::FileEdit` as a `<system-reminder>` string.
pub fn render_file_edit_attachment(
    path: &std::path::Path,
    kind: FileEditKind,
    at: jiff::Timestamp,
    diff: Option<&str>,
) -> String {
    wrap_system_reminder(&render_file_edit_body(path, kind, at, diff))
}

/// Render a `MessageAttachment::FileConflict` as a `<system-reminder>` string.
pub fn render_file_conflict_attachment(path: &std::path::Path, at: jiff::Timestamp) -> String {
    wrap_system_reminder(&render_file_conflict_body(path, at))
}

/// Render a `MessageAttachment::BlockWriteNotifications` as a
/// `<system-reminder>` string. Returns `None` if `writes` is empty.
pub fn render_block_write_attachment(writes: &[BlockWrite]) -> Option<String> {
    if writes.is_empty() {
        return None;
    }
    let bodies: Vec<String> = writes.iter().map(render_block_write_body).collect();
    Some(wrap_system_reminder(&bodies.join("\n\n")))
}

/// Render a `MessageAttachment::ShellOutput` as a `<system-reminder>` string.
///
/// Each variant renders as a distinct framing:
/// - `Output`: fenced code block with the raw text.
/// - `Exit`: one-line status line.
/// - `Backgrounded`: multi-line notice with captured output (forward-compat,
///   not currently enqueued under v2 semantics — see phase_03.md amendment).
pub fn render_shell_output_attachment(
    task_id: &str,
    kind: &ShellOutputKind,
    at: jiff::Timestamp,
) -> String {
    wrap_system_reminder(&render_shell_output_body(task_id, kind, at))
}

/// Render a `MessageAttachment::PortEvent` as a `<system-reminder>` string.
///
/// The payload is pretty-printed JSON; a single-line compact form would lose
/// structure for deeply-nested event payloads (e.g. Slack message objects).
pub fn render_port_event_attachment(
    port_id: &str,
    payload: &serde_json::Value,
    at: jiff::Timestamp,
) -> String {
    wrap_system_reminder(&render_port_event_body(port_id, payload, at))
}

// ---- Composite renderers ---------------------------------------------------

/// Render a single attachment's inner content (NO `<system-reminder>` wrap).
///
/// Multiple attachments on the same message are grouped into a single
/// `<system-reminder>` block by [`render_attachments_for_message`]. Per-variant
/// renderers return raw content; the splice path handles wrapping.
pub fn render_attachment_content(attachment: &MessageAttachment) -> String {
    match attachment {
        MessageAttachment::BatchOpeningSnapshot {
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
                        let names: Vec<&str> = edited_blocks.iter().map(|s| s.as_str()).collect();
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
                let names: Vec<&str> = block_names.iter().map(|s| s.as_str()).collect();
                parts.push(format!("Available blocks: {}", names.join(", ")));
            }

            for block in blocks {
                if let Some(ref rendered) = block.rendered {
                    parts.push(rendered.to_string());
                }
            }

            parts.join("\n\n")
        }
        MessageAttachment::SkillAvailable {
            handle: _,
            name,
            trust_tier,
            description,
            keywords,
        } => {
            let tier_str =
                serde_json::to_string(trust_tier).unwrap_or_else(|_| "\"unknown\"".to_string());
            let tier_kebab = tier_str.trim_matches('"');
            let mut header =
                format!("[skill:available] name=\"{name}\" trust_tier=\"{tier_kebab}\"");
            if let Some(desc) = description.as_deref().filter(|s| !s.is_empty()) {
                header.push_str(&format!(" description=\"{desc}\""));
            }
            let mut parts = vec![header];
            if !keywords.is_empty() {
                parts.push(format!("keywords: [{}]", keywords.join(", ")));
            }
            parts.push("[skill:available:end]".to_string());
            parts.join("\n")
        }
        MessageAttachment::Custom { content } => content.clone(),
        MessageAttachment::FileEdit {
            path,
            kind,
            at,
            diff,
        } => render_file_edit_body(path, *kind, *at, diff.as_deref()),
        MessageAttachment::FileConflict { path, at } => render_file_conflict_body(path, *at),
        MessageAttachment::BlockWriteNotifications { writes } => {
            if writes.is_empty() {
                return String::new();
            }
            let bodies: Vec<String> = writes.iter().map(render_block_write_body).collect();
            bodies.join("\n\n")
        }
        MessageAttachment::ShellOutput { task_id, kind, at } => {
            render_shell_output_body(task_id, kind, *at)
        }
        MessageAttachment::PortEvent {
            port_id,
            payload,
            at,
        } => render_port_event_body(port_id, payload, *at),
        // Future variants — skip gracefully.
        _ => String::new(),
    }
}

/// Render all attachments on a message into a single grouped
/// `<system-reminder>` block. Returns `None` if `attachments` is empty.
pub fn render_attachments_for_message(attachments: &[MessageAttachment]) -> Option<String> {
    if attachments.is_empty() {
        return None;
    }
    let parts: Vec<String> = attachments.iter().map(render_attachment_content).collect();
    let body = parts.join("\n\n");
    Some(wrap_system_reminder(&body))
}

// ---- Splice helper ---------------------------------------------------------

/// Splice rendered text onto a `ChatMessage`'s content.
///
/// For user-role messages: appends as a `ContentPart::Text` AFTER existing
/// content. For tool-role messages: folds into the LAST `ToolResponse`'s
/// content array (same as the old `smooshIntoToolResult` pattern), preserving
/// Anthropic's wire-format constraint that `tool_result` blocks come first.
pub fn splice_text_onto_message(msg: &mut ChatMessage, text: &str) {
    use genai::chat::{ChatRole, ContentPart, MessageContent};

    match msg.role {
        ChatRole::Tool => {
            let original_parts = msg.content.parts().clone();
            let mut new_parts: Vec<ContentPart> = Vec::with_capacity(original_parts.len());
            let mut folded = false;

            for part in original_parts.into_iter().rev() {
                if !folded && let ContentPart::ToolResponse(mut tr) = part {
                    let seg3_block = serde_json::json!({"type": "text", "text": text});
                    let folded_content = match tr.content {
                        serde_json::Value::String(ref s) => {
                            serde_json::json!([
                                seg3_block,
                                {"type": "text", "text": s},
                            ])
                        }
                        serde_json::Value::Array(ref items) => {
                            let mut arr = Vec::with_capacity(items.len() + 1);
                            arr.push(seg3_block);
                            arr.extend(items.iter().cloned());
                            serde_json::Value::Array(arr)
                        }
                        ref other => {
                            serde_json::json!([
                                seg3_block,
                                {"type": "text", "text": other.to_string()},
                            ])
                        }
                    };
                    tr.content = folded_content;
                    new_parts.push(ContentPart::ToolResponse(tr));
                    folded = true;
                    continue;
                }
                new_parts.push(part);
            }
            new_parts.reverse();
            msg.content = MessageContent::from_parts(new_parts);
        }
        _ => {
            let mut parts = msg.content.parts().clone();
            parts.push(ContentPart::Text(text.to_string()));
            msg.content = MessageContent::from_parts(parts);
        }
    }
}

// ---- Skill rendering -------------------------------------------------------

/// Render the `[skill:loaded] ... [skill:loaded:end]` text for a successful
/// `Pattern.Skills.Load` call.
///
/// Returns the raw text (markers + frontmatter line + full body) WITHOUT
/// `<system-reminder>` wrapping — the load handler returns this string as
/// the tool_result content, where the role itself is the system-side
/// framing. The agent pattern-matches on the `[skill:loaded]` markers.
///
/// Because tool_result messages are part of `TurnHistory::active_messages`,
/// the rendered content naturally persists in segment 2 across subsequent
/// turns without needing a separate pseudo-message pipe.
///
/// # Examples
///
/// ```
/// use pattern_core::types::memory_types::SkillTrustTier;
/// use pattern_provider::compose::render::render_skill_loaded_text;
///
/// let text = render_skill_loaded_text("my-skill", SkillTrustTier::ProjectLocal, "## Overview\nDoes things.");
/// assert!(text.starts_with("[skill:loaded]"));
/// assert!(text.ends_with("[skill:loaded:end]"));
/// ```
pub fn render_skill_loaded_text(name: &str, trust_tier: SkillTrustTier, body: &str) -> String {
    let tier_str = serde_json::to_string(&trust_tier).unwrap_or_else(|_| "\"unknown\"".to_string());
    let tier_kebab = tier_str.trim_matches('"');
    format!(
        "[skill:loaded] name=\"{name}\" trust_tier=\"{tier_kebab}\"\n\n{body}\n\n[skill:loaded:end]"
    )
}

// ---- Block-write body rendering (internal) ---------------------------------

/// Render the body text for a single [`BlockWrite`] event.
///
/// Returns the raw text (no `<system-reminder>` wrapper). Used by
/// [`render_block_write_attachment`] (joins multiple bodies then wraps
/// once) and by [`render_attachment_content`] for the
/// `BlockWriteNotifications` variant.
pub fn render_block_write_body(event: &BlockWrite) -> String {
    match event.kind {
        BlockWriteKind::Created => render_created(event),
        BlockWriteKind::Replaced | BlockWriteKind::Appended | BlockWriteKind::Updated => {
            render_updated(event)
        }
        BlockWriteKind::Deleted => render_deleted(event),
        // Non-exhaustive: forward-compatible for future variants.
        _ => render_unknown(event),
    }
}

fn render_created(event: &BlockWrite) -> String {
    let ts = render_local_timestamp(event.at);
    let author = render_author(&event.author);
    let preview = preview(&event.rendered_content, PREVIEW_MAX_CHARS);
    format!(
        "[memory:written] block '{}' (type: {}, author: {}, at: {})\n{}",
        event.handle,
        render_block_type(event.block_type),
        author,
        ts,
        preview,
    )
}

fn render_updated(event: &BlockWrite) -> String {
    let ts = render_local_timestamp(event.at);
    let author = render_author(&event.author);
    let diff_body = match &event.previous_rendered_content {
        Some(previous) => {
            let diff = render_diff(previous, &event.rendered_content);
            if diff.is_empty() {
                format!(
                    "(content unchanged from previous snapshot)\n{}",
                    preview(&event.rendered_content, PREVIEW_MAX_CHARS)
                )
            } else {
                diff
            }
        }
        None => match event.previous_content_hash {
            Some(hash) => format!(
                "(content replaced; previous hash {hash:#018x})\n{}",
                preview(&event.rendered_content, PREVIEW_MAX_CHARS)
            ),
            None => format!(
                "(previous content unavailable)\n{}",
                preview(&event.rendered_content, PREVIEW_MAX_CHARS)
            ),
        },
    };
    format!(
        "[memory:updated] block '{}' (type: {}, author: {}, at: {})\n{}",
        event.handle,
        render_block_type(event.block_type),
        author,
        ts,
        diff_body,
    )
}

fn render_deleted(event: &BlockWrite) -> String {
    let ts = render_local_timestamp(event.at);
    let author = render_author(&event.author);
    format!(
        "[memory:deleted] block '{}' (type: {}, author: {}, at: {})",
        event.handle,
        render_block_type(event.block_type),
        author,
        ts,
    )
}

fn render_unknown(event: &BlockWrite) -> String {
    let ts = render_local_timestamp(event.at);
    let author = render_author(&event.author);
    format!(
        "[memory:changed] block '{}' (type: {}, author: {}, at: {})\n{}",
        event.handle,
        render_block_type(event.block_type),
        author,
        ts,
        preview(&event.rendered_content, PREVIEW_MAX_CHARS),
    )
}

// ---- Internal helpers ------------------------------------------------------

/// FileEdit body WITHOUT `<system-reminder>` wrap (for grouping).
fn render_file_edit_body(
    path: &std::path::Path,
    kind: FileEditKind,
    at: jiff::Timestamp,
    diff: Option<&str>,
) -> String {
    let kind_label = match kind {
        FileEditKind::Open => "you had open",
        FileEditKind::Watch => "you were watching",
    };
    let mut body = format!(
        "External edit while you were thinking:\n- {at} {} ({kind_label}) changed",
        path.display(),
    );
    if let Some(d) = diff {
        body.push_str(":\n```\n");
        body.push_str(d);
        body.push_str("\n```");
    }
    body
}

/// `ShellOutput` body WITHOUT `<system-reminder>` wrap (for grouping when
/// multiple attachments land on the same message).
fn render_shell_output_body(task_id: &str, kind: &ShellOutputKind, at: jiff::Timestamp) -> String {
    match kind {
        ShellOutputKind::Output(text) => {
            format!("shell task {task_id} @ {at}:\n```\n{text}\n```")
        }
        ShellOutputKind::Exit { code, duration_ms } => {
            // Render exit code as `exit=N` for a clean numeric form, or
            // `exit=signal` when the process was killed by a signal (no
            // exit code). Using Rust's Debug format (`code=Some(0)`) was
            // the original but exposes implementation details to the model.
            let code_str = match code {
                Some(c) => format!("exit={c}"),
                None => "exit=signal".to_string(),
            };
            format!("shell task {task_id} @ {at}: [exited {code_str} duration_ms={duration_ms}]")
        }
        ShellOutputKind::Backgrounded { partial_output } => {
            // Forward-compat: no current code path enqueues this variant under v2
            // timeout semantics. See phase_03.md AC3.7 amendment (2026-04-26).
            format!(
                "Shell.Execute timed out; backgrounded as task {task_id} @ {at}.\n\
                 Output captured before backgrounding (more will follow as it arrives):\n\
                 ```\n{partial_output}\n```"
            )
        }
    }
}

/// `PortEvent` body WITHOUT `<system-reminder>` wrap (for grouping when
/// multiple attachments land on the same message).
fn render_port_event_body(
    port_id: &str,
    payload: &serde_json::Value,
    at: jiff::Timestamp,
) -> String {
    let payload_str = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    format!("[port:event] port=\"{port_id}\" at={at}\n{payload_str}")
}

/// FileConflict body WITHOUT `<system-reminder>` wrap (for grouping).
fn render_file_conflict_body(path: &std::path::Path, at: jiff::Timestamp) -> String {
    format!(
        "File modified externally; your last edit may have been overwritten:\n- {at} {path} (conflict)\nA different process wrote to this file in a way that doesn't include your last save. Choices:\n  - File.Reload(path) \u{2014} take the disk version, discard your in-memory edits.\n  - File.ForceWrite(path, your_content) \u{2014} overwrite disk with your version.\n  - File.Write(path, merged) \u{2014} write a manually-merged version.",
        at = at,
        path = path.display(),
    )
}

fn render_author(author: &Author) -> String {
    match author {
        // Phase 6 T8: prefer the human-facing display name when set, fall
        // back to user_id otherwise. The same priority applies to Human.
        Author::Partner(p) => match &p.display_name {
            Some(name) => format!("partner {name}"),
            None => format!("partner {}", p.user_id),
        },
        Author::Human(h) => match &h.display_name {
            Some(name) => format!("human {name}"),
            None => format!("human {}", h.user_id),
        },
        Author::Agent(a) => format!("agent {}", a.agent_id),
        Author::System { reason } => format!("system ({reason:?})"),
        _ => "<unknown source>".to_string(),
    }
}

fn render_local_timestamp(ts: jiff::Timestamp) -> String {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    zoned.strftime("%Y-%m-%d %H:%M:%S %Z (%A)").to_string()
}

fn preview(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_string();
    }
    let head: String = content.chars().take(max_chars).collect();
    let remaining = count - max_chars;
    format!("{head}… ({remaining} chars elided)")
}

fn render_diff(previous: &str, current: &str) -> String {
    let diff = similar::TextDiff::from_lines(previous, current);
    let mut out = String::new();
    for hunk in diff.unified_diff().context_radius(1).iter_hunks() {
        out.push_str(&hunk.to_string());
    }
    out
}

/// Human-readable label for a [`MemoryBlockType`], shared with sibling
/// modules under `compose/` (notably `current_state`). `pub(super)` keeps it
/// out of the public re-export list while letting both renderers agree on a
/// single mapping.
///
/// `MemoryBlockType` is `#[non_exhaustive]`, so external matches require a
/// fallback. New variants render as `"working"` until an explicit arm is added
/// here.
pub(super) fn render_block_type(
    bt: pattern_core::types::memory_types::MemoryBlockType,
) -> &'static str {
    use pattern_core::types::memory_types::MemoryBlockType;
    match bt {
        MemoryBlockType::Core => "core",
        MemoryBlockType::Working => "working",
        _ => "working",
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use genai::chat::ChatRole;
    use jiff::Timestamp;
    use smol_str::SmolStr;
    use std::path::Path;

    use pattern_core::types::block::{BlockWrite, BlockWriteKind};
    use pattern_core::types::ids::new_id;
    use pattern_core::types::memory_types::{MemoryBlockType, SkillTrustTier};
    use pattern_core::types::message::MessageAttachment;
    use pattern_core::types::origin::{AgentAuthor, Author, Human, Partner, SystemReason};

    use super::*;

    fn msg_text(msg: &ChatMessage) -> String {
        msg.content.joined_texts().unwrap_or_default()
    }

    fn make_event(
        handle: &str,
        kind: BlockWriteKind,
        rendered_content: &str,
        previous: Option<&str>,
        previous_hash: Option<u64>,
        author: Author,
    ) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_test_01"),
            block_type: MemoryBlockType::Working,
            rendered_content: rendered_content.to_string(),
            kind,
            previous_content_hash: previous_hash,
            previous_rendered_content: previous.map(|s| s.to_string()),
            at: Timestamp::from_second(1_745_000_000).unwrap(),
            author,
        }
    }

    fn system_author() -> Author {
        Author::System {
            reason: SystemReason::ToolCall,
        }
    }

    // ---- Block-write body rendering ----------------------------------------

    #[test]
    fn created_produces_written_tag_with_attribution() {
        let event = make_event(
            "task_list",
            BlockWriteKind::Created,
            "- [ ] do the thing",
            None,
            None,
            system_author(),
        );
        let body = render_block_write_body(&event);
        assert!(body.contains("[memory:written]"), "missing tag: {body}");
        assert!(body.contains("task_list"), "missing handle: {body}");
        assert!(body.contains("do the thing"), "missing preview: {body}");
        assert!(body.contains("system"), "missing author: {body}");
        assert!(body.contains("2025"), "missing year: {body}");
    }

    #[test]
    fn updated_with_previous_content_produces_diff() {
        let event = make_event(
            "persona",
            BlockWriteKind::Updated,
            "line1\nline2 changed\nline3",
            Some("line1\nline2 original\nline3"),
            None,
            system_author(),
        );
        let body = render_block_write_body(&event);
        assert!(body.contains("[memory:updated]"), "missing tag: {body}");
        assert!(
            body.contains("+line2 changed") || body.contains("-line2 original"),
            "diff missing: {body}"
        );
    }

    #[test]
    fn updated_with_hash_only_produces_hash_fallback() {
        let event = make_event(
            "notes",
            BlockWriteKind::Updated,
            "new content here",
            None,
            Some(0xDEAD_u64),
            system_author(),
        );
        let body = render_block_write_body(&event);
        assert!(body.contains("[memory:updated]"), "missing tag: {body}");
        assert!(
            body.contains("previous hash"),
            "missing hash marker: {body}"
        );
    }

    #[test]
    fn deleted_renders_deleted_tag() {
        let event = make_event(
            "old_block",
            BlockWriteKind::Deleted,
            "",
            None,
            None,
            system_author(),
        );
        let body = render_block_write_body(&event);
        assert!(body.contains("[memory:deleted]"), "missing tag: {body}");
        assert!(body.contains("old_block"), "missing handle: {body}");
    }

    #[test]
    fn empty_diff_falls_back_to_unchanged_notice() {
        let same = "identical content\nline two";
        let event = make_event(
            "block",
            BlockWriteKind::Updated,
            same,
            Some(same),
            None,
            system_author(),
        );
        let body = render_block_write_body(&event);
        assert!(body.contains("unchanged"), "missing fallback: {body}");
    }

    // ---- Block-write attachment rendering -----------------------------------

    #[test]
    fn block_write_attachment_wraps_in_system_reminder() {
        let writes = vec![make_event(
            "tasks",
            BlockWriteKind::Updated,
            "new",
            Some("old"),
            None,
            system_author(),
        )];
        let rendered = render_block_write_attachment(&writes).unwrap();
        assert!(
            rendered.contains("<system-reminder>"),
            "missing wrapper: {rendered}"
        );
        assert!(
            rendered.contains("[memory:updated]"),
            "missing tag: {rendered}"
        );
    }

    #[test]
    fn block_write_attachment_empty_returns_none() {
        assert!(render_block_write_attachment(&[]).is_none());
    }

    // ---- Author rendering --------------------------------------------------

    #[test]
    fn agent_author_attribution() {
        let event = make_event(
            "block",
            BlockWriteKind::Created,
            "content",
            None,
            None,
            Author::Agent(AgentAuthor {
                agent_id: SmolStr::new("peer-agent"),
            }),
        );
        let body = render_block_write_body(&event);
        assert!(
            body.contains("agent peer-agent"),
            "missing attribution: {body}"
        );
    }

    #[test]
    fn partner_author_attribution() {
        let event = make_event(
            "block",
            BlockWriteKind::Created,
            "content",
            None,
            None,
            Author::Partner(Partner {
                user_id: SmolStr::new("user123"),
                display_name: None,
            }),
        );
        let body = render_block_write_body(&event);
        assert!(
            body.contains("partner user123"),
            "missing attribution: {body}"
        );
    }

    /// Phase 6 T8: when a Partner carries a display_name, render the name —
    /// not the opaque user_id — so the agent sees the human-facing label.
    #[test]
    fn partner_author_uses_display_name_when_set() {
        let event = make_event(
            "block",
            BlockWriteKind::Created,
            "content",
            None,
            None,
            Author::Partner(Partner {
                user_id: SmolStr::new("user-opaque-id-abc"),
                display_name: Some("orual".to_string()),
            }),
        );
        let body = render_block_write_body(&event);
        assert!(
            body.contains("partner orual"),
            "Partner with display_name should render the name; got: {body}"
        );
        assert!(
            !body.contains("user-opaque-id-abc"),
            "Partner with display_name must NOT leak the user_id; got: {body}"
        );
    }

    #[test]
    fn human_author_uses_display_name() {
        let event = make_event(
            "block",
            BlockWriteKind::Created,
            "content",
            None,
            None,
            Author::Human(Human {
                user_id: new_id(),
                display_name: Some("alex".to_string()),
            }),
        );
        let body = render_block_write_body(&event);
        assert!(body.contains("human alex"), "display name not used: {body}");
    }

    // ---- Preview helper ----------------------------------------------------

    #[test]
    fn preview_short_content_unchanged() {
        assert_eq!(preview("short", 240), "short");
    }

    #[test]
    fn preview_long_content_truncated() {
        let content: String = "x".repeat(300);
        let result = preview(&content, 240);
        assert!(result.contains("…"), "missing ellipsis: {result}");
        assert!(result.contains("60 chars elided"), "wrong count: {result}");
    }

    // ---- File attachment rendering -----------------------------------------

    #[test]
    fn render_file_edit_open_no_diff_snapshot() {
        let path = Path::new("/home/orual/notes.txt");
        let at = Timestamp::from_second(1_745_000_000).unwrap();
        let rendered = render_file_edit_attachment(path, FileEditKind::Open, at, None);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_file_edit_watch_no_diff_snapshot() {
        let path = Path::new("/tmp/watched.log");
        let at = Timestamp::from_second(1_745_000_000).unwrap();
        let rendered = render_file_edit_attachment(path, FileEditKind::Watch, at, None);
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_file_edit_with_diff_snapshot() {
        let path = Path::new("/home/orual/notes.txt");
        let at = Timestamp::from_second(1_745_000_000).unwrap();
        let diff = "--- before\nhello\n+++ after\nhello world";
        let rendered = render_file_edit_attachment(path, FileEditKind::Open, at, Some(diff));
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_file_conflict_snapshot() {
        let path = Path::new("/home/orual/project/config.kdl");
        let at = Timestamp::from_second(1_745_000_000).unwrap();
        let rendered = render_file_conflict_attachment(path, at);
        insta::assert_snapshot!(rendered);
    }

    // ---- Skill rendering ---------------------------------------------------

    #[test]
    fn render_skill_loaded_text_snapshot() {
        let text = render_skill_loaded_text(
            "fix-authentication",
            SkillTrustTier::ProjectLocal,
            "## Overview\n\nHandles OAuth2 token refresh for expired sessions.",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn render_skill_loaded_text_renders_trust_tier_as_kebab() {
        let cases = [
            (SkillTrustTier::FirstParty, "first-party"),
            (SkillTrustTier::ProjectLocal, "project-local"),
            (SkillTrustTier::AdHoc, "ad-hoc"),
        ];
        for (tier, expected) in cases {
            let text = render_skill_loaded_text("test-skill", tier, "body.");
            assert!(
                text.contains(&format!("trust_tier=\"{expected}\"")),
                "expected trust_tier=\"{expected}\"; got: {text}"
            );
        }
    }

    #[test]
    fn render_skill_loaded_text_no_system_reminder_wrap() {
        let text = render_skill_loaded_text("my-skill", SkillTrustTier::AdHoc, "body.");
        assert!(text.contains("[skill:loaded]"), "missing opening marker");
        assert!(
            text.contains("[skill:loaded:end]"),
            "missing closing marker"
        );
        assert!(
            !text.contains("<system-reminder>"),
            "must not wrap in system-reminder"
        );
    }

    // ---- ShellOutput attachment rendering ----------------------------------

    fn shell_at() -> jiff::Timestamp {
        // Fixed timestamp for snapshot stability.
        jiff::Timestamp::from_second(1_745_000_000).unwrap()
    }

    #[test]
    fn render_shell_output_output_chunk_snapshot() {
        let at = shell_at();
        let rendered = render_shell_output_attachment(
            "a1b2c3d4",
            &ShellOutputKind::Output("hello world\nsecond line\n".to_string()),
            at,
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_shell_output_exit_snapshot() {
        let at = shell_at();
        let rendered = render_shell_output_attachment(
            "a1b2c3d4",
            &ShellOutputKind::Exit {
                code: Some(0),
                duration_ms: 1234,
            },
            at,
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_shell_output_exit_killed_snapshot() {
        let at = shell_at();
        let rendered = render_shell_output_attachment(
            "a1b2c3d4",
            &ShellOutputKind::Exit {
                code: None,
                duration_ms: 500,
            },
            at,
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_shell_output_backgrounded_snapshot() {
        let at = shell_at();
        let rendered = render_shell_output_attachment(
            "a1b2c3d4",
            &ShellOutputKind::Backgrounded {
                partial_output: "partial output before timeout".to_string(),
            },
            at,
        );
        insta::assert_snapshot!(rendered);
    }

    /// Verify ShellOutput attachment renders through render_attachment_content
    /// (the path Segment2Pass uses).
    #[test]
    fn shell_output_renders_through_render_attachment_content() {
        let at = shell_at();
        let attachment = MessageAttachment::ShellOutput {
            task_id: "tid1".to_string(),
            kind: ShellOutputKind::Output("ls output".to_string()),
            at,
        };
        let content = render_attachment_content(&attachment);
        assert!(
            content.contains("shell task tid1"),
            "missing task id in content: {content}"
        );
        assert!(
            content.contains("ls output"),
            "missing output text in content: {content}"
        );
    }

    /// Verify ShellOutput exit renders through render_attachment_content.
    #[test]
    fn shell_output_exit_renders_through_render_attachment_content() {
        let at = shell_at();
        let attachment = MessageAttachment::ShellOutput {
            task_id: "tid2".to_string(),
            kind: ShellOutputKind::Exit {
                code: Some(1),
                duration_ms: 2000,
            },
            at,
        };
        let content = render_attachment_content(&attachment);
        assert!(content.contains("tid2"), "missing task id: {content}");
        assert!(
            content.contains("exited"),
            "missing 'exited' in exit render: {content}"
        );
        assert!(
            content.contains("2000"),
            "missing duration_ms in exit render: {content}"
        );
    }

    // ---- PortEvent attachment rendering ------------------------------------

    fn port_at() -> jiff::Timestamp {
        // Fixed timestamp for snapshot stability.
        jiff::Timestamp::from_second(1_745_000_000).unwrap()
    }

    #[test]
    fn render_port_event_scalar_payload_snapshot() {
        let at = port_at();
        let rendered = render_port_event_attachment(
            "weather-api",
            &serde_json::json!({"temp_c": 22, "condition": "sunny"}),
            at,
        );
        insta::assert_snapshot!(rendered);
    }

    #[test]
    fn render_port_event_renders_through_render_attachment_content() {
        let at = port_at();
        let attachment = MessageAttachment::PortEvent {
            port_id: "slack".to_string(),
            payload: serde_json::json!({"text": "hello team", "channel": "#general"}),
            at,
        };
        let content = render_attachment_content(&attachment);
        assert!(
            content.contains("[port:event]"),
            "missing port:event tag: {content}"
        );
        assert!(
            content.contains("slack"),
            "missing port_id in content: {content}"
        );
        assert!(
            content.contains("hello team"),
            "missing payload in content: {content}"
        );
    }

    #[test]
    fn render_port_event_wraps_in_system_reminder() {
        let at = port_at();
        let rendered =
            render_port_event_attachment("http", &serde_json::json!({"status": 200}), at);
        assert!(
            rendered.contains("<system-reminder>"),
            "missing system-reminder: {rendered}"
        );
        assert!(
            rendered.contains("[port:event]"),
            "missing port:event tag: {rendered}"
        );
    }

    // ---- Grouped attachment rendering --------------------------------------

    #[test]
    fn render_attachments_groups_into_single_system_reminder() {
        let attachments = vec![
            MessageAttachment::Custom {
                content: "first part".to_string(),
            },
            MessageAttachment::Custom {
                content: "second part".to_string(),
            },
        ];
        let rendered = render_attachments_for_message(&attachments).unwrap();
        assert!(rendered.contains("first part"), "missing first part");
        assert!(rendered.contains("second part"), "missing second part");
        // Single outer wrap, not per-attachment.
        assert_eq!(
            rendered.matches("<system-reminder>").count(),
            1,
            "expected single system-reminder wrapper"
        );
    }

    #[test]
    fn render_attachments_empty_returns_none() {
        assert!(render_attachments_for_message(&[]).is_none());
    }

    // ---- Splice helper -----------------------------------------------------

    #[test]
    fn splice_onto_user_message_appends_text() {
        let mut msg = ChatMessage::user("hello");
        splice_text_onto_message(&mut msg, "appended");
        let text = msg_text(&msg);
        assert!(text.contains("hello"), "original missing: {text}");
        assert!(text.contains("appended"), "spliced text missing: {text}");
    }

    #[test]
    fn splice_onto_tool_message_folds_into_tool_response() {
        use genai::chat::{ContentPart, MessageContent, ToolResponse};
        let tr = ToolResponse {
            call_id: "call-1".to_string(),
            content: serde_json::json!("tool output"),
        };
        let mut msg = ChatMessage {
            role: ChatRole::Tool,
            content: MessageContent::from_parts(vec![ContentPart::ToolResponse(tr)]),
            options: None,
        };
        splice_text_onto_message(&mut msg, "memory snapshot");
        let parts = msg.content.parts();
        assert_eq!(
            parts.len(),
            1,
            "should remain one part (folded ToolResponse)"
        );
        if let ContentPart::ToolResponse(tr) = &parts[0] {
            let content_str = tr.content.to_string();
            assert!(
                content_str.contains("memory snapshot"),
                "spliced text not folded into tool response: {content_str}"
            );
        } else {
            panic!("expected ToolResponse part");
        }
    }
}
