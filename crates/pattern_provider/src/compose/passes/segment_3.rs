//! Segment 3 composer pass — `[memory:current_state]` pseudo-turn +
//! cache marker.
//!
//! Pushes the current-state pseudo-turn (rendered by the Task 7
//! renderer) onto `partial.messages` and places the segment-3
//! cache-breakpoint marker on it.
//!
//! # AC7.6 — empty block list
//!
//! Per AC7.6, the pseudo-turn is emitted even when `blocks` is empty
//! (body becomes `"(no blocks loaded)"`). This preserves segment 3's
//! cache-boundary consistency — the marker always has a message to
//! attach to.
//!
//! # Ordering
//!
//! Runs AFTER [`super::segment_2::Segment2Pass`] so the current-state
//! message lands after the prior-turn history. Fresh user input is
//! appended by the caller AFTER segment 3 runs, placing it at the very
//! end (uncached).

use pattern_core::error::ProviderError;
use pattern_core::memory::StructuredDocument;

use crate::compose::current_state::render_current_state;
use crate::compose::{BreakpointLocation, CacheProfile, ComposerPass, PartialRequest};

/// Segment 3: `[memory:current_state]` pseudo-turn + cache marker.
///
/// Constructed with the agent's currently-loaded blocks. The pass
/// renders them via [`render_current_state`] and places the segment-3
/// cache marker on the resulting message.
pub struct Segment3Pass {
    /// Currently-loaded blocks to render.
    blocks: Vec<StructuredDocument>,
    /// Session-latched cache profile.
    profile: CacheProfile,
}

impl Segment3Pass {
    /// Construct a new `Segment3Pass` with the blocks to render and
    /// the session cache profile.
    pub fn new(blocks: Vec<StructuredDocument>, profile: CacheProfile) -> Self {
        Self { blocks, profile }
    }
}

impl ComposerPass for Segment3Pass {
    fn name(&self) -> &'static str {
        "segment_3"
    }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        let msg = render_current_state(&self.blocks);
        partial.messages.push(msg);
        let idx = partial.messages.len() - 1;
        let control = self.profile.segment_3_control();
        partial
            .breakpoints
            .place(BreakpointLocation::MessageBlock(idx), control, self.name())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use genai::chat::{CacheControl, ChatMessage};
    use pattern_core::memory::{BlockMetadata, BlockSchema, BlockType, StructuredDocument};

    use crate::compose::breakpoints::BreakpointLocation;
    use crate::compose::profile::CacheProfile;

    use super::*;

    fn test_profile() -> CacheProfile {
        CacheProfile::default_anthropic_subscriber()
    }

    fn msg_text(msg: &ChatMessage) -> String {
        msg.content.joined_texts().unwrap_or_default()
    }

    fn make_doc(label: &str, content: &str) -> StructuredDocument {
        let mut metadata = BlockMetadata::standalone(BlockSchema::text());
        metadata.label = label.to_string();
        metadata.block_type = BlockType::Working;
        let doc = StructuredDocument::new_with_metadata(metadata, None);
        doc.set_text(content, true).unwrap();
        doc
    }

    // ---- AC7.3: segment 3 contains [memory:current_state] -------------------

    #[test]
    fn segment_3_contains_current_state_tag() {
        let blocks = vec![
            make_doc("persona", "I am Sage."),
            make_doc("tasks", "- [ ] review PR"),
        ];
        let pass = Segment3Pass::new(blocks, test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 1);
        let text = msg_text(&partial.messages[0]);
        assert!(
            text.contains("[memory:current_state]"),
            "segment 3 must contain [memory:current_state]: {text}"
        );
    }

    // ---- AC7.6: empty blocks still emits message + marker -------------------

    #[test]
    fn empty_blocks_still_emits_message_and_marker() {
        let pass = Segment3Pass::new(vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 1);
        let text = msg_text(&partial.messages[0]);
        assert!(
            text.contains("(no blocks loaded)"),
            "empty blocks must produce (no blocks loaded): {text}"
        );
        assert_eq!(
            partial.breakpoints.count(),
            1,
            "marker must still be placed"
        );
    }

    // ---- Marker placed on the current_state message -------------------------

    #[test]
    fn marker_placed_on_current_state_message() {
        let blocks = vec![make_doc("block", "content")];
        let pass = Segment3Pass::new(blocks, test_profile());

        // Pre-populate partial with some messages from segment 2.
        let mut partial = PartialRequest::new("claude-opus-4-7");
        partial.messages.push(ChatMessage::user("prior msg 1"));
        partial.messages.push(ChatMessage::assistant("prior msg 2"));

        pass.apply(&mut partial).unwrap();

        // The current_state message should be at index 2 (after 2 prior).
        assert_eq!(partial.messages.len(), 3);
        let placements = partial.breakpoints.placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placed_by_pass, "segment_3");
        match placements[0].location {
            BreakpointLocation::MessageBlock(idx) => {
                assert_eq!(idx, 2, "marker must be on the current_state message");
            }
            other => panic!("expected MessageBlock, got {other:?}"),
        }
    }

    // ---- Cache control from profile -----------------------------------------

    #[test]
    fn cache_control_uses_segment_3_control() {
        let pass = Segment3Pass::new(vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        assert_eq!(placements[0].control, CacheControl::Ephemeral5m);
    }
}
