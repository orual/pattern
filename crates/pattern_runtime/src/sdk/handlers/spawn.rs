//! Stub handler for `Pattern.Spawn`. Returns a `Handler` error identifying
//! which phase will implement it.

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SpawnReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the Spawn effect. Real implementation
/// arrives in the post-foundation constellation-runtime plan.
#[derive(Default, Clone)]
pub struct SpawnHandler;

impl DescribeEffect for SpawnHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Spawn",
            description: "Subagent / child-agent lifecycle (Start/Stop)",
            constructors: &[
                "Start :: AgentSpec -> Spawn AgentId",
                "Stop  :: AgentId -> Spawn ()",
            ],
            type_defs: &["type AgentSpec = Text", "type AgentId = Text"],
            helpers: &[
                "start :: Member Spawn effs => AgentSpec -> Eff effs AgentId\nstart spec = send (Start spec)",
                "stop :: Member Spawn effs => AgentId -> Eff effs ()\nstop i = send (Stop i)",
            ],
        }
    }
}

impl<U> EffectHandler<U> for SpawnHandler
where
    U: HasCancelState,
{
    type Request = SpawnReq;

    fn handle(&mut self, req: SpawnReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
        // Uniform HandlerGate entry — see ShellHandler for the rationale.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);
        Err(EffectError::Handler(format!(
            "Pattern.Spawn.{req:?} is not implemented in v3 foundation \
             (phase: post-foundation constellation-runtime plan). Agent \
             code should not call Spawn effects in v3-foundation-scope \
             programs."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_repr::DataConTable;

    #[test]
    fn spawn_stub_reports_not_implemented() {
        let mut h = SpawnHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());
        let err = h.handle(SpawnReq::Start("spec".into()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Spawn"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("constellation-runtime plan"), "got: {msg}");
    }
}
