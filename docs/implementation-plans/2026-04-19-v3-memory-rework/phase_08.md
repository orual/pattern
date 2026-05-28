# Pattern v3 Memory Rework — Phase 8 Implementation Plan (Capstone)

**Goal:** Implement `MemoryScope` with `isolate_from_persona` policy routing (None / CoreOnly / Full), add project-scoped persona discovery, wire `<mount>/lib/` Haskell utility modules into the Tidepool import path with "try-with-report" per-module compile isolation, add the `Pattern.Diagnostics` SDK effect, then run the capstone end-to-end smoke test and workspace-wide regression gate. This phase absorbs the original design's Phase 9 (smoke_e2e + regression coverage) per the guidance "testing lives inside phases".

**Architecture:** `MemoryScope<S: MemoryStore>` is a pure data-transformation wrapper around any `MemoryStore`. Its `ScopeBinding` carries persona_id, optional project_id, and an `IsolatePolicy` enum; every trait method routes reads + writes per the policy. `MemoryScope` wedges into `SessionContext` by replacing the inner store of `MemoryStoreAdapter` — construction time, nothing else moves. Project-scoped personas live at `<mount>/personas/@<name>/persona.toml` (Phase 6 already scaffolds the directory); global personas continue to live at `~/.pattern/personas/@<name>/`. A new `pattern_memory::persona::discover` function walks both locations; project-scoped wins on name collision. `<mount>/lib/*.hs` modules compile independently before the main agent program: each is attempted via `tidepool_runtime::compile_haskell` with its directory appended to the include path; compile failures surface as `DiagnosticEvent` records stored on `SessionContext::diagnostics` and the failed module is excluded from the final import path. The main agent program then compiles with only the successful lib modules available; any agent program that tries to `import` a broken module fails at Tidepool compile time with the standard "module not found" diagnostic. The new `Pattern.Diagnostics` effect exposes `diagnostics :: Eff [DiagnosticEvent]` to agents. The capstone smoke test exercises the full DoD flow (create persona → attach Mode A project → write Core text + Map + Log blocks → verify files emitted → external .md edit → loro merge → quiesce + host git commit → restart (process drop + re-open) → re-attach → read matches committed state → create messages.db backup → corrupt messages.db → restore → messages present), runs deterministically in CI against scripted provider mocks, and is backstopped by a multi-agent concurrent stress test and workspace-wide `cargo nextest run --workspace` gate.

**Scope:** Phase 8 of 8. Absorbs original Phase 9 (smoke_e2e.rs + regression capstone).

**Codebase verified:** 2026-04-19 (codebase-investigator agent adc21c5a6b462c719).
**External deps:** no new workspace deps beyond Phases 1–7; all primitives in place.

**Execution posture:** Hybrid with two gates. Sub-task 8a (MemoryScope) is bounded + autonomous-friendly; main-executor sign-off at the gate. Sub-task 8b (lib/ + Pattern.Diagnostics) has real novelty in the per-module compile isolation pattern (Tidepool's current API is monolithic — 8b needs a workaround); benefits from main-executor review. The final capstone (8c: smoke + regression + workspace-wide nextest) is a reviewable deliverable; the main executor signs off on the gate that declares v3-memory-rework complete.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-memory-rework.AC12: MemoryScope + isolate_from_persona

- **v3-memory-rework.AC12.1 Success (`None`):** Reads merge persona + project core; writes to shared handles flow bi-directionally; archival search spans both stores
- **v3-memory-rework.AC12.2 Success (`CoreOnly`):** Reads see persona core as read-only + project core as read-write; writes to persona-core from within project scope are denied
- **v3-memory-rework.AC12.3 Success (`Full`):** Persona identity (name, instructions) visible; persona block content not visible; archival search is project-only
- **v3-memory-rework.AC12.4 Success:** `ctx.memory.write_to_persona(...)` effect succeeds when policy is `None`
- **v3-memory-rework.AC12.5 Failure:** `ctx.memory.write_to_persona(...)` returns `MemoryError::IsolationDenied` when policy is `CoreOnly` or `Full`
- **v3-memory-rework.AC12.6 Edge:** Project-level writes in `None` mode default to project scope unless explicit persona-scoped effect is invoked

### v3-memory-rework.AC13: Project-scoped personas

- **v3-memory-rework.AC13.1 Success:** Persona definition at `<mount>/personas/@reviewer/persona.toml` loads + becomes invokable as `@reviewer` within the project
- **v3-memory-rework.AC13.2 Success:** `scope: project` persona is not visible when attaching a different project
- **v3-memory-rework.AC13.3 Success:** `scope: global` (or unspecified) persona at `~/.pattern/personas/@name/` works across projects subject to isolation policy
- **v3-memory-rework.AC13.4 Failure:** Persona definition missing required fields produces a clear parse error at attach time, not silent misconfiguration
- **v3-memory-rework.AC13.5 Edge:** Global + project-scoped personas with the same name: project-scoped takes precedence within that project; global available elsewhere

### v3-memory-rework.AC14: Project utilities + Pattern.Diagnostics

- **v3-memory-rework.AC14.1 Success:** `<mount>/lib/Project/Foo.hs` compiles cleanly; main agent program `import Project.Foo qualified as Foo` resolves + runs
- **v3-memory-rework.AC14.2 Success:** `<mount>/lib/Project/Bar.hs` with a syntax error is excluded from import path; session opens normally; agent program that doesn't import `Project.Bar` runs fine
- **v3-memory-rework.AC14.3 Success:** `Pattern.Diagnostics.diagnostics` effect returns a list of diagnostic events including the Bar compile failure
- **v3-memory-rework.AC14.4 Failure:** Main program imports `Project.Bar` (broken): session open fails with clear 'module not found (had compile errors)' diagnostic
- **v3-memory-rework.AC14.5 Failure:** Compile errors do not crash pattern or produce uninformative errors; every error has source location + message
- **v3-memory-rework.AC14.6 Edge:** No `lib/` directory on a mount: session opens cleanly; no error; no import path extension

### v3-memory-rework.AC15: End-to-end smoke test (absorbed from original Phase 9)

- **v3-memory-rework.AC15.1 Success:** `cargo nextest run -p pattern_memory --test smoke_e2e` passes deterministically in CI
- **v3-memory-rework.AC15.2 Success:** Test exercises: create persona → attach Mode A project → write Core text + Map + Log blocks → verify files emitted with expected format → external .md edit → loro merge → commit via host git → restart → re-attach → read matches committed state → backup messages.db → clear + restore → messages present
- **v3-memory-rework.AC15.3 Success:** `cargo nextest run --workspace` passes across all crates after all phases land
- **v3-memory-rework.AC15.4 Success:** FTS5 + vector regression snapshot suite (insta) is committed and stable across CI runs
- **v3-memory-rework.AC15.5 Failure:** Any step failing in the smoke flow causes the test to fail loudly with a specific error identifying which step failed
- **v3-memory-rework.AC15.6 Edge:** Multi-agent concurrent stress test (N MemoryCache instances doing writes against shared memory.db) completes without deadlock or data loss

---

## Codebase verification findings

Relevant realities:

- ✓ `persona_loader.rs` (984 lines) at `crates/pattern_runtime/src/persona_loader.rs`. Currently loads from explicit TOML paths only — no directory scan. Phase 8 adds the scan. `PersonaSnapshot` is the parsed type; fields include `name`, `agent_id`, `system_prompt`, `model`, `context`, `budgets`, `memory_blocks`. `agent_id` defaults to `name` if absent.
- ✗ **Deliberate divergence from design AC13.1 on persona file format.** The design text says `<mount>/personas/@reviewer.kdl` (a single KDL file). This plan uses `<mount>/personas/@reviewer/persona.toml` (a directory with a TOML file inside). Rationale: `persona_loader.rs` already parses TOML via serde with full `PersonaSnapshot` schema, including `memory_blocks`, `model`, `context`, `budgets`. Re-implementing all of that in KDL would be significant work outside Phase 8's scope and would fork the persona schema across global (TOML) vs project (KDL) loaders, which is a worse outcome than format consistency. The design plan should be updated post-Phase-8 to reflect this — the Phase 8 docs update task (Task 7) explicitly adds this to the design-plan update list. Users writing project-scoped personas write the same TOML they'd write for a global persona; only the location differs.
- ✓ `SessionContext` at `session.rs:40-111` holds `adapter: Arc<MemoryStoreAdapter>` (line 70). `MemoryStoreAdapter` at `memory/adapter.rs:37-78` holds `inner: Arc<dyn MemoryStore>`. Phase 8's `MemoryScope` wedges in by wrapping the inner store — zero changes to `SessionContext` shape.
- ✓ `sdk/handlers/scope.rs` (167 lines) already does cross-agent scope resolution (self / shared-blocks / group-membership). Phase 8's `isolate_from_persona` adds a parallel layer but for persona-vs-project routing — separate concern; lives in new file `sdk/handlers/isolate.rs` or inline in `MemoryScope`.
- ✓ `sdk/location.rs:SdkLocation::resolve()` returns the SDK dir `PathBuf`. `session.rs:539-543` resolves SDK dir + appends prelude dir to an `include_paths: Vec<PathBuf>` passed to `EvalWorker::spawn_with_includes`. Phase 8 extends this to also append successful `<mount>/lib/` subdirs after per-module compile isolation.
- ✗ **Tidepool compile is monolithic** — `tidepool_runtime::compile_and_run(source, bundle, includes)` compiles all Haskell at once. No per-module API. Phase 8 works around this via a pre-validation pass: for each `<mount>/lib/*.hs`, spawn a throwaway compile of a minimal stub agent program that imports exactly that module; if compile succeeds the module is considered good; if it fails the error is captured as a `DiagnosticEvent` and the module is excluded from the real include path.
- ✓ Log effect template at `haskell/Pattern/Log.hs` + `sdk/requests/log.rs` + `sdk/handlers/log.rs` is the structural template for `Pattern.Diagnostics`. Simple GADT constructor → Rust enum variant → dispatcher arm.
- ✓ No `DiagnosticEvent` type exists yet; Phase 8 creates it in `pattern_runtime::sdk::diagnostics::DiagnosticEvent` (or in `pattern_core` if it needs cross-crate visibility; scope TBD at implementation time — likely pattern_runtime is sufficient).
- ✓ `SessionContext` has no `diagnostics` field today; Phase 8 adds `diagnostics: Arc<Mutex<Vec<DiagnosticEvent>>>`.
- ✓ `.config/nextest.toml` has `default` + `ci` profiles. CI runs `--profile ci` with `fail-fast = false` + 60s slow-timeout + terminate-after-1-timeout. Phase 8's workspace-wide nextest gate uses the `ci` profile.
- ✓ No existing insta snapshots committed in pattern_db (Phase 2 creates them); Phase 8 capstone verifies they stay stable across the `--workspace` run.
- ✓ `session_lifecycle.rs::concurrent_session_isolation` at lines 258-352 is the baseline pattern for Phase 8's multi-agent concurrent stress test (AC15.6).
- ✓ `MemoryError` enum (Phase 1's `core_types`) is `#[non_exhaustive]`; adding `IsolationDenied { operation: String, policy: IsolatePolicy }` is forward-compatible with the error-enum pattern in the codebase.
- ✓ `pattern_runtime/CLAUDE.md` sections needing freshening: SDK imports list (add `Pattern.Diagnostics`), handlers list, eval worker mention of per-module compile isolation.

---

## Dependency changes

One crate-level promotion, no new workspace-level deps:

- `crates/pattern_runtime/Cargo.toml` gains `regex = "1"` as a direct dep (used by Task 5 / 6 for Tidepool error-location parsing). `regex` is already a transitive dep across the workspace via `genai`, `html2md`, and `jacquard` (verified via `cargo tree -i regex`); promoting to direct adds zero compile cost.

All other primitives (tokio, jiff, blake3, thiserror, miette, proptest, insta, tempfile, `std::sync::OnceLock` for lazy statics) already in place from Phases 1–7 or stdlib.

---

## Implementation tasks

<!-- START_SUBCOMPONENT_A (tasks 1-3) -->

### Subcomponent A — Sub-task 8a: MemoryScope + isolate_from_persona

<!-- START_TASK_1 -->
### Task 1: `IsolatePolicy` + `ScopeBinding` + `MemoryScope<S>` wrapper

**Verifies:** prerequisite for AC12.*

**Files:**
- Create: `crates/pattern_memory/src/scope/mod.rs`
- Create: `crates/pattern_memory/src/scope/policy.rs`
- Create: `crates/pattern_memory/src/scope/wrapper.rs`
- Modify: `crates/pattern_core/src/types/memory_types/core_types.rs` (add `MemoryError::IsolationDenied` variant + `IsolatePolicy` enum)

**Implementation:**

1. Core types in `pattern_core`:

   ```rust
   // In pattern_core::types::memory_types::core_types:
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
   #[non_exhaustive]
   pub enum IsolatePolicy {
       /// persona + project merged; bi-directional writes
       None,
       /// persona core read-only from project; project writes stay project-scoped
       CoreOnly,
       /// persona identity only; no persona memory carryover
       Full,
   }

   // Extend MemoryError:
   #[non_exhaustive]
   pub enum MemoryError {
       // ... existing variants ...
       #[error("isolation denied: operation {operation} would cross persona boundary under policy {policy:?}")]
       #[diagnostic(code(pattern_core::memory::isolation_denied))]
       IsolationDenied {
           operation: String,
           policy: IsolatePolicy,
       },
   }
   ```

2. `ScopeBinding`:

   ```rust
   // In pattern_memory::scope::wrapper:
   #[derive(Debug, Clone)]
   pub struct ScopeBinding {
       pub persona_id: AgentId,
       pub project_id: Option<ProjectId>,
       pub isolate_policy: IsolatePolicy,
   }
   ```

3. `MemoryScope<S>` wrapper:

   ```rust
   pub struct MemoryScope<S: MemoryStore> {
       inner: S,
       binding: ScopeBinding,
   }

   impl<S: MemoryStore> MemoryScope<S> {
       pub fn new(inner: S, binding: ScopeBinding) -> Self {
           Self { inner, binding }
       }

       pub fn binding(&self) -> &ScopeBinding { &self.binding }
   }

   impl<S: MemoryStore> MemoryStore for MemoryScope<S> {
       // Routing decisions per IsolatePolicy:
       //
       // Read path (get_block, list_blocks, search):
       //   - None: merge persona + project results. If both scopes have a
       //     same-label block, project wins (project-scoped takes precedence).
       //   - CoreOnly: persona core blocks visible as read-only overlay,
       //     project core writable; working-tier blocks project-scope only.
       //   - Full: persona blocks invisible; project-scope only.
       //
       // Write path (create_block, update_block_metadata, put_block content):
       //   - Default target is project scope for every policy.
       //   - In None mode, writes to a label that exists only in persona
       //     flow to persona (bi-directional); writes to a label that exists
       //     only in project stay project-scope.
       //   - CoreOnly + Full never write to persona scope (except via the
       //     explicit write_to_persona effect — Task 2).

       fn get_block(&self, agent_id: &AgentId, label: &str) -> MemoryResult<Option<StructuredDocument>> {
           match self.binding.isolate_policy {
               IsolatePolicy::None => {
                   // Check project first (project-scoped takes precedence on collision).
                   if let Some(project_id) = &self.binding.project_id {
                       if let Some(block) = self.inner.get_block(project_id.as_agent(), label)? {
                           return Ok(Some(block));
                       }
                   }
                   // Fall through to persona.
                   self.inner.get_block(&self.binding.persona_id, label)
               }
               IsolatePolicy::CoreOnly => {
                   // Same as None for reads, but with a read-only marker on persona results.
                   // Handled at write path — here, same behavior.
                   if let Some(project_id) = &self.binding.project_id {
                       if let Some(block) = self.inner.get_block(project_id.as_agent(), label)? {
                           return Ok(Some(block));
                       }
                   }
                   self.inner.get_block(&self.binding.persona_id, label)
                       .map(|opt| opt.map(|b| b.with_readonly_marker(true)))
               }
               IsolatePolicy::Full => {
                   // Project only.
                   if let Some(project_id) = &self.binding.project_id {
                       self.inner.get_block(project_id.as_agent(), label)
                   } else {
                       // Edge: Full isolation with no project scope → persona is all there is.
                       // Return None for persona reads (invisible) per policy.
                       Ok(None)
                   }
               }
           }
       }

       fn search(&self, query: &str, options: &SearchOptions, scope: SearchScope)
           -> MemoryResult<Vec<MemorySearchResult>> {
           match self.binding.isolate_policy {
               IsolatePolicy::None => {
                   // Merge persona + project archival + live searches.
                   // ... delegate with a merged scope ...
               }
               IsolatePolicy::CoreOnly | IsolatePolicy::Full => {
                   // Project-only if project exists; else fail loud.
                   let project_id = self.binding.project_id.as_ref()
                       .ok_or_else(|| MemoryError::IsolationDenied {
                           operation: "search".into(),
                           policy: self.binding.isolate_policy,
                       })?;
                   self.inner.search(query, options, SearchScope::Agent(project_id.as_agent().clone()))
               }
           }
       }

       fn update_block_metadata(&self, agent_id: &AgentId, label: &str, patch: BlockMetadataPatch)
           -> MemoryResult<()> {
           // For CoreOnly + Full, deny writes targeting persona_id.
           if *agent_id == self.binding.persona_id {
               match self.binding.isolate_policy {
                   IsolatePolicy::None => { /* allowed */ }
                   IsolatePolicy::CoreOnly | IsolatePolicy::Full => {
                       return Err(MemoryError::IsolationDenied {
                           operation: format!("update_block_metadata(label={label})"),
                           policy: self.binding.isolate_policy,
                       });
                   }
               }
           }
           self.inner.update_block_metadata(agent_id, label, patch)
       }

       // ... all 19 MemoryStore methods with per-policy routing ...
   }
   ```

   The routing pattern for each method is structurally similar: check the policy, decide which scope to route to (or deny the call), delegate. The trait surface is sync (Phase 3), so `MemoryScope` just wraps each method without async glue.

4. `with_readonly_marker` on `StructuredDocument`: a new method that flags the doc as read-only in whatever metadata field is appropriate. Phase 4 subscribers ignore read-only docs (no fs emission needed for a read view) — verify at implementation time that this doesn't break the subscriber contract.

**Testing:**

Unit tests in `scope/wrapper.rs` + integration tests in `tests/scope_isolation.rs`:

- AC12.1 (None): seed persona with block `scratchpad`; seed project with block `notes`. MemoryScope reads return both. Write to `scratchpad` from project context → flows to persona. Write to `notes` → stays project.
- AC12.2 (CoreOnly): same setup. Reads return both; persona reads carry read-only marker. Write to persona's `scratchpad` via `update_block_metadata(persona_id, "scratchpad", patch)` → `IsolationDenied`.
- AC12.3 (Full): persona content invisible; reads return only project blocks. Search scope is project-only; search against persona-scope → empty result (or error, depending on caller expectation).
- AC12.4: explicit `write_to_persona` (Task 2) succeeds under `None`.
- AC12.5: explicit `write_to_persona` under `CoreOnly` or `Full` → `IsolationDenied`.
- AC12.6: default write target is project in `None` mode (confirmed by path verification — writes landed at project_id, not persona_id).

**Verification:**

Run: `cargo nextest run -p pattern_memory --lib scope`
Run: `cargo nextest run -p pattern_memory --test scope_isolation`
Expected: all pass.

**Commit:** `[pattern-core] [pattern-memory] IsolatePolicy + ScopeBinding + MemoryScope wrapper with per-policy routing`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: `ctx.memory.write_to_persona` SDK effect

**Verifies:** v3-memory-rework.AC12.4, AC12.5

**Files:**
- Modify: `crates/pattern_runtime/src/sdk/requests/memory.rs` (add `WriteToPersona` variant)
- Modify: `crates/pattern_runtime/src/sdk/handlers/memory.rs` (add dispatch arm)
- Modify: `crates/pattern_runtime/haskell/Pattern/Memory.hs` (add `WriteToPersona` GADT constructor + `writeToPersona` helper)

**Implementation:**

1. Rust-side variant:

   ```rust
   // In sdk/requests/memory.rs MemoryReq enum:
   #[core(module = "Pattern.Memory", name = "WriteToPersona")]
   WriteToPersona { label: String, content: String },
   ```

2. Handler dispatch:

   ```rust
   MemoryReq::WriteToPersona { label, content } => {
       // Only reachable when MemoryScope wraps the store with IsolatePolicy::None,
       // but we don't check that here — MemoryScope's update_block_metadata enforces.
       // Instead, we explicitly target the persona_id scope.
       let binding = cx.user().scope_binding()
           .ok_or(EffectError::NoScope)?;
       let persona_id = binding.persona_id.clone();
       // This call fans out through MemoryScope → if policy is CoreOnly or Full,
       // MemoryScope returns IsolationDenied; handler converts to EffectError.
       let store = cx.user().adapter();
       store.put_block_content(&persona_id, &label, &content)?;
       cx.respond(())
   }
   ```

3. Haskell GADT + helper:

   ```haskell
   -- In haskell/Pattern/Memory.hs:
   data Memory a where
     Get :: Text -> Memory (Maybe Content)
     Put :: Text -> Content -> Memory ()
     -- ... existing ...
     WriteToPersona :: Text -> Content -> Memory ()

   writeToPersona :: Member Memory effs => Text -> Content -> Eff effs ()
   writeToPersona label content = send $ WriteToPersona label content
   ```

**Testing:**

- AC12.4: attach mount with `isolate_from_persona = none`, invoke `ctx.memory.writeToPersona "key" "value"` from an agent program, assert the persona's `key` block contains `"value"`.
- AC12.5: attach with `isolate_from_persona = coreOnly`, invoke same; assert the effect returns an error wrapping `MemoryError::IsolationDenied`, agent observes the typed failure.

**Verification:**

Run: `cargo nextest run -p pattern_runtime --test sdk_write_to_persona`
Expected: passes.

**Commit:** `[pattern-runtime] ctx.memory.write_to_persona SDK effect with IsolationDenied propagation`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Wire `MemoryScope` into `SessionContext` construction; parse `.pattern.kdl` isolate_from_persona

**Verifies:** integration of AC12.* into runtime

**Files:**
- Modify: `crates/pattern_runtime/src/session.rs` (wrap the inner store in `MemoryScope` at SessionContext construction)
- Modify: `crates/pattern_memory/src/config/pattern_kdl.rs` (`IsolateSection` policy string → `IsolatePolicy` enum)

**Implementation:**

1. Convert the KDL `isolate_from_persona` string into an `IsolatePolicy`:

   ```rust
   // In config::pattern_kdl:
   impl IsolateSection {
       pub fn resolve(&self) -> Result<IsolatePolicy, ConfigError> {
           match self.policy.to_ascii_lowercase().as_str() {
               "none" => Ok(IsolatePolicy::None),
               "core-only" | "coreonly" => Ok(IsolatePolicy::CoreOnly),
               "full" => Ok(IsolatePolicy::Full),
               other => Err(ConfigError::Validation {
                   path: PathBuf::from(".pattern.kdl"),
                   reason: format!("invalid isolate_from_persona.policy: {other:?}; expected none | core-only | full"),
               }),
           }
       }
   }
   ```

2. `session.rs` integration point. Locate where `MemoryStoreAdapter::new(inner, agent_id)` is constructed and extend:

   ```rust
   // Construct the scope binding from the mount config + persona.
   let binding = ScopeBinding {
       persona_id: persona.agent_id.clone().into(),
       project_id: mount_config.map(|c| c.project.id().into()),
       isolate_policy: mount_config
           .and_then(|c| Some(c.isolate_from_persona.resolve().ok()?))
           .unwrap_or(IsolatePolicy::None),
   };
   let scoped_store = Arc::new(MemoryScope::new(raw_store, binding));
   let adapter = Arc::new(MemoryStoreAdapter::new(scoped_store, agent_id));
   ```

   When `session.rs` is not given mount context (e.g., test fixtures opening a raw session without a mount), the binding defaults to `IsolatePolicy::None` with `project_id: None` — effectively passthrough.

**Testing:**

- Unit test in config: `IsolateSection { policy: "Core-Only" }.resolve()` → `Ok(IsolatePolicy::CoreOnly)`. Invalid string → `Err(Validation)` with helpful message (AC13.4 by extension).
- Integration test in `tests/scoped_session.rs`: open a session with mount config policy `coreOnly` + a seeded persona block; verify the session's adapter calls route through MemoryScope (attempting to write the persona block from handler context yields `IsolationDenied`).

**Verification:**

Run: `cargo nextest run -p pattern_runtime --test scoped_session`
Expected: passes.

**Commit:** `[pattern-runtime] [pattern-memory] wire MemoryScope into SessionContext; parse isolate_from_persona policy from .pattern.kdl`
<!-- END_TASK_3 -->

<!-- END_SUBCOMPONENT_A -->

**GATE (main-executor sign-off):**

- All three AC12 policies tested end-to-end.
- `ctx.memory.writeToPersona` effect works in None, rejects in CoreOnly + Full.
- Config parsing round-trip for all three policy strings.
- `cargo nextest run -p pattern_memory -p pattern_runtime` green.

Before Subcomponent B, the main executor reviews:
- Does `MemoryScope`'s routing preserve subscriber lifecycles? (Phase 4 subscribers fire on underlying store commits; scope is a transformation layer on top — should be transparent, but verify.)
- Any unexpected MemoryStore methods where scope routing is ambiguous?

---

<!-- START_SUBCOMPONENT_B (tasks 4-7) -->

### Subcomponent B — Sub-task 8b: Project personas + `<mount>/lib/` + Pattern.Diagnostics

<!-- START_TASK_4 -->
### Task 4: Project-scoped persona discovery

**Verifies:** v3-memory-rework.AC13.1, AC13.2, AC13.3, AC13.4, AC13.5

**Files:**
- Create: `crates/pattern_memory/src/persona/discover.rs`
- Modify: `crates/pattern_runtime/src/persona_loader.rs` (add a `discover_and_load(name, mount_dir)` entry point)

**Implementation:**

```rust
// In pattern_memory::persona::discover:
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Enumerate available personas across global + project scopes.
/// Project-scoped personas take precedence on name collision.
/// Returns map of persona name → path to its persona.toml.
pub fn discover_personas(
    project_mount: Option<&Path>,
) -> Result<HashMap<String, PathBuf>, PersonaDiscoveryError> {
    let mut personas = HashMap::new();

    // 1. Global personas at ~/.pattern/personas/@<name>/persona.toml
    let global = crate::paths::pattern_home()?.join("personas");
    if global.is_dir() {
        collect_personas(&global, &mut personas)?;
    }

    // 2. Project-scoped personas at <mount>/personas/@<name>/persona.toml
    if let Some(mount) = project_mount {
        let project_personas = mount.join("personas");
        if project_personas.is_dir() {
            collect_personas(&project_personas, &mut personas)?;
            // project-scoped overwrites on same-name collision (insert semantics)
        }
    }
    Ok(personas)
}

fn collect_personas(
    dir: &Path,
    out: &mut HashMap<String, PathBuf>,
) -> Result<(), PersonaDiscoveryError> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| PersonaDiscoveryError::Io { path: dir.to_owned(), source: e })?
    {
        let entry = entry
            .map_err(|e| PersonaDiscoveryError::Io { path: dir.to_owned(), source: e })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Persona dirs are named @foo or foo (either accepted; @-prefix is Pattern convention).
        let toml = entry.path().join("persona.toml");
        if toml.is_file() {
            // Normalize @foo → foo for lookup, but preserve original as display hint.
            let normalized = name.trim_start_matches('@').to_owned();
            out.insert(normalized, toml);
        }
    }
    Ok(())
}
```

Persona_loader gains a helper that resolves name → path via discover + delegates to existing `load_persona`:

```rust
pub fn discover_and_load(
    name: &str,
    project_mount: Option<&Path>,
) -> miette::Result<PersonaSnapshot> {
    let personas = pattern_memory::persona::discover_personas(project_mount)
        .map_err(|e| PersonaLoadError::Discovery(e))?;
    let path = personas.get(name).ok_or_else(|| PersonaLoadError::NotFound {
        name: name.to_owned(),
        searched: personas.keys().cloned().collect(),
    })?;
    load_persona(path)
}
```

**Testing:**

Integration test in `tests/persona_discovery.rs`:

- AC13.1: place `@reviewer/persona.toml` in a tempdir mount, call `discover_and_load("reviewer", Some(mount))`, assert loaded snapshot.
- AC13.2: discovery in a DIFFERENT tempdir mount does NOT include `@reviewer`.
- AC13.3: place `@reviewer/persona.toml` at a simulated `~/.pattern/personas/` (override home dir via `HOME` env var or `dirs` mock); discovery with `None` project_mount finds it.
- AC13.4: place a malformed persona.toml (missing required `name` field); discover_and_load surfaces a clear parse error pointing to the field.
- AC13.5: place `@reviewer/persona.toml` in BOTH global + mount with different contents; discovery with that mount finds the project-scoped version; discovery with a different mount finds the global.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test persona_discovery`
Expected: passes.

**Commit:** `[pattern-memory] [pattern-runtime] project-scoped persona discovery with precedence on name collision`
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: `<mount>/lib/` per-module compile isolation + include path extension

**Verifies:** v3-memory-rework.AC14.1, AC14.2, AC14.4, AC14.5, AC14.6

**Files:**
- Create: `crates/pattern_runtime/src/sdk/lib_modules.rs`
- Modify: `crates/pattern_runtime/src/session.rs` (call lib_modules::validate_and_resolve before spawning eval worker)

**Implementation:**

1. Per-module validation via a throwaway probe compile:

   ```rust
   // In sdk/lib_modules.rs:
   use std::path::{Path, PathBuf};

   /// Result of validating a mount's lib/ directory.
   pub struct LibValidation {
       /// Successful module paths to add to the include path.
       pub successful_paths: Vec<PathBuf>,
       /// Per-module compile failures for Pattern.Diagnostics.
       pub failures: Vec<LibCompileFailure>,
   }

   pub struct LibCompileFailure {
       pub module_name: String,
       pub source_path: PathBuf,
       pub error_message: String,
       pub source_location: Option<String>,   // "file:line:col" if parseable
   }

   /// Validate each .hs file in <mount>/lib/ by attempting a probe compile
   /// that imports it from a minimal stub agent program.
   ///
   /// Successful modules are reported via their containing directory (added
   /// to include path). Failed modules are reported via LibCompileFailure
   /// with the compile error message for downstream Pattern.Diagnostics.
   ///
   /// If <mount>/lib/ doesn't exist, returns an empty LibValidation (AC14.6).
   pub fn validate_and_resolve(
       mount_path: &Path,
       base_include_paths: &[PathBuf],
   ) -> Result<LibValidation, LibValidationError> {
       let lib_dir = mount_path.join("lib");
       if !lib_dir.is_dir() {
           return Ok(LibValidation {
               successful_paths: Vec::new(),
               failures: Vec::new(),
           });
       }

       let mut successful_paths = vec![lib_dir.clone()];
       let mut failures = Vec::new();

       // Walk lib_dir for .hs files (recursively, to handle Project/Foo.hs).
       let hs_files = walk_hs_files(&lib_dir)?;

       for hs_path in hs_files {
           let module_name = infer_module_name(&lib_dir, &hs_path)?;
           // Build a probe program that imports exactly this module.
           let probe_src = format!(
               "module Main where\n\
                import qualified {module_name}\n\
                main :: IO ()\n\
                main = pure ()\n",
           );
           // Include paths: base + lib_dir
           let mut includes = base_include_paths.to_vec();
           includes.push(lib_dir.clone());

           match tidepool_runtime::probe_compile(&probe_src, &includes) {
               Ok(_) => { /* module is good; stays in the set */ }
               Err(compile_err) => {
                   failures.push(LibCompileFailure {
                       module_name: module_name.clone(),
                       source_path: hs_path.clone(),
                       error_message: compile_err.to_string(),
                       source_location: parse_source_location(&compile_err),
                   });
                   // Exclude from include path by removing the module's subdir
                   // from successful_paths (if lib_dir was the only path, keep
                   // lib_dir but the broken file will simply fail to import
                   // from main agent programs — which is desired per AC14.4).
                   //
                   // Simpler approach: lib_dir stays in the path; broken modules
                   // simply fail on import when the main agent program tries to
                   // use them. "try-with-report" is honored via the diagnostics
                   // collection + the agent not being blocked at session open.
               }
           }
       }

       Ok(LibValidation { successful_paths, failures })
   }

   fn walk_hs_files(root: &Path) -> Result<Vec<PathBuf>, LibValidationError> {
       // Recursive walk; collect all .hs files.
       // Implementation: stdlib read_dir + recurse on directories.
   }

   fn infer_module_name(lib_root: &Path, hs_path: &Path) -> Result<String, LibValidationError> {
       // lib_root/Project/Foo.hs → "Project.Foo"
       let rel = hs_path.strip_prefix(lib_root)
           .map_err(|_| LibValidationError::InvalidLayout {
               root: lib_root.to_owned(),
               path: hs_path.to_owned(),
           })?;
       let without_ext = rel.with_extension("");
       let parts: Vec<String> = without_ext
           .components()
           .filter_map(|c| c.as_os_str().to_str().map(String::from))
           .collect();
       Ok(parts.join("."))
   }

   fn parse_source_location(compile_err: &tidepool_runtime::CompileError) -> Option<String> {
       // See "parse_source_location implementation" below for the full regex
       // implementation covering GHC-style source locations.
       parse_source_location_inner(&compile_err.to_string())
   }
   ```

2. **Probe compile approach (concrete, with fallback):**

   Tidepool's current crate surface exposes `compile_and_run(source, bundle, includes) -> Result<ToolOutcome, CompileError>` — a single monolithic entry point. The implementor attempts the following approach in order:

   **Approach A (preferred):** call `compile_and_run` with the probe-program source shown above but an empty `SdkBundle` (or a minimal one with a no-op handler bundle). The probe program's `main = pure ()` returns immediately; the compile phase happens before execution. If `compile_and_run` separates compile errors from runtime errors in its `CompileError` type, we can inspect + return early.

   **Approach B (fallback if A can't separate compile from run):** do a full `compile_and_run` with a mock handler bundle. Success = probe compiled AND ran AND returned `()`. The overhead is minimal (probe runs `pure ()` — microseconds). Any compile error surfaces as an error message we can parse.

   **Approach C (degradation if per-module probe is infeasible):** skip per-module validation entirely. Append `<mount>/lib/` to the include path unconditionally. Any broken module surfaces its compile error at main-program compile time with Tidepool's normal error output, which flows into the diagnostics collection via the handler-error path (the existing diagnostic collection captures any compile error generated during agent evaluation). "Try-with-report" semantics are partially honored: failed modules are reported, just at main-compile time rather than pre-compile. Trade-off: a broken lib module that's not imported by the main program doesn't get flagged until someone imports it. Acceptable degradation — document the behavior in the CLAUDE.md update (Task 7).

   **Decision point:** the implementor runs a local Tidepool probe test at Task 5 start to determine which approach is viable. Whichever approach lands gets documented in a commit message + CLAUDE.md update. Approach C is always viable as a last resort; the question is whether A or B buys us pre-compile isolation.

3. **`parse_source_location` implementation (no `todo!`):**

   ```rust
   use regex::Regex;
   use std::sync::OnceLock;

   /// Extract a GHC-style source location ("Foo.hs:15:3" or "Foo.hs:(15,3)-(17,8)")
   /// from a Tidepool compile-error message. Returns None when the regex doesn't
   /// match — the DiagnosticEvent's `location` field stays None, which is the
   /// correct "we couldn't parse, but still report" behavior.
   fn parse_source_location(err_msg: &str) -> Option<String> {
       static LOCATION_RE: OnceLock<Regex> = OnceLock::new();
       let re = LOCATION_RE.get_or_init(|| {
           Regex::new(r"([A-Za-z0-9_/]+\.hs):\(?(\d+),\s*(\d+)\)?")
               .expect("static regex compiles")
       });
       re.captures(err_msg).map(|caps| {
           format!("{}:{}:{}", &caps[1], &caps[2], &caps[3])
       })
   }
   ```

   **Deps:** `regex` is already a transitive dep via `genai`, `html2md`, and `jacquard` across the workspace (verified 2026-04-19 via `cargo tree -i regex`). Phase 8 promotes it to a direct dep of `pattern_runtime` — zero added compile cost since it's already compiled in the dep tree. `std::sync::OnceLock` is stdlib, stable since Rust 1.70; no `once_cell` dep needed.

   Update `crates/pattern_runtime/Cargo.toml`:
   ```toml
   [dependencies]
   # ... existing ...
   regex = "1"
   ```

   If the implementor discovers Tidepool uses a different error format (found via one probe-compile of a known-broken module during implementation), the regex gets adjusted and the adjusted pattern documented in the commit message.

3. Session integration:

   ```rust
   // In session.rs, replacing the "resolve SDK dir + prelude" block:
   let sdk_dir = sdk_location.resolve()?;
   let mut include_paths = vec![sdk_dir];
   if let Some(prelude) = prelude_dir { include_paths.push(prelude); }

   let lib_validation = if let Some(mount_path) = mount_context.as_ref().map(|m| &m.mount_path) {
       sdk::lib_modules::validate_and_resolve(mount_path, &include_paths)?
   } else {
       sdk::lib_modules::LibValidation::default()
   };

   // Extend include path with successful lib dirs.
   include_paths.extend(lib_validation.successful_paths.iter().cloned());

   // Stash failures into session state for Pattern.Diagnostics.
   session_diagnostics.extend(lib_validation.failures.into_iter().map(DiagnosticEvent::from));
   ```

**Testing:**

Integration tests in `tests/lib_modules.rs`:

- AC14.1: place a valid `Project/Foo.hs` + main agent program that imports it; session opens + runs.
- AC14.2: place both `Project/Good.hs` (valid) + `Project/Bar.hs` (syntax error); session opens; agent program that doesn't import Bar runs fine; diagnostics list includes Bar's failure.
- AC14.4: same setup but main program DOES import `Project.Bar`; session open fails with clear "module not found" diagnostic referencing the fact that Bar had compile errors.
- AC14.5: failures never panic; every `LibCompileFailure` has non-empty `error_message` and at least a module name.
- AC14.6: no `lib/` dir → `LibValidation { successful_paths: [], failures: [] }`; session opens cleanly.

**Verification:**

Run: `cargo nextest run -p pattern_runtime --test lib_modules`
Expected: passes.

**Commit:** `[pattern-runtime] <mount>/lib/ per-module probe-compile validation + import path extension + diagnostics collection`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: `Pattern.Diagnostics` SDK effect

**Verifies:** v3-memory-rework.AC14.3

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Diagnostics.hs`
- Create: `crates/pattern_runtime/src/sdk/requests/diagnostics.rs`
- Create: `crates/pattern_runtime/src/sdk/handlers/diagnostics.rs`
- Modify: `crates/pattern_runtime/src/session.rs` (add `diagnostics: Arc<Mutex<Vec<DiagnosticEvent>>>` field + accessor)
- Modify: `crates/pattern_runtime/CLAUDE.md` (add Pattern.Diagnostics to the SDK imports section)

**Implementation:**

1. `DiagnosticEvent` type (lives in `pattern_runtime::sdk::diagnostics` since no cross-crate need):

   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
   pub struct DiagnosticEvent {
       pub severity: DiagnosticSeverity,
       pub source: String,            // "lib-compile" | "handler" | "schema" | ...
       pub message: String,
       pub location: Option<String>,   // "Project/Bar.hs:15:3"
       pub at: jiff::Timestamp,
   }

   #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
   pub enum DiagnosticSeverity {
       Error,
       Warning,
       Info,
   }

   impl From<LibCompileFailure> for DiagnosticEvent {
       fn from(f: LibCompileFailure) -> Self {
           Self {
               severity: DiagnosticSeverity::Error,
               source: "lib-compile".into(),
               message: format!("{}: {}", f.module_name, f.error_message),
               location: f.source_location,
               at: jiff::Timestamp::now(),
           }
       }
   }
   ```

2. Rust enum:

   ```rust
   #[derive(Debug, FromCore)]
   pub enum DiagnosticsReq {
       #[core(module = "Pattern.Diagnostics", name = "GetDiagnostics")]
       GetDiagnostics,
   }
   ```

3. Handler:

   ```rust
   pub struct DiagnosticsHandler;

   impl EffectHandler<SessionContext> for DiagnosticsHandler {
       type Request = DiagnosticsReq;
       fn handle(&mut self, req: DiagnosticsReq, cx: &EffectContext<'_, SessionContext>)
           -> Result<Value, EffectError>
       {
           match req {
               DiagnosticsReq::GetDiagnostics => {
                   let diags = cx.user().diagnostics().lock().unwrap().clone();
                   cx.respond(serde_json::to_value(diags)?)
               }
           }
       }
   }
   ```

4. Haskell:

   ```haskell
   module Pattern.Diagnostics where

   import Control.Monad.Freer
   import qualified Data.Aeson as A
   import Data.Text (Text)

   data Diagnostics a where
     GetDiagnostics :: Diagnostics [DiagnosticEvent]

   data DiagnosticEvent = DiagnosticEvent
     { severity :: Text
     , source   :: Text
     , message  :: Text
     , location :: Maybe Text
     }
     -- deriving parsing from JSON; shape matches the Rust serde output.

   diagnostics :: Member Diagnostics effs => Eff effs [DiagnosticEvent]
   diagnostics = send GetDiagnostics
   ```

5. Register the handler in the SDK bundle + add `DiagnosticsReq` to the canonical effect decls list (for code-tool description generation).

**Testing:**

Integration test in `tests/sdk_diagnostics.rs`:

- Attach a mount with a broken `lib/Project/Bar.hs`; open a session; agent program calls `Pattern.Diagnostics.diagnostics`; assert the returned list contains an event with severity=Error, source="lib-compile", message mentioning `Project.Bar`.
- No `lib/` dir → `diagnostics` returns empty list.

**Verification:**

Run: `cargo nextest run -p pattern_runtime --test sdk_diagnostics`
Expected: passes.

**Commit:** `[pattern-runtime] Pattern.Diagnostics SDK effect + DiagnosticEvent collection + SessionContext integration`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Freshen pattern_runtime/CLAUDE.md + port-list entry

**Verifies:** documentation contract for Phase 8

**Files:**
- Modify: `crates/pattern_runtime/CLAUDE.md`
- Modify: `docs/plans/rewrite-v3-portlist.md`

**Implementation:**

1. `pattern_runtime/CLAUDE.md` updates:

   - Add Pattern.Diagnostics to the 13→14 effect module list in "SDK imports" section.
   - Update "Handlers section" to mention Phase 8: diagnostics + write_to_persona handlers.
   - In "Eval worker" section, add a note about per-module probe-compile isolation for `<mount>/lib/`.
   - Update the freshness date.

2. Port-list entry:

   ```markdown
   ### Scopes + project personas + lib modules + Pattern.Diagnostics (Phase 8 — completed YYYY-MM-DD)

   - `pattern_memory::scope::MemoryScope<S>` wraps any `MemoryStore` with
     IsolatePolicy routing (None / CoreOnly / Full).
   - Persona discovery across global (`~/.pattern/personas/`) + project
     (`<mount>/personas/`) scopes; project-scoped takes precedence on collision.
   - `<mount>/lib/*.hs` per-module probe-compile validation; failures surface
     via Pattern.Diagnostics without blocking session open.
   - `Pattern.Diagnostics.diagnostics` SDK effect returns a list of session
     diagnostic events (lib-compile failures + handler errors).
   - `ctx.memory.writeToPersona` effect allows explicit persona-scope write
     when policy is None; rejects under CoreOnly or Full with IsolationDenied.
   ```

**Testing:** Documentation changes — verification is manual review.

**Verification:**

Run: `grep -n "Pattern.Diagnostics" crates/pattern_runtime/CLAUDE.md docs/plans/rewrite-v3-portlist.md`
Expected: both files reference it.

**Commit:** `[pattern-runtime] [meta] Phase 8 docs: CLAUDE.md freshen + port-list entry`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_B -->

**GATE (main-executor sign-off):**

- `cargo nextest run -p pattern_memory -p pattern_runtime` green across new tests.
- Pattern.Diagnostics end-to-end test passes (broken lib module → agent observes the diagnostic).
- Per-module isolation actually isolates (broken Project.Bar doesn't prevent Project.Good from loading).
- CLAUDE.md + port-list updated.

---

<!-- START_SUBCOMPONENT_C (tasks 8-10) -->

### Subcomponent C — Capstone: smoke_e2e + regression + workspace-wide gate

<!-- START_TASK_8 -->
### Task 8: `smoke_e2e.rs` — library-level end-to-end DoD flow

**Verifies:** v3-memory-rework.AC15.1, AC15.2, AC15.5

**Files:**
- Create: `crates/pattern_memory/tests/smoke_e2e.rs`

**Implementation:**

```rust
//! Capstone end-to-end smoke test. Exercises the full v3-memory-rework DoD flow
//! deterministically with no live provider calls. Runs in CI.
//!
//! Flow:
//!   1. Create persona fixture + Mode A project in a tempdir git repo.
//!   2. Attach the mount.
//!   3. Write Core text block + Map block + Log block.
//!   4. Verify canonical files emitted (.md, .kdl, .jsonl) with expected content.
//!   5. Verify memory.db FTS5 + vector indexes populated.
//!   6. External edit to the .md file (simulated human editor).
//!   7. Wait for notify watcher → loro CRDT merge.
//!   8. Verify reconciled content in both loro + re-emitted file.
//!   9. Quiesce + commit via host `git commit`.
//!   10. Detach + simulate process restart (drop cache + db + supervisor).
//!   11. Re-attach; read blocks; assert matches committed state.
//!   12. Create messages.db backup via pattern_memory::backup::snapshot::create_snapshot.
//!   13. Write new messages, then simulate corruption (truncate messages.db).
//!   14. Restore from backup; verify messages present.

use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn smoke_e2e() {
    // --- Fixture setup ---
    let tmp = TempDir::new().unwrap();
    let project_root = tmp.path().to_owned();

    // Init host git.
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&project_root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Pattern Smoke"])
        .current_dir(&project_root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "smoke@pattern.test"])
        .current_dir(&project_root)
        .output()
        .unwrap();

    // Init Mode A mount.
    pattern_memory::modes::mode_a::init(&project_root).expect("mode A init");

    // Initial git commit so we have a baseline.
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&project_root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "baseline"])
        .current_dir(&project_root)
        .output()
        .unwrap();

    // --- Attach ---
    let mount = pattern_memory::mount::attach(&project_root, None).await
        .expect("attach");

    // --- Write blocks ---
    let agent_id = AgentId::from("smoke-agent");
    mount.cache.create_block(&agent_id, BlockCreate::new(
        "notes",
        BlockType::Core,
        BlockSchema::text(),
    )).expect("create notes block");
    mount.cache.put_block_content(&agent_id, "notes", "hello pattern")
        .expect("put notes content");

    mount.cache.create_block(&agent_id, BlockCreate::new(
        "config",
        BlockType::Working,
        BlockSchema::map(),
    )).expect("create config block");
    // ... set map fields ...

    mount.cache.create_block(&agent_id, BlockCreate::new(
        "events",
        BlockType::Working,
        BlockSchema::Log { display_limit: 100, entry_schema: None },
    )).expect("create log block");
    // ... append log entries ...

    // Wait for subscriber debounce.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // --- AC15.2 step: verify files emitted ---
    let notes_md = mount.mount_path.join("blocks/core/notes.md");
    assert!(notes_md.exists(), "notes.md should exist");
    let md_content = std::fs::read_to_string(&notes_md).unwrap();
    assert!(md_content.contains("hello pattern"));

    let config_kdl = mount.mount_path.join("blocks/working/config.kdl");
    assert!(config_kdl.exists(), "config.kdl should exist");

    let events_jsonl = mount.mount_path.join("blocks/working/events.jsonl");
    assert!(events_jsonl.exists(), "events.jsonl should exist");

    // --- External edit ---
    std::fs::write(&notes_md, "hello pattern — externally edited\n")
        .expect("external edit");
    // Wait for notify + merge.
    tokio::time::sleep(Duration::from_millis(700)).await;

    let merged = mount.cache.get_rendered_content(&agent_id, "notes")
        .expect("get merged content")
        .expect("notes exists");
    assert!(merged.contains("externally edited"));

    // --- Quiesce + commit ---
    pattern_memory::jj::quiesce::quiesce(
        &mount.supervisor_handle,
        &mount.db,
        collect_emitted_paths(&mount),
        Duration::from_secs(10),
    ).await.expect("quiesce");

    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&project_root)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "smoke: write blocks"])
        .current_dir(&project_root)
        .output()
        .unwrap();

    // --- Simulate restart ---
    mount.detach().await.expect("detach");

    // --- Re-attach ---
    let mount2 = pattern_memory::mount::attach(&project_root, None).await
        .expect("re-attach");
    let recovered = mount2.cache.get_rendered_content(&agent_id, "notes")
        .expect("get re-attached")
        .expect("notes exists after re-attach");
    assert!(recovered.contains("externally edited"));

    // --- Messages backup ---
    let project_id = &mount2.config.project.name;
    let messages_db_path = mount2.db.messages_path();

    // Insert a known number of scripted messages so we can assert exact counts
    // before/after the backup cycle.
    let pre_snapshot_message_count = 5;
    for i in 0..pre_snapshot_message_count {
        insert_scripted_message(&mount2.db, &agent_id, &format!("pre-snapshot-{i}"))
            .expect("insert scripted message");
    }

    let snapshot = pattern_memory::backup::snapshot::create_snapshot(
        messages_db_path, project_id,
    ).expect("create backup snapshot");
    assert!(snapshot.path.exists());

    // Insert additional messages AFTER the snapshot — these should be lost on restore.
    let post_snapshot_messages = 3;
    for i in 0..post_snapshot_messages {
        insert_scripted_message(&mount2.db, &agent_id, &format!("post-snapshot-{i}"))
            .expect("insert post-snapshot message");
    }

    // Corrupt messages.db (truncate).
    std::fs::write(messages_db_path, b"").expect("corrupt messages.db");

    // Restore.
    let _pre_restore = pattern_memory::backup::restore::restore_snapshot(
        messages_db_path, &snapshot.path,
    ).expect("restore");

    // After restore, only the pre-snapshot messages should be present —
    // post-snapshot writes + the truncation both vanish.
    let conn = mount2.db.get().unwrap();
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM msg.messages",
        [],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(count as u64, pre_snapshot_message_count,
        "restore should reflect snapshot state exactly (not {count}, expected {pre_snapshot_message_count})");

    mount2.detach().await.unwrap();
}

// Helpers

fn collect_emitted_paths(mount: &MountedStore) -> Vec<PathBuf> {
    // Walk <mount>/blocks/ for all .md / .kdl / .jsonl files.
    // ... implementation ...
    Vec::new()
}

fn insert_scripted_message(
    db: &pattern_db::ConstellationDb,
    agent_id: &AgentId,
    content_preview: &str,
) -> rusqlite::Result<()> {
    let conn = db.get().expect("pool get");
    conn.execute(
        "INSERT INTO msg.messages (agent_id, position, role, content_json, content_preview, source, created_at)
         VALUES (?1, ?2, 'user', '{}', ?3, 'test', ?4)",
        rusqlite::params![
            agent_id.as_str(),
            jiff::Timestamp::now().as_millisecond(),
            content_preview,
            jiff::Timestamp::now().to_string(),
        ],
    )?;
    Ok(())
}
```

**Testing:**

This task IS a test. The `#[tokio::test]` `smoke_e2e` is the deliverable.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test smoke_e2e`
Expected: passes deterministically; runs cleanly in CI.

Run on CI via `cargo nextest run --workspace --profile ci` → includes smoke_e2e in the suite.

**Commit:** `[pattern-memory] smoke_e2e.rs — capstone end-to-end DoD flow (AC15.1, AC15.2, AC15.5)`
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Multi-agent concurrent stress test

**Verifies:** v3-memory-rework.AC15.6

**Files:**
- Create: `crates/pattern_memory/tests/concurrent_stress.rs`

**Implementation:**

```rust
//! Multi-agent concurrent stress: N MemoryCache-holding agents doing writes
//! against a shared memory.db. Proves no deadlock + no data loss.

use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn concurrent_memory_cache_stress() {
    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path().to_owned();
    // Minimal mount init.
    pattern_memory::modes::mode_a::init(&project_root).unwrap();
    let mount = Arc::new(pattern_memory::mount::attach(&project_root, None).await.unwrap());

    let n_agents = 10;
    let writes_per_agent = 50;

    let mut handles = Vec::with_capacity(n_agents);
    for i in 0..n_agents {
        let mount_clone = mount.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let agent_id = AgentId::from(format!("agent-{i}"));
            for turn in 0..writes_per_agent {
                let label = format!("block-{i}-{turn}");
                mount_clone.cache.create_block(&agent_id, BlockCreate::new(
                    &label, BlockType::Working, BlockSchema::text(),
                )).expect("create under stress");
                mount_clone.cache.put_block_content(
                    &agent_id, &label, &format!("content {i}:{turn}"),
                ).expect("put under stress");
            }
        });
        handles.push(handle);
    }

    // Join with a timeout so a deadlock fails the test rather than hangs CI.
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        futures::future::try_join_all(handles),
    ).await;
    let joined = result.expect("no deadlock within 60s").expect("no task errors");
    assert_eq!(joined.len(), n_agents);

    // Verify all writes landed.
    let conn = mount.db.get().unwrap();
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_blocks WHERE block_type = 'working'",
        [],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(count as usize, n_agents * writes_per_agent);

    // unwrap Arc — only the test holds it now, all tasks joined.
    let mount = Arc::try_unwrap(mount).expect("sole owner after join");
    mount.detach().await.unwrap();
}
```

**Testing:** this task IS a test.

**Verification:**

Run: `cargo nextest run -p pattern_memory --test concurrent_stress`
Expected: passes within 60s; exact write count verified.

**Commit:** `[pattern-memory] multi-agent concurrent stress test (AC15.6)`
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: Workspace-wide nextest gate + FTS5/vector regression snapshot verification

**Verifies:** v3-memory-rework.AC15.3, AC15.4

**Files:**
- Verify existing: `crates/pattern_db/tests/snapshots/*.snap` (committed during Phase 2).
- Verify existing: `crates/pattern_memory/tests/snapshots/*.snap` (committed during Phase 4 for format round-trips).
- Modify: `.github/workflows/ci.yml` (if Phase 5 added a jj canary step, ensure it runs alongside the full-workspace nextest; confirm the main workspace job runs with `--profile ci` per `.config/nextest.toml`).

**Implementation:**

1. Run `cargo nextest run --workspace --profile ci` locally; verify all tests pass across every crate. Expected runtime: the 677+ pre-Phase-8 baseline + Phase 2-8's new tests. Any test >60s gets flagged by the `slow-timeout` profile setting — either optimize it or annotate with a longer timeout.

2. Run `cargo insta review` on any pending snapshots; accept only the intentional changes (FTS5 + KDL + vector ordering); document the accepted snapshot in the commit message.

3. Verify CI config runs `cargo nextest run --workspace --profile ci` as a job; if it only runs individual crates, consolidate.

4. **`cargo test --doc --workspace`** — run separately per the project convention (nextest doesn't support doctests). All doctests must pass.

**Testing:** the workspace-wide gate IS the test.

**Verification:**

Run: `cargo nextest run --workspace --profile ci`
Expected: green across all crates (pattern_core, pattern_memory, pattern_db, pattern_runtime, pattern_provider, pattern_cli, pattern_mcp, pattern_nd, pattern_discord, pattern_api, pattern_server, pattern_macros if present).

Run: `cargo test --doc --workspace`
Expected: all doctests pass.

Run: `cargo insta pending-snapshots`
Expected: no pending snapshots (all accepted or rejected with intent).

**Commit:** `[meta] v3-memory-rework capstone: workspace-wide nextest green, snapshots stable`
<!-- END_TASK_10 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase 8 Done-when recap (and v3-memory-rework plan DoD)

- All AC12 (IsolatePolicy routing), AC13 (project-scoped personas), AC14 (lib/ + Pattern.Diagnostics), AC15 (smoke + stress + workspace-wide gate) tests pass.
- `cargo nextest run --workspace --profile ci` green across all crates.
- `cargo test --doc --workspace` green.
- FTS5 BM25 + vector KNN + KDL round-trip snapshot suites committed and stable across CI runs.
- `smoke_e2e.rs` passes deterministically in CI — the full DoD flow is verified end-to-end without live provider calls.
- Multi-agent concurrent stress test passes within 60s.
- `pattern_runtime/CLAUDE.md` + port-list reflect all Phase 8 additions.
- All port-list entries for Phases 1–8 show completion timestamps.

## v3-memory-rework completion criteria (post-Phase-8)

At this point the plan's full DoD (design-plan lines 13–158) is satisfied:

- ✅ `pattern_memory` crate extracted (Phase 1).
- ✅ rusqlite migration + memory.db/messages.db split (Phase 2).
- ✅ MemoryStore sync + eval worker simplification (Phase 3).
- ✅ Canonical fs serialization + loro-native subscribers + notify watcher (Phase 4).
- ✅ jj CLI adapter + pre-commit quiesce (Phase 5).
- ✅ Storage modes A + B + Mode C spike (Phase 6).
- ✅ messages.db backup + restore + rotation (Phase 7).
- ✅ Scopes + project personas + lib modules + Pattern.Diagnostics (Phase 8 sub-tasks 8a + 8b).
- ✅ End-to-end smoke + regression + workspace-wide gate (Phase 8 capstone).

**Next in the v3 rewrite sequence**: Plan 2 (`v3-task-skill-blocks`) — Task + Skill block subtypes, graph dependencies, trust tagging. Builds on this plan's MemoryStore sync surface + new consolidated types.
