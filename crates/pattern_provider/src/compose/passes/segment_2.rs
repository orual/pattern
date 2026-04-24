//! Segment 2 composer pass — prior-turn conversation history +
//! summary-head prepend + memory-change pseudo-messages + cache marker.
//!
//! # Message ordering (matters for cache boundary)
//!
//! 1. Summary-head messages (synthesized from archive summaries,
//!    pre-rendered by the caller).
//! 2. Prior-turn messages (from `TurnHistory::active_messages`).
//! 3. Memory-change pseudo-messages (from `render_change_events`,
//!    Task 6 renderer).
//!
//! The segment-2 cache marker lands on the **last** message pushed by
//! this pass. Fresh user input is NOT part of segment 2 — the caller
//! appends it after all three passes have run, so it remains uncached.
//!
//! # Summary-head rendering
//!
//! [`synthesize_summary_message`] converts archive-summary metadata
//! (depth, position range, text) into a `ChatMessage::user` wrapped in
//! `<system-reminder>` tags. The function accepts individual fields
//! rather than `pattern_db::ArchiveSummary` so `pattern_provider` does
//! not depend on `pattern_db`. The turn loop in `pattern_runtime` is
//! responsible for calling this helper with the right fields.

use genai::chat::ChatMessage;
use pattern_core::error::ProviderError;
use pattern_core::types::block::BlockWrite;
use smol_str::SmolStr;

use crate::compose::pseudo_messages::render_change_events;
use crate::compose::{BreakpointLocation, CacheProfile, ComposerPass, PartialRequest};
use crate::shaper::wrap_system_reminder;

// ---- Public helpers ---------------------------------------------------------

/// Render an archive summary as a single `ChatMessage::user` wrapped
/// in `<system-reminder>` tags.
///
/// Body structure:
/// ```text
/// [memory:archive_summary depth=<d> covers=<start>..<end>]
/// <summary text>
/// ```
///
/// Accepts individual fields so `pattern_provider` does not depend on
/// `pattern_db::models::message::ArchiveSummary`.
pub fn synthesize_summary_message(
    depth: i64,
    start_position: &str,
    end_position: &str,
    summary: &str,
) -> ChatMessage {
    let body = format!(
        "[memory:archive_summary depth={depth} covers={start_position}..{end_position}]\n{summary}"
    );
    ChatMessage::user(wrap_system_reminder(&body))
}

// ---- Segment2Pass -----------------------------------------------------------

/// Segment 2: prior-turn conversation history + summary-head +
/// memory-change pseudo-messages.
///
/// Does NOT include fresh user input — the caller appends that after
/// all three passes have run so the cache boundary stays correct.
pub struct Segment2Pass {
    /// Pre-rendered summary-head messages. The turn loop calls
    /// [`synthesize_summary_message`] for each `ArchiveSummary` and
    /// passes the results here.
    summary_head_messages: Vec<ChatMessage>,
    /// Prior-turn messages from `TurnHistory::active_messages`,
    /// paired with their Pattern `MessageId` for origin tagging.
    prior_messages: Vec<(SmolStr, ChatMessage)>,
    /// Pseudo-messages rendered from the most-recent turn's
    /// `BlockWrite`s via the Task 6 renderer.
    pseudo_messages: Vec<ChatMessage>,
    /// Session-latched cache profile.
    profile: CacheProfile,
}

impl Segment2Pass {
    /// Construct from pre-rendered summary-head messages and raw
    /// prior-turn messages (with their MessageIds) + block writes.
    ///
    /// Each prior message is paired with the Pattern `MessageId` it
    /// originated from. Summary-head and pseudo-messages have no
    /// Pattern Message identity and are tagged with `None` origin.
    ///
    /// The block-write → pseudo-message rendering happens inline
    /// (via [`render_change_events`]) so the caller doesn't need to
    /// call the renderer separately.
    pub fn new(
        summary_head_messages: Vec<ChatMessage>,
        prior_messages: Vec<(SmolStr, ChatMessage)>,
        recent_block_writes: &[BlockWrite],
        recent_pseudo_messages: &[ChatMessage],
        profile: CacheProfile,
    ) -> Self {
        let mut pseudo_messages = render_change_events(recent_block_writes);
        // Handler-originated pseudo-messages (e.g. [skill:loaded] markers)
        // are appended after block-write-rendered ones. Order within the
        // group is preserved from the adapter buffer.
        pseudo_messages.extend(recent_pseudo_messages.iter().cloned());
        Self {
            summary_head_messages,
            prior_messages,
            pseudo_messages,
            profile,
        }
    }
}

impl ComposerPass for Segment2Pass {
    fn name(&self) -> &'static str {
        "segment_2"
    }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        // Append in canonical order, using push_message to maintain
        // the message_origins parallel vector.

        // Summary-head messages have no Pattern Message identity.
        for msg in &self.summary_head_messages {
            partial.push_message(msg.clone(), None);
        }

        // Prior messages carry their Pattern MessageId as origin.
        for (id, msg) in &self.prior_messages {
            partial.push_message(msg.clone(), Some(id.clone()));
        }

        // Pseudo-messages (block-write notifications) are synthetic.
        for msg in &self.pseudo_messages {
            partial.push_message(msg.clone(), None);
        }

        // Place marker on the last message we just pushed. If we
        // pushed nothing (empty history + no summaries + no writes),
        // skip the marker — the segment is empty.
        if !partial.messages.is_empty() {
            let last_idx = partial.messages.len() - 1;
            let control = self.profile.segment_2_control();
            partial.breakpoints.place(
                BreakpointLocation::MessageBlock(last_idx),
                control,
                self.name(),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use genai::chat::{CacheControl, ChatRole};
    use jiff::Timestamp;
    use smol_str::SmolStr;

    use pattern_core::types::block::{BlockWrite, BlockWriteKind};
    use pattern_core::types::memory_types::MemoryBlockType;
    use pattern_core::types::origin::{Author, SystemReason};

    use crate::compose::breakpoints::BreakpointLocation;
    use crate::compose::profile::CacheProfile;

    use super::*;

    // ---- fixtures -----------------------------------------------------------

    fn test_profile() -> CacheProfile {
        CacheProfile::default_anthropic_subscriber()
    }

    fn make_block_write(handle: &str, kind: BlockWriteKind) -> BlockWrite {
        BlockWrite {
            handle: SmolStr::new(handle),
            memory_id: SmolStr::new("mem_test"),
            block_type: MemoryBlockType::Working,
            rendered_content: "new content".to_string(),
            kind,
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

    // ---- synthesize_summary_message tests -----------------------------------

    #[test]
    fn synthesize_summary_message_contains_metadata() {
        let msg = synthesize_summary_message(
            1,
            "00000000000001000000",
            "00000000000001000010",
            "Earlier context about tasks.",
        );

        assert_eq!(msg.role, ChatRole::User);
        let text = msg_text(&msg);
        assert!(
            text.contains("[memory:archive_summary depth=1"),
            "missing archive_summary tag: {text}"
        );
        assert!(
            text.contains("covers=00000000000001000000..00000000000001000010"),
            "missing covers range: {text}"
        );
        assert!(
            text.contains("Earlier context about tasks."),
            "missing summary text: {text}"
        );
        assert!(
            text.contains("<system-reminder>"),
            "missing system-reminder wrapper: {text}"
        );
    }

    // ---- AC8.3: pseudo-message in segment 2 after block edits ---------------

    #[test]
    fn pseudo_messages_appear_in_segment_2_for_block_writes() {
        let writes = vec![make_block_write("task_list", BlockWriteKind::Updated)];
        let prior = vec![
            (SmolStr::new("msg-1"), ChatMessage::user("hello")),
            (SmolStr::new("msg-2"), ChatMessage::assistant("hi")),
        ];

        let pass = Segment2Pass::new(vec![], prior, &writes, &[], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        // Should have: 2 prior + 1 pseudo = 3 messages.
        assert_eq!(partial.messages.len(), 3);

        // The pseudo-message must contain [memory:updated].
        let last_text = msg_text(&partial.messages[2]);
        assert!(
            last_text.contains("[memory:updated]"),
            "pseudo-message missing [memory:updated]: {last_text}"
        );
    }

    // ---- Summary-head messages appear first ---------------------------------

    #[test]
    fn summary_head_messages_appear_before_prior() {
        let summary = synthesize_summary_message(0, "pos_a", "pos_b", "summary text");
        let prior = vec![(SmolStr::new("msg-1"), ChatMessage::user("recent message"))];

        let pass = Segment2Pass::new(vec![summary], prior, &[], &[], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 2);
        let first_text = msg_text(&partial.messages[0]);
        let second_text = msg_text(&partial.messages[1]);
        assert!(
            first_text.contains("[memory:archive_summary"),
            "summary must come first: {first_text}"
        );
        assert!(
            second_text.contains("recent message"),
            "prior must come second: {second_text}"
        );
    }

    // ---- Marker placed on last message (before fresh input) -----------------

    #[test]
    fn marker_placed_on_last_message() {
        let prior = vec![
            (SmolStr::new("msg-1"), ChatMessage::user("msg1")),
            (SmolStr::new("msg-2"), ChatMessage::assistant("msg2")),
            (SmolStr::new("msg-3"), ChatMessage::user("msg3")),
        ];

        let pass = Segment2Pass::new(vec![], prior, &[], &[], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placed_by_pass, "segment_2");
        match placements[0].location {
            BreakpointLocation::MessageBlock(idx) => {
                assert_eq!(idx, 2, "marker must be on the last message (index 2)");
            }
            other => panic!("expected MessageBlock, got {other:?}"),
        }
    }

    // ---- Empty segment 2: no messages, no marker ----------------------------

    #[test]
    fn empty_segment_2_no_marker() {
        let pass = Segment2Pass::new(vec![], vec![], &[], &[], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert!(partial.messages.is_empty());
        assert_eq!(partial.breakpoints.count(), 0);
    }

    // ---- Cache control from profile -----------------------------------------

    // ---- message_origins populated correctly ----------------------------------

    #[test]
    fn message_origins_tags_prior_messages_with_ids() {
        let summary = synthesize_summary_message(0, "pos_a", "pos_b", "summary");
        let prior = vec![
            (SmolStr::new("id-aaa"), ChatMessage::user("hello")),
            (SmolStr::new("id-bbb"), ChatMessage::assistant("hi")),
        ];
        let writes = vec![make_block_write("tasks", BlockWriteKind::Updated)];

        let pass = Segment2Pass::new(vec![summary], prior, &writes, &[], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        // Expected order: [summary(None), prior-aaa(Some), prior-bbb(Some), pseudo(None)].
        assert_eq!(partial.messages.len(), 4);
        assert_eq!(partial.message_origins.len(), 4);
        assert_eq!(partial.message_origins[0], None, "summary should be None");
        assert_eq!(
            partial.message_origins[1],
            Some(SmolStr::new("id-aaa")),
            "first prior should carry its id"
        );
        assert_eq!(
            partial.message_origins[2],
            Some(SmolStr::new("id-bbb")),
            "second prior should carry its id"
        );
        assert_eq!(partial.message_origins[3], None, "pseudo should be None");
    }

    // ---- Cache control from profile -----------------------------------------

    #[test]
    fn cache_control_uses_segment_2_control() {
        let prior = vec![(SmolStr::new("msg-1"), ChatMessage::user("msg"))];
        let pass = Segment2Pass::new(vec![], prior, &[], &[], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        // All-1h default profile per long-running-agent policy.
        assert_eq!(placements[0].control, CacheControl::Ephemeral1h);
    }
}
