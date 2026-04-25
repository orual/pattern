# v3-multi-agent Phase 1: Capability system

**Goal:** introduce `CapabilitySet` + `EffectCategory` as pure data types in `pattern_core`, wire compile-time prelude filtering through the existing `canonical_effect_decls()` pipeline, rebuild `PermissionBroker` as a per-runtime instance on `jiff`, lay down policy-rule types with Rust defaults + KDL overrides, and add shape-based detection for pattern-config KDL writes.

**Architecture:** layer 1 (visibility) filters the `Vec<EffectDecl>` returned by `canonical_effect_decls()` before `preamble::build()` concatenates it, so excluded effects never reach the Haskell compiler and agent programs referencing them fail at Tidepool compile. Layer 2 (runtime approval) evaluates `PolicyRule`s at effect-dispatch time, escalating to the new `PermissionBroker` instance when rules require human approval. Config-KDL shape detection is a Rust default — a pure predicate that inspects proposed file writes — so it can be wired into the File handler's eventual real implementation (currently a stub; full `File.Write` implementation is out of scope for this phase and tracked by the sandbox-io plan).

**Tech Stack:** Rust (`pattern_core`, `pattern_runtime`), `knus` 3.3 (KDL parsing, already in workspace), `jiff` 0.2 (already a workspace dep, feature `serde`), `thiserror`, `tokio` (mpsc/broadcast/oneshot patterns per existing broker), `proptest` / `insta` for serialization-shaped assertions.

**Scope:** 1 of 7 phases. Delivers CapabilitySet + prelude filtering + policy evaluation + broker v2 + config-KDL shape detection. Does NOT implement the full File handler (separate plan).

**Codebase verified:** 2026-04-23 — investigation findings below reflect in-tree state at HEAD (commit 41cdae3e on current change). Plan 2 (task-skill-blocks) is mid-landing; Phase 1's touchpoints do not overlap with Plan 2 code.

---

## Codebase verification findings

- ✓ `canonical_effect_decls()` lives at `crates/pattern_runtime/src/sdk/bundle.rs:59`, returns `Vec<EffectDecl>` (list-level filtering works). `EffectDecl` has `type_name: &str`, `constructors: &[&str]`, `helpers: &[&str]`.
- ✓ `CANONICAL_EFFECT_ROW` at `bundle.rs:65` enumerates 14 effects: `Memory, Search, Recall, Message, Display, Time, Log, Shell, File, Sources, Mcp, Rpc, Spawn, Diagnostics`.
- ✗ Design lists `Tasks / Wake / Scope` as effect categories. Reality: `Tasks` lands in Plan 2 (not yet wired into `CANONICAL_EFFECT_ROW`), `Wake` lands in Phase 4 of this plan, `Scope` is not an effect — it's a helper in `handlers/scope.rs`. **`EffectCategory` enum must be defined as the union of current effects + forward-reserved slots (Tasks, Wake) so Plan 2 / later phases can flip them on without schema churn.**
- ✓ `preamble::build(decls: &[EffectDecl])` at `crates/pattern_runtime/src/sdk/preamble.rs:31`. Filtering inserts here — callers pass `canonical_effect_decls()` today; new path passes the filtered slice.
- ✓ Session open at `crates/pattern_runtime/src/session.rs:636` currently calls `preamble::build(&canonical_effect_decls())`. This is the single seam we need to rewire.
- ✓ `PermissionBroker` lives at `crates/pattern_core/src/permission.rs:54`. Shape: `broadcast::Sender<PermissionRequest>` + `HashMap<id, oneshot::Sender<PermissionDecisionKind>>`. Approval variants: `Deny`, `ApproveOnce`, `ApproveForScope`, `ApproveForDuration(std::time::Duration)`.
- ✗ Design says "rebuilt from `chrono`" — actually broker uses `std::time::Duration` for timeouts and `chrono::DateTime<chrono::Utc>` for `PermissionGrant.expires_at`. Migration target is `jiff::Span` / `jiff::Timestamp`.
- ⚠ `PermissionBroker::new()` is private (unrestricted `fn new()`). Grep for the singleton constructor to confirm who instantiates it; Phase 1 moves to a `pub fn new()` constructed per-runtime.
- ✓ `MemoryPermission` enum + `memory_acl::check()` at `crates/pattern_core/src/memory_acl.rs`. Variants referenced in `check()`: `Append, ReadWrite, Admin, Human, Partner, ReadOnly`. Stays unchanged.
- ✓ `FileHandler` is a stub at `crates/pattern_runtime/src/sdk/handlers/file.rs:43` returning `EffectError::Handler("…not implemented…filesystem-sandbox plan")`. **Phase 1 does NOT implement `File.Write`** — it defines the shape-detection predicate and wires it into the policy pipeline so the eventual `File.Write` impl picks it up automatically.
- ✓ `knus` 3.3 + `jiff` 0.2 are already workspace deps. `knus::Decode` derive is the established pattern — see `crates/pattern_memory/src/config/pattern_kdl.rs` for a reference `MountConfig`.
- ✓ `pattern_core` is trait/data-only (confirmed at `crates/pattern_core/src/lib.rs:13-26`). `CapabilitySet` + `EffectCategory` as pure enums/structs respects this.
- ✓ Persona KDL schema lives at `crates/pattern_runtime/src/persona_loader.rs`. Adding `capabilities {}` / `policy {}` blocks extends the existing `PersonaSnapshot` `Decode` derive.
- + Unrelated bonus: `PersonaSnapshot.enabled_tools` was already removed (see `pattern_core/CLAUDE.md`) with a note that "permission/capability control will return via effect-level prelude filtering + per-effect permission structures in a future phase" — **this phase**. That cleanup is still applicable; no residual `enabled_tools` plumbing should be re-introduced.

### Design decisions (resolved against the codebase)

- **Broker stays in `pattern_core`.** `permission.rs` already imports `tokio::sync::{RwLock, broadcast, oneshot}` and `provider_client.rs` defines async trait methods — `pattern_core` is a trait-+-coordination-primitives crate in practice, not strictly-trait-only. The broker is a data-bus (coordination), not execution logic. Task 5 collapses to refactor-in-place: make `PermissionBroker::new()` pub, remove any singleton, swap chrono→jiff. No trait relocation, no crate move.
- **AC2.7 is verified end-to-end in Task 15.** The File handler's `Write` arm evaluates the policy pipeline and short-circuits on `Deny` / `RequireApproval` before any real write logic runs. Actual write mechanics (path sandboxing, fs operations) remain the sandbox-io plan's responsibility, but the gate is live.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-multi-agent.AC1: CapabilitySet and prelude filtering

- **v3-multi-agent.AC1.1 Success:** `CapabilitySet` with `[Memory, Message, Tasks]` produces a prelude containing only those effect GADTs; `Spawn`, `Shell`, `Wake` constructors are absent from the generated Haskell source
- **v3-multi-agent.AC1.2 Success:** Agent program referencing an excluded effect (e.g., `ctx.shell.execute`) fails at Tidepool compilation with a clear "unknown constructor" error, not a runtime error
- **v3-multi-agent.AC1.3 Success:** Agent program using only included effects compiles and executes normally
- **v3-multi-agent.AC1.4 Success:** `CapabilitySet::all()` produces a prelude identical to the unfiltered `canonical_effect_decls()` output
- **v3-multi-agent.AC1.5 Failure:** Attempting to construct a CapabilitySet that adds capabilities not present in the parent's set (for ephemeral/fork) returns `CapabilityError::Escalation`
- **v3-multi-agent.AC1.6 Edge:** Empty CapabilitySet (no effects) produces a prelude with only base types and no effect constructors; agent can still compile a program that does pure computation

### v3-multi-agent.AC2: Runtime approval and policy

- **v3-multi-agent.AC2.1 Success:** Rust default policy gates destructive shell commands (`rm -rf`, `sudo`); agent with Shell capability gets `PermissionRequired` on these commands
- **v3-multi-agent.AC2.2 Success:** KDL config loosens a Rust default (e.g., allows `git push` without gating); agent executes the command without broker intervention
- **v3-multi-agent.AC2.3 Success:** KDL config tightens beyond defaults (e.g., gates all file writes, not just config files); agent gets `PermissionRequired` on any file write
- **v3-multi-agent.AC2.4 Success:** PermissionBroker approve-once allows the specific invocation; subsequent identical invocation is gated again
- **v3-multi-agent.AC2.5 Success:** PermissionBroker approve-for-scope allows all invocations matching the scope pattern until session ends
- **v3-multi-agent.AC2.6 Success:** PermissionBroker approve-for-duration allows invocations for the specified jiff duration; invocation after expiry is gated again
- **v3-multi-agent.AC2.7 Failure:** Agent attempts to write a file that parses as pattern config KDL; write is gated regardless of KDL config settings (Rust default, cannot be loosened) — verified end-to-end in Phase 1 via Task 15's gate-evaluating `File.Write` dispatch.
- **v3-multi-agent.AC2.8 Failure:** PermissionBroker request times out (no human response); effect returns denial, not hang
- **v3-multi-agent.AC2.9 Edge:** PermissionBroker is per-runtime instance; two runtime instances have independent broker state and pending request queues

### Note on AC2.1 / AC2.2 end-to-end

AC2.1 and AC2.2 both exercise the Shell effect. The Shell handler is implemented (not a stub). Phase 1 wires the policy pipeline between the Shell handler and the broker; scripted tests exercise this path without invoking a live shell (we assert `PermissionRequired` surfaces for the right commands, not that `rm -rf` actually runs).

---

<!-- START_SUBCOMPONENT_A (tasks 1-2) -->

<!-- START_TASK_1 -->
### Task 1: `EffectCategory` and `CapabilitySet` data types in `pattern_core`

**Verifies:** none directly (types are structural; behaviour verified by Task 3+). Types compiled + serde-roundtrippable.

**Files:**
- Create: `crates/pattern_core/src/capability.rs`
- Modify: `crates/pattern_core/src/lib.rs` (add `pub mod capability;` and re-export `CapabilitySet`, `EffectCategory`, `CapabilityError`)
- Test: unit tests in the new file.

**Implementation:**

Define `EffectCategory` as a `#[non_exhaustive]` enum covering all 14 current canonical effects **plus** forward-reserved slots for `Tasks` and `Wake`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EffectCategory {
    Memory,
    Search,
    Recall,
    Message,
    Display,
    Time,
    Log,
    Shell,
    File,
    Sources,
    Mcp,
    Rpc,
    Spawn,
    Diagnostics,
    Tasks, // reserved for Plan 2 task effect
    Wake,  // reserved for Phase 4 wake-condition effect
}
```

Define `CapabilityFlag` — an orthogonal set of boolean rights that a CapabilitySet grants beyond effect-category access:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CapabilityFlag {
    /// Permits spawning a persona with a fresh identity (consumed in Phase 2
    /// Task 7 and Phase 3 Task 7). Default off.
    SpawnNewIdentities,
    /// Permits registering custom Haskell wake conditions (consumed in
    /// Phase 4 Task 9). Default off.
    WakeConditionRegistration,
    /// Permits setting the FrontingSet or routing rules (consumed in
    /// Phase 5 Task 6). Default off.
    FrontingControl,
}
```

Define `CapabilitySet` as a struct carrying both effect categories and flags:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub categories: BTreeSet<EffectCategory>,
    pub flags: BTreeSet<CapabilityFlag>,
}
```

Provide constructors: `CapabilitySet::empty()`, `CapabilitySet::all()` (every `EffectCategory` variant + every `CapabilityFlag` variant — "godmode"), `CapabilitySet::from_iter(…)` (categories only; flags default empty). Methods: `contains(cat) -> bool`, `has_flag(flag: CapabilityFlag) -> bool`, `iter_categories()`, `iter_flags()`, `is_subset_of(other)`, `restrict_to(other: &CapabilitySet) -> Result<Self, CapabilityError>` — the restriction check enforces BOTH `self.categories ⊆ other.categories` AND `self.flags ⊆ other.flags`.

Define `CapabilityError` with `thiserror`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityError {
    #[error("capability escalation: cannot add categories {added_categories:?} or flags {added_flags:?} to a set restricted to categories {parent_categories:?} flags {parent_flags:?}")]
    Escalation {
        added_categories: Vec<EffectCategory>,
        added_flags: Vec<CapabilityFlag>,
        parent_categories: Vec<EffectCategory>,
        parent_flags: Vec<CapabilityFlag>,
    },
    #[error("capability denied: effect {category:?} not present in set")]
    Denied { category: EffectCategory },
    #[error("capability flag denied: {flag:?} not present in set")]
    FlagDenied { flag: CapabilityFlag },
}
```

Notes:
- Error messages lowercase, sentence fragments per project conventions.
- No runtime behaviour beyond data + predicates. `pattern_core` trait-only spirit preserved.
- `#[non_exhaustive]` everywhere per project convention.
- `CapabilityFlag` variants are reserved for forward-use; Phase 1 doesn't wire any flag-gated behaviour itself, but the schema is set so Phase 2 / 4 / 5 can add gates without touching Phase 1 data types.

**Testing:**
- Unit: `CapabilitySet::all()` contains every EffectCategory variant and every CapabilityFlag variant. Use a manual match that covers every variant of each enum (adding a new variant forces the test to update).
- Unit: `restrict_to` returns `Err(Escalation{..})` when expanding categories beyond parent OR expanding flags beyond parent; returns `Ok` otherwise.
- Unit: `CapabilitySet::default() == empty()` — no categories, no flags.
- Unit: `has_flag(SpawnNewIdentities)` returns false on a default set; returns true after `set.flags.insert(SpawnNewIdentities)`.
- proptest (`serde_json`): round-trip `CapabilitySet` including flags — parse-serialize-parse.

**Verification:**
`cargo nextest run -p pattern-core capability`
Expected: all new tests pass; full suite still green.

**Commit:** `[pattern-core] add CapabilitySet, EffectCategory, CapabilityFlag types`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `CapabilitySet` ↔ `EffectDecl` alignment test

**Verifies:** foundation for AC1.1, AC1.4 (ensures the data model matches the handler reality).

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` (add a new test — keep in the existing `#[cfg(test)]` module).

**Implementation:**
New test `canonical_row_matches_effect_category_implemented_set` that asserts every string in `CANONICAL_EFFECT_ROW` has a matching `EffectCategory` variant, and vice versa (excluding the forward-reserved `Tasks` and `Wake` variants). This catches drift when someone adds an effect to either side without the other.

Provide a small helper in `bundle.rs` (test-only, `#[cfg(test)]`) that maps `&str` → `Option<EffectCategory>`; this helper stays local — if it grows a real user we promote it in a follow-up. Do **not** pre-abstract.

**Testing:**
- Unit: `cargo nextest run -p pattern-runtime canonical_row_matches_effect_category_implemented_set`

**Verification:**
Expected: new assertion passes; adding a 15th handler to `CANONICAL_EFFECT_ROW` without a matching `EffectCategory` variant fails the test.

**Commit:** `[pattern-runtime] cross-check CANONICAL_EFFECT_ROW against EffectCategory`
<!-- END_TASK_2 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 3-4) -->

<!-- START_TASK_3 -->
### Task 3: Prelude-filtering function in `pattern_runtime`

**Verifies:** AC1.1, AC1.4, AC1.6 (pure-filtering behaviour).

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` — add `pub fn filtered_effect_decls(caps: &CapabilitySet) -> Vec<EffectDecl>`.
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs` — add `pub fn build_for(caps: &CapabilitySet) -> String` that calls `filtered_effect_decls` then `build(&decls)`. Keep the existing `build(decls)` signature for tests.

**Implementation:**

```rust
pub fn filtered_effect_decls(caps: &CapabilitySet) -> Vec<EffectDecl> {
    canonical_effect_decls()
        .into_iter()
        .filter(|decl| {
            EffectCategory::from_type_name(decl.type_name)
                .map(|c| caps.contains(c))
                .unwrap_or(false)
        })
        .collect()
}
```

Add `EffectCategory::from_type_name(&str) -> Option<EffectCategory>` to `pattern_core::capability` (matches the `type_name: &str` in `EffectDecl`). Use `str::eq_ignore_ascii_case` to be robust to naming drift.

Update `preamble.rs::build(&[EffectDecl])` so that when the slice is empty, the prelude still emits base imports (`Pattern.Prelude`, `Data.Text`, `Data.Map.Strict`, etc.) and `type M = '[]` — no effect rows, no effect imports. This is AC1.6.

**Testing:**
- Unit: `filtered_effect_decls(&CapabilitySet::all())` has the same `type_name` list (in the same order) as `canonical_effect_decls()` (AC1.4).
- Unit: `filtered_effect_decls(&cap_set([Memory, Message]))` excludes `Shell`, `Spawn`, etc. (AC1.1).
- Unit: `filtered_effect_decls(&CapabilitySet::empty())` returns an empty vec.
- Snapshot (`insta`): `build_for(&cap_set([Memory, Display]))` — locks the shape of the filtered prelude. Store under `crates/pattern_runtime/src/snapshots/`. AC1.6 is covered by a separate snapshot for the empty set.

**Verification:**
`cargo nextest run -p pattern-runtime preamble` + `cargo nextest run -p pattern-runtime bundle`

**Commit:** `[pattern-runtime] add capability-filtered preamble builder`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: Wire `CapabilitySet` through session open

**Verifies:** AC1.2, AC1.3 (filtered prelude reaches the Haskell compiler).

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs` — session open / `build()` paths that currently call `preamble::build(&canonical_effect_decls())` (around line 636) now thread an `Option<&CapabilitySet>` through from `SessionOpenConfig` (the existing config struct opened sessions with). `None` is treated as "all capabilities" to preserve back-compat during the rewrite.
- Modify: `crates/pattern_runtime/src/sdk/preamble.rs` (if needed) — no public API change beyond Task 3.
- Test: `crates/pattern_runtime/tests/capability_compile.rs` (new integration test).

**Implementation:**
The session open path currently hands a fixed prelude to the Haskell compiler. Add a field to the relevant builder (name it `capabilities: Option<CapabilitySet>`; default `None`) and pass through to `preamble::build_for`. The existing `PersonaSnapshot` structure gets no new fields yet — that's Task 13/14. For now the only way to set `capabilities` is programmatically; callers that don't supply one still get the full row.

For AC1.2/AC1.3: write an integration test `tests/capability_compile.rs` that:
1. Opens a session with `capabilities = CapabilitySet::from_iter([Memory, Message])`.
2. Submits a Haskell program that calls `Shell.execute "…"` — assert the `tidepool-extract` compile step returns an error containing "unknown" / "not in scope" / similar, NOT a runtime error. Use pattern matching on the error string plus commentary; flakiness against upstream Tidepool error wording is acceptable (if the wording shifts in the future, we update the test).
3. Submits a Haskell program that calls only `Memory.get` / `send` — assert session compiles and runs to completion.

The integration test requires the `tidepool-extract` binary (`$TIDEPOOL_EXTRACT`). Follow `pattern_runtime/CLAUDE.md` setup (Nix devshell or env override). Gate via `#[ignore]` with a justification comment **only if** CI cannot resolve the binary; aim to keep it in the default run.

**Testing:**
- Integration: the two cases above.
- Unit: none additional.

**Verification:**
`cargo nextest run -p pattern-runtime capability_compile --nocapture`
Expected: compile-failure case matches expected error; compile-success case runs to a normal completion event.

**Commit:** `[pattern-runtime] thread CapabilitySet through session open`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 5-7) -->

<!-- START_TASK_5 -->
### Task 5: Make `PermissionBroker` per-runtime (in place)

**Verifies:** none directly — sets up AC2.4 / AC2.5 / AC2.6 / AC2.8 / AC2.9.

**Files:**
- Modify: `crates/pattern_core/src/permission.rs` — make `fn new()` pub; keep struct + impls in place.
- Delete: any global singleton (grep for `Lazy.*PermissionBroker`, `OnceCell.*PermissionBroker`, `static.*PermissionBroker` across the workspace).
- Modify: call sites of the singleton — each now accepts the broker as a dependency.

**Implementation:**
The broker's struct shape stays exactly as it is today — `pattern_core::permission.rs` already imports `tokio::sync::{RwLock, broadcast, oneshot}` and hosts async methods, so keeping a coordination primitive here is consistent with established precedent. The refactor is mechanical:

1. `rg -F "PermissionBroker::" crates/` — identify every call site.
2. Grep for the singleton constructor (`Lazy` / `OnceCell` / `static`).
3. Delete the singleton. Make `PermissionBroker::new()` pub.
4. Thread `Arc<PermissionBroker>` through runtime construction (Task 7 wires it onto `SessionContext`).

**Testing:**
- Unit: existing permission tests still pass unchanged.
- Unit: two broker instances created independently have separate pending queues (belt-and-suspenders with Task 6's AC2.9 coverage).

**Verification:**
`cargo nextest run -p pattern-core permission`
Expected: all pre-existing tests still pass; no references to a global `PermissionBroker` remain.

**Commit:** `[pattern-core] make PermissionBroker per-runtime; remove global singleton`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: `PermissionBroker` v2 — jiff durations, origin-aware request, approve-for-scope plumbing

**Verifies:** AC2.4, AC2.5, AC2.6, AC2.8, AC2.9.

**Files:**
- Modify: `crates/pattern_core/src/permission.rs` — change `PermissionGrant.expires_at` from `chrono::DateTime<chrono::Utc>` to `jiff::Timestamp`; add `PermissionDecisionKind::ApproveForDuration(jiff::Span)` (replacing `std::time::Duration`); add the approve-for-scope cache; extend `request()` to take `origin: &MessageOrigin` and short-circuit on `origin.bypasses_permission_gate()` (Partner only, per Phase 4 Task 1's helper).
- Modify: any callers that build `PermissionDecisionKind::ApproveForDuration` or read `PermissionGrant.expires_at` or call `request()`.

**Implementation:**
Swap chrono for jiff using the crate-level `jiff::Timestamp` / `jiff::Span`. Reason about expiry with `now + span`. Keep the `request()` method's external `timeout: std::time::Duration` — this is a host-side timeout and doesn't need jiff; the *grant* duration is the one that flows into the agent-visible data.

Extend `request` signature with `origin: &MessageOrigin`. The broker short-circuits at the top: `if origin.bypasses_permission_gate() { return Some(PermissionGrant::synthesized_partner(req.scope.clone())); }`. The helper itself ships in this phase as `matches!(self.author, Author::Partner(_))` — it's a pure predicate on `MessageOrigin`, no Phase 4 dependency.

**Important — what `origin` actually is at handler-dispatch sites:** the broker reads "who is asking right now," not "what activated this turn." During a normal model-driven turn loop, the immediate caller of every effect is the agent itself (the model emits a tool_use, the eval worker dispatches, the handler runs). So at handler-dispatch sites the origin is `Author::Agent(self)`, **not** the activating Partner's origin. Partner-bypass therefore does NOT fire during normal autonomous activity inside a Partner-activated turn — that prevents "the user typed a message, so the agent can now `rm -rf` without prompting." Partner-bypass fires only from explicit direct-execution paths (admin REPL, audited sandboxed code, debug surfaces) that *intentionally* set the dispatch origin to a Partner. Phase 1 has no such paths, so the bypass is wired but inert during normal flow; Task 7 sets up the slot semantics, and Tasks 10 / 15 verify gates fire in the normal case.

Add `PermissionGrant::synthesized_partner(scope: PermissionScope) -> PermissionGrant` constructor that produces a grant with a fresh id, no `expires_at`, and a marker in metadata (`{"source": "partner_bypass"}`) for audit.

Add an `approve_for_scope` behaviour: when a grant returns `ApproveForScope`, the broker records the `PermissionScope` in an in-memory "session scope cache" (keyed by `(agent_id, scope)`). Subsequent requests matching the cached scope return without re-broadcasting. Cache is per-broker-instance (per-runtime), so two runtimes have independent caches (AC2.9).

`ApproveForDuration(jiff::Span)` stores `expires_at = jiff::Timestamp::now() + span` on the grant. Subsequent matching requests check `now < expires_at` before returning without broadcast.

Timeout path (AC2.8): `request()` already `tokio::time::timeout`s on the oneshot — ensure the timeout returns `None` (denial) and does NOT leak a pending entry in `self.pending` or `self.pending_info`. Today the map entries are populated before the await; add cleanup in the `Err(_timeout)` arm. Write an explicit test for this leak.

**Testing:**
- Unit: request flow end-to-end with synthetic `subscribe()` recipient that calls `respond()` — cover ApproveOnce, ApproveForScope (two calls, second short-circuits), ApproveForDuration (advance `jiff::Timestamp` via injected clock), timeout case.
- Inject a `fn now_fn: Arc<dyn Fn() -> jiff::Timestamp + Send + Sync>` so duration tests don't sleep. Default production constructor uses `jiff::Timestamp::now`.
- Unit: two broker instances with independent scope caches — approving a scope on instance A does not carry to instance B (AC2.9).
- Unit: Partner-bypass predicate — construct an origin with `Author::Partner(...)`, call `request(..., &origin, ...)` *directly on the broker* (this isolates the predicate; Phase 1 sessions never feed Partner origin to the broker via the bridge). Assert returns `Some(PermissionGrant)` with `source: partner_bypass` marker WITHOUT broadcast.
- Unit: non-Partner origins (`Author::Human(_)`, `Author::Agent(_)`, `Author::System`) do NOT short-circuit — broadcast fires normally.

**Verification:**
`cargo nextest run -p pattern-runtime permission`
Expected: all new behaviour covered; no panics / leaks on timeout.

**Commit:** `[pattern-runtime] rebuild PermissionBroker on jiff with scope + duration caches`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Thread per-runtime broker + `PermissionBridge` + current-dispatch origin through handler contexts

**Verifies:** AC2.9, and the plumbing that Task 10 (Shell) and Task 15 (File) rely on.

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs` — construct the broker alongside the runtime, store as `Arc<PermissionBroker>`, expose through `SessionContext`. Add a `current_dispatch_origin: Arc<std::sync::RwLock<Option<MessageOrigin>>>` field — written by `agent_loop::drive_step` per orchestrate iteration, read by handlers.
- Create: `crates/pattern_runtime/src/permission.rs` (single file; promote to a directory only if a second submodule lands later) — `PermissionBridge` type following the `RouterBridge` (`crates/pattern_runtime/src/router.rs`) pattern: a sync-to-async bridge so handlers on the sync `EvalWorker` thread can request broker grants without `futures::executor::block_on`. One bridge per broker instance.
- Modify: `crates/pattern_runtime/src/agent_loop.rs` — `drive_step` builds an Agent-origin (`Author::Agent { agent_id }`) per orchestrate iteration and writes it to `ctx.current_dispatch_origin` via an RAII guard scoped to that iteration; clears on Drop (panic-safe). The same value is reused for the existing `output_origin` persistence at the bottom of the iteration so we don't construct it twice.
- Modify: any handler that will consult the broker — expose via a new `HasPermissionBridge` trait alongside `HasCancelState`.

**Why dispatch-origin, not turn-origin:** the broker's bypass check answers "who is asking right now," not "what activated this turn." During autonomous model-driven activity inside a Partner-activated turn, the immediate caller of every effect is the agent itself — the slot must reflect that, not the activating Partner. Pinning the activating Partner's origin would let any tool_use the model emits run with the Partner's elevated permissions ("agent inherits user permissions"), which defeats the gate. The slot is therefore named `current_dispatch_origin` and is set per-iteration to `MessageOrigin::new(Author::Agent { agent_id }, cur_input.origin.sphere)`. Future direct-execution paths (admin REPL, audited sandboxed code) are responsible for overriding the slot with a Partner origin before invoking handlers — Phase 1 has none.

**Implementation:**

```rust
// session.rs
pub trait HasPermissionBridge {
    fn permission_bridge(&self) -> Option<&Arc<PermissionBridge>>;
    fn current_dispatch_origin(&self) -> Option<MessageOrigin>;
}

impl HasPermissionBridge for SessionContext {
    fn permission_bridge(&self) -> Option<&Arc<PermissionBridge>> {
        self.permission_bridge.as_ref()
    }
    fn current_dispatch_origin(&self) -> Option<MessageOrigin> {
        self.current_dispatch_origin.read().ok()?.clone()
    }
}

// permission.rs — follows RouterBridge shape (router.rs).
// tokio::sync::mpsc inbound (UnboundedSender::send is non-tokio-thread safe);
// std::sync::mpsc::sync_channel for the reply path so the eval-worker
// thread can block on recv without tokio context.
pub struct PermissionBridge {
    tx: tokio::sync::mpsc::UnboundedSender<PermissionBridgeRequest>,
}

// agent_loop.rs — per-iteration RAII guard. The Agent-origin is
// constructed ONCE per iteration and reused: the existing
// `output_origin` construction at the bottom of the iteration is
// replaced with this same value, so handlers and persistence see
// identical attribution.
struct CurrentDispatchOriginGuard {
    slot: Arc<std::sync::RwLock<Option<MessageOrigin>>>,
}
impl CurrentDispatchOriginGuard {
    fn enter(ctx: &SessionContext, origin: &MessageOrigin) -> Self { /* … */ }
}
impl Drop for CurrentDispatchOriginGuard { /* clears slot, panic-safe */ }

// inside drive_step's per-iteration loop:
let dispatch_origin = MessageOrigin::new(
    Author::Agent(AgentAuthor { agent_id: AgentId::from(ctx.agent_id()) }),
    cur_input.origin.sphere,
);
let _origin_guard = CurrentDispatchOriginGuard::enter(&ctx, &dispatch_origin);
let turn = orchestrate(/* … */).await?;
// … later in the same iteration, persistence reuses `dispatch_origin`
// in place of the previous output_origin construction.
```

`SessionContext` constructs the broker eagerly in `from_persona` (sync); the bridge is wired by a `with_permission_bridge` builder called from `open_with_agent_loop` (async — tokio task spawn requires a runtime). The Bridge's `request_sync` matches the broker's `request` parameters (agent_id, tool_name, scope, &origin, reason, metadata, timeout). Handlers that need the broker take `U: HasCancelState + HasPermissionBridge` and call `cx.user().permission_bridge().expect("…").request_sync(…)`.

Do not leave a backwards-compat shim. Delete the global (`pattern_core::permission::broker()`); callers fail to compile until updated. Fix every call site in the same task. (The only consumer of the singleton in the active workspace is none — the legacy `pattern_discord` references are out-of-workspace and don't currently build.)

**Testing:**
- Integration: spin up two `SessionContext`s with independent `PermissionBroker` instances; confirm approving a scope on one does not leak (AC2.9).
- Integration: Shell/File handler running on the sync EvalWorker thread issues `request_sync`; bridge correctly round-trips to the broker and back without deadlock. Use a scripted subscriber that responds in <10ms.
- Integration: panic in drive_step still clears `current_dispatch_origin` (RAII guard's Drop fires on unwind).
- Integration: a Partner-activated session whose model emits a tool_use call to a gated handler (Shell `rm -rf`) sees the gate fire — the dispatch origin during the model's autonomous activity is `Author::Agent`, NOT the activating Partner, so the bypass does NOT short-circuit. (This is verified end-to-end in Task 10's Shell tests, but is restated here as the wiring's correctness predicate.)

**Verification:**
`cargo nextest run` full suite. Expected: every handler that consults the broker reaches it through the per-session `SessionContext` via `PermissionBridge`; no `futures::executor::block_on` in any handler.

**Commit:** `[pattern-runtime] thread per-runtime PermissionBroker through SessionContext`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 8-10) -->

<!-- START_TASK_8 -->
### Task 8: `PolicyRule` types and `PolicySet` in `pattern_core`

**Verifies:** foundation for AC2.1, AC2.2, AC2.3.

**Files:**
- Create: `crates/pattern_core/src/capability/policy.rs` (submodule of `capability`).
- Modify: `crates/pattern_core/src/capability/mod.rs` or `crates/pattern_core/src/capability.rs` (promote to directory module) — re-export.

**Implementation:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyAction {
    Allow,
    RequireApproval { reason: Option<String> },
    Deny { reason: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PolicyRule {
    pub effect: EffectCategory,
    pub matcher: PolicyMatcher,
    pub action: PolicyAction,
    pub precedence: Precedence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PolicyMatcher {
    Always,
    ShellCommand { pattern: String }, // glob or prefix; document which in the doc comment
    FilePath { pattern: String },
    Scope(PermissionScope),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Precedence {
    RustDefault,   // baseline, lowest priority
    KdlConfig,     // loaded from .pattern.kdl or persona
    RuntimeOverride, // e.g. admin command, highest priority
}

pub struct PolicySet {
    rules: Vec<PolicyRule>,
}
```

`PolicySet` offers `evaluate(effect: EffectCategory, context: &PolicyContext) -> PolicyAction`, iterating rules in precedence order (`RuntimeOverride > KdlConfig > RustDefault`) and returning the first matching rule's action. `PolicyContext` carries the runtime details the matcher needs (a shell command string, a file path, a memory scope).

`PolicyMatcher::ShellCommand` semantics: accept a shell-style glob pattern (`*`, `?`, no brace-expansion). `regex` is already a workspace dep (verified `Cargo.toml: regex = "1"`) — translate the glob into a regex at rule-load time and match against the command string. Keep the glob vocabulary small (`*` → `.*`, `?` → `.`, `[...]` passes through), document it in the doc comment, and test both matching and non-matching cases for every supported metacharacter.

**Testing:**
- Unit: precedence ordering — a RuntimeOverride Deny beats a KdlConfig Allow beats a RustDefault RequireApproval.
- Unit: `PolicyMatcher::ShellCommand` matches `rm -rf /` and `rm -rf foo/bar` but not `ls`.
- Unit: empty `PolicySet` evaluates to `Allow` (policies are opt-in; the broker is the gate of last resort).

**Verification:**
`cargo nextest run -p pattern-core policy`

**Commit:** `[pattern-core] add PolicyRule, PolicyMatcher, PolicySet`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Rust default policies (conservative baseline)

**Verifies:** AC2.1.

**Files:**
- Create: `crates/pattern_runtime/src/policy/defaults.rs`.
- Modify: `crates/pattern_runtime/src/policy/mod.rs` (new directory module — create via `mod.rs`).

**Implementation:**
`rust_defaults()` returns a `Vec<PolicyRule>` with `Precedence::RustDefault`. Baseline entries:
- Shell: `RequireApproval` on `rm -rf`, `sudo`, `mkfs`, `dd if=`, `chmod -R 000`, matching prefix / glob per Task 8's decision.
- File: `RequireApproval` matcher `FilePath { pattern: "**/.pattern.kdl" }` — but this is a placeholder; the real shape-based detection lives in Task 11. Policy rule defers to the shape check.
- Spawn new identity: `RequireApproval` on spawn of new persona (this rule is exercised starting in Phase 2).

Keep the list short and documented. Each rule has a one-line `// why:` comment in the source. No speculative rules.

**Testing:**
- Unit: `PolicySet::from(rust_defaults()).evaluate(Shell, ctx{cmd="rm -rf /"})` returns `RequireApproval`.
- Unit: `evaluate(Shell, ctx{cmd="ls"})` returns `Allow`.

**Verification:**
`cargo nextest run -p pattern-runtime policy::defaults`

**Commit:** `[pattern-runtime] seed Rust default policy rules for shell + spawn`
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: Policy evaluation in the Shell handler dispatch path

**Verifies:** AC2.1 gate path (Deny end-to-end), AC2.2 gate-skip path (Allow end-to-end); real command-execution verification deferred to whenever the real Shell handler lands.

**Important scope note:** `crates/pattern_runtime/src/sdk/handlers/shell.rs` is currently a stub that returns `EffectError::Handler("not implemented in v3 foundation (phase: post-foundation shell-tool plan)")`. Task 10 wraps the existing stub with the policy gate — it does NOT implement real command execution. Tests assert on the distinct error prefixes from Task 15 (`PERMISSION_DENIED_PREFIX` on Deny; handler's existing "not implemented" string on Allow). When the real Shell handler eventually lands in its own plan, approve-path tests can assert command output.

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/shell.rs` — thread `PolicySet` into the handler via `SessionContext`, evaluate before command execution, escalate to `PermissionBridge::request_sync` on `RequireApproval`, reject on `Deny`. Preserve the existing "not implemented" stub error for the Allow / approved-after-gate path.
- Modify: `crates/pattern_runtime/src/session.rs` — `SessionContext` gains `policies: Arc<PolicySet>` constructed at open from `rust_defaults() ++ kdl_rules ++ runtime_rules`.

**Implementation:**

```rust
fn handle(&mut self, req: ShellReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
    let _guard = HandlerGuard::enter(&cx.user().cancel_state().gate);
    let policy_ctx = PolicyContext::Shell { command: &req.command };
    match cx.user().policies().evaluate(EffectCategory::Shell, &policy_ctx) {
        PolicyAction::Deny { reason } => Err(EffectError::Handler(format!(
            "{PERMISSION_DENIED_PREFIX}{}",
            reason.unwrap_or_else(|| "shell denied by policy".into()),
        ))),
        PolicyAction::RequireApproval { reason } => {
            let origin = cx.user().current_turn_origin().ok_or_else(|| EffectError::Handler(
                "shell: no current turn origin available".into()))?;
            let grant = cx.user().permission_bridge().request_sync(
                build_shell_request(&req.command, reason),
                &origin,
                request_timeout,
            );
            if grant.is_some() {
                // Gate cleared. Fall through to the existing stub (real execution
                // arrives in a later plan).
                Err(EffectError::Handler(
                    "Pattern.Shell.Execute is not implemented in v3 foundation \
                     (phase: post-foundation shell-tool plan). Gate cleared.".into(),
                ))
            } else {
                Err(EffectError::Handler(format!(
                    "{PERMISSION_DENIED_PREFIX}shell denied by broker",
                )))
            }
        }
        PolicyAction::Allow => Err(EffectError::Handler(
            "Pattern.Shell.Execute is not implemented in v3 foundation \
             (phase: post-foundation shell-tool plan).".into(),
        )),
    }
}
```

For Task 10 specifically, DO NOT yet wire KDL-loaded rules — that's Task 13/14. Construct `PolicySet` from `rust_defaults()` only.

**Testing:**
- AC2.1 Deny path: agent program calls `Shell.execute "rm -rf /tmp/testdir"`. Test harness subscribes to the broker and responds `Deny`. Assert agent sees `EffectError::Handler` whose message starts with `PERMISSION_DENIED_PREFIX`.
- AC2.1 Approve path (stub error): same program, broker responds `ApproveOnce`. Assert agent sees `EffectError::Handler` whose message contains `"Pattern.Shell.Execute is not implemented"` AND `"Gate cleared"`. This proves: (a) the gate fired, (b) approval was recognized, (c) execution stub is reached. Real execution verification is NOT in Phase 1 scope.
- AC2.2 gate-skip: `Shell.execute "ls"` (not matched by defaults). Assert NO broker request observed; agent sees the plain stub "not implemented" error (no "Gate cleared" marker — since the gate short-circuited on `Allow` without the explicit approval ceremony). This proves the gate isn't over-firing.
- Partner bypass (cross-check with Task 6's broker test): origin is `Author::Partner(...)`; the broker's short-circuit returns `Some` synthetically; agent sees the "Gate cleared" stub error WITHOUT the broker observing a request.

**Verification:**
`cargo nextest run -p pattern-runtime shell_policy`

**Commit:** `[pattern-runtime] gate Shell handler stub through PolicySet + PermissionBridge`
<!-- END_TASK_10 -->

<!-- END_SUBCOMPONENT_D -->

<!-- START_SUBCOMPONENT_E (tasks 11-12) -->

<!-- START_TASK_11 -->
### Task 11: Pure KDL-shape detection predicate for pattern config writes

**Verifies:** AC2.7 (unit-level; end-to-end deferred to the phase that lands `File.Write`).

**Files:**
- Create: `crates/pattern_runtime/src/policy/config_guard.rs`.
- Add to: `crates/pattern_runtime/src/policy/mod.rs`.

**Implementation:**
`pub fn is_pattern_config_kdl(path: &Path, content: &[u8]) -> ConfigGuardVerdict`. Verdict is an enum:

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigGuardVerdict {
    NotConfig,
    LikelyConfig { matched_keys: Vec<String> },
}
```

Detection logic (false-positives preferred over false-negatives per design):
1. If path ends in `.pattern.kdl`, return `LikelyConfig { matched_keys: vec!["filename".into()] }` immediately.
2. Otherwise, if path ends in `.kdl`, attempt to parse as KDL (via `knus` in a lightweight way, or a regex/line-scan fallback — pick the lower-effort option; regex over top-level identifiers is fine). Scan the top-level identifiers for pattern-specific keys: `mount`, `personas`, `isolate-from-persona`, `jj`, `project`, `backup`, `capabilities`, `policy`, `persona` (as a top-level node with `name` first argument). Any match → `LikelyConfig`.
3. Non-`.kdl` paths return `NotConfig` without parsing.

Use `knus::parse` only if we already have a lightweight entry point; otherwise a hand-rolled scanner is fine — this function is called per file write and should be cheap. Keep it in **one file**.

**Testing:**
- Unit table (`rstest` or a plain `[#test]` per case): 
  - `path="/foo/.pattern.kdl"`, content=`""` → LikelyConfig (filename rule).
  - `path="/foo/personas/alice.kdl"`, content=`name "Alice"\nsystem-prompt "…"\n` → LikelyConfig (matched `name` + `system-prompt` keys).
  - `path="/foo/notes.md"` → NotConfig.
  - `path="/foo/unrelated.kdl"`, content=`greeting "hello"` → NotConfig (no pattern keys).
  - `path="/foo/pattern.kdl"`, content=`mount mode="A"\n` → LikelyConfig.
  - `path="/foo/my.kdl"`, content=`capabilities { memory; message; }\n` → LikelyConfig.
- proptest: parse doesn't panic on random byte strings with `.kdl` extension (fuzz guard).

**Verification:**
`cargo nextest run -p pattern-runtime config_guard`

**Commit:** `[pattern-runtime] add is_pattern_config_kdl shape-based detection`
<!-- END_TASK_11 -->

<!-- START_TASK_12 -->
### Task 12: Hook `is_pattern_config_kdl` into the policy pipeline

**Verifies:** AC2.7 (pipeline wiring).

**Files:**
- Modify: `crates/pattern_runtime/src/policy/defaults.rs` — replace the placeholder `FilePath` rule from Task 9 with a `PolicyAction::RequireApproval` rule that's evaluated **after** the shape check produces `LikelyConfig`. Structurally: add a new `PolicyMatcher::FileWriteShape { guard: ConfigGuardFn }` variant and wire the default rule to use it.
- Modify: `crates/pattern_core/src/capability/policy.rs` — add the `FileWriteShape` variant to `PolicyMatcher`. Because the guard function holds no config data, use a function pointer (`fn(&Path, &[u8]) -> bool`) rather than a closure — keeps `Serialize` behaviour.

**Implementation:**
`PolicyMatcher::FileWriteShape { check: fn(&Path, &[u8]) -> bool }`. The default rule's `check` field references `is_pattern_config_kdl(...).is_config()` (a helper on the verdict enum).

Serialization concern: a function pointer isn't serde-friendly out of the box. Two options:
- **A.** Gate this variant behind `#[serde(skip)]` — it's a built-in rule, never loaded from config.
- **B.** Define a separate `RuntimePolicyRule` in `pattern_runtime` for built-in rules that can't round-trip, and keep `PolicyRule` in core pure-data.

Choose **(B)** — preserves `pattern_core` purity (matches the trait-only rule). `PolicySet` stays in core and accepts a `Vec<Box<dyn PolicyEvaluator>>` (trait object the runtime supplies). The runtime's built-in rules implement the trait; KDL-loaded rules are plain `PolicyRule` values.

This is a minor scope expansion vs. what Task 8 shipped — if the user pushes back, fall back to (A) and accept the serde-skip.

Add this rule to `rust_defaults()` so File writes are always evaluated against the guard. The rule outcome for `LikelyConfig` is `RequireApproval { reason: "writing to pattern config KDL" }`; it is NOT loosable by KDL config (rule carries `cannot_override: true` or lives in a separate "locked defaults" list that `PolicySet::evaluate` consults before any others).

**Testing:**
- Unit: integration of shape guard + policy — a `PolicySet` seeded with `rust_defaults()` returns `RequireApproval` when asked to evaluate a File write to `/foo/.pattern.kdl`, even after a KDL-config `Allow` rule for all file writes is layered on top (locked-defaults semantics, AC2.7).

**Verification:**
`cargo nextest run -p pattern-runtime policy::defaults config_guard`

**Commit:** `[pattern-runtime] lock pattern-config-KDL writes behind shape-based default`
<!-- END_TASK_12 -->

<!-- END_SUBCOMPONENT_E -->

<!-- START_SUBCOMPONENT_F (tasks 13-14) -->

<!-- START_TASK_13 -->
### Task 13: KDL schema for `capabilities {}` and `policy {}` blocks

**Verifies:** AC2.2, AC2.3 (when loaded rules reach the evaluator).

**Files:**
- Modify: `crates/pattern_runtime/src/persona_loader.rs` — add optional `capabilities: Option<CapabilitiesSection>` and `policy: Option<PolicySection>` fields to `PersonaSnapshot` (behind `#[knus(child, default)]`).
- Modify: `crates/pattern_memory/src/config/pattern_kdl.rs` — same additions at the project (`.pattern.kdl`) level, project-wide policy rules.
- Create: `crates/pattern_runtime/src/persona_loader/capabilities_kdl.rs` (submodule if the existing file is getting long, else inline).

**Implementation:**
Persona KDL fragment (illustrative):

```kdl
capabilities {
    effects {
        - "memory"
        - "message"
        - "tasks"
    }
    flags {
        - "spawn-new-identities"
    }
}

policy {
    rule "allow-git-push" effect="shell" action="allow" {
        matcher "shell-command" pattern="git push*"
    }
    rule "gate-all-file-writes" effect="file" action="require-approval" {
        matcher "file-path" pattern="**/*"
        reason "all file writes gated for this persona"
    }
}
```

`CapabilitiesSection` uses `knus::Decode` to parse both child blocks — `effects` (list of lowercase effect-category strings) and `flags` (list of kebab-case flag names). Unknown names error out clearly (knus already supports `#[knus(argument, str)]`-style conversions via `FromStr` on `EffectCategory` / `CapabilityFlag`). Both child blocks are optional; an empty `capabilities {}` decodes to `CapabilitySet::empty()` (pure-computation persona).

`PolicySection` parses into `Vec<PolicyRule>` with `Precedence::KdlConfig`.

**Testing:**
- Unit: parse a hand-written KDL persona fixture (new file at `crates/pattern_runtime/tests/fixtures/capability_persona.kdl`) and assert capabilities + flags + policy rules decode to the expected in-memory shape.
- Unit: a persona without `capabilities {}` falls back to `CapabilitySet::all()` — back-compat; document this clearly in the doc comment.
- Unit: a persona with `capabilities { effects { ... } }` but no `flags` block decodes with empty flags (no SpawnNewIdentities etc.).
- Unit: a persona with `capabilities { flags { - "spawn-new-identities" } }` decodes with `CapabilityFlag::SpawnNewIdentities` set.
- Unit: invalid effect name (`effects { - "nonsense" }`) or invalid flag name returns a knus/miette error with the bad span.

**Verification:**
`cargo nextest run -p pattern-runtime persona_loader`

**Commit:** `[pattern-runtime] load capabilities + policy blocks from persona KDL`
<!-- END_TASK_13 -->

<!-- START_TASK_14 -->
### Task 14: Merge KDL-loaded rules into `PolicySet` at session open

**Verifies:** AC2.2, AC2.3 (end-to-end).

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs` — session open merges `rust_defaults()` with persona-level KDL policy and project-level `.pattern.kdl` policy when composing the `PolicySet` stored on `SessionContext`. Precedence: `RuntimeOverride > KdlConfig > RustDefault`, but locked defaults (config-KDL shape guard, identity spawn) win over any `KdlConfig` entry per Task 12's locked-defaults semantics.
- Modify: `crates/pattern_runtime/src/policy/mod.rs` — add `PolicySet::merge(defaults, kdl_persona, kdl_project, runtime_overrides) -> PolicySet`.

**Implementation:**
`PolicySet::merge` accepts ordered iterators by precedence, concatenates into a single rule list, and relies on `evaluate()`'s priority sort (from Task 8). Add a test covering:
- KDL allows what defaults gate → KDL wins (AC2.2: `git push` example).
- KDL gates what defaults allow → KDL wins (AC2.3: file-writes example).
- KDL tries to allow a config-KDL write → locked default still wins (AC2.7 again).

**Testing:**
- Integration: open a session with the Task 13 fixture persona, submit an agent program that calls `Shell.execute "git push origin main"` — broker is NOT invoked (AC2.2). Subscribe a test channel to the broker and assert no request is observed within the test window.
- Integration: same persona, `File.Write("/tmp/notes.txt", "hi")` triggers broker invocation (AC2.3).

**Verification:**
`cargo nextest run -p pattern-runtime policy_kdl_merge`

**Commit:** `[pattern-runtime] merge KDL policy rules into session PolicySet at open`
<!-- END_TASK_14 -->

<!-- END_SUBCOMPONENT_F -->

<!-- START_SUBCOMPONENT_G (tasks 15) -->

<!-- START_TASK_15 -->
### Task 15: `File.Write` policy gate — end-to-end AC2.7

**Verifies:** AC2.7 (end-to-end — agent program calls `File.write`, gate fires, write is denied).

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/file.rs`
- Extend: `crates/pattern_core/src/capability/policy.rs` — `PolicyContext` gains `FileWrite { path: &Path, content: &[u8] }` variant if not already added in Task 8.
- Extend: the shell-handler test harness from Task 10 so the same pattern serves file-write tests.

**Implementation:**

Split the File handler's blanket stub so that `FileReq::Write(path, content)` evaluates the policy pipeline before any write logic:

```rust
// Well-known prefix — tests and handlers pattern-match on it.
const PERMISSION_DENIED_PREFIX: &str = "PermissionDenied: ";
const GATE_APPROVED_PREFIX: &str = "GateApproved: ";

fn handle(&mut self, req: FileReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
    let _guard = HandlerGuard::enter(&cx.user().cancel_state().gate);
    match req {
        FileReq::Write(path, content) => {
            let policy_ctx = PolicyContext::FileWrite { path: &path, content: content.as_bytes() };
            match cx.user().policies().evaluate(EffectCategory::File, &policy_ctx) {
                PolicyAction::Deny { reason } => {
                    Err(EffectError::Handler(format!(
                        "{PERMISSION_DENIED_PREFIX}{}",
                        reason.unwrap_or_else(|| "file write denied by policy".into()),
                    )))
                }
                PolicyAction::RequireApproval { reason } => {
                    // Escalate via the same sync bridge the Shell handler uses
                    // (RouterBridge-style sync-to-async channel from router.rs).
                    // Do NOT introduce futures::executor::block_on — that can
                    // deadlock if the broker re-enters tokio.
                    let origin = cx.user().current_turn_origin()
                        .ok_or_else(|| EffectError::Handler(
                            "file write: no current turn origin available".into()))?;
                    let grant = cx.user().permission_bridge().request_sync(
                        build_file_request(&path, reason),
                        &origin,
                        request_timeout,
                    );
                    if grant.is_some() {
                        Err(EffectError::Handler(format!(
                            "{GATE_APPROVED_PREFIX}File.Write gate approved; actual \
                             write mechanics land in sandbox-io plan",
                        )))
                    } else {
                        Err(EffectError::Handler(format!(
                            "{PERMISSION_DENIED_PREFIX}file write denied by broker",
                        )))
                    }
                }
                PolicyAction::Allow => Err(EffectError::Handler(format!(
                    "{GATE_APPROVED_PREFIX}File.Write gate approved; actual \
                     write mechanics land in sandbox-io plan",
                ))),
            }
        }
        FileReq::Read(_) | FileReq::ListDir(_) => Err(EffectError::Handler(
            "Pattern.File.Read / ListDir are not implemented in v3-multi-agent Phase 1 \
             (sandbox-io plan). Agent code should not call these in Phase 1-scope programs."
                .into(),
        )),
    }
}
```

**Why `EffectError::Handler(prefix)` and not a new variant:** `EffectError` lives in the external `tidepool-effect` crate (our fork at `github:orual/tidepool`). Adding a `PermissionDenied` variant there would require an upstream patch + `flake.lock` bump, out of scope for this phase. The `Handler(prefix)` pattern matches the existing convention used by other Phase 1 stubs, and tests can match on the `PERMISSION_DENIED_PREFIX` / `GATE_APPROVED_PREFIX` constants. When this pattern accumulates enough users to warrant the upstream patch, promote to a dedicated variant.

**Why not `futures::executor::block_on`:** it spins up a mini-executor that can deadlock if the broker re-enters the ambient tokio runtime. Reuse `RouterBridge`'s sync-to-async channel pattern (`crates/pattern_runtime/src/router.rs`) via a new `PermissionBridge` on the same shape — one sync channel per broker, one tokio task drains it. Task 7 establishes the bridge; Task 10 (Shell) and Task 15 (File) consume it.

**Testing (integration, matches AC2.7):**
- AC2.7 core: agent program with `File` capability calls `File.write "/tmp/.pattern.kdl" "mount mode=\"A\"\n"`. The broker's test subscriber observes a `PermissionRequest` with scope matching the write; test responds `Deny`; the agent sees an `EffectError::Handler` whose message starts with `PERMISSION_DENIED_PREFIX`. Assert the message mentions "pattern config kdl".
- AC2.7 locked-default: the persona's KDL config contains `policy { rule "allow-all-writes" effect="file" action="allow" { matcher "file-path" pattern="**/*" } }` — a loosening rule. Submit the same write to `/tmp/.pattern.kdl`. Assert the broker STILL receives the request (locked default wins over KDL `Allow`).
- Non-config file: `File.write "/tmp/notes.txt" "hello"`. Assert NO broker request observed; the agent sees an `EffectError::Handler` whose message starts with `GATE_APPROVED_PREFIX`. This proves the gate isn't over-firing — the distinct prefix is the signal.

**Verification:**
`cargo nextest run -p pattern-runtime file_write_gate -- --nocapture`

**Commit:** `[pattern-runtime] wire File.Write policy gate with shape-guard enforcement`
<!-- END_TASK_15 -->

<!-- END_SUBCOMPONENT_G -->

---

## Phase done-when checklist

- [ ] `CapabilitySet`, `EffectCategory`, `CapabilityError` types live in `pattern_core`.
- [ ] `filtered_effect_decls` + `preamble::build_for` produce capability-scoped preambles.
- [ ] Session open accepts an optional `CapabilitySet`; integration test shows compile-time rejection of excluded effects.
- [ ] `PermissionBroker` refactored in place: `new()` pub, global singleton deleted, per-runtime instances threaded through `SessionContext`.
- [ ] `PermissionBroker` v2 on jiff, per-runtime, with approve-for-scope + approve-for-duration caches, no leaks on timeout.
- [ ] `PolicyRule` / `PolicySet` types; Rust defaults seeded; KDL blocks parsed; merge order respected.
- [ ] Shell handler routes through `PolicySet` + broker on `RequireApproval`.
- [ ] Config-KDL shape guard locked as a default that KDL cannot loosen; unit-tested exhaustively AND verified end-to-end via the Task 15 `File.Write` gate (agent program → handler → policy evaluation → broker → denied).
- [ ] All existing tests still pass. New tests cover AC1.1–1.6 and AC2.1–2.9 (2.7 at predicate level only, flagged in the AC coverage section above).

---

## Notes for executor

- Do not reintroduce `PersonaSnapshot.enabled_tools`. The capabilities block replaces it cleanly.
- Plan 2 (task-skill-blocks) is mid-landing in parallel. If the `Tasks` effect lands in `CANONICAL_EFFECT_ROW` during Phase 1 execution, the `EffectCategory::Tasks` slot is already there; no schema churn. If it does NOT land, Phase 1 still works — the variant is reserved.
- All design questions resolved in this plan: broker stays in `pattern_core` (in-place refactor, Tasks 5-7), AC2.7 lands end-to-end via Task 15's File handler gate.
- Commit style per project: `[pattern-core] …` / `[pattern-runtime] …` / `[pattern-core] [pattern-runtime] …` for cross-crate moves.
- Always `cargo nextest run`; `cargo test --doc` for doctests; `cargo fmt`; `cargo clippy --all-features --all-targets`; `just pre-commit-all` before merging.
