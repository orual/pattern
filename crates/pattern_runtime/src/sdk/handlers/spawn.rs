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
use pattern_core::types::memory_types::Scope;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SpawnReq;
use crate::sdk::requests::spawn::{
    WireEphemeralSpawn, WireForkOpKind, WireForkOpResult, WireSiblingSpawn, WireSpawnAwaitOutcome,
    WireSpawnResult,
};
use crate::session::SessionContext;
use crate::spawn::ForkIsolationState;
use crate::spawn::sibling::{SiblingExistingOutcome, spawn_sibling_existing, spawn_sibling_new};
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
            constructors: std::borrow::Cow::Borrowed(&[
                "Ephemeral  :: EphemeralConfig -> Spawn EphemeralSpawn",
                "AwaitSpawn :: SpawnId -> Spawn SpawnResult",
                "AwaitAll   :: [SpawnId] -> Spawn [SpawnAwaitOutcome]",
                "Fork       :: ForkConfig -> Spawn ForkHandle",
                "Sibling    :: SiblingConfig -> Spawn SiblingSpawn",
                "Stop       :: SpawnId -> Spawn ()",
                "ForkOp     :: SpawnId -> ForkOpKind -> Spawn ForkOpResult",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type SpawnId   = Text",
                "type PersonaId = Text",
                // Typed records — full field definitions live in Pattern.Spawn.hs.
                "data EphemeralSpawn = EphemeralSpawn { ephemeralSpawnId :: SpawnId, ephemeralSpawnLogLabel :: Text }",
                "data TerminationReason = TermEndTurn | TermToolUse | TermMaxTurns | TermTimeout | TermCancelled | TermError",
                "data SpawnResult = SpawnResult { spawnResultChildId :: SpawnId, spawnResultFinalText :: Maybe Text, spawnResultTurns :: Int, spawnResultTerminated :: TerminationReason, spawnResultProgressLogLabel :: Maybe Text }",
                "data SpawnAwaitOutcome = SpawnOk SpawnResult | SpawnFail Text",
                "data ForkHandle = ForkHandle { forkHandleId :: SpawnId, forkHandleChildId :: SpawnId }",
                "data SiblingSpawn = SiblingExistingActive PersonaId | SiblingNewActive PersonaId Text | SiblingNewDraft PersonaId Text",
                // Fork resolution types (Task 8.3). Three resolution paths:
                // MergeBack (non-consuming, handle stays), Discard (consuming),
                // Promote (consuming; requires SpawnNewIdentities capability).
                // No AwaitResult — lightweight forks are memory snapshots, not
                // running sessions; there is nothing to await.
                "data ForkOpKind = ForkOpMergeBack | ForkOpDiscard | ForkOpPromote PersonaConfig",
                "data ForkOpResult = ForkOpUnit | ForkOpMergeReport Text | ForkOpPersonaId PersonaId",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "ephemeral :: Member Spawn effs => EphemeralConfig -> Eff effs EphemeralSpawn\nephemeral cfg = send (Ephemeral cfg)",
                "awaitSpawn :: Member Spawn effs => SpawnId -> Eff effs SpawnResult\nawaitSpawn sid = send (AwaitSpawn sid)",
                "awaitAll :: Member Spawn effs => [SpawnId] -> Eff effs [SpawnAwaitOutcome]\nawaitAll ids = send (AwaitAll ids)",
                "fork :: Member Spawn effs => ForkConfig -> Eff effs ForkHandle\nfork cfg = send (Fork cfg)",
                "sibling :: Member Spawn effs => SiblingConfig -> Eff effs SiblingSpawn\nsibling cfg = send (Sibling cfg)",
                "stop :: Member Spawn effs => SpawnId -> Eff effs ()\nstop sid = send (Stop sid)",
                "mergeBack :: Member Spawn effs => SpawnId -> Eff effs ForkOpResult\nmergeBack fid = send (ForkOp fid ForkOpMergeBack)",
                "discardFork :: Member Spawn effs => SpawnId -> Eff effs ForkOpResult\ndiscardFork fid = send (ForkOp fid ForkOpDiscard)",
                "promoteFork :: Member Spawn effs => SpawnId -> PersonaConfig -> Eff effs ForkOpResult\npromoteFork fid cfg = send (ForkOp fid (ForkOpPromote cfg))",
            ]),
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

        // Effect-class runtime guard. Ephemeral/Fork/Sibling/Stop/ForkOp are
        // Coordinate/Skip; AwaitSpawn/AwaitAll are Observe/Enforce.
        let constructor_name = match &req {
            SpawnReq::Ephemeral(_) => "Ephemeral",
            SpawnReq::AwaitSpawn(_) => "AwaitSpawn",
            SpawnReq::AwaitAll(_) => "AwaitAll",
            SpawnReq::Stop(_) => "Stop",
            SpawnReq::Fork(_) => "Fork",
            SpawnReq::Sibling(_) => "Sibling",
            SpawnReq::ForkOp(_, _) => "ForkOp",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Spawn",
            constructor_name,
        )?;

        match req {
            SpawnReq::Ephemeral(wire_cfg) => handle_ephemeral(wire_cfg, cx),
            SpawnReq::AwaitSpawn(id) => handle_await_spawn(id, cx),
            SpawnReq::AwaitAll(ids) => handle_await_all(ids, cx),
            SpawnReq::Stop(id) => handle_stop(id, cx),
            SpawnReq::Fork(wire_cfg) => handle_fork(wire_cfg, cx),
            SpawnReq::Sibling(wire_cfg) => handle_sibling(wire_cfg, cx),
            SpawnReq::ForkOp(id, op) => handle_fork_op(id, op, cx),
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

    // Mint the child id + progress-log label first, so they can flow
    // into `fork_for_ephemeral` and tag the child's turn_sink with
    // `SpawnSource::Ephemeral { spawn_id, progress_log_label }` (when the
    // parent has a `SpawnSinkFactory` installed — daemon-driven sessions
    // do, headless/test sessions don't).
    let child_id: SmolStr = new_id();
    let progress_log_label: SmolStr = format!("spawn-log-{child_id}").into();

    // Build child include paths + child SessionContext via the parent's
    // fork helper (capability set + costume override applied there).
    let child_includes = child_include_paths(parent, lib_dir.as_ref());
    let child_ctx = parent.fork_for_ephemeral(
        &cfg,
        child_caps,
        Arc::new(child_includes.clone()),
        child_id.clone(),
        progress_log_label.clone(),
    );

    // Create the constellation-scoped progress-log block synchronously
    // before the runner is spawned. The parent gets the label back as
    // part of EphemeralSpawn and may read the block immediately.
    let progress_scope = crate::spawn::progress_log_scope(&child_ctx);
    crate::spawn::create_progress_log_block(
        child_ctx.adapter(),
        progress_log_label.as_str(),
        &progress_scope,
    )
    .map_err(|e| EffectError::Handler(e.to_string()))?;

    // Build the child's preamble. build_for uses the full canonical
    // effect set for imports + type M (tag alignment with the handler
    // HList) and the child's filtered capabilities for the API docs
    // (so the LLM only sees effects it's allowed to use).
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

    cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
        pattern_core::hooks::tags::SPAWN_EPHEMERAL_START,
        serde_json::json!({ "spawn_id": child_id.to_string() }),
    ));
    let wire = WireEphemeralSpawn {
        spawn_id: child_id.into(),
        progress_log_label: progress_log_label.into(),
    };
    cx.respond(wire)
}

/// Await a single in-flight ephemeral by id.
///
/// Exposed `pub` so integration tests can drive the `block_on` path from a
/// `tokio::task::spawn_blocking` context without the full Haskell eval path
/// (Critical review item C#3). Matches the visibility pattern of
/// `sdk/handlers/tasks.rs` and `sdk/handlers/skills.rs`.
pub fn handle_await_spawn(
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

/// Resolve a fork via one of the three operations: `MergeBack`, `Discard`,
/// or `Promote`.
///
/// - `MergeBack` is non-consuming: the handle stays in the registry so the
///   caller may continue operating on it (e.g. merge again, then discard).
///   Internally it calls `ForkHandle::merge_back_lightweight` or
///   `ForkHandle::merge_back_persistent` depending on isolation mode.
///
/// - `Discard` consumes the handle. The outer `Option` on `registry.remove`
///   is "is the id known?"; the inner `Option` is "could we take ownership?"
///   (it is `None` when another call still holds the `Arc<Mutex<ForkHandle>>`
///   from a `get` call). We surface both cases as distinct errors rather than
///   silently succeeding.
///
/// - `Promote` also consumes the handle and requires `SpawnNewIdentities` on
///   the spawner's capability snapshot. It delegates to
///   `ForkHandle::promote(cfg, drafts_dir)`.
fn handle_fork_op(
    id: String,
    op: WireForkOpKind,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let registry = cx.user().fork_registry().clone();
    let id: SmolStr = id.into();

    match op {
        WireForkOpKind::MergeBack => {
            // Non-consuming path — `get` borrows the handle via the registry's
            // Arc<Mutex>. The lock is dropped at end of this block.
            let arc = registry
                .get(&id)
                .ok_or_else(|| EffectError::Handler(format!("fork not found: {id}")))?;
            let handle = arc.lock();
            let report = match &handle.isolation_state {
                ForkIsolationState::Lightweight { .. } => handle.merge_back_lightweight(),
                ForkIsolationState::Persistent { .. } => handle.merge_back_persistent(),
                ForkIsolationState::Resolved => {
                    return Err(EffectError::Handler(format!("fork already resolved: {id}")));
                }
            }
            .map_err(|e| EffectError::Handler(e.to_string()))?;
            cx.respond(WireForkOpResult::MergeReport(format!("{:?}", report)))
        }

        WireForkOpKind::Discard => {
            // Consuming path — `remove` takes ownership of the inner ForkHandle.
            // Outer None: fork id not known.
            // Inner None: Arc is outstanding from a concurrent `get` call.
            let handle = match registry.remove(&id) {
                None => {
                    return Err(EffectError::Handler(format!("fork not found: {id}")));
                }
                Some(None) => {
                    return Err(EffectError::Handler(format!(
                        "fork in use; cannot discard now: {id}"
                    )));
                }
                Some(Some(h)) => h,
            };
            handle
                .discard()
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            cx.respond(WireForkOpResult::Unit)
        }

        WireForkOpKind::Promote(persona_cfg) => {
            // Consuming path — same remove-pattern as Discard.
            let handle = match registry.remove(&id) {
                None => {
                    return Err(EffectError::Handler(format!("fork not found: {id}")));
                }
                Some(None) => {
                    return Err(EffectError::Handler(format!(
                        "fork in use; cannot promote now: {id}"
                    )));
                }
                Some(Some(h)) => h,
            };
            let cfg: pattern_core::spawn::PersonaConfig = persona_cfg.into();
            let drafts_dir = cx.user().drafts_dir().to_owned();
            let pid = handle
                .promote(cfg, &drafts_dir)
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            cx.respond(WireForkOpResult::PersonaId(pid.to_string()))
        }
    }
}

fn handle_fork(
    wire_cfg: crate::sdk::requests::spawn::WireForkConfig,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let cfg: pattern_core::spawn::ForkConfig = wire_cfg.into();
    let parent: &SessionContext = cx.user();

    // Gate: validate the requested capability set against the parent's before
    // doing any work. `compute_child_caps` takes `EphemeralConfig`; build a
    // minimal one carrying the fork's program and capabilities directly.
    let mut cap_check_cfg = pattern_core::spawn::EphemeralConfig::new(&cfg.program);
    if let Some(caps) = cfg.capabilities.clone() {
        cap_check_cfg = cap_check_cfg.with_capabilities(caps);
    }
    compute_child_caps(parent, &cap_check_cfg).map_err(|e| EffectError::Handler(e.to_string()))?;

    let fork_id: smol_str::SmolStr = pattern_core::types::ids::new_id();
    let child_id: smol_str::SmolStr = pattern_core::types::ids::new_id();
    // Use the encoded scope key so fork_for_child can match against blocks
    // stored with `scope.to_db_key()` (e.g. "global:<id>" or "local:<id>").
    let parent_scope_key: smol_str::SmolStr = Scope::global(parent.agent_id()).to_db_key().into();
    let child_scope_key: smol_str::SmolStr = Scope::global(child_id.as_str()).to_db_key().into();

    // Allocate a fresh cancel state for the child. The child's cancel state
    // is NOT the parent's — calling `discard()` (which fires
    // `child_cancel.request_cancel()`) must not cancel the parent session.
    //
    // Parent→child cancellation is wired via a background watcher task
    // (below) that parks on the parent's `wait_for_cancel()` and propagates
    // it to the child. The watcher holds only a `Weak<CancelState>` for the
    // child so that a cleanly-resolved fork (discard/merge/promote) does not
    // prevent the child's state from being dropped — the `Weak` upgrade will
    // return `None` and the watcher exits without firing.
    let child_cancel = Arc::new(crate::timeout::CancelState::new());
    let spawner_caps = parent
        .capabilities()
        .cloned()
        .unwrap_or_else(pattern_core::CapabilitySet::all);

    // Parent→child cancel-propagation watcher. Mirrors the ephemeral pattern
    // in `session.rs::fork_for_ephemeral`, but propagates to a CancelState
    // directly rather than to a SpawnRegistry (forks do not have a registry
    // of their own child sessions at this layer).
    let parent_cancel_arc = parent.cancel_state();
    let child_cancel_weak = Arc::downgrade(&child_cancel);
    let watcher = parent.tokio_handle().spawn(async move {
        parent_cancel_arc.wait_for_cancel().await;
        if let Some(child) = child_cancel_weak.upgrade() {
            child.request_cancel();
        }
        // If `upgrade` returned None, the child cancel state was already
        // dropped (fork resolved cleanly). Nothing to do; closure exits.
    });

    let handle = match cfg.isolation {
        pattern_core::spawn::ForkIsolation::Lightweight => {
            // `memory_cache` must be wired on the session. A missing cache
            // would silently make `merge_back` a no-op, causing data loss when
            // the fork's writes are never propagated to the parent. Returning
            // an error here makes the misconfiguration visible at fork time
            // rather than at a silent merge-back that discards all changes.
            let parent_cache = parent.memory_cache().cloned().ok_or_else(|| {
                EffectError::Handler(
                    "lightweight fork requires a memory cache wired on the session; \
                         call with_memory_cache() before opening a session that forks"
                        .to_string(),
                )
            })?;
            let forked = parent_cache
                .fork_for_child(parent_scope_key.as_str(), child_scope_key.as_str())
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            let (child_cache, parent_weak) = (Arc::new(forked), Arc::downgrade(&parent_cache));
            crate::spawn::ForkHandle::new_lightweight(
                fork_id.clone(),
                child_id.clone(),
                child_cache,
                parent_scope_key.clone(),
                parent_weak,
                child_cancel,
            )
            .with_spawner_capabilities(spawner_caps)
            .with_cancel_watcher(watcher)
            .with_cfg(cfg.clone())
        }
        pattern_core::spawn::ForkIsolation::Persistent => handle_fork_persistent(
            parent,
            fork_id.clone(),
            child_id.clone(),
            parent_scope_key.clone(),
            child_scope_key.clone(),
            child_cancel,
            spawner_caps,
            cfg.task_ref.as_ref(),
            cfg.clone(),
        )
        .map_err(|e| EffectError::Handler(e.to_string()))?
        .with_cancel_watcher(watcher),
    };

    // Insert into the per-session ForkRegistry so subsequent ForkOps
    // (`MergeBack`, `Discard`, `Promote`) can address the fork by id.
    parent
        .fork_registry()
        .insert(fork_id.clone(), handle)
        .map_err(|e| EffectError::Handler(e.to_string()))?;

    cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
        pattern_core::hooks::tags::SPAWN_FORK,
        serde_json::json!({ "fork_id": fork_id.to_string(), "child_id": child_id.to_string() }),
    ));
    let wire = WireForkHandle {
        fork_id: fork_id.to_string(),
        child_id: child_id.to_string(),
    };
    cx.respond(wire)
}

/// Build a persistent `ForkHandle`. Sequence:
/// 1. Verify mount + memory_cache are wired and jj is available.
/// 2. Compute the bookmark name and resolve the workspace path.
/// 3. Pre-check for bookmark collision.
/// 4. Run `workspace_add` + `bookmark_set`. Cleanup on failure.
/// 5. Fork the parent's memory cache. Cleanup on failure.
/// `parent_scope_key` and `child_scope_key` are the encoded scope keys
/// (`"global:<id>"` / `"local:<id>"`) used by `fork_for_child` to match
/// blocks stored with `scope.to_db_key()`.
fn handle_fork_persistent(
    parent: &SessionContext,
    fork_id: SmolStr,
    child_id: SmolStr,
    parent_scope_key: SmolStr,
    child_scope_key: SmolStr,
    cancel_state: Arc<crate::timeout::CancelState>,
    spawner_caps: pattern_core::CapabilitySet,
    task_ref: Option<&pattern_core::BlockRef>,
    cfg: pattern_core::ForkConfig,
) -> Result<crate::spawn::ForkHandle, crate::spawn::fork::ForkError> {
    use crate::spawn::fork::ForkError;
    use pattern_memory::jj::JjAdapter;
    use pattern_memory::jj::fork_bookmark::fork_bookmark_name;

    let mount = parent
        .mount_info()
        .ok_or_else(|| ForkError::PersistentNotAvailable {
            mode: "session has no mount info wired".into(),
        })?;
    if !mount.mode.requires_jj() && !mount.jj_enabled {
        return Err(ForkError::PersistentNotAvailable {
            mode: format!("{:?} mode without jj enabled", mount.mode),
        });
    }
    let parent_cache =
        parent
            .memory_cache()
            .cloned()
            .ok_or_else(|| ForkError::PersistentNotAvailable {
                mode: "session has no memory_cache wired".into(),
            })?;

    let adapter = JjAdapter::detect()
        .map_err(|e| ForkError::JjOp {
            message: e.to_string(),
        })?
        .ok_or(ForkError::JjUnavailable)?;

    // Bookmark names use the raw agent id (not the encoded scope key) for
    // human-readable jj bookmarks.
    let raw_parent_agent_id = parent.agent_id();
    let bookmark_name = fork_bookmark_name(raw_parent_agent_id, task_ref);

    // Pre-check for bookmark conflicts before mutating the workspace.
    let bookmarks = adapter
        .bookmark_list(&mount.repo_root)
        .map_err(|e| ForkError::JjOp {
            message: format!("bookmark_list: {e}"),
        })?;
    if bookmarks.iter().any(|b| b.name == bookmark_name) {
        return Err(ForkError::BookmarkConflict {
            name: bookmark_name,
        });
    }

    // Resolve workspace path. Mode-dependent; conservative default
    // places fork workspaces under `<workspace_root>/workspaces/<bookmark>`.
    // Bookmark names contain `/`, so flatten for filesystem use.
    let safe_dir = bookmark_name.replace('/', "__");
    let workspace_path = mount.workspace_root.join("workspaces").join(&safe_dir);
    if let Some(parent_dir) = workspace_path.parent() {
        std::fs::create_dir_all(parent_dir).map_err(|e| ForkError::JjOp {
            message: format!("create_dir_all({parent_dir:?}): {e}"),
        })?;
    }

    adapter
        .workspace_add(&mount.repo_root, &workspace_path)
        .map_err(|e| ForkError::JjOp {
            message: format!("workspace_add: {e}"),
        })?;

    if let Err(e) = adapter.bookmark_set(&mount.repo_root, &bookmark_name, "@") {
        // Cleanup: rollback the workspace_add.
        let workspace_name = workspace_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let _ = adapter.workspace_forget(&mount.repo_root, workspace_name);
        return Err(ForkError::JjOp {
            message: format!("bookmark_set: {e}"),
        });
    }

    // Fork the parent's memory cache. Cleanup workspace + bookmark on
    // failure so the session doesn't leak persistent state.
    let child_cache =
        match parent_cache.fork_for_child(parent_scope_key.as_str(), child_scope_key.as_str()) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                let workspace_name = workspace_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let _ = adapter.workspace_forget(&mount.repo_root, workspace_name);
                let _ = adapter.bookmark_delete(&mount.repo_root, &bookmark_name);
                return Err(ForkError::MemoryStore(e.to_string()));
            }
        };

    Ok(crate::spawn::ForkHandle::new_persistent(
        fork_id,
        child_id,
        workspace_path,
        bookmark_name,
        mount.repo_root.clone(),
        child_cache,
        parent_scope_key,
        Arc::downgrade(&parent_cache),
        cancel_state,
    )
    .with_spawner_capabilities(spawner_caps)
    .with_cfg(cfg))
}

fn handle_sibling(
    wire_cfg: crate::sdk::requests::spawn::WireSiblingConfig,
    cx: &EffectContext<'_, SessionContext>,
) -> Result<Value, EffectError> {
    let cfg: pattern_core::spawn::SiblingConfig = wire_cfg.into();
    let parent: &SessionContext = cx.user();
    let handle = parent.tokio_handle().clone();

    let outcome: WireSiblingSpawn = match &cfg.persona {
        pattern_core::spawn::SiblingPersona::Existing(id) => {
            let resolver = parent.sibling_resolver().clone();
            let id_clone = id.clone();
            let cfg_clone = cfg.clone();
            let outcome: SiblingExistingOutcome = handle
                .block_on(spawn_sibling_existing(
                    parent, &cfg_clone, &id_clone, resolver,
                ))
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            // Existing-persona adoption is always ExistingActive — the
            // persona is already a registered identity, no draft involved.
            // `outcome.capabilities` carries the sibling's own caps (T8/
            // Phase 6 wiring for cap-restricted interactions).
            WireSiblingSpawn::ExistingActive(outcome.persona_id.to_string())
        }
        pattern_core::spawn::SiblingPersona::New(persona_cfg) => {
            let drafts_dir = parent.drafts_dir().to_owned();
            let cfg_clone = cfg.clone();
            let persona_cfg_clone = persona_cfg.clone();
            let new_outcome = handle
                .block_on(spawn_sibling_new(
                    parent,
                    &cfg_clone,
                    &persona_cfg_clone,
                    &drafts_dir,
                ))
                .map_err(|e| EffectError::Handler(e.to_string()))?;
            new_outcome.into()
        }
    };

    // Siblings are NOT added to the spawn registry — they live independently
    // of the parent session's lifetime.
    cx.user().hook_bridge().emit(pattern_core::hooks::HookEvent::notification(
        pattern_core::hooks::tags::SPAWN_SIBLING,
        serde_json::json!({ "kind": "sibling" }),
    ));
    cx.respond(outcome)
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
            model: None,
            name: None,
        }
    }

    fn empty_fork() -> WireForkConfig {
        WireForkConfig {
            program: String::new(),
            isolation: WireForkIsolation::Lightweight,
            capabilities: None,
            timeout_hint_ms: None,
            task_ref: None,
            model: None,
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
    fn effect_decl_advertises_seven_constructors_and_helpers() {
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
                "Stop",
                "ForkOp",
            ],
            "constructor list drift; update Pattern.Spawn.hs in lockstep"
        );
        assert!(
            !names.contains(&"Start"),
            "legacy `Start` constructor must be retired"
        );
        // Each constructor must have a corresponding helper.
        // ForkOp has three helpers (mergeBack, discardFork, promoteFork).
        for ctor in [
            "Ephemeral",
            "AwaitSpawn",
            "AwaitAll",
            "Fork",
            "Sibling",
            "Stop",
            "ForkOp",
        ] {
            assert!(
                decl.helpers.iter().any(|h| h.contains(ctor)),
                "no helper references constructor {ctor}"
            );
        }
        // Verify the three ForkOp-specific helpers exist and name the
        // right operations.
        let helper_text: Vec<&str> = decl.helpers.to_vec();
        let joined = helper_text.join("\n");
        assert!(
            joined.contains("mergeBack"),
            "mergeBack helper must be declared"
        );
        assert!(
            joined.contains("discardFork"),
            "discardFork helper must be declared"
        );
        assert!(
            joined.contains("promoteFork"),
            "promoteFork helper must be declared"
        );
        // Silence dead-code warnings on the test fixtures imported above
        // for use by the integration test file.
        let _ = empty_ephemeral();
        let _ = empty_fork();
        let _ = empty_sibling();
    }
}
