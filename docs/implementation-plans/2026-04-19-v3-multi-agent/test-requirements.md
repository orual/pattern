# v3-multi-agent Test Requirements

Mapping from every acceptance criterion in
`docs/design-plans/2026-04-19-v3-multi-agent.md` (AC1.1 – AC10.6) to a verification
strategy. Derived from the phase files' task-level "Verifies:" labels and each
phase's "Acceptance Criteria Coverage" section. File paths are the expected
location when the corresponding task lands; all paths are absolute within the
repository.

## Automated test coverage

### v3-multi-agent.AC1: CapabilitySet and prelude filtering

- **AC1.1 Success** — `[Memory, Message, Tasks]` prelude omits `Spawn`/`Shell`/`Wake`.
  - Type: unit + snapshot (insta)
  - File: `crates/pattern_runtime/src/sdk/bundle.rs` (unit); `crates/pattern_runtime/src/snapshots/` (insta)
  - Verified by: Phase 1 Task 2 (alignment), Phase 1 Task 3 (`filtered_effect_decls` + snapshot)

- **AC1.2 Failure** — program referencing excluded effect fails at Tidepool compile, not runtime.
  - Type: integration (requires `tidepool-extract`)
  - File: `crates/pattern_runtime/tests/capability_compile.rs`
  - Verified by: Phase 1 Task 4; end-to-end re-verified in Phase 7 Task 5 smoke step 5

- **AC1.3 Success** — program using only included effects compiles + runs.
  - Type: integration
  - File: `crates/pattern_runtime/tests/capability_compile.rs`
  - Verified by: Phase 1 Task 4

- **AC1.4 Success** — `CapabilitySet::all()` preamble equals unfiltered canonical.
  - Type: unit
  - File: `crates/pattern_runtime/src/sdk/bundle.rs`
  - Verified by: Phase 1 Task 3

- **AC1.5 Failure** — expanding CapabilitySet beyond parent returns `CapabilityError::Escalation`.
  - Type: unit
  - File: `crates/pattern_core/src/capability.rs`
  - Verified by: Phase 1 Task 1 (`restrict_to` unit tests)

- **AC1.6 Edge** — empty CapabilitySet produces prelude with only base types; pure programs still compile.
  - Type: unit + snapshot (insta)
  - File: `crates/pattern_runtime/src/sdk/bundle.rs` + `crates/pattern_runtime/src/snapshots/`
  - Verified by: Phase 1 Task 3 (empty-set snapshot)

### v3-multi-agent.AC2: Runtime approval and policy

- **AC2.1 Success** — destructive shell commands trigger `RequireApproval`.
  - Type: unit + integration
  - File: unit at `crates/pattern_runtime/src/policy/defaults.rs`; integration at `crates/pattern_runtime/tests/shell_policy.rs`
  - Verified by: Phase 1 Task 9 (unit), Phase 1 Task 10 (Deny-path integration)

- **AC2.2 Success** — KDL config loosens Rust default; no broker invocation.
  - Type: integration
  - File: `crates/pattern_runtime/tests/policy_kdl_merge.rs`
  - Verified by: Phase 1 Task 14

- **AC2.3 Success** — KDL config tightens defaults; all file writes gated.
  - Type: integration
  - File: `crates/pattern_runtime/tests/policy_kdl_merge.rs`
  - Verified by: Phase 1 Task 14

- **AC2.4 Success** — PermissionBroker approve-once allows one, gates next.
  - Type: unit
  - File: `crates/pattern_core/src/permission.rs`
  - Verified by: Phase 1 Task 6

- **AC2.5 Success** — approve-for-scope allows matching invocations until session end.
  - Type: unit
  - File: `crates/pattern_core/src/permission.rs`
  - Verified by: Phase 1 Task 6

- **AC2.6 Success** — approve-for-duration allows until jiff expiry; injected clock.
  - Type: unit
  - File: `crates/pattern_core/src/permission.rs`
  - Verified by: Phase 1 Task 6

- **AC2.7 Failure** — config-KDL shape writes gated regardless of KDL loosening (locked default).
  - Type: unit + integration
  - File: unit at `crates/pattern_runtime/src/policy/config_guard.rs` (+ proptest fuzz); integration at `crates/pattern_runtime/tests/file_write_gate.rs`
  - Verified by: Phase 1 Task 11 (predicate), Phase 1 Task 12 (pipeline), Phase 1 Task 15 (end-to-end File.Write gate)

- **AC2.8 Failure** — broker request timeout returns denial, no hang, no map leak.
  - Type: unit
  - File: `crates/pattern_core/src/permission.rs`
  - Verified by: Phase 1 Task 6 (explicit leak test)

- **AC2.9 Edge** — two per-runtime brokers have independent queues + scope caches.
  - Type: unit + integration
  - File: unit at `crates/pattern_core/src/permission.rs`; integration at `crates/pattern_runtime/tests/permission_bridge.rs` (two `SessionContext`s)
  - Verified by: Phase 1 Task 5 (belt-and-suspenders), Phase 1 Task 6 (scope-cache isolation), Phase 1 Task 7 (bridge isolation)

### v3-multi-agent.AC3: Ephemeral spawn

- **AC3.1 Success** — `ctx.spawn.ephemeral` creates separate EvalWorker; ephemeral returns result.
  - Type: integration (MockProviderClient)
  - File: `crates/pattern_runtime/tests/ephemeral_spawn.rs`
  - Verified by: Phase 2 Task 4

- **AC3.2 Success** — ephemeral CapabilitySet is subset of parent; prelude reflects restriction.
  - Type: integration
  - File: `crates/pattern_runtime/tests/ephemeral_spawn.rs`
  - Verified by: Phase 2 Task 4 (capability-escalation test)

- **AC3.3 Success** — ephemeral costume overrides prompt; log attribution is parent's identity.
  - Type: integration
  - File: `crates/pattern_runtime/tests/ephemeral_spawn.rs`
  - Verified by: Phase 2 Task 4

- **AC3.4 Success** — ephemeral timeout cancels session, returns `SpawnError::Timeout`.
  - Type: integration
  - File: `crates/pattern_runtime/tests/ephemeral_spawn.rs`
  - Verified by: Phase 2 Task 4

- **AC3.5 Success** — concurrent ephemeral count respects semaphore limit.
  - Type: unit + integration
  - File: unit at `crates/pattern_runtime/src/spawn/registry.rs`; integration at `crates/pattern_runtime/tests/ephemeral_spawn.rs`
  - Verified by: Phase 2 Task 3 (unit), Phase 2 Task 4 (integration)

- **AC3.6 Failure** — parent resolves → all child ephemerals cancelled; no EvalWorker leaks.
  - Type: unit + integration (deterministic leak counter)
  - File: unit at `crates/pattern_runtime/src/spawn/registry.rs`; integration at `crates/pattern_runtime/tests/ephemeral_chain.rs`
  - Verified by: Phase 2 Task 3 (`cancel_on_drop`), Phase 2 Task 5 (`LIVE_EVAL_WORKERS` counter)

- **AC3.7 Edge** — nested ephemerals; full parent→child→grandchild cancellation chain.
  - Type: integration
  - File: `crates/pattern_runtime/tests/ephemeral_chain.rs`
  - Verified by: Phase 2 Task 3 (unit scripted handles), Phase 2 Task 5 (3-level chain)

### v3-multi-agent.AC4: Fork spawn and isolation

- **AC4.1 Success** — lightweight fork over `LoroDoc::fork()`; parent + fork write independently.
  - Type: unit + integration
  - File: unit at `crates/pattern_memory/src/cache.rs`; integration at `crates/pattern_runtime/tests/fork_lightweight.rs`
  - Verified by: Phase 3 Task 1

- **AC4.2 Success** — persistent fork creates jj workspace; writes land on disk.
  - Type: integration (jj required; Nix devshell)
  - File: `crates/pattern_runtime/tests/fork_persistent.rs`
  - Verified by: Phase 3 Task 4

- **AC4.3 Success** — lightweight `merge_back` imports fork state into parent.
  - Type: integration + snapshot (insta diamond outcome)
  - File: `crates/pattern_runtime/tests/fork_merge_lightweight.rs`
  - Verified by: Phase 3 Task 2

- **AC4.4 Success** — persistent `merge_back` composes jj merge + loro import.
  - Type: integration (jj required)
  - File: `crates/pattern_runtime/tests/fork_merge_persistent.rs`
  - Verified by: Phase 3 Task 5

- **AC4.5 Success** — lightweight `discard` drops child state without propagation.
  - Type: integration + unit (double-discard)
  - File: `crates/pattern_runtime/tests/fork_discard.rs`
  - Verified by: Phase 3 Task 3

- **AC4.6 Success** — persistent `discard` runs `workspace_forget` + `bookmark_delete`.
  - Type: integration (jj required)
  - File: `crates/pattern_runtime/tests/fork_discard_persistent.rs`
  - Verified by: Phase 3 Task 6 (includes partial-failure path)

- **AC4.7 Success** — `fork.promote(cfg)` creates Draft persona with seeded memory.
  - Type: integration
  - File: `crates/pattern_runtime/tests/fork_promote.rs`; end-to-end promotion flow `crates/pattern_server/tests/promote_draft.rs`
  - Verified by: Phase 2 Task 8 (scaffold + gate wiring), Phase 3 Task 7 (capability gate + draft seed), Phase 6 Task 6 (queue drain + session open on promote)

- **AC4.8 Failure** — `fork.promote()` without `SpawnNewIdentities` returns `CapabilityError::Denied`.
  - Type: integration
  - File: `crates/pattern_runtime/tests/fork_promote.rs`
  - Verified by: Phase 2 Task 8 (gate wiring), Phase 3 Task 7 (end-to-end)

- **AC4.9 Edge** — concurrent parent+fork writes merge deterministically (loro CRDT).
  - Type: proptest + snapshot
  - File: `crates/pattern_runtime/tests/fork_merge_lightweight.rs`
  - Verified by: Phase 3 Task 2 (proptest over operation traces)

- **AC4.10 Edge** — persistent bookmark is `<agent>/<task-id>`; collision detection.
  - Type: unit + integration
  - File: unit at `crates/pattern_memory/src/jj/fork_bookmark.rs`; integration at `crates/pattern_runtime/tests/fork_persistent.rs`
  - Verified by: Phase 3 Task 4

### v3-multi-agent.AC5: Sibling spawn and identity authorization

- **AC5.1 Success** — sibling of existing persona opens; no authorization required.
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_spawn.rs`
  - Verified by: Phase 2 Task 6

- **AC5.2 Success** — new-identity sibling with `SpawnNewIdentities` creates + opens.
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_new_identity.rs`
  - Verified by: Phase 2 Task 7

- **AC5.3 Success** — new-identity sibling without flag creates as Draft; no session opened.
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_new_identity.rs`
  - Verified by: Phase 2 Task 7

- **AC5.4 Success** — sibling CapabilitySet sourced from sibling's own persona config.
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_spawn.rs`
  - Verified by: Phase 2 Task 6 (fixture-restricted caps)

- **AC5.5 Success** — sibling auto-registers with specified relationship.
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_autoregister.rs`
  - Verified by: Phase 6 Task 6

- **AC5.6 Failure** — nonexistent PersonaId returns `RegistryError::PersonaNotFound`.
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_spawn.rs`
  - Verified by: Phase 2 Task 6 (via registry stub)

- **AC5.7 Edge** — Draft persona visible in `constellation.list()`; messages to Draft queue (delivered on promote).
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_autoregister.rs`; queue semantics at `crates/pattern_server/tests/promote_draft.rs`
  - Verified by: Phase 4 Task 4 (queue path), Phase 6 Task 6 (list visibility + drain-on-promote)

### v3-multi-agent.AC6: Agent mailbox and message delivery

- **AC6.1 Success** — `ctx.message.send` delivers to target mailbox; target steps when idle.
  - Type: integration
  - File: `crates/pattern_runtime/tests/agent_registry.rs` + `crates/pattern_runtime/tests/mailbox_task.rs`
  - Verified by: Phase 4 Task 2 (mailbox primitive), Phase 4 Task 3 (task drain), Phase 4 Task 4 (router)

- **AC6.2 Success** — message to busy agent queues; delivered after current turn.
  - Type: integration
  - File: `crates/pattern_runtime/tests/mailbox_task.rs`
  - Verified by: Phase 4 Task 3

- **AC6.3 Success** — task delegation pins task `BlockRef` into recipient's snapshot.
  - Type: integration + snapshot (insta of composed request)
  - File: `crates/pattern_runtime/tests/message_delegate.rs`
  - Verified by: Phase 4 Task 5

- **AC6.4 Failure** — `ctx.message.send` to nonexistent PersonaId returns `RouterError::PersonaNotFound`.
  - Type: integration
  - File: `crates/pattern_runtime/tests/agent_registry.rs`
  - Verified by: Phase 4 Task 4

- **AC6.5 Failure** — send to Draft persona queues; no step triggered.
  - Type: integration
  - File: `crates/pattern_runtime/tests/agent_registry.rs`
  - Verified by: Phase 4 Task 4

- **AC6.6 Edge** — rapid sequential / concurrent sends preserve FIFO per-sender; no loss.
  - Type: integration
  - File: `crates/pattern_runtime/tests/agent_registry.rs` + `crates/pattern_runtime/tests/mailbox_task.rs`
  - Verified by: Phase 4 Task 3 (in-order under single sender), Phase 4 Task 4 (10 concurrent from 3 senders)

### v3-multi-agent.AC7: Wake conditions

- **AC7.1 Success** — `TaskTimeout` fires; TurnInput carries `WakeReason::TaskTimeout`.
  - Type: integration
  - File: `crates/pattern_runtime/tests/wake_rust_primitives.rs`
  - Verified by: Phase 4 Task 7

- **AC7.2 Success** — `BlockChanged` fires on any-agent modification; correct WakeReason.
  - Type: integration
  - File: `crates/pattern_runtime/tests/wake_block_changed.rs`
  - Verified by: Phase 4 Task 8

- **AC7.3 Success** — `TaskDependencyResolved` fires on target task Completed transition.
  - Type: integration
  - File: `crates/pattern_runtime/tests/wake.rs`
  - Verified by: Phase 4 Task 9 (via loro subscriber + `ctx.tasks.get`)

- **AC7.4 Success** — `Interval` fires every N seconds.
  - Type: integration
  - File: `crates/pattern_runtime/tests/wake_rust_primitives.rs`
  - Verified by: Phase 4 Task 7

- **AC7.5 Failure** — `ctx.wake.register` without `WakeConditionRegistration` returns denial.
  - Type: integration (matches on `CAPABILITY_DENIED_PREFIX`)
  - File: `crates/pattern_runtime/tests/wake.rs`
  - Verified by: Phase 4 Task 9

- **AC7.6 Edge** — multiple conditions registered; first-to-fire pokes; remainder persist.
  - Type: integration
  - File: `crates/pattern_runtime/tests/wake_rust_primitives.rs`
  - Verified by: Phase 4 Task 7

- **AC7.7 Edge** — wake during mid-turn is queued, delivered after turn completes.
  - Type: integration
  - File: `crates/pattern_runtime/tests/wake_rust_primitives.rs`
  - Verified by: Phase 4 Task 7

### v3-multi-agent.AC8: Fronting and routing

- **AC8.1 Success** — FrontingSet persists to `pattern_db`; survives daemon restart.
  - Type: unit + integration
  - File: unit at `crates/pattern_db/src/queries/fronting.rs`; integration at `crates/pattern_server/tests/fronting_persistence.rs`
  - Verified by: Phase 5 Task 2 (CRUD round-trip), Phase 5 Task 3 (daemon restart)

- **AC8.2 Success** — routing rule match delivers to rule's target mailbox.
  - Type: integration
  - File: `crates/pattern_runtime/tests/fronting_dispatch.rs` (+ end-to-end in `fronting_supervisor.rs`)
  - Verified by: Phase 5 Task 4, Phase 5 Task 7

- **AC8.3 Success** — unmatched message routes to fallback persona.
  - Type: integration
  - File: `crates/pattern_runtime/tests/fronting_dispatch.rs` (+ `fronting_supervisor.rs`)
  - Verified by: Phase 5 Task 4, Phase 5 Task 7

- **AC8.4 Success** — `@persona-name` bypasses routing; direct delivery.
  - Type: integration
  - File: `crates/pattern_runtime/tests/fronting_dispatch.rs`
  - Verified by: Phase 5 Task 4

- **AC8.5 Success** — co-fronting: both active personas receive unrouted messages (fan-out).
  - Type: integration
  - File: `crates/pattern_runtime/tests/fronting_dispatch.rs`
  - Verified by: Phase 5 Task 4

- **AC8.6 Success** — `MessageOrigin.author` variants correctly tag Partner / Human / Agent / System turns.
  - Type: unit + integration
  - File: unit at `crates/pattern_core/src/types/origin.rs`; integration at `crates/pattern_runtime/tests/origin_short_circuit.rs`
  - Verified by: Phase 4 Task 1 (bypass helper), Phase 5 Task 5

- **AC8.7 Success** — human-as-caller uses fronting persona's SessionContext (no fresh context).
  - Type: integration
  - File: `crates/pattern_runtime/tests/origin_short_circuit.rs` (+ `fronting_supervisor.rs`)
  - Verified by: Phase 5 Task 5, Phase 5 Task 7

- **AC8.8 Edge** — fronting update during in-flight messages: queued uses old routing, new uses new.
  - Type: integration
  - File: `crates/pattern_runtime/tests/fronting_dispatch.rs`; plus RPC-driven variant at `crates/pattern_server/tests/fronting_rpc.rs`
  - Verified by: Phase 5 Task 4 (routing shape), Phase 5 Task 6 (RPC interleave)

### v3-multi-agent.AC9: Agent registry

- **AC9.1 Success** — `ctx.constellation.list()` returns personas with status / relationships / groups.
  - Type: unit + integration
  - File: unit at `crates/pattern_db/src/queries/constellation.rs`; integration at `crates/pattern_runtime/tests/constellation_sdk.rs`
  - Verified by: Phase 6 Task 4 (query), Phase 6 Task 5 (SDK surface)

- **AC9.2 Success** — `ctx.constellation.find(project, SupervisorOf)` filters by project + relationship.
  - Type: unit + integration
  - File: `crates/pattern_db/src/queries/constellation.rs` + `crates/pattern_runtime/tests/constellation_sdk.rs`
  - Verified by: Phase 6 Task 4, Phase 6 Task 5

- **AC9.3 Success** — named group with project scope visible only in that project's context.
  - Type: unit + integration
  - File: `crates/pattern_db/src/queries/constellation.rs` + `crates/pattern_runtime/tests/constellation_sdk.rs`
  - Verified by: Phase 6 Task 3 (types), Phase 6 Task 4 (query), Phase 6 Task 5 (SDK)

- **AC9.4 Success** — sibling spawn auto-registers; immediately visible in list().
  - Type: integration
  - File: `crates/pattern_runtime/tests/sibling_autoregister.rs`
  - Verified by: Phase 6 Task 6

- **AC9.5 Failure** — nonexistent-project query returns empty, not error.
  - Type: unit
  - File: `crates/pattern_db/src/queries/constellation.rs`
  - Verified by: Phase 6 Task 4

- **AC9.6 Edge** — Draft personas listed with `status: Draft`; discoverable but not steppable.
  - Type: integration
  - File: `crates/pattern_runtime/tests/constellation_sdk.rs`
  - Verified by: Phase 6 Task 5 (cross-references Phase 4 AC6.5 draft-queue semantics)

### v3-multi-agent.AC10: End-to-end integration

- **AC10.1 Success** — smoke test passes deterministically (two-persona constellation, delegation, capability enforcement).
  - Type: e2e (smoke)
  - File: `crates/pattern_runtime/tests/multi_agent_smoke.rs`
  - Verified by: Phase 7 Task 5

- **AC10.2 Success** — mock `ProviderClient`; no live model.
  - Type: e2e
  - File: `crates/pattern_runtime/tests/multi_agent_smoke.rs` + `crates/pattern_runtime/tests/support/multi_agent_scripts.rs`
  - Verified by: Phase 7 Task 1 (scripted fixtures), Phase 7 Task 5

- **AC10.3 Success** — fork-and-merge flow: lightweight fork, write, merge_back, parent sees merged state.
  - Type: e2e (smoke step 6)
  - File: `crates/pattern_runtime/tests/multi_agent_smoke.rs`
  - Verified by: Phase 7 Task 5 (step 6)

- **AC10.4 Success** — Haskell delegation modules importable + functional.
  - Type: integration (multi_module_sdk compile) + e2e
  - File: `crates/pattern_runtime/tests/multi_agent_smoke.rs`; module compile checks follow `crates/pattern_runtime/tests/multi_module_sdk.rs` pattern
  - Verified by: Phase 7 Tasks 2/3/4 (per-module compile), Phase 7 Task 5 (composed)

- **AC10.5 Failure** — any smoke-step failure identifies the step + assertion.
  - Type: e2e (assertion-message contract)
  - File: `crates/pattern_runtime/tests/multi_agent_smoke.rs`
  - Verified by: Phase 7 Task 5 (each `assert*!` carries a `"step N: ..."` context message)

- **AC10.6 Edge** — smoke test runs concurrently with other `pattern-runtime` tests; no shared-state interference.
  - Type: e2e (unique-temp-dir contract under `cargo nextest run`)
  - File: `crates/pattern_runtime/tests/multi_agent_smoke.rs`
  - Verified by: Phase 7 Task 5 (step 8)

## Human verification

None. Every AC case in the design has an automated verification strategy
defined in the phase files. AC cases that require platform-dependent tooling
(jj CLI for AC4.2 / AC4.4 / AC4.6 / AC4.10) are still automated; Phase 3's
notes for the executor require fixing the CI image rather than stubbing, and
the Nix devshell ships jj today.

## Flags / gaps

- **No mismatches observed between phase-level "Acceptance Criteria Coverage"
  sections and per-task `Verifies:` labels.** Cross-checks performed:
  - Phase 1 coverage lists AC1.1–1.6 and AC2.1–2.9; every AC has at least one
    task with a matching `Verifies:` line (AC1.1 → Tasks 2+3; AC1.4 → Tasks
    2+3; AC2.7 → Tasks 11/12/15; others 1-to-1 or 1-to-N).
  - Phase 2 coverage lists AC3.1–3.7, AC5.1/5.2/5.3/5.4/5.6, and AC4.7/4.8
    scaffold. Each listed AC maps to at least one task. AC5.5 + AC5.7 are
    explicitly deferred to Phase 6 (called out in the phase narrative), and
    Phase 6's coverage section claims them — matches.
  - Phase 3 coverage lists AC4.1–4.10; every AC has a dedicated task. AC4.7
    + AC4.8 are verified end-to-end here after Phase 2's scaffold — matches.
  - Phase 4 coverage lists AC6.1–6.6 and AC7.1–7.7; each AC has a task. AC7.3's
    `Verifies:` sits on Task 9 (alongside AC7.5) and consumes Phase 4 Task 8's
    subscriber hook — consistent.
  - Phase 5 coverage lists AC8.1–8.8; each AC has a task.
  - Phase 6 coverage lists AC5.5, AC5.7, AC9.1–9.6; each AC has a task.
  - Phase 7 coverage lists AC10.1–10.6; Task 5 is the single smoke test that
    verifies all six (supported by Tasks 1–4 for the fixtures and delegation
    modules).
- **Deliberate design-level overlap, not a mismatch.** AC1.2 is called out in
  Phase 1 Task 4 (primary verifier) AND re-exercised end-to-end in Phase 7
  Task 5 step 5 (smoke). The smoke is a composition check, not a duplicate
  verifier; the design plan calls this out in §Testing.
- **Tooling prerequisite (not an AC gap).** AC4.2 / AC4.4 / AC4.6 / AC4.10
  depend on the `jj` CLI being present; Phase 3's executor notes require the
  Nix devshell or a CI image fix rather than `#[ignore]`-ing the tests.
