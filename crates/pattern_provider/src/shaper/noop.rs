//! [`NoOpShaper`] — default shaper for providers that don't need
//! pattern-side request rewriting (Gemini, OpenAI, etc.).
//!
//! Emits only a minimal `User-Agent` header for honest identification.
//! The shaper performs ONE structural translation: if the composer
//! placed system content in [`ChatRequest::system_blocks`] (the
//! Anthropic-shaped multi-block representation), it gets flattened
//! into [`ChatRequest::system`] (a plain string). This is needed
//! because genai's `openai_resp`, `openai`, and `gemini` adapters all
//! consume `chat_req.system` (the string field) and ignore
//! `system_blocks` — without the flatten step, OpenAI requests would
//! arrive with no system prompt at all.
//!
//! When a provider grows pattern-specific shaping needs, implement a
//! dedicated shaper (see `shaper/anthropic.rs`) and register it
//! per-provider in the gateway.

use pattern_core::error::ProviderError;
use pattern_core::types::provider::SystemBlock;

use super::{RequestShaper, ShapeContext};

/// No-op shaper. Emits a minimal `User-Agent` for honest identification.
/// Flattens `system_blocks` → `system` for genai-adapter compatibility
/// (see module docs).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpShaper;

impl RequestShaper for NoOpShaper {
    fn shape(
        &self,
        req: &mut genai::chat::ChatRequest,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        flatten_system_blocks_into_system(req);
        self.identification_headers(ctx)
    }

    fn identification_headers(
        &self,
        _ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
        let mut out = std::collections::BTreeMap::new();
        out.insert(
            "user-agent".into(),
            format!("pattern/{}", env!("CARGO_PKG_VERSION")),
        );
        Ok(out)
    }
}

/// If `system_blocks` has content, join the block texts with `\n\n`,
/// place the result in `chat.system`, and clear `system_blocks`. If
/// `chat.system` is already populated, the joined blocks are prepended
/// (Pattern composer always builds via blocks; an existing string is
/// pre-composer baseline content, e.g. a chat-request constructor
/// helper, and should remain).
///
/// Empty blocks (those with no text) are dropped; if all blocks are
/// empty, `system_blocks` is cleared without setting `chat.system`.
fn flatten_system_blocks_into_system(req: &mut genai::chat::ChatRequest) {
    let Some(blocks) = req.system_blocks.take() else {
        return;
    };
    let joined = join_block_texts(&blocks);
    if joined.is_empty() {
        return;
    }
    req.system = match req.system.take() {
        Some(existing) if !existing.is_empty() => Some(format!("{joined}\n\n{existing}")),
        _ => Some(joined),
    };
}

fn join_block_texts(blocks: &[SystemBlock]) -> String {
    blocks
        .iter()
        .map(|b| b.text.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthTier;
    use crate::session_uuid::SessionUuidRotator;

    fn make_ctx<'a>(
        session: &'a crate::session_uuid::PatternSessionUuid,
    ) -> ShapeContext<'a> {
        ShapeContext {
            session_uuid: session,
            model: "gpt-4o",
            auth_tier: AuthTier::ApiKey,
            persona: "persona",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        }
    }

    #[test]
    fn noop_shaper_emits_only_user_agent_when_no_blocks() {
        let shaper = NoOpShaper;
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = genai::chat::ChatRequest::from_user("hi");
        let ctx = make_ctx(&session);
        let headers = shaper.shape(&mut req, &ctx).expect("shape ok");

        // No system content was ever set: nothing for the shaper to flatten.
        assert!(req.system.is_none(), "no input → no chat.system");
        assert!(req.system_blocks.is_none(), "no input → no system_blocks");

        assert_eq!(headers.len(), 1);
        assert!(
            headers
                .get("user-agent")
                .expect("user-agent")
                .starts_with("pattern/")
        );
    }

    #[test]
    fn noop_shaper_flattens_system_blocks_into_system() {
        let shaper = NoOpShaper;
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = genai::chat::ChatRequest::from_user("hi");
        req.system_blocks = Some(vec![
            SystemBlock::new("you are pattern"),
            SystemBlock::new("you help with adhd executive function"),
        ]);
        let ctx = make_ctx(&session);
        shaper.shape(&mut req, &ctx).expect("shape ok");

        assert!(
            req.system_blocks.is_none(),
            "system_blocks should be cleared after flatten"
        );
        let flat = req.system.as_deref().expect("flat system populated");
        assert_eq!(
            flat,
            "you are pattern\n\nyou help with adhd executive function"
        );
    }

    #[test]
    fn noop_shaper_drops_empty_blocks_during_flatten() {
        let shaper = NoOpShaper;
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = genai::chat::ChatRequest::from_user("hi");
        req.system_blocks = Some(vec![
            SystemBlock::new(""),
            SystemBlock::new("real content"),
            SystemBlock::new(""),
        ]);
        let ctx = make_ctx(&session);
        shaper.shape(&mut req, &ctx).expect("shape ok");

        assert_eq!(req.system.as_deref(), Some("real content"));
        assert!(req.system_blocks.is_none());
    }

    #[test]
    fn noop_shaper_prepends_blocks_when_chat_system_already_set() {
        let shaper = NoOpShaper;
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = genai::chat::ChatRequest::from_user("hi");
        req.system = Some("preexisting baseline".into());
        req.system_blocks = Some(vec![SystemBlock::new("pattern persona")]);
        let ctx = make_ctx(&session);
        shaper.shape(&mut req, &ctx).expect("shape ok");

        assert_eq!(
            req.system.as_deref(),
            Some("pattern persona\n\npreexisting baseline")
        );
    }

    #[test]
    fn noop_shaper_no_op_when_only_empty_blocks() {
        let shaper = NoOpShaper;
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = genai::chat::ChatRequest::from_user("hi");
        req.system_blocks = Some(vec![SystemBlock::new(""), SystemBlock::new("")]);
        let ctx = make_ctx(&session);
        shaper.shape(&mut req, &ctx).expect("shape ok");

        // All-empty blocks are dropped → no chat.system emitted; blocks cleared.
        assert!(req.system.is_none());
        assert!(req.system_blocks.is_none());
    }
}
