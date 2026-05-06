//! Segment 2 composer pass — prior-turn conversation history +
//! summary-head prepend + inline attachment rendering + cache marker.
//!
//! # Message ordering (matters for cache boundary)
//!
//! 1. Summary-head messages (synthesized from archive summaries,
//!    pre-rendered by the caller).
//! 2. Prior-turn Pattern Messages (from `TurnHistory::active_messages`),
//!    with attachments (snapshots, block-write notifications, file-edit
//!    reminders, etc.) rendered inline via the `compose::render` module.
//!
//! The segment-2 cache marker lands on the **last** message pushed by
//! this pass. Fresh user input is handled by [`super::FreshInputPass`].
//!
//! # Summary-head rendering
//!
//! [`synthesize_summary_message`] converts archive-summary metadata
//! (depth, position range, text) into a `ChatMessage::user` wrapped in
//! `<system-reminder>` tags. The function accepts individual fields
//! rather than `pattern_db::ArchiveSummary` so `pattern_provider` does
//! not depend on `pattern_db`.

use genai::chat::ChatMessage;
use jiff::Unit;
use jiff::tz::TimeZone;
use pattern_core::error::ProviderError;
use pattern_core::types::message::Message;

use crate::compose::render::{render_attachments_for_message, splice_text_onto_message};
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

/// Segment 2: prior-turn conversation history + summary-head.
///
/// Takes full Pattern `Message`s for prior-turn history. Attachments
/// on each message are rendered inline at `apply()` time via
/// [`crate::compose::render::render_attachments_for_message`] and
/// spliced onto the corresponding `ChatMessage`. This eliminates the
/// need for a post-compose attachment splice in the agent loop.
///
/// Does NOT include fresh user input — that is handled by
/// [`super::FreshInputPass`].
pub struct Segment2Pass {
    /// Pre-rendered summary-head messages. The turn loop calls
    /// [`synthesize_summary_message`] for each `ArchiveSummary` and
    /// passes the results here.
    summary_head_messages: Vec<ChatMessage>,
    /// Prior-turn Pattern Messages from `TurnHistory::active_messages`.
    /// Their `attachments` field is rendered inline at `apply()` time.
    prior_messages: Vec<Message>,
    /// Session-latched cache profile.
    profile: CacheProfile,
}

impl Segment2Pass {
    /// Construct from pre-rendered summary-head messages and full
    /// Pattern Messages for prior-turn history.
    ///
    /// Block-write notifications are now carried as
    /// `MessageAttachment::BlockWriteNotifications` on the relevant
    /// Pattern Messages — no separate `recent_block_writes` parameter.
    pub fn new(
        summary_head_messages: Vec<ChatMessage>,
        prior_messages: Vec<Message>,
        profile: CacheProfile,
    ) -> Self {
        Self {
            summary_head_messages,
            prior_messages,
            profile,
        }
    }
}

impl ComposerPass for Segment2Pass {
    fn name(&self) -> &'static str {
        "segment_2"
    }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        // Summary-head messages have no Pattern Message identity.
        for msg in &self.summary_head_messages {
            partial.push_message(msg.clone(), None);
        }

        // Prior messages: render attachments inline, splice, push with origin.
        for msg in &self.prior_messages {
            let mut chat = msg.chat_message.clone();
            if let Some(mut rendered) = render_attachments_for_message(&msg.attachments) {
                let time = msg
                    .created_at
                    .to_zoned(TimeZone::system())
                    .round(Unit::Minute)
                    .unwrap_or(msg.created_at.to_zoned(TimeZone::system()));
                rendered.push_str(format!("\n\nmessage time: {time}").as_str());
                splice_text_onto_message(&mut chat, &rendered);
            }
            partial.push_message(chat, Some(msg.id.clone()));
        }

        // Place marker on the last message we just pushed. If we
        // pushed nothing (empty history + no summaries), skip the
        // marker — the segment is empty.
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
    use pattern_core::types::ids::new_snowflake_id;
    use pattern_core::types::memory_types::MemoryBlockType;
    use pattern_core::types::message::MessageAttachment;
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

    /// Build a Pattern `Message` from a `ChatMessage` with optional attachments.
    fn make_pattern_message(
        id: &str,
        chat: ChatMessage,
        attachments: Vec<MessageAttachment>,
    ) -> Message {
        Message {
            chat_message: chat,
            id: SmolStr::new(id),
            position: new_snowflake_id(),
            owner_id: SmolStr::new("agent-1"),
            created_at: Timestamp::UNIX_EPOCH,
            batch: new_snowflake_id(),
            response_meta: None,
            block_refs: vec![],
            attachments,
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

    // ---- BlockWriteNotifications rendered inline in segment 2 ---------------

    #[test]
    fn block_write_attachments_rendered_inline_in_segment_2() {
        let writes = vec![make_block_write("task_list", BlockWriteKind::Updated)];
        let prior = vec![
            make_pattern_message("msg-1", ChatMessage::user("hello"), vec![]),
            make_pattern_message(
                "msg-2",
                ChatMessage::assistant("hi"),
                vec![MessageAttachment::BlockWriteNotifications { writes }],
            ),
        ];

        let pass = Segment2Pass::new(vec![], prior, test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        // Should have: 2 prior messages (no separate pseudo-message).
        assert_eq!(partial.messages.len(), 2);

        // The assistant message (index 1) must have [memory:updated]
        // rendered inline via its BlockWriteNotifications attachment.
        let last_text = msg_text(&partial.messages[1]);
        assert!(
            last_text.contains("[memory:updated]"),
            "block write attachment not rendered inline: {last_text}"
        );
    }

    // ---- Summary-head messages appear first ---------------------------------

    #[test]
    fn summary_head_messages_appear_before_prior() {
        let summary = synthesize_summary_message(0, "pos_a", "pos_b", "summary text");
        let prior = vec![make_pattern_message(
            "msg-1",
            ChatMessage::user("recent message"),
            vec![],
        )];

        let pass = Segment2Pass::new(vec![summary], prior, test_profile());
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
            make_pattern_message("msg-1", ChatMessage::user("msg1"), vec![]),
            make_pattern_message("msg-2", ChatMessage::assistant("msg2"), vec![]),
            make_pattern_message("msg-3", ChatMessage::user("msg3"), vec![]),
        ];

        let pass = Segment2Pass::new(vec![], prior, test_profile());
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
        let pass = Segment2Pass::new(vec![], vec![], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert!(partial.messages.is_empty());
        assert_eq!(partial.breakpoints.count(), 0);
    }

    // ---- message_origins populated correctly ----------------------------------

    #[test]
    fn message_origins_tags_prior_messages_with_ids() {
        let summary = synthesize_summary_message(0, "pos_a", "pos_b", "summary");
        let prior = vec![
            make_pattern_message("id-aaa", ChatMessage::user("hello"), vec![]),
            make_pattern_message("id-bbb", ChatMessage::assistant("hi"), vec![]),
        ];

        let pass = Segment2Pass::new(vec![summary], prior, test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        // Expected order: [summary(None), prior-aaa(Some), prior-bbb(Some)].
        assert_eq!(partial.messages.len(), 3);
        assert_eq!(partial.message_origins.len(), 3);
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
    }

    // ---- Cache control from profile -----------------------------------------

    #[test]
    fn cache_control_uses_segment_2_control() {
        let prior = vec![make_pattern_message(
            "msg-1",
            ChatMessage::user("msg"),
            vec![],
        )];
        let pass = Segment2Pass::new(vec![], prior, test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        let placements = partial.breakpoints.placements();
        // All-1h default profile per long-running-agent policy.
        assert_eq!(placements[0].control, CacheControl::Ephemeral1h);
    }

    // ---- Tool-result message with FileEdit attachment via compose pipeline ---

    /// Bug-fix verification test: a tool-result Pattern Message with a
    /// FileEdit attachment, walked through the compose pipeline, must have
    /// the `<system-reminder>` block rendered on the tool-result message.
    /// This is the gap the previous architecture had — tool-result messages
    /// in history might not have their attachments rendered if
    /// `message_origins` didn't tag them correctly.
    #[test]
    fn tool_result_with_file_edit_attachment_renders_via_compose() {
        use genai::chat::{ContentPart, MessageContent, ToolResponse};

        let tool_response = ToolResponse {
            call_id: "call-123".to_string(),
            content: serde_json::json!("file written successfully"),
        };
        let tool_msg = ChatMessage {
            role: genai::chat::ChatRole::Tool,
            content: MessageContent::from_parts(vec![ContentPart::ToolResponse(tool_response)]),
            options: None,
        };
        let prior = vec![
            make_pattern_message("msg-1", ChatMessage::user("write a file"), vec![]),
            make_pattern_message("msg-2", ChatMessage::assistant("calling tool"), vec![]),
            make_pattern_message(
                "msg-3",
                tool_msg,
                vec![MessageAttachment::FileEdit {
                    path: std::path::PathBuf::from("/tmp/test.txt"),
                    kind: pattern_core::types::message::FileEditKind::Open,
                    at: Timestamp::from_second(1_745_000_000).unwrap(),
                    diff: None,
                }],
            ),
        ];

        let pass = Segment2Pass::new(vec![], prior, test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 3);

        // The tool-result message (index 2) must have the FileEdit
        // attachment rendered inline via splice_text_onto_message.
        let tool_text = partial.messages[2]
            .content
            .parts()
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolResponse(tr) => {
                    // The spliced content lives inside the tool response's
                    // content array as a JSON text block.
                    Some(tr.content.to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            tool_text.contains("External edit") || tool_text.contains("system-reminder"),
            "FileEdit attachment not rendered on tool-result message: {tool_text}"
        );
    }

    // ---- Assistant message with BlockWrite attachment via compose pipeline ---

    #[test]
    fn assistant_message_with_block_write_attachment_renders_via_compose() {
        let writes = vec![make_block_write("scratchpad", BlockWriteKind::Created)];
        let prior = vec![
            make_pattern_message("msg-1", ChatMessage::user("remember this"), vec![]),
            make_pattern_message(
                "msg-2",
                ChatMessage::assistant("stored in scratchpad"),
                vec![MessageAttachment::BlockWriteNotifications { writes }],
            ),
        ];

        let pass = Segment2Pass::new(vec![], prior, test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 2);

        // The assistant message must have the block write rendered inline.
        let text = msg_text(&partial.messages[1]);
        assert!(
            text.contains("[memory:written]"),
            "BlockWrite Created attachment not rendered on assistant message: {text}"
        );
    }
}
