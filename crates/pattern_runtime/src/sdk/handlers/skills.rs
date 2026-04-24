//! Handler for `Pattern.Skills` — skill-operation surface (list, get_metadata, load, search, get_usage_stats).
//!
//! The handler wires five methods: list, get_metadata, load, search, and get_usage_stats.
//! Methods delegate to MemoryStore and pattern_db for data access.
//!
//! Tasks 5–7 of Phase 5 fill the method bodies. This module currently provides
//! the `DescribeEffect` declaration and a stub `EffectHandler` that returns a
//! clear error until the full implementation lands.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SkillsReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Skills handler (implementation details in later tasks).
#[derive(Clone)]
pub struct SkillsHandler;

impl std::fmt::Debug for SkillsHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillsHandler").finish_non_exhaustive()
    }
}

impl DescribeEffect for SkillsHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Skills",
            description: "Skill-block operations: list, get_metadata, load, search, get_usage_stats",
            constructors: &[
                "List          :: Skills Text",
                "GetMetadata   :: BlockHandle -> Skills Text",
                "Load          :: BlockHandle -> Skills ()",
                "Search        :: Text -> Skills Text",
                "GetUsageStats :: BlockHandle -> Skills Text",
            ],
            type_defs: &[
                "type BlockHandle = Text",
                "type SkillInfo = Text       -- JSON: {handle:BlockHandle, name:Text, description?:Text, trust_tier:Text, keywords:[Text], last_used?:Text}",
                "type SkillMetadata = Text   -- JSON: {name:Text, description?:Text, version?:Text, trust_tier:Text, keywords:[Text], hooks:Value}",
                "type SkillUsageStats = Text -- JSON: {handle:BlockHandle, use_count:Int, last_used?:Text, last_used_by?:Text}",
            ],
            helpers: &[
                "listSkills :: Member Skills effs => Eff effs Text\nlistSkills = send List",
                "getSkillMetadata :: Member Skills effs => BlockHandle -> Eff effs Text\ngetSkillMetadata h = send (GetMetadata h)",
                "loadSkill :: Member Skills effs => BlockHandle -> Eff effs ()\nloadSkill h = send (Load h)",
                "searchSkills :: Member Skills effs => Text -> Eff effs Text\nsearchSkills q = send (Search q)",
                "getSkillUsageStats :: Member Skills effs => BlockHandle -> Eff effs Text\ngetSkillUsageStats h = send (GetUsageStats h)",
            ],
        }
    }
}

/// Stub `EffectHandler` impl. Returns a clear diagnostic error for every
/// variant until Phase 5 Tasks 5–7 fill the bodies.
///
/// The `HasCancelState` bound mirrors the pattern used by other pre-implementation
/// handlers (ShellHandler, FileHandler, etc.) — it keeps the HList usable
/// without coupling to `SessionContext` before the full wiring lands.
impl<U> EffectHandler<U> for SkillsHandler
where
    U: HasCancelState,
{
    type Request = SkillsReq;

    fn handle(&mut self, req: SkillsReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Enter the HandlerGate so the watchdog's bookkeeping does not see a
        // skills-only agent as non-yielding. The stub errors fast so the gate
        // is entered and exited within the same call.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        let method = match &req {
            SkillsReq::List => "List",
            SkillsReq::GetMetadata(_) => "GetMetadata",
            SkillsReq::Load(_) => "Load",
            SkillsReq::Search(_) => "Search",
            SkillsReq::GetUsageStats(_) => "GetUsageStats",
        };
        Err(EffectError::Handler(format!(
            "Pattern.Skills.{method} is not yet implemented \
             (Phase 5 Tasks 5-7). Agent code should not call Skills \
             effects before the handler implementation lands."
        )))
    }
}
