# v3-multi-agent Phase 2: Spawn primitives

**Goal:** replace the stub `SpawnHandler` with a real dispatcher that spawns three kinds of child sessions — ephemeral workers, forks (phase 3 fleshes out isolation modes), and sibling personas — threaded through an extended `SpawnReq` grammar with structured `EphemeralConfig / ForkConfig / SiblingConfig`. Parent sessions track child handles for lifetime management; a tokio semaphore enforces concurrency limits per parent; capability inheritance restricts children to a subset of the parent's caps.

**Architecture:** the `SpawnHandler` parses the Haskell-side request into one of three typed config variants. For ephemeral, it clones the parent's `SessionContext` (share the `Arc`s; rebuild per-session fields fresh), builds a new EvalWorker thread, runs the child's program to completion, and returns a result. For sibling, it loads a persona via the existing `persona_loader` and opens a fully independent session with its own `CapabilitySet` (from the sibling's persona config). For fork, Phase 2 delivers only the scaffolding + the lightweight path; Phase 3 adds persistent (jj workspace) isolation and merge/promote flows. Parent-child lifetime is enforced via a shared `Arc<ChildSessionRegistry>` on the parent's context; when the parent session's `CancelState` fires, children inherit that signal.

**Tech Stack:** Rust, `tokio::sync::Semaphore` (introduced in this phase — no existing precedent), existing `EvalWorker` (`std::thread::spawn` with 256 MiB stack), existing `CancelState` shared via `Arc`, existing `persona_loader` for sibling persona loading, `frunk` HList patterns from `sdk/bundle.rs`, proptest/insta for config serde.

**Scope:** 2 of 7. Delivers all ephemeral behaviour (AC3 in full) and sibling scaffolding (AC5 — identity authorization, config plumbing; the draft-state + registry interaction lands in Phase 6). Fork structural scaffolding is wired so Phase 3 can swap in isolation modes.

**Codebase verified:** 2026-04-23. Plan 2 may land a `Tasks` effect in parallel; that does not touch Spawn.

---

## Codebase verification findings

- ✓ `SpawnHandler` stub at `crates/pattern_runtime/src/sdk/handlers/spawn.rs` currently wraps `HandlerGuard` then returns `EffectError::Handler("not implemented in v3 foundation")`.
- ✓ Existing `SpawnReq` at `crates/pattern_runtime/src/sdk/requests/spawn.rs`:
  ```rust
  pub enum SpawnReq {
      #[core(module = "Pattern.Spawn", name = "Start")] Start(String),
      #[core(module = "Pattern.Spawn", name = "Stop")]  Stop(String),
  }
  ```
  The `Start(String)` variant is load-bearing in the Haskell preamble. **Changing the constructor surface is a breaking change on the Haskell side** — the `Pattern.Spawn` module in `crates/pattern_runtime/haskell/Pattern/Spawn.hs` ships with the crate, so we update both sides atomically.
- ✓ `SessionContext` structure at `crates/pattern_runtime/src/session.rs:40-121`. Per-session mutable fields: `cancel_state: Arc<CancelState>`, `pending_messages: Arc<Mutex<Vec<_>>>`, `checkpoint_log: Arc<Mutex<CheckpointLog>>`, `current_turn: Arc<AtomicU64>`, `adapter: Arc<MemoryStoreAdapter>`. Shared-read: `provider`, `db`, `router` (all `Arc`).
- ⚠ `include_paths` is currently a local variable in `TidepoolSession::open_with_agent_loop`, not on `SessionContext`. Child spawn needs it, so Phase 2 adds `include_paths: Arc<Vec<PathBuf>>` to the context.
- ✓ `EvalWorker::spawn_with_includes(ctx, include_paths, session_id)` at `crates/pattern_runtime/src/agent_loop/eval_worker.rs:137-165`. New worker = new 256 MiB OS thread — **cheap in CPU/memory terms for small fan-out, expensive at scale**. Semaphore limit keeps this sane.
- ✓ `CancelState` at `crates/pattern_runtime/src/timeout.rs:191-198`. Shared-via-`Arc` is the existing pattern.
- ✓ Persona loader at `crates/pattern_runtime/src/persona_loader.rs` is self-contained; `load_persona(&Path) -> Result<PersonaSnapshot, PersonaLoadError>` reusable.
- ✗ No `PersonaId` type alias. Only `AgentId: SmolStr` in `crates/pattern_core/src/types/ids.rs`. **Decision:** add `pub type PersonaId = SmolStr;` as a readability alias (documented as "same underlying type as AgentId; used in multi-agent code").
- ✓ `BlockRef` exists at `crates/pattern_core/src/types/block_ref.rs` with fields `label: String`, `block_id: String`, `agent_id: String`, plus constructors `new`, `with_owner`, `owned_by`. `ForkConfig.task_ref` can reference it directly — no placeholder needed.
- ✗ No `tokio::sync::Semaphore` usage anywhere. Phase 2 introduces the first use.
- ⚠ `LoroDoc` access is through `MemoryStoreAdapter::inner().get_block(...) -> StructuredDocument` which wraps `Arc<LoroDoc>`. For sibling spawn with its own memory root, we construct a fresh `MemoryCache` with a new `LoroDoc::new()`; for ephemeral, the child shares the parent's adapter (memory reads but no isolated scope, unless explicitly restricted); for fork (Phase 3), we `LoroDoc::fork()` and build a new adapter over the forked doc.
- ✓ `HasCancelState` trait exists and is used by every handler; Phase 1 introduces `HasPermissionBridge` alongside. Phase 2 extends the same user-trait pattern with `HasSpawnRegistry` so handlers can reach the child registry cheanly.

### Design decisions locked in

- **Spawn grammar.** `SpawnReq::Start(String)` is retired. Phase 2 ships six discrete constructors at the Haskell layer (`Ephemeral`, `AwaitSpawn`, `AwaitAll`, `Fork`, `Sibling`, `Stop`). `Ephemeral` returns `SpawnId` immediately (non-blocking — the child session runs in the background); `AwaitSpawn(SpawnId)` blocks until the ephemeral completes and returns its `SpawnResult`; `AwaitAll([SpawnId])` blocks until every id in the list completes, returning `Vec<Result<SpawnResult, SpawnError>>` in id-order (Rust-side ``futures::future::join_all`` — single sync-bridge round-trip, partial-failure-preserving so ensemble patterns can inspect per-id outcomes). `Fork` returns a structured `ForkHandle` that Phase 3 Task 8 gives resolution helpers. `Sibling` returns `PersonaId`. `Stop` returns unit. Both the Rust enum and the Haskell `Pattern.Spawn` module move atomically.
- **Parent→child cancel propagation.** Ephemeral and Fork share the parent's `Arc<CancelState>` (sub-lives tie to parent turn by design). Sibling gets a fresh `CancelState` (independent lifetime). Revisit only if Phase 4 mailbox work surfaces timing flakes.
- **Child-handle storage.** Dedicated `SpawnRegistry` type with its own `Drop` behaviour (abort all children), rather than inlining `Vec<ChildSessionHandle>` on `SessionContext`. Keeps the cancellation contract local to one type.

---

## Acceptance Criteria Coverage

### v3-multi-agent.AC3: Ephemeral spawn

- **v3-multi-agent.AC3.1 Success:** `ctx.spawn.ephemeral(config)` creates a new TidepoolSession with a separate EvalWorker thread; the ephemeral executes its program and returns a result to the parent
- **v3-multi-agent.AC3.2 Success:** Ephemeral's CapabilitySet is a subset of parent's; prelude filtering reflects the restricted set
- **v3-multi-agent.AC3.3 Success:** Ephemeral with costume has its system prompt override set to the costume's content; persona identity remains the parent's in logs
- **v3-multi-agent.AC3.4 Success:** Ephemeral timeout fires; session is cancelled; parent receives a timeout error, not a hang
- **v3-multi-agent.AC3.5 Success:** Concurrent ephemeral count respects the configured semaphore limit; attempt to exceed returns a clear error
- **v3-multi-agent.AC3.6 Failure:** Parent session resolves (completes or errors); all child ephemeral sessions are cancelled; no orphaned EvalWorker threads remain
- **v3-multi-agent.AC3.7 Edge:** Ephemeral spawning its own ephemeral (nested); grandchild dies when child dies, child dies when parent resolves — full lifetime chain

### v3-multi-agent.AC5 (partial — identity-auth portion)

- **v3-multi-agent.AC5.1 Success:** `ctx.spawn.sibling(SiblingConfig { persona: Existing(id), .. })` opens a session for the existing persona; no authorization required
- **v3-multi-agent.AC5.2 Success:** `ctx.spawn.sibling(SiblingConfig { persona: New(config), .. })` with `SpawnNewIdentities` capability creates the persona and opens its session
- **v3-multi-agent.AC5.3 Success:** `ctx.spawn.sibling(SiblingConfig { persona: New(config), .. })` without `SpawnNewIdentities` capability creates persona config as Draft; no session opened; returns the draft PersonaId
- **v3-multi-agent.AC5.4 Success:** Sibling session's CapabilitySet comes from its own persona config, not from the spawner's CapabilitySet
- **v3-multi-agent.AC5.6 Failure:** Sibling spawn referencing a nonexistent PersonaId returns `RegistryError::PersonaNotFound`

AC5.5 (auto-registration in agent registry) and AC5.7 (draft PersonaId visibility in constellation) are verified in Phase 6 when the registry schema lands. Phase 2 ships the code paths but the registry queries they call are stubs returning in-memory results; the draft state produced here is consumed by Phase 6's registration.

### v3-multi-agent.AC4.7, AC4.8 (partial — fork promote scaffold)

- **v3-multi-agent.AC4.7 Success:** `fork.promote(persona_config)` creates a new persona config, registers as Draft in the registry, inherits the fork's memory state — **scaffolding only in Phase 2; fork-to-sibling memory transfer verified in Phase 3.**
- **v3-multi-agent.AC4.8 Failure:** `fork.promote()` without `SpawnNewIdentities` capability returns `CapabilityError::Denied` — **gate wiring verified in Phase 2; end-to-end verification in Phase 3.**

---

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

<!-- START_TASK_1 -->
### Task 1: Define spawn-config types in `pattern_core`

**Verifies:** foundation for AC3.2, AC3.3, AC5.*.

**Files:**
- Create: `crates/pattern_core/src/spawn.rs`
- Modify: `crates/pattern_core/src/lib.rs` (re-export)
- Modify: `crates/pattern_core/src/types/ids.rs` — add `pub type PersonaId = SmolStr;` with a doc comment noting it's an alias for `AgentId`.

**Implementation:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EphemeralConfig {
    pub program: String,             // Haskell source to compile + run
    pub costume: Option<String>,     // system-prompt override; parent identity retained
    pub capabilities: Option<CapabilitySet>, // None = inherit parent's full set
    pub timeout: Option<jiff::Span>, // None = inherit parent/runtime default
    pub metadata: serde_json::Value, // caller-supplied tags for logs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ForkConfig {
    pub program: String,
    pub isolation: ForkIsolation,           // Lightweight | Persistent
    pub capabilities: Option<CapabilitySet>,
    pub timeout_hint: Option<jiff::Span>,
    pub task_ref: Option<BlockRef>,         // used for jj bookmark naming in Phase 3
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ForkIsolation {
    Lightweight,  // LoroDoc::fork(); no disk writes
    Persistent,   // jj workspace; Phase 3 wiring
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SiblingConfig {
    pub persona: SiblingPersona,
    pub relationship: RelationshipKind, // SupervisorOf | SpecialistFor | PeerWith | ObserverOf
    pub shared_blocks: Vec<String>,     // block labels the sibling can read from spawner
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SiblingPersona {
    Existing(PersonaId),
    New(PersonaConfig),               // simplified copy of PersonaSnapshot
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PersonaConfig {
    pub name: String,
    pub system_prompt: String,
    pub capabilities: CapabilitySet,
    // Further fields deferred to Phase 6 registry work
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RelationshipKind {
    SupervisorOf,
    SpecialistFor,
    PeerWith,
    ObserverOf,
}
```

`BlockRef` already lives at `crates/pattern_core/src/types/block_ref.rs`; import it directly.

**Testing:**
- Unit: serde round-trip for each config struct (plain `serde_json`).
- Unit: `RelationshipKind` display/FromStr round-trip (if we emit it in logs or KDL).
- Ensure `#[non_exhaustive]` everywhere so future fields don't force a major bump on downstream crates.

**Verification:**
`cargo nextest run -p pattern-core spawn`

**Commit:** `[pattern-core] add spawn-config types (Ephemeral, Fork, Sibling) + PersonaId alias`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Extend `SpawnReq` grammar for three spawn modes

**Verifies:** foundation for AC3.1 / AC5.1-3 / AC4.7 dispatch.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/requests/spawn.rs`
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — update `DescribeEffect::effect_decl` advertised constructors/helpers.
- Modify: `crates/pattern_runtime/haskell/Pattern/Spawn.hs` — update GADT constructors + helpers.
- Modify: `crates/pattern_runtime/src/sdk/code_tool.rs` or wherever the code-tool description is built from `canonical_effect_decls()` — ensure the regenerated description picks up the new helpers (should be automatic via `DescribeEffect`).

**Implementation:**

Replace existing `SpawnReq` with:

```rust
#[derive(Debug, FromCore)]
pub enum SpawnReq {
    #[core(module = "Pattern.Spawn", name = "Ephemeral")]
    Ephemeral(EphemeralConfig),
    /// Block until the given ephemeral completes; return its result. Separate
    /// from Ephemeral so delegation patterns can spawn many workers in parallel
    /// and await them.
    #[core(module = "Pattern.Spawn", name = "AwaitSpawn")]
    AwaitSpawn(SpawnId),
    /// Block until every id in the list completes; return per-id results in
    /// id-order (`Vec<Result<SpawnResult, SpawnError>>`). Handler uses
    /// `futures::future::join_all` (not `try_join_all`) — a single sync-bridge
    /// round-trip awaits N parallel children AND preserves per-id failures so
    /// ensemble / voting patterns (Pattern.Delegation.FanOut) can inspect
    /// partial outcomes.
    #[core(module = "Pattern.Spawn", name = "AwaitAll")]
    AwaitAll(Vec<SpawnId>),
    #[core(module = "Pattern.Spawn", name = "Fork")]
    Fork(ForkConfig),
    #[core(module = "Pattern.Spawn", name = "Sibling")]
    Sibling(SiblingConfig),
    #[core(module = "Pattern.Spawn", name = "Stop")]
    Stop(SpawnId),
}
```

The `FromCore` derive must decode each `*Config` struct directly. Pattern-match the shape used by other structured requests (check `MessageReq` or similar). If the derive can't carry a nested struct payload, implement `FromCore` by hand — do NOT fall back to JSON-over-string.

Update `effect_decl()` to advertise the new constructors + `ephemeral`/`fork`/`sibling`/`stop` helpers. Keep the description succinct (the code-tool description is user-facing for agents).

Match in `handle()` to each variant — all six variants currently return `EffectError::Handler("phase 2 task 3+ not yet wired")`. Actual dispatch lands in subsequent tasks.

**Testing:**
- Unit: `effect_decl().constructors` contains `"Ephemeral"`, `"AwaitSpawn"`, `"AwaitAll"`, `"Fork"`, `"Sibling"`, `"Stop"`. No residue of the old `"Start"` constructor.
- Unit: `canonical_effect_decls()` still parses under `parse_constructor` (the existing test at `bundle.rs:115`).
- **SdkBundle ordering note:** `Spawn` already occupies its canonical slot in the bundle HList (position 13, just before `Diagnostics`). Phase 2 Task 2 redesigns the Haskell-side grammar but does NOT re-position `Spawn` in the HList — the effect-tag numbering agent programs encode in their `Eff '[...]` rows MUST stay stable. Plan 2 (task-skill-blocks) is expected to have added `Tasks` to `CANONICAL_EFFECT_ROW` by Phase 2 execution time; if it did, confirm the ordering and slot alignment between `Pattern.Spawn` and `Pattern.Tasks` at kickoff. Do NOT reshuffle existing slots.
- Snapshot (insta): Haskell preamble contains the updated `Pattern.Spawn` imports/helpers list. Update or add a snapshot so the diff is obvious.

**Verification:**
`cargo nextest run -p pattern-runtime spawn`. `cargo test --doc`.

**Commit:** `[pattern-runtime] redesign Pattern.Spawn as Ephemeral|Fork|Sibling|Stop`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `SpawnRegistry` — child handle tracking with lifetime enforcement

**Verifies:** AC3.6, AC3.7.

**Files:**
- Create: `crates/pattern_runtime/src/spawn/registry.rs`
- Create: `crates/pattern_runtime/src/spawn/mod.rs` (new module).
- Modify: `crates/pattern_runtime/src/session.rs` — add `spawn_registry: Arc<SpawnRegistry>` field on `SessionContext`; a parent's registry is the child's parent pointer.
- Create `HasSpawnRegistry` trait alongside `HasCancelState` / `HasPermissionBridge` and implement on `SessionContext`.

**Implementation:**

```rust
pub struct SpawnRegistry {
    parent_id: SmolStr,
    children: Mutex<Vec<ChildSessionHandle>>,
    concurrent_ephemeral_limit: Arc<Semaphore>,
}

pub struct ChildSessionHandle {
    pub child_id: SmolStr,
    pub kind: SpawnKind,                 // Ephemeral | Fork | Sibling
    pub cancel_state: Arc<CancelState>,  // shared for ephemeral/fork; independent for sibling
    // The background task running the child session. Wrapped as Shared so
    // multiple awaiters (AwaitSpawn + follow-up AwaitAll including the same
    // id) can all observe the same result without panicking (tokio's
    // JoinHandle is single-consume; Shared gives Clone + multi-await).
    pub result: futures::future::Shared<
        futures::future::BoxFuture<'static, Result<SpawnResult, SpawnError>>
    >,
    // Semaphore permit held for the duration of ephemeral life; Some for Ephemeral only
    pub _permit: Option<OwnedSemaphorePermit>,
}
```

Methods:
- `SpawnRegistry::new(parent_id, limit: usize)` — constructs with `Semaphore::new(limit)`.
- `try_acquire_ephemeral_slot(&self) -> Option<OwnedSemaphorePermit>` — fails fast if full.
- `register(&self, handle: ChildSessionHandle)` — push under mutex.
- `cancel_all(&self)` — sets every child's `cancel_state.cancellation` atomic (signals `run_ephemeral` to short-circuit at its next poll point); drops permits (releasing semaphore slots); drops the `Shared<BoxFuture>` result caches. The underlying tokio task completes on its own once `run_ephemeral` observes the cancel signal; the `Shared` drop just forgets the cached outcome. Idempotent.
- `Drop` for `SpawnRegistry` calls `cancel_all()` — enforces AC3.6.

Ephemeral and Fork children share the parent's `Arc<CancelState>`; Sibling children get their own `CancelState` and are NOT added to the parent's registry (they outlive the parent).

**Testing:**
- Unit: create a `SpawnRegistry` with limit=2; acquire three permits; third returns None with `TryAcquireError::NoPermits` — convert to `SpawnError::ConcurrencyLimitExceeded` with a helpful message (AC3.5).
- Unit: call `cancel_all()` — children's `CancelState::cancellation` flips to true.
- Unit: drop registry — ditto (AC3.6).
- Unit: nested registry (child's registry has its own limit) — grandchild cancellation propagates when parent cancels (AC3.7). Use scripted handles (no real EvalWorker) to keep the test fast + deterministic.

**Verification:**
`cargo nextest run -p pattern-runtime spawn::registry`

**Commit:** `[pattern-runtime] introduce SpawnRegistry with semaphore + cancel-on-drop`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 4-5) -->

<!-- START_TASK_4 -->
### Task 4: Ephemeral dispatch — non-blocking spawn returning `SpawnId`, plus `AwaitSpawn`

**Verifies:** AC3.1, AC3.2, AC3.3, AC3.4.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — real `Ephemeral(cfg)` handling.
- Modify: `crates/pattern_runtime/src/session.rs` — `SessionContext::fork_for_ephemeral(&self, cfg: &EphemeralConfig) -> Arc<SessionContext>` method that clones Arc-shared fields and rebuilds per-session state fresh (new `CancelState`? No — **share** parent's `Arc<CancelState>` so parent cancel propagates; rebuild `pending_messages`, `checkpoint_log`, `current_turn`, and `spawn_registry` sub-registry).
- Modify: `crates/pattern_runtime/src/session.rs` — persist `include_paths: Arc<Vec<PathBuf>>` on `SessionContext` (currently a local in `open_with_agent_loop`). Populate at session open from the existing construction path.

**Implementation:**

```rust
// In spawn.rs handler
SpawnReq::Ephemeral(cfg) => {
    let parent = cx.user();
    let registry = parent.spawn_registry();
    let permit = registry.try_acquire_ephemeral_slot()
        .ok_or_else(|| EffectError::Handler(
            "concurrent ephemeral limit reached for parent session".into()))?;

    // Restrict child capabilities (must be subset of parent).
    let parent_caps = parent.capabilities();
    let child_caps = match &cfg.capabilities {
        Some(set) => set.clone().restrict_to(&parent_caps)
            .map_err(|e| EffectError::Handler(format!("capability escalation: {e}")))?,
        None => parent_caps.clone(),
    };

    let child_ctx = parent.fork_for_ephemeral(cfg, child_caps)?;
    let child_id = child_ctx.session_id().clone();
    // Spawn the child session asynchronously; do NOT block the handler.
    let join_handle = tokio::spawn(run_ephemeral(child_ctx.clone(), cfg.clone()));

    // Adapt the JoinHandle's Result<Result<StepReply, SpawnError>, JoinError>
    // into the Spawn-effect-level Result<SpawnResult, SpawnError>, then wrap as
    // Shared<BoxFuture> so multiple awaiters (AwaitSpawn / AwaitAll / repeated
    // AwaitSpawn on the same id) can poll idempotently.
    let result = async move {
        match join_handle.await {
            Ok(Ok(step_reply)) => Ok(SpawnResult::from_step_reply(step_reply)),
            Ok(Err(spawn_err)) => Err(spawn_err),
            Err(join_err) => Err(SpawnError::JoinPanicked(join_err.to_string())),
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

    // Return the SpawnId immediately. Caller uses AwaitSpawn(id) to block
    // for the result (or lets the registry drop handle the child if it
    // doesn't care about the output — fire-and-forget is supported).
    Ok(Value::spawn_id(child_id))
}

SpawnReq::AwaitSpawn(id) => {
    let registry = cx.user().spawn_registry();
    match registry.wait_for(id).await {
        Ok(reply) => Ok(Value::from_spawn_result(&reply)),
        Err(e) => Err(EffectError::Handler(e.to_string())),
    }
}

SpawnReq::AwaitAll(ids) => {
    let registry = cx.user().spawn_registry();
    // Genuinely parallel await — join_all polls every future concurrently.
    // Use join_all (not try_join_all) so ensemble / voting patterns like
    // Pattern.Delegation.FanOut get per-id results even when some children
    // fail. Caller sees Vec<Result<SpawnResult, SpawnError>> and decides
    // how to handle partial failure.
    let futures = ids.into_iter().map(|id| registry.wait_for(id));
    let replies: Vec<Result<SpawnResult, SpawnError>> =
        futures::future::join_all(futures).await;
    Ok(Value::spawn_result_list(&replies))
}
```

Add `SpawnRegistry::wait_for(id: SpawnId) -> Result<SpawnResult, SpawnError>`: looks up the `ChildSessionHandle` by id, clones its `Shared<Future>`, awaits it. The `Shared` wrapper means every call on the same id observes the same result (idempotent); multi-await is safe (unlike raw `JoinHandle`, which panics on second await). The wrapping future inside `Shared` is built at `register` time: it awaits the real `JoinHandle`, maps `Result<StepReply, SpawnError>` → `Result<SpawnResult, SpawnError>` (the type adapter from the session-level result to the spawn-effect-level payload), and caches the outcome. The handle stays in the registry until parent resolution drops everything — this way `Stop(id)` and subsequent `AwaitSpawn(id)` calls remain valid across the child's lifetime.

`run_ephemeral` constructs the EvalWorker (via existing `EvalWorker::spawn_with_includes`), compiles `cfg.program` against the filtered preamble (Phase 1's `preamble::build_for(&caps)`), executes, returns `StepReply` or error. Timeout is wrapped via `tokio::time::timeout(cfg.timeout.unwrap_or(runtime_default), …)`. On timeout, the child's `cancel_state` is tripped (for AC3.4), the EvalWorker thread is asked to stop via its channel, and the cached result becomes `Err(SpawnError::Timeout)` — observable via the next `AwaitSpawn`.

**Why non-blocking:** delegation patterns like FanOut must spawn every worker in parallel and await them as a batch. A blocking `Ephemeral` serializes the parallelism. Phase 7's `Pattern.Delegation.FanOut` uses `traverse Spawn.ephemeral workers >>= Spawn.awaitAll` — one sync-bridge round-trip for N parallel awaits. `awaitSpawn` (single-id) exists for patterns like Pipeline where stages are sequential by design; FanOut and RoundRobin use `awaitAll`.

Costume: `child_ctx` overrides the system-prompt slot with `cfg.costume` when set. The persona's identity in logs stays as the parent's. This is consistent with the design: "attributed to parent in logs."

**Testing:**
- Integration (use `pattern_runtime::testing::MockProviderClient` — already exists at `crates/pattern_runtime/src/testing.rs:110`; script a response for "spawn ephemeral" probes via `MockProviderClient::with_turns(...)`): 
  - AC3.1: parent spawns an ephemeral whose program is `pure (T.pack "ok")`; parent receives `"ok"`.
  - AC3.2: parent has CapabilitySet `[Memory, Spawn]`; ephemeral config asks for `[Memory, Spawn, Shell]` → handler returns `SpawnError::CapabilityEscalation`.
  - AC3.3: ephemeral with costume "be terse"; assert the child's compiled prompt contains "be terse" and the log line attributes to the parent's persona id.
  - AC3.4: ephemeral with `timeout = jiff::Span::new().seconds(1)` and program that loops forever (`_ <- loopForever`); parent gets `SpawnError::Timeout` within ~1.5s; no EvalWorker thread leaks (best-effort: spawn-then-collect test asserts at end).
  - AC3.5: sequential 3 ephemerals on a registry with limit=2; third fails with `ConcurrencyLimitExceeded`.

- Integration: use deterministic programs — no live model.

**Verification:**
`cargo nextest run -p pattern-runtime ephemeral_spawn`

**Commit:** `[pattern-runtime] implement ephemeral spawn dispatch with timeout + semaphore`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Sub-spawn lifetime chain

**Verifies:** AC3.6, AC3.7.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/registry.rs` — ensure child `SpawnRegistry` instances point back to parent for cascade.
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — when the child handler spawns its own ephemeral, the grandchild registers with the child's registry, whose `cancel_on_parent_signal` is tied to the parent's CancelState.

**Implementation:**
Each child's `SpawnRegistry` carries a weak reference to the grandparent's cancel watcher (or inherits the parent's `Arc<CancelState>`). When the parent's `CancelState::cancellation` flips, a tokio task subscribed to it calls `child_registry.cancel_all()`. The subscription is fire-and-forget — nothing else needs to hold the watcher alive.

Concretely: when `fork_for_ephemeral` builds a child context, it spawns:

```rust
tokio::spawn({
    let parent_cancel = parent.cancel_state().clone();
    let child_registry = child_ctx.spawn_registry().clone();
    async move {
        parent_cancel.wait_for_cancel().await; // add this helper if not present
        child_registry.cancel_all();
    }
});
```

`wait_for_cancel` is a convenience over the existing atomic; it polls or hooks into whatever notification mechanism already exists (`tokio::sync::Notify` is the likely fit; verify at implementation time).

**Testing:**
- Integration: 3-level chain. Parent spawns ephemeral-child, ephemeral-child spawns grandchild. Parent's cancel → both child and grandchild observe `cancel_state.cancellation=true` within 100ms.
- Integration: parent completes normally (no cancel) → child and grandchild complete normally.
- **EvalWorker orphan check (AC3.6):** `EvalWorker` spawns an OS thread via `std::thread::spawn` with a 256 MiB stack — leaks are expensive. Add an atomic counter `static LIVE_EVAL_WORKERS: AtomicUsize` at `agent_loop/eval_worker.rs`, incremented in `spawn`/`spawn_with_includes` and decremented on worker-thread exit (drop-guard at the worker's thread-local). At test start: record `initial = LIVE_EVAL_WORKERS.load()`. At test end (after parent resolves): assert `LIVE_EVAL_WORKERS.load() == initial` within a 500ms grace window. This is a deterministic leak test, not best-effort.

**Verification:**
`cargo nextest run -p pattern-runtime ephemeral_chain`

**Commit:** `[pattern-runtime] propagate parent cancel through child spawn registries`
<!-- END_TASK_5 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 6-7) -->

<!-- START_TASK_6 -->
### Task 6: Sibling dispatch — existing-persona adoption

**Verifies:** AC5.1, AC5.4, AC5.6.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — `Sibling(cfg)` arm handling `SiblingPersona::Existing(id)`.
- Create: `crates/pattern_runtime/src/spawn/sibling.rs` — helper to open a session for an existing persona.
- Modify: `crates/pattern_runtime/src/session.rs` — `open_sibling_session(persona: PersonaSnapshot, relationship: RelationshipKind) -> Result<TidepoolSession, SpawnError>`.

**Implementation:**
Adopt-existing flow:

1. Resolve `persona_id` via persona loader (find the persona config path — Phase 6 will have the registry lookup; Phase 2 accepts a direct path or uses a lookup stub that errors `PersonaNotFound` if not found).
2. Call `load_persona(path)` → `PersonaSnapshot`.
3. Open a fully independent `TidepoolSession` with the sibling's own `CapabilitySet` (from `PersonaSnapshot.capabilities` — field added by Phase 1 Task 13).
4. DO NOT add the sibling to the parent's `SpawnRegistry` — siblings live independently of parent lifetime.
5. Return the sibling's `PersonaId` to the Haskell caller.

The sibling's `SessionContext` gets a fresh `CancelState`, fresh `pending_messages`, fresh `checkpoint_log`, fresh `SpawnRegistry`, a fresh `adapter` over a fresh `MemoryCache` (sibling has own memory root per design).

**Testing:**
- Integration: parent spawns a sibling pointing at a persona fixture KDL in `crates/pattern_runtime/tests/fixtures/sibling_persona.kdl`. Assert a new session with `persona_id == fixture.name` exists and runs a trivial program.
- AC5.4: the fixture restricts capabilities to `[Memory]`; parent has `[Memory, Shell]`; sibling program calling `Shell.execute` fails at compile — the sibling's caps come from its own config, NOT the parent's.
- AC5.6: unknown persona id → `SpawnError::PersonaNotFound(RegistryError::PersonaNotFound(id))`. `SpawnError` wraps `RegistryError` as a dedicated variant so the design's `RegistryError::PersonaNotFound` propagation (AC5.6 in the design) is preserved while the spawn call site gets a domain-local error type. Use a registry stub that returns `Err(RegistryError::PersonaNotFound(id))` for unknown ids.

**Verification:**
`cargo nextest run -p pattern-runtime sibling_spawn`

**Commit:** `[pattern-runtime] implement sibling spawn for existing personas`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Sibling dispatch — new-identity draft flow

**Verifies:** AC5.2, AC5.3.

**Files:**
- Modify: `crates/pattern_runtime/src/spawn/sibling.rs` — `SiblingPersona::New(cfg)` arm.
- Create: `crates/pattern_runtime/src/spawn/draft.rs` — writes a persona KDL to disk (at a well-known drafts location, e.g. `<mount>/drafts/<persona_id>.kdl`) and records a `DraftPersona { id, config_path, created_at }` via a small interface that Phase 6 replaces with the real registry.
- No structural changes to `CapabilitySet` or `CapabilityFlag` — both types land in Phase 1 Task 1 with `SpawnNewIdentities` reserved. Phase 2 Task 7 only *reads* the flag at runtime via `parent.capabilities().has_flag(CapabilityFlag::SpawnNewIdentities)`.

**Implementation:**
When `cfg.persona == New(persona_config)`:

- If `parent.capabilities().has_flag(CapabilityFlag::SpawnNewIdentities)`: create the persona config on disk, register in the runtime-visible draft table (stub in Phase 2, real in Phase 6), **open the session** just like the existing-persona path. Return the new `PersonaId`. (AC5.2.)
- If NOT: still create the KDL on disk, register as draft, but **do not open a session**. Return the draft `PersonaId`. A later human-driven promote (Phase 6) opens it. (AC5.3.)

The draft file is written by the runtime itself (not through the File effect), using `std::fs::write` into a runtime-owned drafts directory. This is the correct trust boundary: the File handler's shape-based gate (Phase 1 Task 15) exists to prevent **agent programs** from mutating pattern-config KDL; runtime-internal writes are authorised by the runtime's own code paths (the `spawn.sibling` handler ran, not the Haskell agent directly writing to disk), so the gate does not apply.

For audit-trail parity, the draft write goes through a small `RuntimeConfigWriter` helper that:
1. Resolves the drafts directory relative to the mount.
2. Logs the write at `info` with the persona id and a `source = "runtime.spawn.sibling"` tag.
3. Calls `std::fs::write`.

This gives the same observability the Phase 1 shape gate provides for agent writes, without conflating runtime-authorised writes with agent-driven ones. Tests: assert the log line appears; assert the file lands at the expected path; assert no `PermissionRequest` is broadcast through the broker (it's a runtime-internal operation).

**Testing:**
- AC5.2: parent has `SpawnNewIdentities`; spawn sibling with `New(cfg)`. Draft file written, registry entry created, session opened, new `PersonaId` returned, session steps at least once successfully.
- AC5.3: same but parent lacks the flag. Draft file written, registry entry with `status = Draft`, no session opened, returned `PersonaId` appears in the runtime-local drafts list but `session_manager.get(&id).is_none()`.
- Unit: scope restrictions — `CapabilitySet` flag serde round-trip.

**Verification:**
`cargo nextest run -p pattern-runtime sibling_new_identity`

**Commit:** `[pattern-runtime] implement sibling new-identity draft flow with capability flag gate`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 8-9) -->

<!-- START_TASK_8 -->
### Task 8: Fork dispatch — lightweight path (scaffolding; full semantics in Phase 3)

**Verifies:** AC4.7 scaffold, AC4.8 gate wiring.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — `Fork(cfg)` arm.
- Create: `crates/pattern_runtime/src/spawn/fork.rs` — type `ForkHandle` with `await_result()`, `merge_back()` (stub), `discard()` (stub), `promote(cfg)` (stub).

**Implementation:**
For `ForkIsolation::Lightweight`:
- Accept the fork config.
- Call `LoroDoc::fork()` on the parent's memory doc (accessor added in Task 4 helper).
- Build a child `SessionContext` with the forked doc wrapped in a new `MemoryCache` → new `MemoryStoreAdapter`.
- Spawn the child program like an ephemeral.
- Return a `ForkHandle` with stored child-session info.

For `ForkIsolation::Persistent`: return `EffectError::Handler("persistent fork isolation lands in Phase 3")`. Do not attempt jj workspace creation here.

`ForkHandle.promote(persona_config)`: verify `SpawnNewIdentities` flag on the forker's CapabilitySet; if absent, return `CapabilityError::Denied`. Actual persona-config construction + registry entry is identical to Task 7. The fork's memory state is handed off to the new persona — in Phase 2 we structurally wire this (accept a `promote` call, verify the gate) but the actual memory-state transfer is a Phase 3 problem (jj merge + loro import).

**Testing:**
- Integration: lightweight fork with a trivial program; `fork.await_result()` returns.
- Capability test: fork without `SpawnNewIdentities`; `fork.promote(new_cfg)` returns `CapabilityError::Denied`.
- Gate test: persistent fork returns the "Phase 3" handler error — verifies the Phase 3 path is intentionally blocked.

**Verification:**
`cargo nextest run -p pattern-runtime fork_spawn`

**Commit:** `[pattern-runtime] scaffold fork spawn dispatch (lightweight only; Phase 3 persistent)`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Expose `ctx.spawn.{ephemeral,awaitSpawn,awaitAll,fork,sibling,stop}` on the Haskell SDK

**Verifies:** AC3.1, AC4.7, AC5.1 at the agent-facing surface.

**Files:**
- Modify: `crates/pattern_runtime/haskell/Pattern/Spawn.hs` — helpers for all six variants with proper Haskell types.
- Modify: `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — ensure `DescribeEffect::effect_decl` helpers list matches the updated `Pattern/Spawn.hs`.
- Update: `crates/pattern_runtime/src/sdk/preamble.rs` snapshot (if one exists) to reflect new helper signatures.

**Implementation:**

Haskell-side helpers + minimal type declarations. `SpawnId` is the handle returned by `ephemeral`; callers pass it to `awaitSpawn` (block-for-result) or `stop` (cancel). `SpawnResult` is the structured return from `awaitSpawn` (carries a JSON payload in Phase 2; Phase 3 Task 8 may extend with field accessors). `ForkHandle` + `MergeReport` land here as opaque placeholders that Phase 3 Task 8 fleshes out with resolution helpers.

```haskell
-- Type placeholders. Phase 3 Task 8 extends ForkHandle with awaitResult /
-- mergeBack / discard / promote helpers; Phase 2 just needs the wire shape.
newtype SpawnId   = SpawnId   { spawnIdText :: Text } deriving (Eq, Show)
newtype PersonaId = PersonaId { personaIdText :: Text } deriving (Eq, Show)
data ForkHandle   = ForkHandle { forkId :: SpawnId } deriving (Eq, Show)

-- Result of an ephemeral's await or a fork's awaitResult (Phase 3 extends).
-- Phase 2 surfaces it as opaque JSON; Phase 3 Task 8 adds field accessors.
newtype SpawnResult = SpawnResult { spawnResultJson :: Value }
  deriving (Eq, Show)

-- Placeholder; Phase 3 Task 2 fills in the concrete record shape.
newtype MergeReport = MergeReport { mergeReportJson :: Value }
  deriving (Eq, Show)

-- Non-blocking: returns a SpawnId immediately; child session runs in the
-- background. Use awaitSpawn to block on the result. Separation lets
-- delegation patterns spawn in parallel then await as a batch.
ephemeral :: Member Spawn effs => EphemeralConfig -> Eff effs SpawnId
ephemeral cfg = Freer.send (Ephemeral cfg)

awaitSpawn :: Member Spawn effs => SpawnId -> Eff effs SpawnResult
awaitSpawn sid = Freer.send (AwaitSpawn sid)

-- Await many ephemerals in a single round-trip; results come back in id order.
-- Uses futures::future::join_all on the Rust side — all children poll concurrently,
-- and partial failure is preserved (each slot is Either SpawnError SpawnResult).
awaitAll :: Member Spawn effs => [SpawnId] -> Eff effs [Either SpawnError SpawnResult]
awaitAll ids = Freer.send (AwaitAll ids)

fork :: Member Spawn effs => ForkConfig -> Eff effs ForkHandle
fork cfg = Freer.send (Fork cfg)

sibling :: Member Spawn effs => SiblingConfig -> Eff effs PersonaId
sibling cfg = Freer.send (Sibling cfg)

stop :: Member Spawn effs => SpawnId -> Eff effs ()
stop sid = Freer.send (Stop sid)
```

Corresponding `EphemeralConfig`, `ForkConfig`, `SiblingConfig` Haskell record types with fields mirroring the Rust config structs. Use record syntax with safe defaults. JSON wire format bridges via existing `Pattern.Aeson` helpers; Rust-side `FromCore` is implemented by hand (no JSON-over-string fallback).

Snapshot tests from Phase 1 Task 3 pick up the new helpers automatically; review the insta diff and approve.

**Testing:**
- Multi-module compilation test: an agent program imports `Pattern.Spawn` and calls `ephemeral (EphemeralConfig { program = "pure ()", ...})`. Compile via the existing `tests/multi_module_sdk.rs` pattern.
- Integration: `ephemeral cfg >>= awaitSpawn` runs end-to-end and returns a `SpawnResult` with the ephemeral's output in the JSON payload.
- Integration: parallel pattern — `traverse ephemeral [cfg1, cfg2, cfg3]` then `awaitAll ids` returns 3 results; workers genuinely ran in parallel (assert via wall-clock vs. sequential baseline); single sync-bridge round-trip for the await batch.

**Verification:**
`cargo nextest run -p pattern-runtime spawn_sdk_surface` + `cargo test --doc -p pattern-runtime`

**Commit:** `[pattern-runtime] surface ctx.spawn.{ephemeral,awaitSpawn,awaitAll,fork,sibling,stop} in Pattern.Spawn`
<!-- END_TASK_9 -->

<!-- END_SUBCOMPONENT_D -->

---

## Phase done-when checklist

- [ ] Spawn-config types (Ephemeral/Fork/Sibling/PersonaConfig + RelationshipKind) live in `pattern_core`. `PersonaId` alias added. `CapabilityFlag` + the `flags` field on `CapabilitySet` already land in Phase 1 Task 1.
- [ ] `SpawnReq` grammar replaced with six-variant enum (Ephemeral, AwaitSpawn, AwaitAll, Fork, Sibling, Stop); Haskell `Pattern.Spawn` module updated.
- [ ] `SpawnRegistry` with per-parent semaphore + cancel-on-drop exists and is threaded through `SessionContext`.
- [ ] Ephemeral dispatch produces a live child session with capability inheritance, costume, and timeout; full AC3 coverage.
- [ ] Sub-spawn chain cancels cleanly when parent resolves (AC3.6, AC3.7).
- [ ] Sibling dispatch supports existing personas (AC5.1, AC5.4, AC5.6) and new-identity drafts (AC5.2, AC5.3).
- [ ] Fork dispatch compiles and runs for `Lightweight` isolation; `Persistent` returns a clear "Phase 3" handler error; `promote` gate wiring is in place.
- [ ] All existing tests still pass. New tests cover ACs listed above using deterministic (mock-provider, no-live-model) harnesses.
- [ ] No orphan EvalWorker threads under test — confirm with a thread-count snapshot at end of each integration test.

---

## Notes for executor

- `FromCore` derive must decode each config struct directly; implement the trait by hand if the derive doesn't cover nested payloads. No JSON-over-string.
- `BlockRef` lives at `crates/pattern_core/src/types/block_ref.rs` — import directly.
- Memory ACL integration with sibling spawn (sibling reading shared blocks) is out of Phase 2 scope; Phase 6 / existing shared-block pattern handles it.
- Commit style per project conventions. Include `[pattern-core]` for all pattern_core changes; `[pattern-runtime]` for runtime work; `[pattern-runtime] [haskell]` for combined Rust+Haskell commits.
