//! Handler for `Pattern.Spawn`. Wires Ephemeral / AwaitSpawn / AwaitAll
//! / Stop to the spawn registry + ephemeral runner. Sibling and Fork
//! remain stubs returning per-task placeholder errors (Tasks 6/7 and 8
//! of the v3-multi-agent plan).
//!
//! Sync→async glue: handlers run on the eval-worker OS thread (no
//! ambient tokio runtime). For paths that need to await a future, this
//! handler uses `cx.user().tokio_handle().block_on(...)`. The await
//! target is bounded — the `SpawnRegistry`'s `Shared<BoxFuture>`
//! resolves when the child's `run_ephemeral` future completes, which is
//! itself bounded by `tokio::time::timeout`. No plugin code in the
//! await path; sandbox-io's "block_on can deadlock if plugin code
//! recursively calls spawn_blocking" caution does not apply here.

use std::sync::Arc;

use futures::FutureExt;
use smol_str::SmolStr;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use pattern_core::types::ids::new_id;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SpawnReq;
use crate::sdk::requests::spawn::{WireEphemeralSpawn, WireSpawnAwaitOutcome, WireSpawnResult};
use crate::session::SessionContext;
use crate::spawn::sibling::{spawn_sibling_existing, spawn_sibling_new};
use crate::spawn::{
    ChildSessionHandle, SpawnError, SpawnKind, WireForkHandle, child_include_paths,
    compute_child_caps, run_ephemeral, synthesize_program_lib,
};
use crate::timeout::HandlerGuard;

/// Handler for the `Pattern.Spawn` effect.
#[derive(Default, Clone)]
pub struct SpawnHandler;

impl DescribeEffect for SpawnHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Spawn",
            description: "Subagent / child-agent lifecycle: ephemeral workers, forks, sibling personas, await + stop",
            constructors: &[
                "Ephemeral  :: EphemeralConfig -> Spawn EphemeralSpawn",
                "AwaitSpawn :: SpawnId -> Spawn SpawnResult",
                "AwaitAll   :: [SpawnId] -> Spawn [SpawnAwaitOutcome]",
                "Fork       :: ForkConfig -> Spawn ForkHandle",
                "Sibling    :: SiblingConfig -> Spawn PersonaId",
                "Stop       :: SpawnId -> Spawn ()",
            ],
            type_defs: &[
                "type SpawnId   = Text",
                "type PersonaId = Text",
                // Typed records — full field definitions live in Pattern.Spawn.hs.
                "data EphemeralSpawn = EphemeralSpawn { ephemeralSpawnId :: SpawnId, ephemeralSpawnLogLabel :: Text }",
                "data TerminationReason = TermEndTurn | TermToolUse | TermMaxTurns | TermTimeout | TermCancelled | TermError",
                "data SpawnResult = SpawnResult { spawnResultChildId :: SpawnId, spawnResultFinalText :: Maybe Text, spawnResultTurns :: Int, spawnResultTerminated :: TerminationReason, spawnResultProgressLogLabel :: Maybe Text }",
                "data SpawnAwaitOutcome = SpawnOk SpawnResult | SpawnFail Text",
                "data ForkHandle = ForkHandle { forkHandleId :: SpawnId, forkHandleChildId :: SpawnId }",
            ],
            helpers: &[
                "ephemeral :: Member Spawn effs => EphemeralConfig -> Eff effs EphemeralSpawn\nephemeral cfg = send (Ephemeral cfg)",
                "awaitSpawn :: Member Spawn effs => SpawnId -> Eff effs SpawnResult\nawaitSpawn sid = send (AwaitSpawn sid)",
                "awaitAll :: Member Spawn effs => [SpawnId] -> Eff effs [SpawnAwaitOutcome]\nawaitAll ids = send (AwaitAll ids)",
                "fork :: Member Spawn effs => ForkConfig -> Eff effs ForkHandle\nfork cfg = send (Fork cfg)",
                "sibling :: Member Spawn effs => SiblingConfig -> Eff effs PersonaId\nsibling cfg = send (Sibling cfg)",
                "stop :: Member Spawn effs => SpawnId -> Eff effs ()\nstop sid = send (Stop sid)",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for SpawnHandler {
    type Request = SpawnReq;

    fn handle(
        &mut self,
        req: SpawnReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        // Uniform HandlerGate entry — see ShellHandler for the rationale.
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        match req {
            SpawnReq::Ephemeral(wire_cfg) => handle_ephemeral(wire_cfg, cx),
            SpawnReq::AwaitSpawn(id) => handle_await_spawn(id, cx),
            SpawnReq::AwaitAll(ids) => handle_await_all(ids, cx),
            SpawnReq::Stop(id) => handle_stop(id, cx),
            SpawnReq::Fork(wire_cfg) => handle_fork(wire_cfg, cx),
            SpawnReq::Sibling(wire_cfg) => handle_sibling(wire_cfg, cx),
        }
    }
}

fn handle_ephemeral(
    wire_cfg: crate::sdk::requests::spawn::WireEphemeralConfig,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let cfg: pattern_core::spawn::EphemeralConfig = wire_cfg.into();
    let parent: &SessionContext = cx.user();
    let parent_arc = parent.spawn_registry().clone();
    let _ = parent_arc; // silence: we use parent's registry directly below.

    // Acquire concurrency permit. Fail fast on saturation.
    let registry = parent.spawn_registry().clone();
    let permit = registry.try_acquire_ephemeral_slot().ok_or_else(|| {
        EffectError::Handler(
            SpawnError::ConcurrencyLimitExceeded {
                limit: registry.concurrent_ephemeral_limit(),
            }
            .to_string(),
        )
    })?;

    // Compute child capabilities (subset of parent).
    let child_caps =
        compute_child_caps(parent, &cfg).map_err(|e| EffectError::Handler(e.to_string()))?;

    // Synthesize lib module from cfg.program (if non-empty) — owned by
    // the spawned future for lifetime alignment.
    let lib_dir =
        synthesize_program_lib(&cfg.program).map_err(|e| EffectError::Handler(e.to_string()))?;

    // Build child include paths + child SessionContext via the parent's
    // fork helper (capability set + costume override applied there).
    let child_includes = child_include_paths(parent, lib_dir.as_ref());
    let child_ctx = parent.fork_for_ephemeral(&cfg, child_caps, Arc::new(child_includes.clone()));

    // Mint the child id + progress-log label.
    let child_id: SmolStr = new_id();
    let progress_log_label: SmolStr = format!("spawn-log-{child_id}").into();

    // Create the constellation-scoped progress-log block synchronously
    // before the runner is spawned. The parent gets the label back as
    // part of EphemeralSpawn and may read the block immediately.
    crate::spawn::create_progress_log_block(child_ctx.adapter(), progress_log_label.as_str())
        .map_err(|e| EffectError::Handler(e.to_string()))?;

    // Build the child's preamble from its restricted capability set.
    let child_caps_for_preamble = child_ctx
        .capabilities()
        .cloned()
        .unwrap_or_else(pattern_core::CapabilitySet::all);
    let preamble = crate::sdk::preamble::build_for(&child_caps_for_preamble);

    // Spawn the child task on the runtime. The lib_dir TempDir is moved
    // into the future so its drop is tied to the future's lifetime —
    // the temp directory cleans up when the spawn resolves or is
    // dropped via the registry's cancel-on-drop.
    let runner_fut = run_ephemeral(
        child_ctx.clone(),
        cfg,
        child_id.clone(),
        progress_log_label.clone(),
        child_includes,
        preamble,
        lib_dir,
    );
    let join = parent.tokio_handle().spawn(runner_fut);

    // Adapt JoinHandle<Result<SpawnResult, SpawnError>> → BoxFuture
    // mapping JoinError to SpawnError::JoinPanicked.
    let result = async move {
        match join.await {
            Ok(r) => r,
            Err(je) => Err(SpawnError::JoinPanicked(je.to_string())),
        }
    }
    .boxed()
    .shared();

    registry.register(ChildSessionHandle {
        child_id: child_id.clone(),
        kind: SpawnKind::Ephemeral,
        cancel_state: child_ctx.cancel_state().clone(),
        result,
        _permit: Some(permit),
    });

    let wire = WireEphemeralSpawn {
        spawn_id: child_id.into(),
        progress_log_label: progress_log_label.into(),
    };
    cx.respond(wire)
}

fn handle_await_spawn(
    id: String,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let registry = cx.user().spawn_registry().clone();
    let handle = cx.user().tokio_handle().clone();
    let id: SmolStr = id.into();
    let outcome = handle
        .block_on(registry.wait_for(&id))
        .map_err(|e| EffectError::Handler(e.to_string()))?;
    let wire = WireSpawnResult::from(outcome);
    cx.respond(wire)
}

fn handle_await_all(
    ids: Vec<String>,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let registry = cx.user().spawn_registry().clone();
    let handle = cx.user().tokio_handle().clone();
    let outcomes: Vec<Result<crate::spawn::SpawnResult, SpawnError>> =
        handle.block_on(async move {
            let futures = ids.into_iter().map(|id| {
                let reg = registry.clone();
                let id: SmolStr = id.into();
                async move { reg.wait_for(&id).await }
            });
            futures::future::join_all(futures).await
        });
    let wires: Vec<WireSpawnAwaitOutcome> = outcomes
        .into_iter()
        .map(WireSpawnAwaitOutcome::from)
        .collect();
    cx.respond(wires)
}

fn handle_stop(id: String, cx: &EffectContext<'_, SessionContext>) -> Result<Value, EffectError> {
    let _ = cx.user().spawn_registry().cancel_one(&SmolStr::from(id));
    cx.respond(())
}

fn handle_fork(
    wire_cfg: crate::sdk::requests::spawn::WireForkConfig,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let cfg: pattern_core::spawn::ForkConfig = wire_cfg.into();
    let parent: &SessionContext = cx.user();

    // Both isolation paths exercise the capability gate so Phase 2
    // wire-grammar verification works end-to-end.
    let phantom_eph = phantom_eph_cfg_for_fork(&cfg);
    compute_child_caps(parent, &phantom_eph).map_err(|e| EffectError::Handler(e.to_string()))?;

    match cfg.isolation {
        pattern_core::spawn::ForkIsolation::Lightweight => {
            // Phase 2 scaffold: generate ids but do not execute the fork's
            // program. Phase 3 wires LoroDoc::fork() + real compute path.
            let fork_id = pattern_core::types::ids::new_id();
            let child_id = pattern_core::types::ids::new_id();
            let handle = crate::spawn::ForkHandle { fork_id, child_id };
            let wire: WireForkHandle = handle.into();
            cx.respond(wire)
        }
        pattern_core::spawn::ForkIsolation::Persistent => Err(EffectError::Handler(
            "ForkIsolation::Persistent requires Phase 3 (jj workspace path not wired)".to_string(),
        )),
    }
}

/// Builds a minimal `EphemeralConfig` from a `ForkConfig` so that
/// `compute_child_caps` (which takes `EphemeralConfig`) can be reused as
/// the capability gate for fork paths.
fn phantom_eph_cfg_for_fork(
    fork_cfg: &pattern_core::spawn::ForkConfig,
) -> pattern_core::spawn::EphemeralConfig {
    let mut eph = pattern_core::spawn::EphemeralConfig::new(&fork_cfg.program);
    if let Some(caps) = fork_cfg.capabilities.clone() {
        eph = eph.with_capabilities(caps);
    }
    eph
}

fn handle_sibling(
    wire_cfg: crate::sdk::requests::spawn::WireSiblingConfig,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let cfg: pattern_core::spawn::SiblingConfig = wire_cfg.into();
    let parent: &SessionContext = cx.user();
    let handle = parent.tokio_handle().clone();

    let persona_id: SmolStr = match &cfg.persona {
        pattern_core::spawn::SiblingPersona::Existing(id) => {
            let resolver = parent.sibling_resolver().clone();
            let id_clone = id.clone();
            let cfg_clone = cfg.clone();
            handle
                .block_on(spawn_sibling_existing(
                    parent, &cfg_clone, &id_clone, resolver,
                ))
                .map_err(|e| EffectError::Handler(e.to_string()))?
        }
        pattern_core::spawn::SiblingPersona::New(persona_cfg) => {
            let drafts_dir = parent.drafts_dir().to_owned();
            let cfg_clone = cfg.clone();
            let persona_cfg_clone = persona_cfg.clone();
            handle
                .block_on(spawn_sibling_new(
                    parent,
                    &cfg_clone,
                    &persona_cfg_clone,
                    &drafts_dir,
                ))
                .map_err(|e| EffectError::Handler(e.to_string()))?
        }
    };

    // Siblings are NOT added to the spawn registry — they live independently
    // of the parent session's lifetime.
    cx.respond(persona_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::requests::spawn::{
        WireEphemeralConfig, WireForkConfig, WireForkIsolation, WireRelationshipKind,
        WireSiblingConfig, WireSiblingPersona,
    };

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
        // Silence dead-code warnings on the test fixtures imported above
        // for use by the integration test file.
        let _ = empty_ephemeral();
        let _ = empty_fork();
        let _ = empty_sibling();
    }
}
