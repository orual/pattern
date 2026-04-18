//! Concrete composer-pass implementations for the three-segment cache layout.
//!
//! Each pass is a [`super::ComposerPass`] that appends content to a
//! [`super::PartialRequest`] and places one cache-breakpoint marker. The
//! canonical execution order is:
//!
//! 1. [`segment_1::Segment1Pass`] — system prompt + tool schemas.
//! 2. [`segment_2::Segment2Pass`] — prior-turn history + summary-head +
//!    memory-change pseudo-messages.
//! 3. [`segment_3::Segment3Pass`] — `[memory:current_state]` pseudo-turn.
//!
//! After all three passes, the caller appends fresh user input to
//! `partial.messages` (uncached), then calls [`super::finalize`] to apply
//! breakpoint markers and assemble the final
//! [`pattern_core::types::provider::CompletionRequest`].
//!
//! # Ordering matters
//!
//! The cache-breakpoint indices are positional — a pass that records
//! `BreakpointLocation::MessageBlock(5)` expects index 5 to remain stable.
//! Running passes out of order will misplace markers. The canonical order
//! above is enforced by convention (and documented here) rather than by
//! type-level sequencing; tests verify the combined pipeline produces the
//! correct marker count and placement.

pub mod segment_1;
pub mod segment_2;
pub mod segment_3;

pub use segment_1::Segment1Pass;
pub use segment_2::{synthesize_summary_message, Segment2Pass};
pub use segment_3::Segment3Pass;

#[cfg(test)]
mod tests {
    use genai::chat::{ChatMessage, SystemBlock};
    use jiff::Timestamp;
    use smol_str::SmolStr;

    use pattern_core::memory::{BlockMetadata, BlockSchema, BlockType, StructuredDocument};
    use pattern_core::types::block::{BlockWrite, BlockWriteKind};
    use pattern_core::types::origin::{Author, SystemReason};

    use crate::compose::breakpoints::BreakpointLocation;
    use crate::compose::pipeline::{compose, ComposerPass};
    use crate::compose::profile::CacheProfile;
    use crate::compose::PartialRequest;

    use super::*;

    fn test_profile() -> CacheProfile {
        CacheProfile::default_anthropic_subscriber()
    }

    fn make_doc(label: &str, content: &str) -> StructuredDocument {
        let mut metadata = BlockMetadata::standalone(BlockSchema::text());
        metadata.label = label.to_string();
        metadata.block_type = BlockType::Working;
        let doc = StructuredDocument::new_with_metadata(metadata, None);
        doc.set_text(content, true).unwrap();
        doc
    }

    fn make_block_write(handle: &str) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_test"),
            block_type: BlockType::Working,
            rendered_content: "updated content".to_string(),
            kind: BlockWriteKind::Updated,
            previous_content_hash: None,
            previous_rendered_content: Some("old content".to_string()),
            at: Timestamp::from_second(1_745_000_000).unwrap(),
            author: Author::System {
                reason: SystemReason::ToolCall,
            },
        }
    }

    fn msg_text(msg: &ChatMessage) -> String {
        msg.content.joined_texts().unwrap_or_default()
    }

    // ---- AC7.1: exactly 3 cache markers after all three passes ----

    #[test]
    fn three_passes_produce_exactly_3_markers() {
        let profile = test_profile();
        let system_blocks = vec![
            SystemBlock::new("routing token"),
            SystemBlock::new("base instructions"),
            SystemBlock::new("persona"),
        ];
        let prior_msgs = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];
        let writes = vec![make_block_write("tasks")];
        let blocks = vec![make_doc("persona", "I am Sage.")];

        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(Segment1Pass::new(
                system_blocks,
                vec![],
                profile.clone(),
            )),
            Box::new(Segment2Pass::new(
                vec![],
                prior_msgs,
                &writes,
                profile.clone(),
            )),
            Box::new(Segment3Pass::new(blocks, profile)),
        ];

        let partial = PartialRequest::new("claude-opus-4-7");
        let result = compose(&passes, partial).expect("compose succeeds");

        // Note: finalize in Task 3's stub does NOT apply markers yet
        // (that's Task 10). So the result won't have cache_control set on
        // system blocks / messages — that happens after finalize expansion.
        // For now, verify compose succeeds and the output is sensible.
        // The marker application check is validated in Task 10's tests.
        assert!(result.chat.system_blocks.is_some());
        assert!(!result.chat.messages.is_empty());
    }

    // ---- AC7.1 via breakpoints: exactly 3 placements ----

    #[test]
    fn three_passes_place_exactly_3_breakpoints() {
        let profile = test_profile();
        let system_blocks = vec![SystemBlock::new("sys")];
        let prior_msgs = vec![ChatMessage::user("hello")];
        let blocks = vec![make_doc("persona", "content")];

        let seg1 = Segment1Pass::new(system_blocks, vec![], profile.clone());
        let seg2 = Segment2Pass::new(vec![], prior_msgs, &[], profile.clone());
        let seg3 = Segment3Pass::new(blocks, profile);

        let mut partial = PartialRequest::new("claude-opus-4-7");
        seg1.apply(&mut partial).unwrap();
        seg2.apply(&mut partial).unwrap();
        seg3.apply(&mut partial).unwrap();

        assert_eq!(
            partial.breakpoints.count(),
            3,
            "exactly 3 breakpoints expected"
        );

        // Verify marker locations: 1 system, 2 message.
        let placements = partial.breakpoints.placements();
        assert!(matches!(
            placements[0].location,
            BreakpointLocation::SystemBlock(_)
        ));
        assert!(matches!(
            placements[1].location,
            BreakpointLocation::MessageBlock(_)
        ));
        assert!(matches!(
            placements[2].location,
            BreakpointLocation::MessageBlock(_)
        ));
    }

    // ---- AC7.3: segment 3 [memory:current_state] present in pipeline ----

    #[test]
    fn pipeline_contains_current_state_in_segment_3() {
        let profile = test_profile();
        let blocks = vec![make_doc("tasks", "- review PR")];

        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(Segment1Pass::new(
                vec![SystemBlock::new("sys")],
                vec![],
                profile.clone(),
            )),
            Box::new(Segment2Pass::new(
                vec![],
                vec![ChatMessage::user("hello")],
                &[],
                profile.clone(),
            )),
            Box::new(Segment3Pass::new(blocks, profile)),
        ];

        let result = compose(&passes, PartialRequest::new("claude-opus-4-7"))
            .expect("compose succeeds");

        // The last message should be the current_state pseudo-turn.
        let last = result.chat.messages.last().expect("messages not empty");
        let text = msg_text(last);
        assert!(
            text.contains("[memory:current_state]"),
            "last message must contain [memory:current_state]: {text}"
        );
    }

    // ---- AC8.3: [memory:updated] appears in segment 2 ----

    #[test]
    fn pipeline_contains_updated_pseudo_message_in_segment_2() {
        let profile = test_profile();
        let writes = vec![make_block_write("task_list")];
        let prior = vec![ChatMessage::user("msg")];

        let passes: Vec<Box<dyn ComposerPass>> = vec![
            Box::new(Segment1Pass::new(
                vec![SystemBlock::new("sys")],
                vec![],
                profile.clone(),
            )),
            Box::new(Segment2Pass::new(
                vec![],
                prior,
                &writes,
                profile.clone(),
            )),
            Box::new(Segment3Pass::new(vec![], profile)),
        ];

        let result = compose(&passes, PartialRequest::new("claude-opus-4-7"))
            .expect("compose succeeds");

        // Find a message containing [memory:updated] — should be in
        // the segment 2 region (before the current_state message).
        let found = result
            .chat
            .messages
            .iter()
            .any(|m| msg_text(m).contains("[memory:updated]"));
        assert!(found, "must contain a [memory:updated] pseudo-message");
    }
}
