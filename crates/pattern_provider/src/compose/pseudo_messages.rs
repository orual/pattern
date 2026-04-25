//! Pseudo-message renderer for memory-change events.
//!
//! Converts [`pattern_core::types::block::BlockWrite`] audit records into
//! `genai::chat::ChatMessage` values carrying `<system-reminder>`-wrapped
//! bodies. These messages are injected into segment 2 of the next turn's
//! composed request (see Phase 5 §"Three-segment cache layout").
//!
//! # Dispatch table
//!
//! | `BlockWriteKind` | Tag emitted | Diff body |
//! |---|---|---|
//! | `Created` | `[memory:written]` | preview of `rendered_content` |
//! | `Replaced` / `Appended` / `Updated` | `[memory:updated]` | unified diff when `previous_rendered_content` is `Some`; hash-fallback + preview otherwise |
//! | `Deleted` | `[memory:deleted]` | none (tombstone note only) |
//!
//! # Public surface
//!
//! - [`render_change_event`] — single `BlockWrite → ChatMessage`.
//! - [`render_change_events`] — batch convenience for segment-2 pass.
//!
//! # Design notes
//!
//! - Diff context radius is 1 (tighter than the `similar` default of 3).
//!   Memory-block edits in an agent context are typically small and
//!   targeted; agents don't benefit from three-line context windows around
//!   every hunk.
//! - `PREVIEW_MAX_CHARS` (240) is chosen to be short enough that several
//!   previews fit within a typical segment-2 cache budget without the
//!   segment-2 TTL becoming fragile. If the content is smaller than the
//!   limit it is emitted whole.
//! - Author rendering uses `display_name` when available, falling back to
//!   the stable id.  `Partner` uses `user_id` (partners don't have
//!   display names in v3; a future phase may add one). `System { reason }`
//!   renders the reason via its `Debug` representation, which is stable and
//!   descriptive enough for agent attribution.

use genai::chat::ChatMessage;
use pattern_core::types::block::{BlockWrite, BlockWriteKind};
use pattern_core::types::memory_types::SkillTrustTier;
use pattern_core::types::origin::Author;

use crate::shaper::wrap_system_reminder;

/// Maximum number of characters to show in a content preview before
/// eliding the remainder with a suffix message.
const PREVIEW_MAX_CHARS: usize = 240;

// ---- Public API ------------------------------------------------------------

/// Render a single [`BlockWrite`] into the corresponding pseudo-message.
///
/// The returned message has `role = User` and carries a
/// `<system-reminder>`-wrapped body.  The body format depends on
/// [`BlockWriteKind`]:
///
/// - `Created` → `[memory:written]` with a content preview.
/// - `Replaced | Appended | Updated` → `[memory:updated]` with a
///   unified diff when `previous_rendered_content` is available, or a
///   hash-fallback marker + preview when only the hash is known.
/// - `Deleted` → `[memory:deleted]` with handle + author + timestamp
///   but no content (reserved for future tombstone wiring).
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use smol_str::SmolStr;
///
/// use pattern_core::types::memory_types::MemoryBlockType;
/// use pattern_core::types::block::{BlockHandle, BlockWrite, BlockWriteKind};
/// use pattern_core::types::origin::{Author, SystemReason};
/// use pattern_provider::compose::pseudo_messages::render_change_event;
///
/// let event = BlockWrite {
///     handle: SmolStr::new("task_list"),
///     memory_id: SmolStr::new("mem_01"),
///     block_type: MemoryBlockType::Working,
///     rendered_content: "- [ ] do the thing".to_string(),
///     kind: BlockWriteKind::Created,
///     previous_content_hash: None,
///     previous_rendered_content: None,
///     at: Timestamp::UNIX_EPOCH,
///     author: Author::System { reason: SystemReason::ToolCall },
/// };
/// let msg = render_change_event(&event);
/// assert_eq!(msg.role, genai::chat::ChatRole::User);
/// ```
pub fn render_change_event(event: &BlockWrite) -> ChatMessage {
    let body = render_body(event);
    let wrapped = wrap_system_reminder(&body);
    ChatMessage::user(wrapped)
}

/// Render a batch of [`BlockWrite`]s into a `Vec<ChatMessage>` in order.
///
/// Convenience wrapper for the segment-2 composer pass, which must emit
/// all memory-change pseudo-messages for the immediately-prior turn at
/// once. The ordering of `events` is preserved in the output.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use smol_str::SmolStr;
///
/// use pattern_core::types::memory_types::MemoryBlockType;
/// use pattern_core::types::block::{BlockHandle, BlockWrite, BlockWriteKind};
/// use pattern_core::types::origin::{Author, SystemReason};
/// use pattern_provider::compose::pseudo_messages::render_change_events;
///
/// let events: Vec<BlockWrite> = vec![];
/// let msgs = render_change_events(&events);
/// assert!(msgs.is_empty());
/// ```
pub fn render_change_events(events: &[BlockWrite]) -> Vec<ChatMessage> {
    events.iter().map(render_change_event).collect()
}

/// Render the `[skill:loaded] … [skill:loaded:end]` text for a successful
/// `Pattern.Skills.Load` call.
///
/// Format:
///
/// ```text
/// [skill:loaded] name="<name>" trust_tier="<kebab>"
///
/// <body>
///
/// [skill:loaded:end]
/// ```
///
/// `trust_tier` is rendered as its kebab-case serde form (e.g.
/// `"project-local"`).
///
/// Returns the raw text (markers + frontmatter line + full body) WITHOUT
/// `<system-reminder>` wrapping — the load handler returns this string as
/// the tool_result content, where the role itself is the system-side
/// framing. The agent pattern-matches on the `[skill:loaded]` markers.
///
/// Because tool_result messages are part of `TurnHistory::active_messages`,
/// the rendered content naturally persists in segment 2 across subsequent
/// turns without needing a separate pseudo-message pipe. The
/// `render_skill_loaded_text_snapshot` test in this module pins the exact
/// rendered form across changes.
///
/// # Examples
///
/// ```
/// use pattern_core::types::memory_types::SkillTrustTier;
/// use pattern_provider::compose::pseudo_messages::render_skill_loaded_text;
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

// ---- Body rendering --------------------------------------------------------

fn render_body(event: &BlockWrite) -> String {
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
                // Previous == current edge case: fall back to preview so we
                // never ship an empty diff body.
                format!(
                    "(content unchanged from previous snapshot)\n{}",
                    preview(&event.rendered_content, PREVIEW_MAX_CHARS)
                )
            } else {
                diff
            }
        }
        None => {
            // Older records that only carry the hash — emit a compact marker
            // and a preview of the new state.
            match event.previous_content_hash {
                Some(hash) => format!(
                    "(content replaced; previous hash {hash:#018x})\n{}",
                    preview(&event.rendered_content, PREVIEW_MAX_CHARS)
                ),
                None => format!(
                    "(previous content unavailable)\n{}",
                    preview(&event.rendered_content, PREVIEW_MAX_CHARS)
                ),
            }
        }
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

/// Fallback for unknown future variants (non-exhaustive forward compat).
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

// ---- Helper functions -------------------------------------------------------

/// Render an [`Author`] to a short human-readable attribution string.
///
/// Uses the display name where available, falling back to the stable id.
/// `System { reason }` renders as `"system (<reason>)"`.
fn render_author(author: &Author) -> String {
    match author {
        Author::Partner(p) => format!("partner {}", p.user_id),
        Author::Human(h) => match &h.display_name {
            Some(name) => format!("human {name}"),
            None => format!("human {}", h.user_id),
        },
        Author::Agent(a) => format!("agent {}", a.agent_id),
        Author::System { reason } => format!("system ({reason:?})"),
        // Non-exhaustive: forward-compatible catchall.
        _ => "<unknown source>".to_string(),
    }
}

/// Render a [`jiff::Timestamp`] to local wall-clock time.
///
/// Format: `"2026-04-17 14:30:00 PDT (Friday)"` — date-first for sortability,
/// TZ abbreviation for disambiguation, weekday at end for agent-readable context.
/// On formatting failure, falls back to the `Zoned`'s `Display` impl.
fn render_local_timestamp(ts: jiff::Timestamp) -> String {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    // %Z gives the TZ abbreviation; %A gives the full weekday name.
    zoned.strftime("%Y-%m-%d %H:%M:%S %Z (%A)").to_string()
}

/// Render a preview of `content` truncated to at most `max_chars` characters.
///
/// If `content` fits within the limit it is returned unchanged. Otherwise the
/// first `max_chars` characters are taken and a suffix of the form
/// `"… (N chars elided)"` is appended so the agent can see that content was
/// cut and how much was removed.
fn preview(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_string();
    }
    let head: String = content.chars().take(max_chars).collect();
    let remaining = count - max_chars;
    format!("{head}… ({remaining} chars elided)")
}

/// Produce a unified diff between `previous` and `current` lines.
///
/// Context radius is 1 (tighter than `similar`'s default of 3) — memory-block
/// edits in an agent context are targeted, and agents don't need three-line
/// context windows around every hunk.
///
/// Returns an empty string when `previous == current`.
fn render_diff(previous: &str, current: &str) -> String {
    let diff = similar::TextDiff::from_lines(previous, current);
    let mut out = String::new();
    for hunk in diff.unified_diff().context_radius(1).iter_hunks() {
        out.push_str(&hunk.to_string());
    }
    out
}

/// Human-readable label for a [`pattern_core::types::memory_types::MemoryBlockType`].
fn render_block_type(bt: pattern_core::types::memory_types::MemoryBlockType) -> &'static str {
    use pattern_core::types::memory_types::MemoryBlockType;
    match bt {
        MemoryBlockType::Core => "core",
        MemoryBlockType::Working | _ => "working",
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use genai::chat::ChatRole;
    use jiff::Timestamp;
    use smol_str::SmolStr;

    use pattern_core::types::block::{BlockWrite, BlockWriteKind};
    use pattern_core::types::ids::new_id;
    use pattern_core::types::memory_types::MemoryBlockType;
    use pattern_core::types::origin::{AgentAuthor, Author, Human, Partner, SystemReason};

    use super::*;

    /// Extract the full text content from a `ChatMessage` for assertion.
    ///
    /// `MessageContent` does not implement `Display`; `joined_texts()` returns
    /// all text parts joined with double newlines, which is the correct view for
    /// our single-part user messages.
    fn msg_text(msg: &ChatMessage) -> String {
        msg.content.joined_texts().unwrap_or_default()
    }

    // ---- fixtures -----------------------------------------------------------

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
            // Use a fixed UTC timestamp for deterministic output.
            at: Timestamp::from_second(1_745_000_000).unwrap(),
            author,
        }
    }

    fn system_author() -> Author {
        Author::System {
            reason: SystemReason::ToolCall,
        }
    }

    // ---- AC8.3: Created produces [memory:written] ---------------------------

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
        let msg = render_change_event(&event);
        assert_eq!(msg.role, ChatRole::User);

        let text = msg_text(&msg);
        // Must carry the <system-reminder> wrapper.
        assert!(
            text.contains("<system-reminder>"),
            "missing wrapper: {text}"
        );
        assert!(
            text.contains("</system-reminder>"),
            "missing wrapper: {text}"
        );
        // Must carry the [memory:written] tag.
        assert!(text.contains("[memory:written]"), "missing tag: {text}");
        // Must carry the block handle.
        assert!(text.contains("task_list"), "missing handle: {text}");
        // Must carry a preview of the content.
        assert!(
            text.contains("do the thing"),
            "missing content preview: {text}"
        );
        // Must carry author.
        assert!(text.contains("system"), "missing author: {text}");
        // Must carry a timestamp with a year (fixture is 2025).
        assert!(text.contains("2025"), "missing year in timestamp: {text}");
    }

    // ---- AC8.3: Updated with previous_rendered_content produces diff --------

    #[test]
    fn updated_with_previous_content_produces_diff_body() {
        let event = make_event(
            "persona",
            BlockWriteKind::Updated,
            "line1\nline2 changed\nline3",
            Some("line1\nline2 original\nline3"),
            None,
            system_author(),
        );
        let msg = render_change_event(&event);
        let text = msg_text(&msg);

        assert!(text.contains("[memory:updated]"), "missing tag: {text}");
        // Diff must show at least one + or - line.
        assert!(
            text.contains("+line2 changed") || text.contains("-line2 original"),
            "diff body missing +/- lines: {text}"
        );
    }

    // ---- Updated with previous=None + hash → hash-fallback marker -----------

    #[test]
    fn updated_with_hash_only_produces_hash_fallback_marker() {
        let event = make_event(
            "notes",
            BlockWriteKind::Updated,
            "new content here",
            None,
            Some(0xDEAD_u64),
            system_author(),
        );
        let msg = render_change_event(&event);
        let text = msg_text(&msg);

        assert!(text.contains("[memory:updated]"), "missing tag: {text}");
        assert!(
            text.contains("previous hash"),
            "missing hash-fallback marker: {text}"
        );
        // The hash value must appear (in hex form).
        assert!(
            text.contains("dead") || text.contains("0xdead") || text.contains("0x000000000000dead"),
            "hash value missing: {text}"
        );
        // Preview of new content must appear.
        assert!(text.contains("new content here"), "missing preview: {text}");
    }

    // ---- AC8.6: Agent author attribution ------------------------------------

    #[test]
    fn agent_author_attribution() {
        let event = make_event(
            "shared_block",
            BlockWriteKind::Updated,
            "content",
            Some("old content"),
            None,
            Author::Agent(AgentAuthor {
                agent_id: SmolStr::new("peer-agent"),
            }),
        );
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        assert!(
            text.contains("agent peer-agent"),
            "agent attribution missing or wrong: {text}"
        );
    }

    // ---- AC8.6: System author renders readably ------------------------------

    #[test]
    fn system_author_memory_change_renders() {
        let event = make_event(
            "block",
            BlockWriteKind::Created,
            "content",
            None,
            None,
            Author::System {
                reason: SystemReason::MemoryChange,
            },
        );
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        // Must not panic, must produce readable output.
        assert!(text.contains("system"), "system author missing: {text}");
        assert!(text.contains("MemoryChange"), "reason missing: {text}");
    }

    // ---- preview helper: content ≤ max → unchanged --------------------------

    #[test]
    fn preview_short_content_unchanged() {
        let content = "short";
        let result = preview(content, 240);
        assert_eq!(result, content);
    }

    // ---- preview helper: content > max → truncated + ellipsis + elided ------

    #[test]
    fn preview_long_content_truncated() {
        let content: String = "x".repeat(300);
        let result = preview(&content, 240);
        assert!(result.contains("…"), "missing ellipsis: {result}");
        assert!(
            result.contains("60 chars elided"),
            "wrong elided count: {result}"
        );
        // The first 240 chars must be the head.
        let x_count = result.chars().take_while(|c| *c == 'x').count();
        assert_eq!(x_count, 240, "head not 240 chars: got {x_count}");
    }

    // ---- Deleted renders [memory:deleted] sensibly --------------------------

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
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        assert!(text.contains("[memory:deleted]"), "missing tag: {text}");
        assert!(text.contains("old_block"), "missing handle: {text}");
    }

    // ---- Local-time rendering: structural checks ----------------------------

    #[test]
    fn local_timestamp_renders_non_empty_with_date_components() {
        // 2026-04-17 00:00:00 UTC — date is known even though local TZ may
        // vary. We just verify structural shape rather than pinning a TZ.
        // 2025-04-25 20:00:00 EDT (or nearby depending on local TZ)
        let ts = Timestamp::from_second(1_745_625_600).unwrap();
        let rendered = render_local_timestamp(ts);
        assert!(!rendered.is_empty(), "timestamp rendered empty");
        // Must contain the year.
        assert!(rendered.contains("2025"), "year missing: {rendered}");
        // Must contain colons from HH:MM:SS.
        assert!(
            rendered.contains(':'),
            "no colons (HH:MM:SS) in timestamp: {rendered}"
        );
    }

    // ---- Empty-diff fallback: previous == current ----------------------------

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
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        // No empty diff body should be shipped; the fallback notice must appear.
        assert!(
            text.contains("unchanged"),
            "empty-diff fallback missing: {text}"
        );
        // The current content preview must still appear.
        assert!(
            text.contains("identical content"),
            "preview missing: {text}"
        );
    }

    // ---- Partner author attribution ------------------------------------------

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
            }),
        );
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        assert!(
            text.contains("partner user123"),
            "partner attribution missing: {text}"
        );
    }

    // ---- Human author: display_name preferred over id -----------------------

    #[test]
    fn human_author_uses_display_name_when_present() {
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
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        assert!(text.contains("human alex"), "display name not used: {text}");
    }

    // ---- render_change_events batch helper ----------------------------------

    #[test]
    fn render_change_events_preserves_order_and_count() {
        let events = vec![
            make_event(
                "a",
                BlockWriteKind::Created,
                "a content",
                None,
                None,
                system_author(),
            ),
            make_event(
                "b",
                BlockWriteKind::Created,
                "b content",
                None,
                None,
                system_author(),
            ),
            make_event(
                "c",
                BlockWriteKind::Created,
                "c content",
                None,
                None,
                system_author(),
            ),
        ];
        let msgs = render_change_events(&events);
        assert_eq!(msgs.len(), 3);
        // Order preserved: 'a' before 'b' before 'c'.
        let text0 = msg_text(&msgs[0]);
        let text1 = msg_text(&msgs[1]);
        assert!(text0.contains("'a'"), "order wrong at [0]: {text0}");
        assert!(text1.contains("'b'"), "order wrong at [1]: {text1}");
    }

    // ---- Replaced uses [memory:updated] tag ---------------------------------

    #[test]
    fn replaced_uses_updated_tag() {
        let event = make_event(
            "block",
            BlockWriteKind::Replaced,
            "new full content",
            Some("old full content"),
            None,
            system_author(),
        );
        let msg = render_change_event(&event);
        let text = msg_text(&msg);
        assert!(
            text.contains("[memory:updated]"),
            "Replaced must use [memory:updated]: {text}"
        );
    }

    // ---- render_skill_loaded_text snapshot ---------------------------------

    #[test]
    fn render_skill_loaded_text_snapshot() {
        // Known input → deterministic rendered text. This is the body that
        // `Pattern.Skills.Load` returns as the tool_result content.
        use pattern_core::types::memory_types::SkillTrustTier;

        let text = render_skill_loaded_text(
            "fix-authentication",
            SkillTrustTier::ProjectLocal,
            "## Overview\n\nHandles OAuth2 token refresh for expired sessions.",
        );
        insta::assert_snapshot!(text);
    }

    // ---- render_skill_loaded_text: trust tier variants ---------------------

    #[test]
    fn render_skill_loaded_text_renders_trust_tier_as_kebab() {
        use pattern_core::types::memory_types::SkillTrustTier;

        let cases = [
            (SkillTrustTier::FirstParty, "first-party"),
            (SkillTrustTier::ProjectLocal, "project-local"),
            (SkillTrustTier::AdHoc, "ad-hoc"),
        ];

        for (tier, expected_kebab) in cases {
            let text = render_skill_loaded_text("test-skill", tier, "body.");
            assert!(
                text.contains(&format!("trust_tier=\"{expected_kebab}\"")),
                "expected trust_tier=\"{expected_kebab}\" in output; got: {text}"
            );
        }
    }

    // ---- render_skill_loaded_text: structural markers + no <system-reminder>

    #[test]
    fn render_skill_loaded_text_has_opening_and_closing_markers() {
        use pattern_core::types::memory_types::SkillTrustTier;

        let text = render_skill_loaded_text("my-skill", SkillTrustTier::AdHoc, "skill body here.");
        assert!(
            text.contains("[skill:loaded]"),
            "missing opening marker: {text}"
        );
        assert!(
            text.contains("[skill:loaded:end]"),
            "missing closing marker: {text}"
        );
        // Tool_result has its own role-based framing; we MUST NOT wrap in
        // <system-reminder> (that's user-role-message framing).
        assert!(
            !text.contains("<system-reminder>"),
            "must not contain system-reminder wrapper: {text}"
        );
        assert!(
            text.contains("skill body here."),
            "missing skill body in output: {text}"
        );
    }
}
