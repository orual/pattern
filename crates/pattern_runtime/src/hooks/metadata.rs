//! Hook event metadata builder.
//!
//! Extracts session/agent/batch context from the handler's EffectContext
//! to populate HookEventMetadata.

use pattern_core::hooks::event::HookEventMetadata;
use smol_str::SmolStr;

use crate::session::SessionContext;

/// Build hook metadata from the current session context.
pub fn build_metadata(ctx: &SessionContext) -> HookEventMetadata {
    HookEventMetadata::now()
        .with_agent(SmolStr::from(ctx.agent_id()))
        .with_session(SmolStr::from(ctx.session_id()))
}

/// Build hook metadata from an EffectContext.
pub fn build_metadata_from_cx(
    cx: &tidepool_effect::EffectContext<'_, SessionContext>,
) -> HookEventMetadata {
    build_metadata(cx.user())
}
