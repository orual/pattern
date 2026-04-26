# v3-multi-agent Phase 7: Haskell delegation libraries and integration smoke

**Goal:** ship three starter Haskell delegation patterns (`Pattern.Delegation.RoundRobin`, `Pattern.Delegation.Pipeline`, `Pattern.Delegation.FanOut`) as reusable library code that composes `Spawn.ephemeral` + `ctx.tasks.*` primitives, then prove the full multi-agent surface composes end-to-end via a deterministic smoke test using a mock provider — no live model, no network.

**Architecture:** delegation patterns are pure Haskell — they live in the same `crates/pattern_runtime/haskell/Pattern/` tree as the existing 14 SDK modules and get picked up by the standard include-path logic at session open. No Rust changes unless the patterns surface new capability requirements or helper gaps. The smoke test at `crates/pattern_runtime/tests/multi_agent_smoke.rs` instantiates a two-persona constellation (supervisor + specialist), routes a human message through fronting, triggers a task delegation, verifies the specialist completes the task, verifies capability enforcement refuses an unauthorised effect, and exercises a fork-and-merge cycle. All provider calls go through a scripted mock. Phase 7 is mostly integration verification — most actual Rust code landed in Phases 1-6.

**Tech Stack:** Haskell (same SDK style as existing `Pattern.*` modules; no new language features), the existing `pattern_runtime::testing::MockProviderClient` at `crates/pattern_runtime/src/testing.rs:110` — scripted via `MockProviderClient::with_turns(...)`. No new provider mock.

**Scope:** 7 of 7. Closes AC10.

**Codebase verified:** 2026-04-23.

---

## Codebase verification findings

- ✓ Existing SDK Haskell modules at `crates/pattern_runtime/haskell/Pattern/`: `Aeson`, `Diagnostics`, `Display`, `File`, `Log`, `Mcp`, `Memory`, `Message`, `Prelude`, `Recall`, `Rpc`, `Search`, `Shell`, `Sources`, `Spawn`, `Table`, `Text`, `Time`. Delegation modules go alongside: `Pattern.Delegation.RoundRobin`, `Pattern.Delegation.Pipeline`, `Pattern.Delegation.FanOut`.
- ✗ The design plan's path `crates/pattern_runtime/src/tidepool/sdk/lib/` does not exist — it's `crates/pattern_runtime/haskell/Pattern/` in-tree. Plan uses the real path.
- ✓ Integration test dir `crates/pattern_runtime/tests/` already has 17 integration suites (`hello_world.rs`, `multi_module_sdk.rs`, `session_lifecycle.rs`, etc.). `multi_agent_smoke.rs` follows the same shape.
- ✓ `MockProviderClient` lives at `crates/pattern_runtime/src/testing.rs:110` with `with_turns`, `text_turn`, `tool_use_turn`, `with_token_count` helpers (and a `rotate_count` inspection hook). Re-exported via `pattern_runtime::testing`. Phase 7 reuses it — no new provider mock.

### Design decisions locked in

- **Delegation patterns are stateless combinators.** They take a list of worker configurations + a task generator and return an `Eff effs [Result]`. No hidden state, no per-pattern registry — everything lives in the caller's scope.
- **RoundRobin** — assign tasks in turn to a fixed list of worker personas (or costumes for ephemeral workers). Used for load-balancing identical specialists.
- **Pipeline** — chain stages where stage N's output feeds stage N+1's input. Used for multi-step processing where each step is a distinct specialist.
- **FanOut** — same task submitted in parallel to all workers; caller aggregates results. Used for voting / ensemble patterns.
- **Smoke test is the single comprehensive end-to-end test.** We do NOT add one integration test per AC — the smoke test exercises the full surface in one go, and failure modes get diagnosed from the test's structured output per AC10.5.
- **Mock provider contract.** `MockProviderClient::with_turns(...)` at `pattern_runtime::testing` takes a `Vec<Vec<ProviderEvent>>` (one vec per scripted turn). Use the `text_turn` / `tool_use_turn` helpers for the common shapes; add inline event lists when the scripted flow needs custom content. Deterministic; no network.

---

## Acceptance Criteria Coverage

### v3-multi-agent.AC10: End-to-end integration

- **v3-multi-agent.AC10.1 Success:** Smoke test at `crates/pattern_runtime/tests/multi_agent_smoke.rs` passes deterministically: creates two personas, one fronting as supervisor with routing rules, spawns ephemeral worker, assigns task, worker completes task, supervisor receives result, capability enforcement prevents unauthorized effects
- **v3-multi-agent.AC10.2 Success:** Mock ProviderClient; no live model dependency in CI
- **v3-multi-agent.AC10.3 Success:** Fork-and-merge flow: parent forks (lightweight), fork writes to memory, merge_back succeeds, parent sees merged state
- **v3-multi-agent.AC10.4 Success:** Haskell delegation modules (`Pattern.Delegation.RoundRobin` etc.) importable and functional in agent programs
- **v3-multi-agent.AC10.5 Failure:** Any step in the smoke flow failing produces a clear error identifying which step and which assertion
- **v3-multi-agent.AC10.6 Edge:** Smoke test runs concurrently with other `pattern-runtime` tests without shared-state interference

---

<!-- START_SUBCOMPONENT_A (tasks 1-4) -->

<!-- START_TASK_1 -->
### Task 1: Author scripted turns for the multi-agent smoke

**Verifies:** AC10.2 (ensures the smoke test runs against a deterministic provider).

**Files:**
- Create: `crates/pattern_runtime/tests/support/multi_agent_scripts.rs` — a module that builds the `Vec<Vec<ProviderEvent>>` script for the smoke test using `MockProviderClient::{text_turn, tool_use_turn}` helpers from `pattern_runtime::testing`. The `tests/support/` location keeps runnable test code separate from inert data fixtures (KDL / HS under `tests/fixtures/`); consume via `#[path = "support/multi_agent_scripts.rs"] mod scripts;` at the top of `multi_agent_smoke.rs`.

**Implementation:**

The `MockProviderClient` already exists and is the correct vehicle — don't introduce a second mock. The work here is pure fixture authoring: write the scripted turn sequence the smoke needs (supervisor routes → specialist completes task → supervisor summarizes). Keep the fixtures in a named module so other multi-agent tests can reuse them if they grow.

Review the existing `MockProviderClient` tests at `crates/pattern_runtime/src/testing.rs` bottom to see the builder idioms; match that style.

**Testing:**
- None beyond what the smoke test itself (Task 5) exercises.

**Verification:**
Compilation alone is sufficient; the smoke test in Task 5 exercises the fixtures.

**Commit:** `[pattern-runtime] add scripted turn fixtures for multi-agent smoke`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `Pattern.Delegation.RoundRobin`

**Verifies:** AC10.4.

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Delegation/RoundRobin.hs`

**Implementation:**

```haskell
{-# LANGUAGE FlexibleContexts, NoImplicitPrelude #-}
module Pattern.Delegation.RoundRobin (roundRobin) where

import Pattern.Prelude
import qualified Pattern.Spawn as Spawn

-- Distribute tasks across a fixed list of ephemeral costumes.
-- Each task is run on the "next" worker in the ring; results are returned
-- in the task-submission order.
roundRobin
    :: Member Spawn effs
    => [Spawn.EphemeralConfig]  -- ^ worker pool (cycled)
    -> [task]                    -- ^ tasks (preserves order in result)
    -> (task -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
       -- ^ merge task payload into the per-task ephemeral config
    -> Eff effs [Either Spawn.SpawnError Spawn.SpawnResult]
roundRobin workers tasks attach = do
    let assignments = zip tasks (cycle workers)
    -- Spawn all workers in parallel, then batch-await (single sync-bridge
    -- round-trip via AwaitAll on the Rust side).
    ids <- traverse (\(t, w) -> Spawn.ephemeral (attach t w)) assignments
    Spawn.awaitAll ids
```

(Signatures illustrative — Haskell imports actually in-tree may differ slightly; match existing style at `Pattern/Spawn.hs`.)

**Testing:**
- Integration: 4 tasks, 2 workers; assert each worker runs exactly 2 tasks; result ordering matches input order.
- Covered in the smoke test Task 5.

**Verification:**
Compilation test via existing `multi_module_sdk.rs` pattern (import the module into a test agent program).

**Commit:** `[pattern-runtime] add Pattern.Delegation.RoundRobin`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: `Pattern.Delegation.Pipeline`

**Verifies:** AC10.4.

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Delegation/Pipeline.hs`

**Implementation:**

```haskell
module Pattern.Delegation.Pipeline (pipeline) where

import Pattern.Prelude
import qualified Pattern.Spawn as Spawn

-- Chain ephemeral stages where each stage's output becomes the next's input.
-- Each stage is an (EphemeralConfig, output-decoder) pair: the caller knows how
-- to turn the stage's SpawnResult into the input payload for the next stage.
pipeline
    :: Member Spawn effs
    => input                                          -- ^ initial input
    -> [(Spawn.EphemeralConfig, Spawn.SpawnResult -> stageOutput)]
    -> (stageOutput -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
       -- ^ feed stage-N output into stage-(N+1) ephemeral config
    -> Eff effs stageOutput
pipeline initialInput stages attach =
    foldM step initialInput stages
  where
    -- Pipeline stages are sequential by design (stage N+1 depends on stage N's
    -- output), so spawn + awaitSpawn in sequence is correct here.
    step acc (cfg, decode) = do
      let cfg' = attach acc cfg
      sid <- Spawn.ephemeral cfg'
      result <- Spawn.awaitSpawn sid
      pure (decode result)
```

**Testing:**
- Integration: 3-stage pipeline (parser → transformer → formatter); assert final output reflects all three transforms, in order.
- Covered in the smoke test.

**Commit:** `[pattern-runtime] add Pattern.Delegation.Pipeline`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: `Pattern.Delegation.FanOut`

**Verifies:** AC10.4.

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Delegation/FanOut.hs`

**Implementation:**

```haskell
module Pattern.Delegation.FanOut (fanOut) where

import Pattern.Prelude
import qualified Pattern.Spawn as Spawn

-- Submit the same task to every worker in parallel; collect results in
-- worker order.
fanOut
    :: Member Spawn effs
    => [Spawn.EphemeralConfig]   -- ^ workers
    -> task                       -- ^ shared task
    -> (task -> Spawn.EphemeralConfig -> Spawn.EphemeralConfig)
    -> Eff effs [Either Spawn.SpawnError Spawn.SpawnResult]
fanOut workers task attach = do
    -- Genuine parallel fan-out: spawn every worker, then awaitAll as a batch.
    ids <- traverse (\w -> Spawn.ephemeral (attach task w)) workers
    Spawn.awaitAll ids
```

**Testing:**
- Integration: 3 workers, 1 task; assert 3 distinct results returned; assert concurrency semaphore is respected (if limit < 3, spawns queue — but this is the Rust-side concern and is already tested in Phase 2).
- Covered in the smoke test.

**Commit:** `[pattern-runtime] add Pattern.Delegation.FanOut`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 5-6) -->

<!-- START_TASK_5 -->
### Task 5: Smoke test — two-persona constellation with delegation + fronting

**Verifies:** AC10.1, AC10.2, AC10.4, AC10.5, AC10.6.

**Files:**
- Create: `crates/pattern_runtime/tests/multi_agent_smoke.rs`
- Create: `crates/pattern_runtime/tests/fixtures/multi_agent/supervisor.kdl`
- Create: `crates/pattern_runtime/tests/fixtures/multi_agent/specialist.kdl`
- Create: `crates/pattern_runtime/tests/fixtures/multi_agent/supervisor_program.hs`
- Create: `crates/pattern_runtime/tests/fixtures/multi_agent/specialist_program.hs`

**Implementation:**

Test flow:

1. **Setup.** Build a `MockProviderClient::with_turns(...)` using the scripted fixtures from Task 1. Use a temp data dir with **InRepo mount mode, jj disabled** — the smoke only exercises lightweight forks (step 6 spawns `ForkIsolation::Lightweight`), which do not require jj. This keeps the test runnable on any CI image. Persistent-fork coverage stays in Phase 3's dedicated `fork_persistent.rs` integration test, which is jj-gated there.
2. **Persona loading.** Load `supervisor.kdl` (has `FrontingControl` + `SpawnNewIdentities` capability flags; `Constellation`, `Spawn`, `Message`, `Memory`) and `specialist.kdl` (has only `Memory` + `Message`). Register both via the registry; set FrontingSet to `{ active: [supervisor], fallback: supervisor }`.
3. **Human message.** Simulate an `InitSession` + `SendMessage` RPC with the human message `"please delegate: compute 2+2"`. The supervisor's scripted response dispatches a `MessageReq::Delegate { task: TaskRef, target: specialist, body: "2+2" }`.
4. **Delegation lands in specialist's mailbox.** Specialist steps, reads the task from its pinned working-memory snapshot, scripted response emits a result `"4"`.
5. **Capability enforcement.** The specialist's program tries to call `Shell.execute`; assert **compile-time failure** from `tidepool-extract` — the specialist's `CapabilitySet` excludes `Shell`, so Phase 1 Task 3's filtered prelude omits the `Shell` GADT constructors and the program fails to compile with a clear "unknown constructor" / "not in scope" error. This is AC1.2 end-to-end. Do NOT accept a runtime dispatch error here — the whole point of Phase 1's prelude filtering is that the program can't even be expressed.
6. **Fork-and-merge.** Supervisor spawns a lightweight fork; fork writes `"fork-note"` to its own `notes` block; `fork.merge_back()`; assert parent's `notes` block contains the merge outcome per loro semantics.
7. **Result propagation.** Specialist's `"4"` message routes back to the supervisor (via `Message.send(supervisor_id, ...)`); supervisor observes it in its next turn.
8. **Concurrency check (AC10.6).** The test uses a unique temp dir per run — no shared-state collisions with concurrent `pattern-runtime` tests under `cargo nextest run`.
9. **Error clarity (AC10.5).** For each assertion, wrap in a context message (`assert_eq!(foo, bar, "step 4: specialist did not receive delegation")`). When a step fails, the panic identifies the step.

Runtime budget: < 30 seconds. If the test takes longer, the scripted provider or test setup has a bug.

**Testing:** the smoke test is itself the test. No nested test structure.

**Verification:**
`cargo nextest run -p pattern-runtime multi_agent_smoke -- --nocapture`

**Commit:** `[pattern-runtime] add multi-agent smoke test covering AC10`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Custom Haskell wake-condition evaluator

**Verifies:** closes the Phase 4 Task 9 deferral. After this task, `ctx.wake.register` accepting a custom Haskell condition no longer logs-and-returns — it actually evaluates the user's program on its trigger and pokes the mailbox if the result is true.

**Background:** Phase 4 Task 9 shipped the registration path (`Pattern.Wake.register`) and the `CapabilityFlag::WakeConditionRegistration` gate, but stored the user's program without running it. The deferral was on Tidepool concurrent-evaluation design — running periodic Haskell condition checks alongside the agent's main turn loop wasn't scoped. Phase 7 closes it now that the broader multi-agent surface is settled.

**Files:**
- Modify: `crates/pattern_runtime/src/wake/mod.rs` — replace the no-op storage with a `CustomEvaluator` that owns a tokio task per registered condition.
- Modify: `crates/pattern_runtime/src/sdk/handlers/wake.rs` — `WakeReq::Register` for `WakeCondition::Custom { id, program }` now spawns the evaluator instead of logging.
- Modify: `crates/pattern_runtime/haskell/Pattern/Wake.hs` — delete the "evaluator deferred" comment in the docstring.
- Modify: `crates/pattern_runtime/CLAUDE.md` — remove the "deferred to when Tidepool concurrent evaluation is better understood" note added in Phase 4.
- Tests: integration test exercising a custom interval condition that fires and pokes the mailbox.

**Architecture:**

One tokio task per registered custom condition. Triggered by:
- `Interval(period)` — `tokio::time::interval(period)` ticks; min period 1s (rejected at register-time if smaller — **no subsecond polling**).
- `BlockChanged(label)` — piggyback on the existing `pattern_memory::subscriber` fan-out introduced in Phase 4 Task 8.

On trigger, the evaluator runs the user's Haskell condition program once via a fresh **`compile_and_run`** dispatch on a dedicated 256 MiB OS thread (matching the eval-worker pattern from `agent_loop::eval_worker.rs`). Bounded by `tokio::time::timeout` (default: 30s per evaluation; rejects evaluation if a prior one is still running for the same condition — single-flight). The condition program's effect row is restricted to **read-only** capabilities (Time, Log, Memory.Get, Search) — no `Memory.Put`, no `Message.Send`, no `Spawn`. The condition program returns `Bool`.

If the result is `True`, the evaluator pushes a `MailboxInput::Message` (synthesised with `MessageOrigin::Author::System { reason: SystemReason::CustomWake { id } }`) onto the agent's mailbox. The agent's next idle moment surfaces the wake.

**Resource accounting:**
- Per-session cap: at most 32 concurrent registered custom conditions (configurable via `SessionContext::with_max_custom_wakes`). Registration beyond cap returns `EffectError::Handler("CustomWakeLimit: ...")`.
- Per-evaluation cap: 30s wall-clock timeout.
- Single-flight per condition: a still-running evaluation skips the next trigger and emits a `tracing::warn!` instead of queuing.

**Implementation:**

```rust
// pattern_runtime/src/wake/custom.rs (new file)
pub struct CustomEvaluator {
    /// Registered conditions, keyed by user id.
    tasks: parking_lot::Mutex<HashMap<SmolStr, JoinHandle<()>>>,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
    sdk_dir: PathBuf,
    // Restricted bundle for evaluating user programs (read-only).
    bundle_factory: Arc<dyn Fn() -> ReadOnlyBundle + Send + Sync>,
    inflight: Arc<DashMap<SmolStr, ()>>,
}

impl CustomEvaluator {
    pub fn register(&self, id: SmolStr, condition: WakeCondition, program: String) -> Result<(), WakeError> {
        // Validate: min period 1s, cap not exceeded.
        // Spawn tokio task with the appropriate trigger source.
    }

    pub fn unregister(&self, id: &SmolStr) {
        // Abort the JoinHandle; remove from map.
    }
}
```

Trigger task body (sketch):
```rust
let mut interval = tokio::time::interval(period);
loop {
    interval.tick().await;
    if inflight.contains_key(&id) {
        tracing::warn!(?id, "custom wake skipped: prior evaluation still running");
        continue;
    }
    inflight.insert(id.clone(), ());
    let result = tokio::time::timeout(EVAL_TIMEOUT, run_user_program(&program)).await;
    inflight.remove(&id);
    match result {
        Ok(Ok(true)) => { let _ = mailbox_tx.send(make_custom_wake_input(&id)); }
        Ok(Ok(false)) | Ok(Err(_)) | Err(_) => {} // log; do nothing
    }
}
```

`run_user_program` spawns the OS thread and `compile_and_run`s the user's program against the read-only bundle.

**Testing:**
- Integration: register a custom condition that returns `True` on every other tick. Verify the mailbox receives exactly the expected number of wake messages over a 5s window.
- Integration: register a long-running custom program (sleep 60s). Verify timeout fires, no mailbox poke, condition stays registered for next trigger.
- Integration: register a condition that uses `Memory.Put` (a write effect). Verify Tidepool compile rejects with capability error.
- Integration: register two conditions with overlapping triggers, both fire correctly, single-flight per condition.
- Negative: register with `period: Duration::from_millis(500)`. Verify register returns the min-period error, no task spawned.

**Verification:**
`cargo nextest run -p pattern-runtime wake::custom`

**Future improvement (post-Phase 7):** the architecturally cleaner shape is to queue custom-wake evaluations on the **session's existing eval worker** at a lower priority than normal turn inputs, rather than spawning a fresh OS thread per evaluation. This avoids per-eval thread cost and cleanly bounds resource use to the one worker that's already accounted for. The async/separate-thread impl described above is the explicit ship-now choice — simpler to land, isolated, no priority-queue scheduler work needed. Switch when the eval worker grows a priority queue (likely alongside any future "agent thinks while idle" feature that wants the same primitive).

**Commit:** `[pattern-runtime] implement custom Haskell wake-condition evaluator`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Audit + final cleanup

**Verifies:** overall phase integrity.

**Files:**
- Audit: the entire `crates/pattern_runtime/` + `crates/pattern_core/` tree for leftover `todo!()` / `unimplemented!()` / `// TODO:` / commented-out code introduced during phases 1-6.
- Audit: `pattern_runtime/CLAUDE.md` + `pattern_core/CLAUDE.md` for stale notes (e.g. "Router trait fix blocked" must be gone after Phase 4).
- Update: project `CLAUDE.md` status section to mark v3-multi-agent as complete.

**Implementation:**

Run:
```bash
rg -F 'todo!()' crates/pattern_runtime crates/pattern_core crates/pattern_memory
rg -F 'unimplemented!()' crates/pattern_runtime crates/pattern_core crates/pattern_memory
rg -n '^// TODO' crates/pattern_runtime crates/pattern_core crates/pattern_memory
rg -n 'blocked on' crates/ docs/
```

Address each hit — either fix it now, or if it's a known Phase-4+-deferred item, verify the deferral is still valid. Delete stale CLAUDE.md notes.

Update `pattern_runtime/CLAUDE.md`:
- Remove the "Open work: Router trait + daemon CliRouter" section (resolved in Phase 4).
- Update the "v3-TUI integration note" with a parallel "v3-multi-agent integration note" if the multi-agent work changed any invariants (mailbox task ownership, FrontingSet daemon-level state, etc.).

Update `project/CLAUDE.md`:
- Bump "Last verified" date.
- In the "Current State" block, add a sentence: "v3-multi-agent (7 phases) complete. CapabilitySet + spawn primitives + fork/merge + mailbox/wake + fronting/routing + constellation registry + Haskell delegation patterns all landed."

**Testing:**
- `cargo nextest run --workspace` full suite green.
- `just pre-commit-all` passes (format + clippy + doctests).
- `rg` searches above produce no unresolved hits.

**Verification:**
Manual: read the diff of updated CLAUDE.md files and confirm accuracy.

**Commit:** `[meta] [pattern-runtime] post-multi-agent audit + CLAUDE.md refresh`
<!-- END_TASK_6 -->

<!-- END_SUBCOMPONENT_B -->

---

## Phase done-when checklist

- [ ] `MockProviderClient` scripted-turn fixtures cover the smoke test deterministically.
- [ ] Three delegation Haskell modules (RoundRobin, Pipeline, FanOut) importable and tested in the smoke.
- [ ] `multi_agent_smoke.rs` exercises the full surface and passes under `cargo nextest run` without external dependencies.
- [ ] No residual `todo!()` / `unimplemented!()` / stale CLAUDE.md notes left over from phases 1-6.
- [ ] Project CLAUDE.md reflects multi-agent completion.

---

## Notes for executor

- Use `pattern_runtime::testing::MockProviderClient` — it exists and is the canonical vehicle. Do not invent a second mock.
- The smoke test intentionally overlaps with per-phase tests. Per-phase tests isolate regressions; the smoke test proves composition. Both are load-bearing.
- If the smoke test takes >60s, something is mis-wired — pause and diagnose before adding timeouts. The scripted provider should complete each turn in milliseconds.
- Commit style per project.
