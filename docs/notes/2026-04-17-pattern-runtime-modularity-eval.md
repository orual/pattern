# Pattern Runtime Modularity Evaluation

**Date**: 2026-04-17  
**Scope**: Assess feasibility of swapping Tidepool runtime substrate (e.g., cosa, Deno) without rewriting core abstractions  
**Status**: Investigation only — no code changes  

---

## Summary

Pattern's runtime exhibits a **moderately good substrate boundary** but with significant Tidepool-specific leakage in three critical areas. The `AgentRuntime` and `Session` traits in `pattern_core` correctly abstract the substrate interface, and the SDK effect system is largely substrate-agnostic once instantiated. However, **compilation machinery, effect handler plumbing, and checkpoint serialization** are tightly coupled to Tidepool's type surface. A cosa migration would require extracting these into a substrate-agnostic layer, but the cognitive complexity is manageable (not a fundamental redesign). Three refactors would pay off significantly in terms of future flexibility: (1) relocate compilation and bundle creation out of session open, (2) introduce a substrate-agnostic `EffectHandler` trait, and (3) extract checkpoint event serialization from debug-repr strings to a proper format.

---

## Current State Inventory

### Module Tree (src/ structure)

| Module | Purpose | Substrate-specific? |
|--------|---------|-----|
| `lib.rs` | Module exports | No |
| `runtime.rs` | `TidepoolRuntime` → `AgentRuntime` impl | Yes (Tidepool name, but trait-correct) |
| `session.rs` | `TidepoolSession` → `Session` impl, `SessionContext` | **Highly Yes** (see leak points) |
| `tidepool/` | FFI boundary (compile, machine, error mapping) | **100% Yes** (deliberate) |
| `tidepool/compile.rs` | Haskell compilation via `tidepool_runtime::compile_haskell` | **100% Yes** (subprocess call) |
| `tidepool/machine.rs` | JIT wrapper: `SessionMachine` wraps `JitEffectMachine` | **100% Yes** (Tidepool internals) |
| `tidepool/error_map.rs` | Tidepool error → `RuntimeError` translation | **100% Yes** (bridge-specific) |
| `sdk/` | SDK request/handler definitions + bundle | **Mixed** (see breakdown) |
| `sdk/requests/` | 11 enums mirroring Haskell GADT constructors | **Tidepool-coupled** (via `tidepool_bridge_derive::FromCore`) |
| `sdk/requests/memory.rs` (et al.) | Type defs + conversion impls | **Coupled** (e.g. `#[core(module, name)]` attrs are Tidepool-specific) |
| `sdk/handlers/` | Effect handler implementations | **Substrate-agnostic** (correct trait abstraction) |
| `sdk/handlers/time.rs`, `log.rs`, `display.rs` | Fully-wired handlers | **Agnostic** (impl `EffectHandler<U>` generically) |
| `sdk/handlers/memory.rs` | Dispatches to `Arc<dyn MemoryStore>` | **Agnostic** (store trait is generic) |
| `sdk/handlers/shell.rs` (et al.) | Stubs returning not-implemented errors | **Agnostic** (pattern is generalizable) |
| `sdk/bundle.rs` | `SdkBundle` HList type alias | **Tidepool-coupled** (frunk HList is Tidepool's choice) |
| `sdk/location.rs` | SDK directory resolution | **Agnostic** (generic file-path logic) |
| `checkpoint.rs` | Event log + snapshot logic | **Partially coupled** (see detail below) |
| `timeout.rs` | Watchdog + cancel state harness | **Agnostic** (generic cancellation) |
| `preflight.rs` | Binary-existence checks for tidepool-extract | **100% Yes** (Tidepool-specific) |
| `testing.rs` | Test fixture re-exports | **Depends on what's exported** |

### Substrate Boundary (Trait Contracts)

**Good:**
- `pattern_core::traits::AgentRuntime` — correctly abstract; `TidepoolRuntime` is a concrete impl
- `pattern_core::traits::Session` — correctly abstract; `TidepoolSession` is a concrete impl
- `pattern_core::traits::MemoryStore` — correctly trait-object'd in handlers; `pattern_runtime` has no concrete dependency
- `timeout::CancelState` — agnostic state machine
- `session::HasCancelState` — generic protocol (also blanket impl on `()` for testing)

**Leaky:**
- `SessionMachine`, `CompiledProgram`, `CancelHandle` all leak out of `tidepool/` module and into session machinery (via `pub use`)
- `SdkLocation::resolve()` returns `PathBuf` but callers assume GHC-compatible include paths (only Tidepool-style)
- `CheckpointEvent::request_repr` / `response_repr` are Debug-string round-trips (Tidepool `Value`-specific; cosa would have its own value type)

---

## Leak Points: Where Tidepool Assumptions Escape the Boundary

### 1. **Request Type Derivation (HIGH IMPACT)**

**File**: `crates/pattern_runtime/src/sdk/requests/memory.rs:19-40` (and 10 siblings)  
**Problem**: Request enums use `#[core(module = "Pattern.Memory", name = "Get")]` from `tidepool_bridge_derive`.

```rust
#[derive(Debug, FromCore)]
pub enum MemoryReq {
    #[core(module = "Pattern.Memory", name = "Get")]
    Get(String),
    ...
}
```

**Why it leaks**: `tidepool_bridge_derive::FromCore` is a Tidepool-specific derive macro that bridges GHC's DataCon tags to Rust enums via the `tidepool_repr::DataConTable`. Cosa would have a different type/tag system (AST nodes, not constructors). 

**Scope of impact**: All 11 request modules (`memory.rs`, `message.rs`, ..., `spawn.rs`) use this derive. A cosa runtime would need to re-derive or hand-implement `FromCore`-like deserialization for its own AST node types.

**Mitigation difficulty**: Medium. Extract a substrate-agnostic `RequestType` trait and make each substrate provide a `FromValue` implementation. Current code would change from:
```rust
// Now (Tidepool-specific)
#[derive(Debug, FromCore)]
pub enum MemoryReq { ... }

// Future (substrate-agnostic)
pub enum MemoryReq { ... }
impl FromValue for MemoryReq {
    fn from_value(v: &Value) -> Result<Self> { ... }
}
// Tidepool submodule:
impl TidepoolFromValue for MemoryReq {
    fn from_datacontable(dc: &DataConTable, tag: u32, args: &[Value]) -> Result<Self> { ... }
}
```

### 2. **CheckpointEvent Serialization Format (MEDIUM IMPACT)**

**File**: `crates/pattern_runtime/src/checkpoint.rs:27-91`  
**Problem**: Events record `request_repr` and `response_repr` as `Debug` string representations:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointEvent {
    pub tag: u32,
    pub request_repr: String,  // format!("{request:?}") — Tidepool Value's Debug impl
    pub response_repr: String, // ditto
    pub turn: u64,
    pub sequence: u64,
}
```

**Why it leaks**: The `Debug` impl for `tidepool_eval::Value` is substrate-specific. A cosa runtime's values would format differently. The checkpoint file format is not self-describing — a replay bundle would fail to reconstruct cosa values from Tidepool debug strings, and vice versa.

**Module-level comment** (line 14-19) already acknowledges this as a Phase 3 limitation:
> Faithful replay (re-driving the JIT with recorded responses) is deferred until the replay bundle lands in a later phase; the event shape can be extended then.

**Scope of impact**: Moderate now (Phase 3 doesn't use replay). Becomes critical if Phase 4/5 adds replay functionality. Future-migration coupling.

**Mitigation difficulty**: Low. Use `tidepool_repr` for Tidepool and a cosa-equivalent for cosa:
```rust
pub struct CheckpointEvent {
    pub tag: u32,
    pub request: serde_json::Value,      // structured, substrate-agnostic
    pub response: serde_json::Value,     // same
    pub turn: u64,
    pub sequence: u64,
}
```
Record via a substrate-provided serialization step. Costs some precision in the interchange format (AST nodes → JSON → reconstructed AST) but avoids round-trip fragility.

### 3. **Compilation + Warm JIT in Session::open (HIGH IMPACT)**

**File**: `crates/pattern_runtime/src/session.rs:263-300`  
**Problem**: `TidepoolSession::open` calls `compile_program` (subprocess GHC invoke) and `SessionMachine::new` (JIT warmup) synchronously:

```rust
pub fn open(
    persona: PersonaConfig,
    sdk: &SdkLocation,
    memory_store: Arc<dyn MemoryStore>,
) -> Result<Self, RuntimeError> {
    crate::preflight::check()?;
    let sdk_dir = sdk.resolve()?;
    let program = compile_program(&persona.program, "agent", &sdk_dir)?;
    let nursery = persona.nursery_size.unwrap_or(64 * 1024 * 1024);
    let machine = SessionMachine::new(program, nursery)?;
    ...
}
```

**Why it leaks**: `compile_program` is Tidepool-specific (calls `tidepool_runtime::compile_haskell`). Cosa (or Deno) would have different compilation machinery. The entire open path is locked to Tidepool's model.

**Scope of impact**: High. Every session open goes through this path. Cosa migration requires reimplementing `compile_program` and `SessionMachine::new` for the new substrate.

**Mitigation difficulty**: Medium-high. Requires extracting compilation into a trait:
```rust
// In pattern_core::traits or pattern_runtime::substrate
pub trait RuntimeCompiler: Send + Sync {
    type CompiledProgram;
    type Machine;
    
    async fn compile(&self, program: &str, target: &str) -> Result<Self::CompiledProgram>;
    async fn warm(&self, compiled: &Self::CompiledProgram) -> Result<Self::Machine>;
}

// Tidepool impl
pub struct TidepoolCompiler { sdk: SdkLocation };
impl RuntimeCompiler for TidepoolCompiler {
    type CompiledProgram = CompiledProgram;
    type Machine = SessionMachine;
    fn compile(...) { /* call tidepool_runtime::compile_haskell */ }
    fn warm(...) { /* call SessionMachine::new */ }
}

// Then TidepoolSession::open becomes:
pub async fn open(..., compiler: &impl RuntimeCompiler) -> Result<Self> {
    let prog = compiler.compile(&persona.program, "agent").await?;
    let machine = compiler.warm(&prog).await?;
    ...
}
```

This blocks further progress until a compiler trait is stable; it's not a light refactor.

### 4. **SdkBundle Type Is Tidepool-Specific (MEDIUM IMPACT)**

**File**: `crates/pattern_runtime/src/sdk/bundle.rs:36-48`  
**Problem**: `SdkBundle` is a hardcoded `frunk::HList!` type alias:

```rust
pub type SdkBundle = frunk::HList![
    MemoryHandler,
    MessageHandler,
    DisplayHandler,
    TimeHandler,
    LogHandler,
    ShellHandler,
    FileHandler,
    SourcesHandler,
    McpHandler,
    RpcHandler,
    SpawnHandler,
];
```

The HList itself is Tidepool-specific — it's how `tidepool_effect::DispatchEffect` expects handlers to be bundled (order-sensitive, type-indexed).

**Why it leaks**: The bundle type is baked into `SessionMachine::run<H>(&mut self, handlers: &mut H, user: &U)` where `H: DispatchEffect<U>`. Cosa (or any future substrate) with a different effect system would not use an HList. It might use a trait object, a different struct layout, or a registry pattern.

**Scope of impact**: Medium. The bundle lives in `session.rs:296-314`, and its construction is substrate-coupled. Handler implementations themselves (time, log, etc.) are reusable — only the bundling is Tidepool-specific.

**Mitigation difficulty**: Medium. Introduce a trait:
```rust
pub trait EffectBundle<U>: Send {
    fn dispatch<R>(&mut self, tag: u32, req: R, user: &U) -> Result<Value, EffectError>;
}

// Tidepool impl
pub struct TidepoolBundle {
    handlers: SdkBundle,
}
impl<U> EffectBundle<U> for TidepoolBundle {
    fn dispatch(&mut self, tag: u32, req: R, user: &U) -> Result<Value, EffectError> {
        // frunk HList dispatch logic here
    }
}
```

Then handlers (TimeHandler, MemoryHandler, etc.) become composable inputs to the bundle rather than hardcoded in an HList type. The trait boundary becomes the substrate boundary.

### 5. **Preflight Is Tidepool-Only (LOW IMPACT)**

**File**: `crates/pattern_runtime/src/preflight.rs`  
**Problem**: `check()` hardcodes checks for `tidepool-extract` binary:

```rust
fn resolve_binary() -> Result<PathBuf, RuntimeError> {
    if let Some(path_str) = std::env::var_os(ENV_TIDEPOOL_EXTRACT) {
        let path = PathBuf::from(&path_str);
        if path.is_file() {
            return Ok(path);
        }
        ...
    }
    match which::which(BINARY_NAME) {
        Ok(path) => Ok(path),
        ...
    }
}
```

**Why it leaks**: Specific to Tidepool's `tidepool-extract` binary. Cosa would have its own compiler (or none if AST-interpreted). This module becomes obsolete or needs substrate-specific reimplementation.

**Scope of impact**: Low. Preflight is called once at session open; it's not on the hot path.

**Mitigation difficulty**: Low. Introduce a trait:
```rust
pub trait SubstratePrecheck: Send + Sync {
    fn check(&self) -> Result<(), RuntimeError>;
}

pub struct TidepoolPrecheck;
impl SubstratePrecheck for TidepoolPrecheck { /* current preflight.rs logic */ }

// In TidepoolRuntime::new, pass in a precheck.
```

---

## Abstractions Already Substrate-Generic

### What Works Without Change

1. **`pattern_core::traits::AgentRuntime` / `Session`** — trait-perfect, zero Tidepool refs.  
   **File**: `/crates/pattern_core/src/traits/agent_runtime.rs`, `session.rs`

2. **`SessionContext`** and **`HasCancelState`** — cancellation machinery is substrate-agnostic.  
   **File**: `crates/pattern_runtime/src/session.rs:35-97`

3. **All handler implementations** (TimeHandler, LogHandler, DisplayHandler, MemoryHandler) — reusable.  
   **File**: `crates/pattern_runtime/src/sdk/handlers/{time,log,display,memory}.rs`  
   **Why**: They only depend on `EffectHandler<U>` trait from `tidepool_effect`, which is generic. The Tidepool coupling is at the **bundling** level (HList dispatch), not the handler level.

4. **Checkpoint log structure** — the event recording pattern is substrate-agnostic (once debug-string serialization is fixed).  
   **File**: `crates/pattern_runtime/src/checkpoint.rs:93-173`  
   **Note**: Event recording (`record_exchange`) is currently handler-specific; refactoring to a generic `record(tag, req, resp)` helper would make it substrate-agnostic.

5. **SDK location resolution** — generic path logic.  
   **File**: `crates/pattern_runtime/src/sdk/location.rs:48-84`

6. **Timeout/budget logic** — generic state machine.  
   **File**: `crates/pattern_runtime/src/timeout.rs`

### Reuse Opportunities for Cosa

- Compile the 10 handler modules (time, log, display, memory, message, shell, file, sources, mcp, rpc, spawn) as-is once the HList bundle decoupling is done.
- Reuse `CheckpointLog` structure with a cosa-specific checkpoint event serialization impl.
- Reuse `SdkLocation` and preflight pattern (probe for cosa compiler binary instead of tidepool-extract).
- Reuse `SessionContext` if cosa's effect system supports the same user-context threading.

---

## Recommended Refactors (Priority Order)

### 1. Extract RuntimeCompiler Trait (HIGHEST VALUE / HIGHEST EFFORT)

**Why first**: Compilation and warmup are the largest substrate-specific operations. Isolating them unblocks cosa migration architecture.

**What to do**:
- Create `pattern_runtime::substrate::Compiler` trait with `async fn compile()` and `async fn warm()` methods.
- Move `tidepool/compile.rs` logic into `TidepoolCompiler` impl.
- Move `SessionMachine` instantiation from `Session::open` into `TidepoolCompiler::warm`.
- Update `TidepoolSession::open` to accept a compiler trait object or concrete type param.
- Update `TidepoolRuntime` to hold / pass a compiler.

**Effort**: 2–3 days (moderate-high).  
**Risk**: Medium — changes session open path, but signature changes are internal.  
**Blocker for**: Any cosa migration; essential before swapping substrates.

**Location edits**:
- New: `crates/pattern_runtime/src/substrate/mod.rs`, `substrate/compiler.rs`
- Modify: `crates/pattern_runtime/src/session.rs` (open signature)
- Modify: `crates/pattern_runtime/src/runtime.rs` (store compiler)
- Modify: `Cargo.toml` (expose new module)

---

### 2. Extract EffectBundle Trait (HIGH VALUE / MEDIUM EFFORT)

**Why second**: Decouples handler bundling from Tidepool's HList. Enables handler reuse.

**What to do**:
- Create `pattern_runtime::substrate::EffectBundle<U>` trait with abstract dispatch.
- Create `TidepoolEffectBundle` wrapper implementing the trait over the HList.
- Move HList construction logic from `Session::open` into `TidepoolEffectBundle::new`.
- Update `SessionMachine::run` signature (if possible) or introduce a `RunWith<B: EffectBundle>` wrapper.
- **Challenge**: `tidepool_effect::DispatchEffect` is Tidepool-internal; we can't change its signature. Workaround: introduce an adapter trait in pattern_runtime that calls through to frunk's dispatch.

**Effort**: 3–4 days (medium).  
**Risk**: Medium — touches hot path (run), but behind trait abstraction.  
**Blocker for**: Handler reuse; enables sharing time/log/display/memory across substrates.

**Location edits**:
- New: `crates/pattern_runtime/src/substrate/mod.rs` or `sdk/bundle_trait.rs`
- Modify: `crates/pattern_runtime/src/sdk/bundle.rs` (add wrapper impl)
- Modify: `crates/pattern_runtime/src/session.rs` (use trait object or type param)

---

### 3. Refactor Checkpoint Event Serialization (MEDIUM VALUE / LOW EFFORT)

**Why third**: Eliminates debug-string format fragility; enables faithful replay.

**What to do**:
- Replace `request_repr: String` / `response_repr: String` with structured fields.
- For Tidepool: capture `tidepool_repr::CoreExpr` or serialize via `serde_json` round-trip.
- For cosa: use cosa's value serialization (TBD when cosa lands).
- Update `record_exchange` helpers to accept pre-serialized data.
- Update snapshot/restore to handle new format + versioning.

**Effort**: 2 days (low).  
**Risk**: Low — affects checkpoint I/O, which is non-critical in Phase 3.  
**Blocker for**: Replay bundles (Phase 4/5).

**Location edits**:
- Modify: `crates/pattern_runtime/src/checkpoint.rs` (event structure + serialization)
- Modify: `crates/pattern_runtime/src/sdk/handlers/memory.rs` + all handler files (record calls)

---

### 4. Generalize Request Types via FromValue Trait (MEDIUM VALUE / MEDIUM EFFORT)

**Why fourth**: Currently blocked by `tidepool_bridge_derive::FromCore` hard requirement. Extracting to a trait enables cosa request types.

**What to do**:
- Create `pattern_runtime::substrate::FromValue` trait: `fn from_value(v: &Value, context: &DeserializeContext) -> Result<Self>`.
- Implement for all 11 request types (memory, message, ..., spawn) via manual impls (Tidepool-specific) or codegen (future).
- **Challenge**: Request types are currently coupled to Haskell GADT constructor names via `#[core(name)]`. A cosa impl would use cosa's AST node types (unknown until cosa lands). For now, keep Tidepool request enums as-is but introduce the trait.

**Effort**: 3–5 days (medium-high).  
**Risk**: Medium — touches every request module, but no runtime behavior change.  
**Blocker for**: Request decoding in cosa runtime.

**Location edits**:
- New: `crates/pattern_runtime/src/substrate/request.rs`
- Modify: all `crates/pattern_runtime/src/sdk/requests/*.rs` files (add FromValue impl)
- Modify: handler dispatch to use trait instead of direct enum handling.

---

### 5. Introduce SubstratePrecheck Trait (LOW VALUE / LOW EFFORT)

**Why last**: Lowest impact, but consistent with trait refactoring.

**What to do**:
- Create `pattern_runtime::substrate::Precheck` trait: `fn check() -> Result<(), RuntimeError>`.
- Implement for Tidepool (current preflight logic).
- Compose into runtime startup.

**Effort**: 1 day (trivial).  
**Risk**: None.  
**Blocker for**: None (optional enhancement).

---

## Known Constraints

### Cannot Change Without Upstream Tidepool

1. **`tidepool_effect::DispatchEffect<U>` interface** — we can't modify Tidepool's effect dispatch trait. Workaround: introduce a `pattern_runtime`-level adapter trait that calls through to frunk's dispatch.

2. **`tidepool_bridge_derive::FromCore` macro** — specific to Tidepool's DataConTable system. Cosa would have its own; we can't unify them. Accept that request enums will differ per substrate.

3. **`tidepool_repr::Value` type** — FFI boundary for JIT results. We can't change its Debug impl without forking Tidepool. Workaround: use structured serialization (serde_json) instead of debug-string round-trips.

### Cannot Change Without Cosa / Future Substrate

1. **Compilation model** — Cosa may be AST-interpreted (no separate compilation step) or have a different compiler. The `RuntimeCompiler` trait must be elastic enough to handle "no compile step" (synchronous, zero-latency).

2. **Effect system** — Cosa may not use freer-simple. Its effect system will determine how requests and responses flow. The `EffectBundle` trait abstracts over this, but the exact interface depends on cosa's model.

3. **Value types** — Cosa's internal values (AST nodes, interpreter state) are not yet defined. Checkpoint serialization must be flexible enough to round-trip whatever cosa produces.

---

## Test Coupling & Anti-Patterns to Avoid

### Current Testing Strengths

1. **Preflight verification** (`tests/preflight.rs`) — isolated, no tidepool-extract dependency, very good.
2. **Handler unit tests** (time/log handlers have their own test modules) — generically structured, reusable.
3. **Session lifecycle tests** (`tests/session_lifecycle.rs`) — comprehensive, but **Tidepool-coupled** (see below).

### Anti-Patterns to Avoid

1. **Don't** hardcode `SessionMachine` types in tests. Use the `Session` trait.  
   **Current**: Many tests construct `SessionMachine` directly.  
   **Impact**: Tests become Tidepool-specific, blocking cosa testing.  
   **Fix**: Introduce a test-only `SessionFactory` trait that both Tidepool and test-mocks implement.

2. **Don't** assume `Value` type in checkpoint tests.  
   **Current**: Tests like `tests/session_lifecycle.rs` match on `tidepool_eval::Value`.  
   **Impact**: Checkpoint assertions become Tidepool-specific.  
   **Fix**: Use opaque value equality; compare serialized checkpoint events instead of raw values.

3. **Don't** leak `CheckpointEvent` debug-string format into assertions.  
   **Current**: Tests might assert on `event.request_repr.contains("...")`.  
   **Impact**: Fragile to value debug-repr changes; won't work with cosa.  
   **Fix**: Compare structured fields (tag, turn, sequence) only.

### What to Do

- Create a `test_runtime.rs` helper that constructs a complete runtime stack via traits.
- Add feature-gated mock implementations (`MockCompiler`, `MockBundle`) for unit testing without Tidepool.
- Update integration tests to use trait-based factories rather than hardcoded types.

---

## Risks & Uncertainties

### Risks

1. **Compilation overhead migration**: If cosa is AST-interpreted, the `RuntimeCompiler::warm()` step may be cheap (or nonexistent). We need to validate that the compiler trait can handle zero-cost substrates.

2. **Effect dispatch performance**: frunk's HList dispatch is type-indexed; Tidepool's JIT understands HList positions as numeric tags. A cosa runtime with a different dispatch mechanism might have different perf characteristics. The `EffectBundle` trait must remain transparent about this.

3. **Request/response serialization fidelity**: If cosa values don't serialize to the same JSON shape as Tidepool values, checkpoint round-trips will break. The refactor to structured serialization must coordinate with cosa's serialization design (unknown until cosa lands).

### Uncertainties

1. **Cosa's effect system design** — not yet finalized. The trait design should be flexible, but without seeing cosa's actual request/response types, we're extrapolating.

2. **Multi-substrate coexistence** — the design doesn't yet account for running Tidepool and cosa runtimes in the same binary (e.g., for gradual migration). If that's needed, additional trait unification is required.

3. **Performance impact of traits** — introducing `RuntimeCompiler`, `EffectBundle`, and `FromValue` traits adds indirection. Measure to ensure no hot-path regression.

---

## Implementation Path (Not Recommended Until Cosa Arrives)

**Suggested sequence** (when cosa substrate is ready to spike):

1. **Spike 1** (1 week): Implement cosa's equivalent of `RuntimeCompiler`. Validate that the trait interface is elastic enough.  
   → **Gate**: If trait doesn't work, redesign before proceeding.

2. **Spike 2** (1 week): Implement cosa's request types and a `FromValue` impl. Verify that request serialization works.  
   → **Gate**: If serialization is too divergent, adjust checkpoint format.

3. **Refactor 1** (2–3 days): Extract `RuntimeCompiler` trait into pattern_runtime. Port Tidepool implementation.  
   → **Validation**: All Tidepool tests pass, no behavior change.

4. **Refactor 2** (3–4 days): Extract `EffectBundle` trait. Implement both Tidepool and cosa wrappers.  
   → **Validation**: Handler dispatch still works, no perf regression.

5. **Refactor 3** (2 days): Refactor checkpoint serialization. Implement cosa and Tidepool serialization impls.  
   → **Validation**: Snapshots round-trip correctly for both.

6. **Refactor 4** (3–5 days): Generalize request types. Implement cosa request enums.  
   → **Validation**: Both Tidepool and cosa agents compile and run.

7. **Testing** (ongoing): Update integration tests to use trait-based factories. Add cosa-specific tests.

---

## Summary of Findings

| Area | Current State | Refactor Value | Effort | Blocker? |
|------|---------------|---|---|---|
| **Compilation** | Tidepool-hardcoded | Extract trait | 2–3d | **Yes** (highest) |
| **Effect bundling** | HList-hardcoded | Extract trait + adapter | 3–4d | **Yes** (high) |
| **Request types** | FromCore derive | Introduce trait impl | 3–5d | Medium |
| **Checkpoint format** | Debug strings | Structured serialization | 2d | Medium |
| **Preflight** | Tidepool-specific binary check | Extract trait | 1d | Low |
| **Session/Runtime traits** | Substrate-agnostic | None needed | — | No |
| **Handlers** | Substrate-agnostic | None needed | — | No |
| **Timeout/cancel** | Substrate-agnostic | None needed | — | No |

**Conclusion**: Modular refactoring is feasible. No fundamental redesign needed. Cosa migration is achievable with ~2–3 weeks of focused trait extraction and validation, once cosa lands and its interfaces stabilize.

