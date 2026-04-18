//! [`NoOpShaper`] — default shaper for providers that don't need pattern-
//! side request rewriting (Gemini, future OpenAI, etc.).
//!
//! Emits only a minimal `User-Agent` header for honest identification.
//! Never touches `ChatRequest`. When a provider grows pattern-specific
//! shaping needs, implement a dedicated shaper (see `shaper/anthropic.rs`)
//! and register it per-provider in the gateway.

use pattern_core::error::ProviderError;

use super::{RequestShaper, ShapeContext};

/// No-op shaper. Emits a minimal `User-Agent` for honest identification
/// but never touches `ChatRequest`. The default shaper for providers
/// (Gemini, OpenAI, etc.) that don't need pattern-side shaping.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpShaper;

impl RequestShaper for NoOpShaper {
    fn shape(
        &self,
        _req: &mut genai::chat::ChatRequest,
        ctx: &ShapeContext<'_>,
    ) -> Result<std::collections::BTreeMap<String, String>, ProviderError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthTier;
    use crate::session_uuid::SessionUuidRotator;

    #[test]
    fn noop_shaper_emits_only_user_agent() {
        let shaper = NoOpShaper;
        let uuid = SessionUuidRotator::new();
        let session = uuid.current();

        let mut req = genai::chat::ChatRequest::from_user("hi");
        let before_system = req.system.clone();

        let ctx = ShapeContext {
            session_uuid: &session,
            model: "gemini-2.5-pro",
            auth_tier: AuthTier::ApiKey,
            persona: "persona",
            system_instructions_override: None,
            extra_long_lived_blocks: &[],
        };
        let headers = shaper.shape(&mut req, &ctx).expect("shape ok");

        // Request untouched.
        assert_eq!(req.system, before_system);
        assert!(
            req.system_blocks.is_none(),
            "NoOpShaper must leave system_blocks untouched (None)"
        );

        // Only a user-agent header (lowercased for HTTP case-insensitivity).
        assert_eq!(headers.len(), 1);
        let user_agent = headers
            .get("user-agent")
            .expect("NoOpShaper should emit user-agent");
        assert!(user_agent.starts_with("pattern/"));
    }
}
