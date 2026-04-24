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

Define `CapabilitySet` as a wrapper around `BTreeSet<EffectCategory>` (sorted, deterministic for hashing / serde):

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet(BTreeSet<EffectCategory>);
```

Provide constructors: `CapabilitySet::empty()`, `CapabilitySet::all()` (every variant of `EffectCategory`), `CapabilitySet::from_iter(…)`. Methods: `contains(cat) -> bool`, `iter()`, `is_subset_of(other)`, and `restrict_to(other: &CapabilitySet) -> Result<Self, CapabilityError>` — used for ephemeral/fork inheritance (cannot escalate; returning `CapabilityError::Escalation` if `self` introduces caps absent from `other`).

Define `CapabilityError` with `thiserror`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CapabilityError {
    #[error("capability escalation: cannot add {added:?} to a set restricted to {parent:?}")]
    Escalation { added: Vec<EffectCategory>, parent: Vec<EffectCategory> },
    #[error("capability denied: effect {category:?} not present in set")]
    Denied { category: EffectCategory },
}
```

Notes:
- Error messages lowercase, sentence fragments per project conventions.
- No runtime behaviour beyond data + predicates. `pattern_core` trait-only rule preserved.
- `#[non_exhaustive]` everywhere per project convention.

**Testing:**
- Unit: `CapabilitySet::all().len() == <variant_count>`; keep this assertion resilient — use `strum::EnumIter` or a manual match enumerating every variant (adding a new variant forces the test to update).
- Unit: `restrict_to` returns `Err(Escalation{..})` when expanding beyond parent; returns `Ok` otherwise.
- Unit: `CapabilitySet::default() == empty()`.
- proptest (`serde_json`): round-trip `CapabilitySet` — parse-serialize-parse.

**Verification:**
`cargo nextest run -p pattern-core capability`
Expected: all new tests pass; full suite still green.

**Commit:** `[pattern-core] add CapabilitySet and EffectCategory types`
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
### Task 6: `PermissionBroker` v2 — jiff durations, approve-for-scope plumbing

**Verifies:** AC2.4, AC2.5, AC2.6, AC2.8, AC2.9.

**Files:**
- Modify: `crates/pattern_core/src/permission.rs` — change `PermissionGrant.expires_at` from `chrono::DateTime<chrono::Utc>` to `jiff::Timestamp`; add `PermissionDecisionKind::ApproveForDuration(jiff::Span)` (replacing `std::time::Duration`); add the approve-for-scope cache.
- Modify: any callers that build `PermissionDecisionKind::ApproveForDuration` or read `PermissionGrant.expires_at`.

**Implementation:**
Swap chrono for jiff using the crate-level `jiff::Timestamp` / `jiff::Span`. Reason about expiry with `now + span`. Keep the `request()` method's external `timeout: std::time::Duration` — this is a host-side timeout and doesn't need jiff; the *grant* duration is the one that flows into the agent-visible data.

Add an `approve_for_scope` behaviour: when a grant returns `ApproveForScope`, the broker records the `PermissionScope` in an in-memory "session scope cache" (keyed by `(agent_id, scope)`). Subsequent requests matching the cached scope return without re-broadcasting. Cache is per-broker-instance (per-runtime), so two runtimes have independent caches (AC2.9).

`ApproveForDuration(jiff::Span)` stores `expires_at = jiff::Timestamp::now() + span` on the grant. Subsequent matching requests check `now < expires_at` before returning without broadcast.

Timeout path (AC2.8): `request()` already `tokio::time::timeout`s on the oneshot — ensure the timeout returns `None` (denial) and does NOT leak a pending entry in `self.pending` or `self.pending_info`. Today the map entries are populated before the await; add cleanup in the `Err(_timeout)` arm. Write an explicit test for this leak.

**Testing:**
- Unit: request flow end-to-end with synthetic `subscribe()` recipient that calls `respond()` — cover ApproveOnce, ApproveForScope (two calls, second short-circuits), ApproveForDuration (advance `jiff::Timestamp` via injected clock), timeout case.
- Inject a `fn now_fn: Arc<dyn Fn() -> jiff::Timestamp + Send + Sync>` so duration tests don't sleep. Default production constructor uses `jiff::Timestamp::now`.
- Unit: two broker instances with independent scope caches — approving a scope on instance A does not carry to instance B (AC2.9).

**Verification:**
`cargo nextest run -p pattern-runtime permission`
Expected: all new behaviour covered; no panics / leaks on timeout.

**Commit:** `[pattern-runtime] rebuild PermissionBroker on jiff with scope + duration caches`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Thread per-runtime broker through handler contexts

**Verifies:** AC2.9.

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs` — construct the broker alongside the runtime, store as `Arc<PermissionBroker>`, expose through `SessionContext` (accessor `ctx.permission_broker() -> &Arc<PermissionBroker>`).
- Modify: any handler (`shell.rs`, future `file.rs`, etc.) that will consult the broker — switch to `cx.user().permission_broker()` via a new `HasPermissionBroker` trait (alongside the existing `HasCancelState`).

**Implementation:**
Define `HasPermissionBroker` in `pattern_runtime::session` (or wherever `HasCancelState` lives):

```rust
pub trait HasPermissionBroker {
    fn permission_broker(&self) -> &Arc<pattern_core::permission::PermissionBroker>;
}
```

`SessionContext` implements it. Handlers that need the broker take an additional bound `U: HasCancelState + HasPermissionBroker` (example in `shell.rs` at Task 10).

**Testing:**
- Integration: spin up two `SessionContext`s with independent `PermissionBroker` instances; confirm approving a scope on one does not leak (AC2.9, belt-and-suspenders with Task 6).

**Verification:**
`cargo nextest run` full suite. Expected: every handler that consults the broker reaches it through the per-session `SessionContext`.

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

Pin down the `PolicyMatcher::ShellCommand` semantics: accept a shell-style glob pattern (`*`, `?`, no brace-expansion) using the `globset` crate if not already a dep — **ASK user before adding.** If `globset` is off the table, fall back to prefix matching (`"rm -rf".starts_with(&pat)`) and document the limitation. Either way, make the semantics obvious in the doc comment.

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

**Verifies:** AC2.1 (end-to-end), AC2.2, AC2.3 (after Task 13 lands KDL overrides).

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/handlers/shell.rs` — thread `PolicySet` into the handler via `SessionContext`, evaluate before command execution, escalate to `PermissionAuthority` on `RequireApproval`, reject on `Deny`.
- Modify: `crates/pattern_runtime/src/session.rs` — `SessionContext` gains `policies: Arc<PolicySet>` constructed at open from `rust_defaults() ++ kdl_rules ++ runtime_rules`.

**Implementation:**
Shell handler pseudocode:

```rust
let policies = cx.user().policies();
let ctx = PolicyContext::Shell { command: &req.command };
match policies.evaluate(EffectCategory::Shell, &ctx) {
    PolicyAction::Allow => run_shell(req),
    PolicyAction::Deny { reason } => Err(EffectError::PermissionDenied(reason)),
    PolicyAction::RequireApproval { reason } => {
        let grant = cx.user().permission_authority()
            .request(build_request(req, reason), request_timeout).await;
        if grant.is_some() { run_shell(req) } else { Err(EffectError::PermissionDenied(None)) }
    }
}
```

(Names are illustrative — use the existing error types. If `EffectError::PermissionDenied` does not exist, add it.)

For Task 10 specifically, DO NOT yet wire KDL-loaded rules — that's Task 13/14. Construct `PolicySet` from `rust_defaults()` only. This keeps the Shell path testable in isolation now.

**Testing:**
- Integration (scripted, no live shell): open a session, submit an agent program that calls `Shell.execute "rm -rf /tmp/testdir"`. A test harness subscribes to the broker and responds with `Deny` — the agent sees an error matching `PermissionDenied` (AC2.1).
- Integration: same setup, respond with `ApproveOnce` — the agent proceeds (Shell handler runs the command; tests use a neutered command like `echo ok` combined with a test-side pattern to exercise the gate).
- Integration: `Shell.execute "ls"` (not matched by defaults) runs without broker interaction.

**Verification:**
`cargo nextest run -p pattern-runtime shell_policy`

**Commit:** `[pattern-runtime] gate Shell handler through PolicySet`
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
    - "memory"
    - "message"
    - "tasks"
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

`CapabilitiesSection` uses `knus::Decode` to parse a list of lowercase strings into a `CapabilitySet`. Unknown effect names error out clearly (knus already supports `#[knus(argument, str)]`-style conversions via `FromStr` on `EffectCategory`).

`PolicySection` parses into `Vec<PolicyRule>` with `Precedence::KdlConfig`.

**Testing:**
- Unit: parse a hand-written KDL persona fixture (new file at `crates/pattern_runtime/tests/fixtures/capability_persona.kdl`) and assert capabilities + policy rules decode to the expected in-memory shape.
- Unit: a persona without `capabilities {}` falls back to `CapabilitySet::all()` — back-compat; document this clearly in the doc comment.
- Unit: invalid effect name (`capabilities { - "nonsense" }`) returns a knus/miette error with the bad span.

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
fn handle(&mut self, req: FileReq, cx: &EffectContext<'_, U>) -> Result<Value, EffectError> {
    let _guard = HandlerGuard::enter(&cx.user().cancel_state().gate);
    match req {
        FileReq::Write(path, content) => {
            let ctx = PolicyContext::FileWrite { path: &path, content: content.as_bytes() };
            match cx.user().policies().evaluate(EffectCategory::File, &ctx) {
                PolicyAction::Deny { reason } => {
                    Err(EffectError::PermissionDenied(reason.unwrap_or_default()))
                }
                PolicyAction::RequireApproval { reason } => {
                    // Same escalation shape as Shell handler (Task 10): build
                    // PermissionRequest, call broker.request, map None → denied.
                    let granted = futures::executor::block_on(async {
                        cx.user().permission_authority()
                            .request(build_file_request(&path, reason), cx.user().caller(), request_timeout)
                            .await
                    });
                    if granted.is_some() {
                        Err(EffectError::Handler(
                            "File.Write gate approved; actual write mechanics land in sandbox-io plan".into(),
                        ))
                    } else {
                        Err(EffectError::PermissionDenied("file write denied by broker".into()))
                    }
                }
                PolicyAction::Allow => Err(EffectError::Handler(
                    "File.Write gate approved; actual write mechanics land in sandbox-io plan".into(),
                )),
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

The "Allow" and "RequireApproval-approved" arms return a distinct error from the "Deny" / "RequireApproval-denied" arm so tests can assert which path fired. `PermissionDenied` is a new `EffectError` variant if it doesn't already exist — add it in the same commit.

The blocking `futures::executor::block_on` call mirrors the Shell handler (which runs on the sync EvalWorker thread and cannot `.await`). If the Shell handler uses a different bridge (`RouterBridge`-style sync channel), reuse that instead — align with the established precedent, don't invent a second bridge.

**Testing (integration, matches AC2.7):**
- AC2.7 core: agent program with `File` capability calls `File.write "/tmp/.pattern.kdl" "mount mode=\"A\"\n"`. The broker's test subscriber observes a `PermissionRequest` with scope matching the write; test responds `Deny`; the agent sees `PermissionDenied`. Assert the error message mentions "pattern config kdl".
- AC2.7 locked-default: the persona's KDL config contains `policy { rule "allow-all-writes" effect="file" action="allow" { matcher "file-path" pattern="**/*" } }` — a loosening rule. Submit the same write to `/tmp/.pattern.kdl`. Assert the broker STILL receives the request (locked default wins over KDL `Allow`).
- Non-config file: `File.write "/tmp/notes.txt" "hello"`. Assert NO broker request observed; the agent sees the "Allow; actual write mechanics land in sandbox-io plan" error. This proves the gate isn't over-firing — the distinct error variant is the signal.

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
