// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
