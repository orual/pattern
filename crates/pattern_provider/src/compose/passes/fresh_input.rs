//! Fresh input composer pass — appends current-turn user messages with
//! inline attachment rendering and places the segment-3 cache marker.
//!
//! Fresh input sits AFTER the segment-2 cache boundary (uncached by
//! design until the next turn promotes it into history). Attachments
//! (e.g. `BatchOpeningSnapshot`, `FileEdit`, `BlockWriteNotifications`)
//! are rendered inline at compose time — no post-compose splice needed.

use jiff::Unit;
use jiff::tz::TimeZone;
use pattern_core::error::ProviderError;
use pattern_core::types::message::Message;

use crate::compose::render::{render_attachments_for_message, splice_text_onto_message};
use crate::compose::{BreakpointLocation, CacheProfile, ComposerPass, PartialRequest};

/// Segment 3 / fresh input: appends the current turn's input messages
/// with attachments rendered inline and places the segment-3 cache
/// marker on the last message that had an attachment spliced.
pub struct FreshInputPass {
    /// Current-turn input Pattern Messages.
    messages: Vec<Message>,
    /// Session-latched cache profile for the seg3 marker.
    profile: CacheProfile,
}

impl FreshInputPass {
    /// Construct from the current turn's input messages.
    pub fn new(messages: Vec<Message>, profile: CacheProfile) -> Self {
        Self { messages, profile }
    }
}

impl ComposerPass for FreshInputPass {
    fn name(&self) -> &'static str {
        "fresh_input"
    }

    fn apply(&self, partial: &mut PartialRequest) -> Result<(), ProviderError> {
        let mut last_spliced_idx: Option<usize> = None;

        for msg in &self.messages {
            let mut chat = msg.chat_message.clone();
            if let Some(mut rendered) = render_attachments_for_message(&msg.attachments) {
                let time = msg
                    .created_at
                    .to_zoned(TimeZone::system())
                    .round(Unit::Minute)
                    .unwrap_or(msg.created_at.to_zoned(TimeZone::system()));
                rendered.push_str(format!("\n\nmessage time: {time}").as_str());
                splice_text_onto_message(&mut chat, &rendered);
                last_spliced_idx = Some(partial.messages.len());
            }
            partial.push_message(chat, Some(msg.id.clone()));
        }

        // Place seg3 cache marker on the last message that had an
        // attachment spliced. If no attachments were spliced (e.g.
        // continuation turn with no fresh input or no attachments),
        // skip — the seg2 marker is the last cache boundary.
        if let Some(idx) = last_spliced_idx {
            let control = self.profile.segment_3_control();
            partial.breakpoints.place(
                BreakpointLocation::MessageBlock(idx),
                control,
                self.name(),
            )?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use smol_str::SmolStr;

    use pattern_core::types::ids::{new_id, new_snowflake_id};
    use pattern_core::types::memory_types::MemoryBlockType;
    use pattern_core::types::message::{Message, MessageAttachment, RenderedBlock, SnapshotKind};

    use crate::compose::partial_request::PartialRequest;
    use crate::compose::pipeline::ComposerPass;
    use crate::compose::profile::CacheProfile;

    use super::FreshInputPass;

    fn test_profile() -> CacheProfile {
        CacheProfile::default_anthropic_subscriber()
    }

    fn make_message(role_text: &str, attachments: Vec<MessageAttachment>) -> Message {
        let chat = genai::chat::ChatMessage::user(role_text);
        Message {
            chat_message: chat,
            id: new_id(),
            position: new_snowflake_id(),
            owner_id: SmolStr::new("agent-1"),
            created_at: Timestamp::UNIX_EPOCH,
            batch: new_snowflake_id(),
            response_meta: None,
            block_refs: vec![],
            attachments,
        }
    }

    #[test]
    fn fresh_input_renders_attachments_inline() {
        let msg = make_message(
            "hello",
            vec![MessageAttachment::Custom {
                content: "injected context".to_string(),
            }],
        );
        let pass = FreshInputPass::new(vec![msg], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 1);
        let text = partial.messages[0]
            .content
            .joined_texts()
            .unwrap_or_default();
        assert!(
            text.contains("injected context"),
            "attachment not rendered inline: {text}"
        );
    }

    #[test]
    fn fresh_input_no_attachments_no_marker() {
        let msg = make_message("hello", vec![]);
        let pass = FreshInputPass::new(vec![msg], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        assert_eq!(partial.messages.len(), 1);
        assert_eq!(partial.breakpoints.count(), 0);
    }

    #[test]
    fn fresh_input_with_snapshot_places_seg3_marker() {
        let msg = make_message(
            "hello",
            vec![MessageAttachment::BatchOpeningSnapshot {
                kind: SnapshotKind::Full,
                block_names: vec![SmolStr::new("persona")],
                blocks: vec![RenderedBlock {
                    label: SmolStr::new("persona"),
                    block_type: MemoryBlockType::Core,
                    rendered: Some("I am a test agent.".into()),
                    content_hash: 42,
                }],
                edited_blocks: vec![],
            }],
        );
        let pass = FreshInputPass::new(vec![msg], test_profile());
        let mut partial = PartialRequest::new("claude-opus-4-7");
        pass.apply(&mut partial).unwrap();

        // Marker placed.
        let placements = partial.breakpoints.placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].placed_by_pass, "fresh_input");

        // Content rendered inline.
        let text = partial.messages[0]
            .content
            .joined_texts()
            .unwrap_or_default();
        assert!(
            text.contains("[memory:current_state]"),
            "snapshot not rendered: {text}"
        );
    }
}
