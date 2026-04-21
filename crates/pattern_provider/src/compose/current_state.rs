//! Segment-3 pseudo-turn renderer: `[memory:current_state]`.
//!
//! Produces a single user-role `ChatMessage` wrapping all blocks currently
//! loaded in the agent's working context. The composer's segment-3 pass pushes
//! this message onto `partial.messages` and places its `cache_control` marker
//! on the result.
//!
//! # Format
//!
//! ```text
//! [memory:current_state]
//!
//! <block:label type="working" permission="read_write">
//! optional description text
//!
//! rendered content from StructuredDocument::render()
//! </block:label>
//!
//! <block:other_label type="core" permission="read_only">
//! rendered content
//! </block:other_label>
//! ```
//!
//! This matches v2's `context/builder.rs:432-529` block-tag shape with the
//! addition of a `type=` attribute (the v2 shape carried `permission=` only).
//! The extra attribute costs a few bytes per block and lets agents reason about
//! which tier of block they're reading without needing a separate schema table.
//!
//! # AC7.6 — empty block list
//!
//! When `blocks` is empty the message is still emitted with the body
//! `"[memory:current_state]\n(no blocks loaded)"`. Segment 3's cache boundary
//! is preserved regardless of how many blocks are loaded — the composer's
//! segment-3 pass always places its `cache_control` marker on this message,
//! so omitting it when there are zero blocks would misplace the boundary.
//!
//! # Public surface
//!
//! - [`render_current_state`] — the single public function; returns exactly one
//!   `ChatMessage`.

use genai::chat::ChatMessage;
use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::BlockType;

use crate::shaper::wrap_system_reminder;

// ---- Public API ------------------------------------------------------------

/// Render the current-state pseudo-turn for segment 3.
///
/// Always produces exactly one [`ChatMessage`] with `role = User`, even when
/// `blocks` is empty (AC7.6). The message carries a `<system-reminder>`-wrapped
/// body listing all blocks with their type, permission, optional description,
/// and schema-aware rendered content.
///
/// # Format
///
/// Non-empty: one `<block:label type="..." permission="...">` section per
/// block, with an optional description line before the rendered content when
/// [`StructuredDocument::description`] is non-empty.
///
/// Empty: `"[memory:current_state]\n(no blocks loaded)"` — preserves segment
/// 3's cache boundary.
///
/// # Examples
///
/// ```
/// use pattern_core::memory::StructuredDocument; // trait-signature type
/// use pattern_provider::compose::current_state::render_current_state;
///
/// let msg = render_current_state(&[]);
/// assert_eq!(msg.role, genai::chat::ChatRole::User);
/// ```
pub fn render_current_state(blocks: &[StructuredDocument]) -> ChatMessage {
    let body = if blocks.is_empty() {
        "[memory:current_state]\n(no blocks loaded)".to_string()
    } else {
        let mut parts = vec!["[memory:current_state]".to_string()];
        for block in blocks {
            parts.push(render_block(block));
        }
        // Join sections with a blank line between them for readability.
        parts.join("\n\n")
    };
    ChatMessage::user(wrap_system_reminder(&body))
}

// ---- Block rendering -------------------------------------------------------

/// Render a single block as a `<block:label type="..." permission="...">` section.
fn render_block(block: &StructuredDocument) -> String {
    let label = block.label();
    let block_type = render_block_type(block.block_type());
    let permission = block.permission().to_string();
    let content = block.render();

    let open_tag = format!("<block:{label} type=\"{block_type}\" permission=\"{permission}\">");
    let close_tag = format!("</block:{label}>");

    let description = block.description();
    let inner = if description.is_empty() {
        content
    } else {
        // Description first, then a blank line, then content — matches v2 pattern.
        format!("{description}\n\n{content}")
    };

    format!("{open_tag}\n{inner}\n{close_tag}")
}

/// Human-readable label for a [`BlockType`].
fn render_block_type(bt: BlockType) -> &'static str {
    match bt {
        BlockType::Core => "core",
        BlockType::Working => "working",
        BlockType::Archival => "archival",
        BlockType::Log => "log",
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use genai::chat::ChatRole;
    use pattern_core::memory::StructuredDocument;
    use pattern_core::types::memory_types::{BlockMetadata, BlockSchema, BlockType};

    use super::*;

    // ---- helpers ------------------------------------------------------------

    /// Extract the full joined text from a `ChatMessage`.
    ///
    /// `MessageContent` has no `Display` impl; `joined_texts()` is the correct
    /// way to pull out the text content of a single-part user message.
    fn msg_text(msg: &ChatMessage) -> String {
        msg.content.joined_texts().unwrap_or_default()
    }

    /// Build a minimal `StructuredDocument` for testing.
    ///
    /// `StructuredDocument::new` creates a standalone Text-schema document
    /// with empty metadata. Tests that need a label, description, or non-text
    /// schema can call `new_with_metadata` directly.
    fn make_doc(label: &str, description: &str, content: &str) -> StructuredDocument {
        let mut metadata = BlockMetadata::standalone(BlockSchema::text());
        metadata.label = label.to_string();
        metadata.description = description.to_string();
        metadata.block_type = BlockType::Working;
        let doc = StructuredDocument::new_with_metadata(metadata, None);
        doc.set_text(content, true).unwrap();
        doc
    }

    fn make_doc_with_type(
        label: &str,
        description: &str,
        content: &str,
        block_type: BlockType,
    ) -> StructuredDocument {
        let mut metadata = BlockMetadata::standalone(BlockSchema::text());
        metadata.label = label.to_string();
        metadata.description = description.to_string();
        metadata.block_type = block_type;
        let doc = StructuredDocument::new_with_metadata(metadata, None);
        doc.set_text(content, true).unwrap();
        doc
    }

    // ---- AC7.6: empty slice → message still emitted with body ---------------

    #[test]
    fn empty_blocks_emits_present_but_empty_message() {
        let msg = render_current_state(&[]);
        let text = msg_text(&msg);
        assert!(
            text.contains("[memory:current_state]"),
            "header tag missing: {text}"
        );
        assert!(
            text.contains("(no blocks loaded)"),
            "empty-state body missing: {text}"
        );
    }

    // ---- AC7.6: empty result is still wrapped in <system-reminder> ----------

    #[test]
    fn empty_blocks_has_system_reminder_wrapper() {
        let msg = render_current_state(&[]);
        let text = msg_text(&msg);
        assert!(
            text.contains("<system-reminder>"),
            "missing <system-reminder>: {text}"
        );
        assert!(
            text.contains("</system-reminder>"),
            "missing </system-reminder>: {text}"
        );
    }

    // ---- Non-empty: labels + tag structure present -------------------------

    #[test]
    fn non_empty_contains_both_block_labels_and_tags() {
        let blocks = vec![
            make_doc("persona", "", "I am a helpful agent."),
            make_doc("task_list", "", "- [ ] review PR"),
        ];
        let msg = render_current_state(&blocks);
        let text = msg_text(&msg);

        assert!(
            text.contains("<block:persona"),
            "persona open-tag missing: {text}"
        );
        assert!(
            text.contains("</block:persona>"),
            "persona close-tag missing: {text}"
        );
        assert!(
            text.contains("<block:task_list"),
            "task_list open-tag missing: {text}"
        );
        assert!(
            text.contains("</block:task_list>"),
            "task_list close-tag missing: {text}"
        );
        assert!(
            text.contains("I am a helpful agent."),
            "persona content missing: {text}"
        );
        assert!(
            text.contains("review PR"),
            "task_list content missing: {text}"
        );
    }

    // ---- <system-reminder> present on non-empty path -----------------------

    #[test]
    fn non_empty_has_system_reminder_wrapper() {
        let blocks = vec![make_doc("persona", "", "content")];
        let msg = render_current_state(&blocks);
        let text = msg_text(&msg);
        assert!(
            text.contains("<system-reminder>"),
            "missing wrapper: {text}"
        );
        assert!(text.contains("</system-reminder>"), "missing close: {text}");
    }

    // ---- type= attribute present in tags -----------------------------------

    #[test]
    fn block_tag_includes_type_attribute() {
        let blocks = vec![make_doc_with_type(
            "myblock",
            "",
            "content",
            BlockType::Core,
        )];
        let msg = render_current_state(&blocks);
        let text = msg_text(&msg);
        assert!(
            text.contains("type=\"core\""),
            "type attribute missing: {text}"
        );
    }

    // ---- description appears inside block when non-empty -------------------

    #[test]
    fn non_empty_description_appears_inside_block() {
        let blocks = vec![make_doc(
            "myblock",
            "This block tracks tasks.",
            "content here",
        )];
        let msg = render_current_state(&blocks);
        let text = msg_text(&msg);
        assert!(
            text.contains("This block tracks tasks."),
            "description missing: {text}"
        );
        // Description must appear *before* closing tag.
        let desc_pos = text.find("This block tracks tasks.").unwrap();
        let close_pos = text.find("</block:myblock>").unwrap();
        assert!(desc_pos < close_pos, "description after close tag: {text}");
    }

    // ---- empty description → no stray blank line between tag and content ---

    #[test]
    fn empty_description_no_stray_blank_line() {
        let blocks = vec![make_doc("myblock", "", "actual content")];
        let msg = render_current_state(&blocks);
        let text = msg_text(&msg);

        // The content must be directly after the opening tag (only one newline),
        // not with a blank line between them.
        // i.e. "<block:myblock ...>\nactual content\n</block:myblock>"
        // NOT: "<block:myblock ...>\n\nactual content\n</block:myblock>"
        let after_open = text
            .split_once("<block:myblock")
            .and_then(|(_, rest)| rest.split_once('>'))
            .map(|(_, body)| body)
            .unwrap_or("");

        assert!(
            !after_open.starts_with("\n\n"),
            "stray blank line between open tag and content: {text}"
        );
    }

    // ---- AC7.3: message role is User on both paths -------------------------

    #[test]
    fn role_is_user_on_empty_path() {
        let msg = render_current_state(&[]);
        assert_eq!(msg.role, ChatRole::User);
    }

    #[test]
    fn role_is_user_on_non_empty_path() {
        let blocks = vec![make_doc("block", "", "content")];
        let msg = render_current_state(&blocks);
        assert_eq!(msg.role, ChatRole::User);
    }
}
