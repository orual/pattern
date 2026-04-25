//! Stub handler for `Pattern.Spawn`. Returns a per-variant `Handler`
//! error identifying the Phase 2 task that wires the real implementation.
//!
//! Phase 2 Task 2 (this file) lands the typed wire grammar and the
//! per-variant stub messages. Subsequent tasks replace the stubs:
//! Task 4 wires `Ephemeral` / `AwaitSpawn` / `AwaitAll` / `Stop`;
//! Tasks 6+7 wire `Sibling`; Task 8 scaffolds `Fork` (lightweight only;
//! persistent isolation is Phase 3).

use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SpawnReq;
use crate::session::HasCancelState;
use crate::timeout::HandlerGuard;

/// Not-implemented placeholder for the Spawn effect. Real implementation
/// arrives in Phase 2 Tasks 4–8 of the v3-multi-agent plan.
#[derive(Default, Clone)]
pub struct SpawnHandler;

impl DescribeEffect for SpawnHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Spawn",
            description: "Subagent / child-agent lifecycle: ephemeral workers, forks, sibling personas, await + stop",
            constructors: &[
                "Ephemeral  :: EphemeralConfig -> Spawn SpawnId",
                "AwaitSpawn :: SpawnId -> Spawn SpawnResult",
                "AwaitAll   :: [SpawnId] -> Spawn AwaitAllResult",
                "Fork       :: ForkConfig -> Spawn ForkHandle",
                "Sibling    :: SiblingConfig -> Spawn PersonaId",
                "Stop       :: SpawnId -> Spawn ()",
            ],
            type_defs: &[
                "type SpawnId   = Text",
                "type PersonaId = Text",
                // Result types remain opaque text in Phase 2; ergonomic
                // accessors land in Task 9 / Phase 3.
                "type SpawnResult     = Text",
                "type AwaitAllResult  = Text",
                "type ForkHandle      = Text",
                // Config records — full record definitions live in
                // Pattern.Spawn.hs; agents construct them positionally
                // or via the helper functions below.
            ],
            helpers: &[
                "ephemeral :: Member Spawn effs => EphemeralConfig -> Eff effs SpawnId\nephemeral cfg = send (Ephemeral cfg)",
                "awaitSpawn :: Member Spawn effs => SpawnId -> Eff effs SpawnResult\nawaitSpawn sid = send (AwaitSpawn sid)",
                "awaitAll :: Member Spawn effs => [SpawnId] -> Eff effs AwaitAllResult\nawaitAll ids = send (AwaitAll ids)",
                "fork :: Member Spawn effs => ForkConfig -> Eff effs ForkHandle\nfork cfg = send (Fork cfg)",
                "sibling :: Member Spawn effs => SiblingConfig -> Eff effs PersonaId\nsibling cfg = send (Sibling cfg)",
                "stop :: Member Spawn effs => SpawnId -> Eff effs ()\nstop sid = send (Stop sid)",
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
        let (variant, wiring_task) = match &req {
            SpawnReq::Ephemeral(_) => ("Ephemeral", "Phase 2 Task 4"),
            SpawnReq::AwaitSpawn(_) => ("AwaitSpawn", "Phase 2 Task 4"),
            SpawnReq::AwaitAll(_) => ("AwaitAll", "Phase 2 Task 4"),
            SpawnReq::Fork(_) => ("Fork", "Phase 2 Task 8"),
            SpawnReq::Sibling(_) => ("Sibling", "Phase 2 Tasks 6–7"),
            SpawnReq::Stop(_) => ("Stop", "Phase 2 Task 4"),
        };
        Err(EffectError::Handler(format!(
            "Pattern.Spawn.{variant} is not implemented (wiring lands in {wiring_task} of the v3-multi-agent plan)."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::requests::spawn::{
        WireEphemeralConfig, WireForkConfig, WireForkIsolation, WireRelationshipKind,
        WireSiblingConfig, WireSiblingPersona,
    };
    use tidepool_repr::DataConTable;

    fn empty_ephemeral() -> WireEphemeralConfig {
        WireEphemeralConfig {
            program: String::new(),
            costume: None,
            capabilities: None,
            timeout_ms: None,
            prompt: None,
        }
    }

    fn empty_fork() -> WireForkConfig {
        WireForkConfig {
            program: String::new(),
            isolation: WireForkIsolation::Lightweight,
            capabilities: None,
            timeout_hint_ms: None,
            task_ref: None,
        }
    }

    fn empty_sibling() -> WireSiblingConfig {
        WireSiblingConfig {
            persona: WireSiblingPersona::Existing(String::new()),
            relationship: WireRelationshipKind::PeerWith,
            shared_blocks: Vec::new(),
        }
    }

    #[test]
    fn spawn_stub_reports_not_implemented_per_variant() {
        let mut h = SpawnHandler;
        let table = DataConTable::new();
        let cx = EffectContext::with_user(&table, &());

        let err = h
            .handle(SpawnReq::Ephemeral(empty_ephemeral()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Spawn.Ephemeral"), "got: {msg}");
        assert!(msg.contains("not implemented"), "got: {msg}");
        assert!(msg.contains("Phase 2 Task 4"), "got: {msg}");

        let err = h
            .handle(SpawnReq::Sibling(empty_sibling()), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Spawn.Sibling"), "got: {msg}");
        assert!(msg.contains("Phase 2 Tasks 6"), "got: {msg}");

        let err = h.handle(SpawnReq::Fork(empty_fork()), &cx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Spawn.Fork"), "got: {msg}");
        assert!(msg.contains("Phase 2 Task 8"), "got: {msg}");

        let err = h
            .handle(SpawnReq::AwaitAll(vec!["a".into(), "b".into()]), &cx)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Pattern.Spawn.AwaitAll"), "got: {msg}");
    }

    #[test]
    fn effect_decl_advertises_six_constructors_and_helpers() {
        let decl = SpawnHandler::effect_decl();
        let names: Vec<&str> = decl
            .constructors
            .iter()
            .filter_map(|c| c.split_whitespace().next())
            .collect();
        assert_eq!(
            names,
            vec![
                "Ephemeral",
                "AwaitSpawn",
                "AwaitAll",
                "Fork",
                "Sibling",
                "Stop"
            ],
            "constructor list drift; update Pattern.Spawn.hs in lockstep"
        );
        assert!(
            !names.contains(&"Start"),
            "legacy `Start` constructor must be retired"
        );

        for ctor in [
            "Ephemeral",
            "AwaitSpawn",
            "AwaitAll",
            "Fork",
            "Sibling",
            "Stop",
        ] {
            assert!(
                decl.helpers.iter().any(|h| h.contains(ctor)),
                "no helper references constructor {ctor}"
            );
        }
    }
}
