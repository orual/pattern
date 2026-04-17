# Pattern v3 Foundation — Phase 3: Tidepool FFI + minimal runtime

**Goal:** Embed tidepool-runtime in `pattern_runtime`. Stand up a session lifecycle that compiles the agent's Haskell program once per session and runs it per turn. Wire a `freer-simple` effect algebra across 11 SDK namespaces (`time`, `log`, and `display` fully implemented; rest stubs). Wrap execution with an external wall-clock + CPU timeout harness. Ship a minimal `AgentRuntime` + `Session` impl against the Phase 2 trait surface, plus an event-log checkpoint.

**Architecture:**
- `pattern_runtime` gains a `tidepool/` module wrapping `tidepool-runtime` + `tidepool-effect` + `tidepool-bridge`.
- `Session::open` = `compile_haskell` + `JitEffectMachine::compile`, cached on the Session.
- `Session::step` = `machine.run(...)` with per-turn input flowing in via effect dispatch.
- Timeout harness sits outside the JIT loop: tokio's wall-clock timeout + a sampling CPU watchdog that polls `/proc/self/stat` (Linux) at 100ms intervals.
- Checkpoint = append-only log of `(EffectRequest, Value)` exchanges, persisted per session. Restart = fresh compile + replay.

**Tech Stack:** Rust 2024, tidepool-runtime (path dep to `../tidepool`), freer-simple (via tidepool-effect), `frunk::HList` for handler bundling, tokio for async + timeouts, tracing for logs.

**Scope:** Phase 3 of 6. Covers v3-foundation.AC2.*.

**Codebase verified:** 2026-04-16

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-foundation.AC2: Tidepool runtime embedded with bounded execution

- **v3-foundation.AC2.1 Success:** A trivial Haskell program (`pure "hello"`) loads, runs, and returns its result to the Rust caller
- **v3-foundation.AC2.2 Success:** `ctx.time.now` effect dispatched from Haskell is handled in Rust and returns current time correctly to agent code
- **v3-foundation.AC2.3 Success:** `ctx.log` effect writes a structured entry observable from Rust-side instrumentation
- **v3-foundation.AC2.4 Success:** Turn-level checkpoint captures session environment; restore replays the captured state deterministically (round-trip equality)
- **v3-foundation.AC2.5 Failure:** Program exceeding wall-clock budget (e.g., `forever $ pure ()`) is killed by timeout harness before exceeding 1.5× the configured budget
- **v3-foundation.AC2.6 Failure:** Program exceeding CPU budget (non-yielding compute) is killed with `RuntimeError::Timeout { cpu_ms, .. }`
- **v3-foundation.AC2.7 Failure:** Effect hitting Tidepool's 10K-node response limit returns `RuntimeError::EffectOverflow`
- **v3-foundation.AC2.8 Failure:** GHC runtime crash mid-execution returns `RuntimeError::RuntimeCrashed`; session is marked unusable
- **v3-foundation.AC2.9 Edge:** Stubbed `spawn`/`mcp`/`ipc` effects return clear "not yet implemented" error, not silent hang
- **v3-foundation.AC2.10 Edge:** Concurrent turns on distinct sessions do not interfere with each other's state (thread-safety of FFI boundary)

---

## Executor Context

**Repo root:** `/home/orual/Projects/PatternProject/pattern`
**Working bookmark:** `rewrite-v3`
**Pre-phase state:** After Phase 2, `pattern_core` is traits + types + errors + preserved memory storage. `pattern_runtime` is an empty skeleton (lib.rs + CLAUDE.md only). `rewrite-staging/agent_runtime/` holds the pre-v3 agent loop code for reference only.

**Tidepool source:** `github:tidepool-heavy-industries/tidepool`. Local checkout expected at `/home/orual/Projects/PatternProject/tidepool` (sibling of pattern repo) for Cargo path-dep consumption and for overriding the nix flake input during tidepool-side iteration. **Re-verified against tidepool commit `746da8b` ("feat: consolidate error handling with thiserror")**; the original Phase 3 research was done at `cc0ebf815…`, and this phase file's error-mapping table + API references have been refreshed to match `746da8b`.

**Path-dep policy:** Phase 3 uses Cargo path deps (`tidepool-runtime = { path = "../tidepool/tidepool-runtime" }`) during the v3 rewrite for ease of iteration on both sides; the Nix flake input is pinned via `flake.lock` against the GitHub repo for reproducible devshells. When the foundation lands and tidepool stabilises, convert the Cargo deps to a git dep pinned to commit (or an upstream crates.io release if tidepool publishes one). Tracked as a follow-up in the post-foundation dep-hardening plan.

**Runtime dependency:** `tidepool-extract` GHC plugin binary (~300MB, GHC 9.12) must be on `$PATH` at runtime, or pointed at via `$TIDEPOOL_EXTRACT` (absolute path to the binary). **Reviewer note:** the earlier research notes mentioned `TIDEPOOL_PRELUDE_DIR` and `TIDEPOOL_GHC_LIBDIR` as overrides; verified against tidepool `746da8b`, only `TIDEPOOL_EXTRACT` is read by `tidepool_runtime`. The Nix-built derivation wraps the extractor with a shell script that sets up GHC PATH internally, so no prelude/libdir overrides are needed in practice. Pattern ships a preflight check (Task 5) and flake.nix integration (Task 4) to reduce setup friction.

**Build tools:**
- `cargo check -p pattern_runtime`
- `cargo nextest run -p pattern_runtime`
- `cargo test --doc -p pattern_runtime`
- `cargo clippy -p pattern_runtime --all-features --all-targets -- -D warnings`
- `just pre-commit-all`

**Commit convention:** `[pattern-runtime] …` for this phase's work. `[meta]` for workspace / flake edits.

**Design reference:** `/home/orual/Projects/PatternProject/pattern/docs/design-plans/2026-04-16-v3-foundation.md` Phase 3 section. Updated tidepool API reference at `/home/orual/Projects/PatternProject/pattern/docs/reference/tidepool.md` (revised 2026-04-16 with public `JitEffectMachine::compile`/`run` split).

**Key tidepool APIs (verified in source):**
- `tidepool_runtime::compile_haskell(source, target, include) -> Result<CompileResult, CompileError>` — returns `(CoreExpr, DataConTable, Warnings)`. CBOR-cached in `~/.cache/tidepool/`.
- `tidepool_codegen::JitEffectMachine::compile(&CoreExpr, &DataConTable, nursery_size) -> Result<Self, JitError>` — JITs to native code.
- `JitEffectMachine::run<U, H: DispatchEffect<U>>(&mut self, &DataConTable, &mut H, &U) -> Result<Value, JitError>` — dispatches effects, returns final value. Re-runnable without recompile.
- `tidepool_effect::EffectHandler<U>` trait — one impl per SDK namespace.
- `frunk::hlist![H0, H1, ...]` — HList bundling handlers; tag dispatch routes automatically by handler position.
- Error hierarchy: `RuntimeError { Compile(CompileError), Jit(JitError) }`; `JitError { Compilation, Pipeline, Effect, Yield, Signal }`; `YieldError { DivisionByZero, Overflow, StackOverflow, HeapOverflow, Signal(i32), UserError, UserErrorMsg(String), Undefined, BlackHole, BadThunkState }`.

**Mapping tidepool errors to `pattern_core::error::RuntimeError`** (verified against tidepool commit `746da8b` — "feat: consolidate error handling with thiserror"):

| Tidepool | Pattern |
|---|---|
| `CompileError::ExtractFailed(stderr)` | `RuntimeError::GhcPanic { reason: stderr }` |
| `CompileError::Io(_)` / `ReadError(_)` / `MissingOutput(_)` / `IOTypeDetected` | `RuntimeError::GhcPanic { reason: e.to_string() }` (extractor setup / IO sandbox violation) |
| `JitError::Signal(SignalError)` | `RuntimeError::RuntimeCrashed` (JIT-time signal during codegen or heap bridge) |
| `JitError::HeapBridge(_)` | `RuntimeError::RuntimeCrashed` (heap-object conversion failed) |
| `JitError::MissingConTags(name)` | `RuntimeError::GhcPanic { reason: format!("missing freer-simple constructor: {name}") }` (agent DSL missing required constructors) |
| `JitError::EffectResponseTooLarge { nodes, limit }` | `RuntimeError::EffectOverflow` (dedicated variant as of 746da8b; older research notes conflated with `JitError::Effect`) |
| `JitError::Effect(EffectError)` | bubble up the handler's `EffectError` as an `SdkError` (handler-local) — this is an SDK call failing, not a runtime crash |
| `JitError::Yield(YieldError::StackOverflow \| HeapOverflow)` | `RuntimeError::RuntimeCrashed` (treat as unrecoverable) |
| `JitError::Yield(YieldError::Signal(sig))` | `RuntimeError::RuntimeCrashed` |
| `JitError::Yield(YieldError::DivisionByZero \| Overflow \| BlackHole \| BadThunkState \| NullFunPtr \| BadFunPtrTag \| UnresolvedVar \| TypeMetadata)` | `RuntimeError::RuntimeCrashed` (runtime-semantic errors from agent code) |
| `JitError::Yield(YieldError::UserError \| UserErrorMsg)` | surface as agent-logic output, not `RuntimeError` (agent called Haskell's `error`) |
| `JitError::Yield(YieldError::UnexpectedTag \| UnexpectedConTag \| BadValFields \| BadEFields \| BadUnionFields \| NullPointer)` | `RuntimeError::RuntimeCrashed` (heap-parse errors at the result boundary — implementation bugs, should be rare) |
| `JitError::Pipeline(_)` / `JitError::Compilation(_)` | `RuntimeError::GhcPanic` (compile/codegen pipeline failed — generally happens at `compile_haskell`/`JitEffectMachine::compile` time, not during `run`) |
| (external wrapper) wall-clock timeout expired | `RuntimeError::Timeout { wall_ms, cpu_ms: <last sample> }` |
| (external wrapper) CPU sample exceeded budget | `RuntimeError::Timeout { wall_ms: <elapsed>, cpu_ms }` |

**Rust-coding-style reminders:**
- All errors `#[non_exhaustive]` via the Phase 2 hierarchy. New variants added this phase go to `pattern_core::error::RuntimeError` if shared, or a new `pattern_runtime::SdkError` if handler-local.
- Newtype IDs for anything identity-bearing (`SessionId`, `TurnId`).
- `module.rs + module/submodule.rs` layout.
- `cargo fmt` + `cargo clippy -- -D warnings` clean before every commit.

---

<!-- START_SUBCOMPONENT_A (tasks 1-6) -->
<!-- START_TASK_1 -->
### Task 1: Add tidepool deps to workspace

**Verifies:** contributes to AC2.1 (runtime can link against tidepool).

**Files:**
- Modify: `/home/orual/Projects/PatternProject/pattern/Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `/home/orual/Projects/PatternProject/pattern/crates/pattern_runtime/Cargo.toml` (use workspace deps)

**Step 1: Add workspace entries**

In the root `Cargo.toml` `[workspace.dependencies]` section, add:

```toml
# Tidepool: Haskell-in-Rust JIT runtime. Path deps during the v3 rewrite
# for ease of dual-iteration; tracked for conversion to git-rev deps
# (or upstream crates.io releases) in the post-foundation dep-hardening plan.
tidepool-runtime = { path = "../tidepool/tidepool-runtime" }
tidepool-codegen = { path = "../tidepool/tidepool-codegen" }
tidepool-effect  = { path = "../tidepool/tidepool-effect"  }
tidepool-bridge  = { path = "../tidepool/tidepool-bridge"  }
tidepool-repr    = { path = "../tidepool/tidepool-repr"    }
frunk = "0.4"
```

**Step 2: Wire pattern_runtime/Cargo.toml**

```toml
[dependencies]
pattern_core = { path = "../pattern_core" }
tidepool-runtime = { workspace = true }
tidepool-codegen = { workspace = true }
tidepool-effect  = { workspace = true }
tidepool-bridge  = { workspace = true }
tidepool-repr    = { workspace = true }
frunk = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true, features = ["rt", "time", "sync", "macros"] }
tracing = { workspace = true }
thiserror = { workspace = true }
miette = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "test-util", "macros"] }
```

**Step 3: Verify**

```bash
cd /home/orual/Projects/PatternProject/pattern
cargo check -p pattern_runtime 2>&1 | tail -20
```
Expected: compiles (still an empty lib.rs; we've just pulled in deps).

**Step 4: Sanity check tidepool path**

```bash
ls ../tidepool/tidepool-runtime/Cargo.toml
```
Expected: file exists. If not, pull tidepool (the tidepool repo must be cloned as a sibling of `pattern/`).

**Commit:**
```bash
jj describe -m "[pattern-runtime] wire tidepool path deps (path deps during rewrite; convert to git-rev in post-foundation)"
jj new
```
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: FFI wrapper module scaffolding

**Verifies:** AC2.1, AC2.8, AC2.10 — organises where FFI boundary logic will land; enforces error-mapping centralisation.

**Files:**
- Create: `crates/pattern_runtime/src/tidepool.rs` (module root)
- Create: `crates/pattern_runtime/src/tidepool/compile.rs` (compile_haskell wrapper + fate-marker policy)
- Create: `crates/pattern_runtime/src/tidepool/machine.rs` (JitEffectMachine wrapper with Send safety assertions)
- Create: `crates/pattern_runtime/src/tidepool/error_map.rs` (tidepool error → pattern_core::RuntimeError conversion)

**Step 1: tidepool.rs**

```rust
//! Tidepool FFI boundary.
//!
//! Wraps `tidepool-runtime` and `tidepool-codegen` public APIs into a
//! Pattern-shaped surface: one compile call per session, many run calls per turn,
//! thread-safety assertions, and error-hierarchy translation.
//!
//! Phase 3 focuses on the minimum needed for the agent loop:
//! - `compile::compile_program` — warm a reusable `JitEffectMachine` for a persona
//! - `machine::SessionMachine` — one compiled program, many runs
//! - `error_map::map_runtime_error` / `map_jit_error` — central translation point

pub mod compile;
pub mod error_map;
pub mod machine;

pub use compile::{compile_program, CompiledProgram};
pub use machine::SessionMachine;
```

**Step 2: tidepool/compile.rs**

Signature sketch (task-implementor fills in the body with actual tidepool API calls):

```rust
use pattern_core::error::RuntimeError;
use std::path::Path;
use tidepool_runtime::{compile_haskell, CompileResult};

pub struct CompiledProgram {
    pub core: tidepool_repr::core::CoreExpr,
    pub data_cons: tidepool_codegen::DataConTable,
    pub warnings: Vec<String>,
}

/// Compile a Haskell agent program once per session.
///
/// `source` is the full Haskell source text for the agent. `target` is the
/// top-level binder to extract (e.g., "agent"). `include_dirs` must contain
/// the Pattern SDK modules (see `sdk::location`).
pub fn compile_program(
    source: &str,
    target: &str,
    include_dirs: &[&Path],
) -> Result<CompiledProgram, RuntimeError> {
    // 1. Call compile_haskell, map CompileError → RuntimeError::GhcPanic.
    // 2. Unpack CompileResult into CompiledProgram.
    // 3. Log warnings via tracing.
    // ... implementation per task-implementor ...
    todo!(
        "implement in task 2 per tidepool-runtime::compile_haskell wrapper; \
         phase: 3; AC: AC2.1"
    )
}
```

**Step 3: tidepool/machine.rs**

```rust
use pattern_core::error::RuntimeError;
use tidepool_codegen::JitEffectMachine;

/// Wraps a tidepool `JitEffectMachine` with session-scoped nursery + Send assertions.
///
/// `JitEffectMachine` internally contains an `unsafe impl Send` on its hot loop;
/// this wrapper documents Pattern's contract: at most one thread mutates it at a
/// time (enforced by &mut self on `run`). Multiple `SessionMachine`s across
/// distinct sessions run concurrently without interference (AC2.10).
pub struct SessionMachine {
    inner: JitEffectMachine,
    data_cons: tidepool_codegen::DataConTable,
    nursery_size: usize,
}

impl SessionMachine {
    pub fn new(program: CompiledProgram, nursery_size: usize) -> Result<Self, RuntimeError> {
        // JitEffectMachine::compile(&program.core, &program.data_cons, nursery_size)
        todo!("phase: 3; AC: AC2.1")
    }

    pub fn run<U, H>(&mut self, handlers: &mut H, user: &U) -> Result<tidepool_eval::Value, RuntimeError>
    where
        H: tidepool_effect::DispatchEffect<U>,
    {
        // self.inner.run(&self.data_cons, handlers, user)
        //   .map_err(crate::tidepool::error_map::map_jit_error)
        todo!("phase: 3; AC: AC2.1")
    }
}
```

**Step 4: tidepool/error_map.rs**

One function per source-error type. Implements the mapping table in the Executor Context section.

**Step 5: Lib.rs wiring**

```rust
pub mod tidepool;
pub use tidepool::{CompiledProgram, SessionMachine};
```

**Step 6: Verify skeleton compiles** — `cargo check -p pattern_runtime`. All `todo!()` bodies carry phase + AC refs, per AC1.8 (satisfied by the cruft audit).

**Commit:**
```bash
jj describe -m "[pattern-runtime] FFI wrapper scaffolding: tidepool/{compile,machine,error_map}"
jj new
```
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: Error-map fleshed out

**Verifies:** AC2.7, AC2.8, AC2.9.

**Files:**
- Modify: `crates/pattern_runtime/src/tidepool/error_map.rs` — full mapping implementation
- Modify: `crates/pattern_core/src/error/runtime.rs` — if any `RuntimeError` variants need adding (e.g., `EffectOverflow` detail), do it here

**Step 1:** Implement per the mapping table. Handle nested `JitError::Yield(YieldError::*)` structure properly.

**Step 2:** Add unit tests in `tidepool/error_map.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_failure_becomes_ghc_panic() {
        let input = tidepool_runtime::CompileError::ExtractFailed("kaboom".into());
        let mapped = map_compile_error(input);
        assert!(matches!(mapped, RuntimeError::GhcPanic { .. }));
    }

    #[test]
    fn signal_becomes_runtime_crashed() {
        // ... construct JitError::Signal, assert mapping ...
    }

    // One test per mapping row.
}
```

**Step 3:** Verify.

```bash
cargo nextest run -p pattern_runtime tidepool::error_map 2>&1 | tail
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] tidepool error mapping with unit coverage (AC2.7, AC2.8, AC2.9)"
jj new
```
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: flake.nix + README setup instructions for tidepool-extract

**Verifies:** reduces setup friction. Contributes to AC2.1 (compile succeeds when environment is right).

**Files:**
- Modify: `/home/orual/Projects/PatternProject/pattern/flake.nix` — pull `tidepool-extract` into the dev shell
- Modify: `/home/orual/Projects/PatternProject/pattern/crates/pattern_runtime/CLAUDE.md` — add setup section
- Modify: `/home/orual/Projects/PatternProject/pattern/README.md` — one-liner pointing at the setup section

**Step 1: flake.nix integration** (folded into Phase 3 prep — see the prep commit before Task 1 dispatch)

Pattern's flake uses `flake-parts` with per-system modules under `nix/modules/`. Integration is two edits:

1. Add tidepool as a flake input in `flake.nix`:

   ```nix
   tidepool.url = "github:tidepool-heavy-industries/tidepool";
   ```

   `github:` inputs are pure and lock properly via `flake.lock`. (An earlier draft suggested `path:../tidepool` — rejected as impure.)

2. Wire the derivation into `nix/modules/devshell.nix`:

   ```nix
   let
     tidepool-extract = inputs.tidepool.packages.${system}.tidepool-extract;
   in {
     devShells.default = pkgsWithUnfree.mkShell {
       # ...existing config...
       TIDEPOOL_EXTRACT = "${tidepool-extract}/bin/tidepool-extract";
       packages = [ /* existing packages */ ] ++ [ tidepool-extract ];
     };
   }
   ```

The tidepool-built derivation is a `writeShellScriptBin` wrapper that sets up GHC PATH internally, so no `TIDEPOOL_PRELUDE_DIR` or `TIDEPOOL_GHC_LIBDIR` exports are needed. Only `TIDEPOOL_EXTRACT` is consumed by tidepool-runtime in the current commit.

Developers iterating on tidepool itself should override the flake input locally:

```sh
nix develop --override-input tidepool path:../tidepool
```

**Step 2: pattern_runtime/CLAUDE.md**

Append a "Runtime setup" section documenting:
- `tidepool-extract` binary dependency, where it comes from, how to confirm it's installed
- The three env vars and when each is needed
- Error message surface when setup is wrong (Task 5 preflight gives actionable errors)

**Step 3: README.md**

Append to "Getting Started" (or equivalent section): one paragraph noting that Pattern v3 requires `tidepool-extract` on `$PATH` for the agent runtime, and to enter the dev shell (`nix develop`) or follow `crates/pattern_runtime/CLAUDE.md` for manual setup.

**Step 4: Verify**

Nix check: `nix develop` succeeds and `which tidepool-extract` prints a path. If on a non-Nix host, the README's manual-setup instructions are followed and the same check passes.

**Commit:**
```bash
jj describe -m "[meta] flake + docs for tidepool-extract runtime dep"
jj new
```
<!-- END_TASK_4 -->

<!-- START_TASK_5 -->
### Task 5: Preflight check — tidepool-extract on PATH

**Verifies:** contributes to AC2.1 failure modes being informative.

**Files:**
- Create: `crates/pattern_runtime/src/preflight.rs`
- Modify: `crates/pattern_runtime/src/lib.rs` — expose `preflight::check()`

**Implementation:**

```rust
//! Preflight checks Pattern runs at Session open (or callable explicitly).
//! Returns early with a human-readable diagnostic if the runtime environment
//! can't support Tidepool compilation.

use pattern_core::error::RuntimeError;

pub fn check() -> Result<(), RuntimeError> {
    // 1. `tidepool-extract` on PATH (or TIDEPOOL_EXTRACT override set and points at an
    //    executable file).
    // 2. Invoke `tidepool-extract --version` with a short timeout; surface stderr on failure.
    // 3. Warn (don't fail) if TIDEPOOL_PRELUDE_DIR is unset — binary's default may or may
    //    not be correct depending on how it was built.
    // Return miette::Diagnostic-rich error on failure.
    todo!("phase: 3; AC: AC2.1 infrastructure")
}
```

Diagnostic content on failure (example):
```
error: tidepool-extract not found on PATH (or TIDEPOOL_EXTRACT env var)

Pattern v3 agents require the tidepool-extract GHC plugin binary to compile
agent programs. To install:

  - Nix users: `nix develop` in the pattern repo root.
  - Manual: see crates/pattern_runtime/CLAUDE.md for GHC 9.12 + cabal setup.

Expected one of:
  - `tidepool-extract` on PATH
  - $TIDEPOOL_EXTRACT set to an executable path
```

**Step 1:** Write `preflight::check()`. Use `which` crate for PATH lookup (workspace dep — add if not already).

**Step 2:** Call preflight from `Session::open` (added in Task 14) before any compile call. Return the preflight error from `Session::open` directly.

**Step 3:** Test with two scenarios — `tidepool-extract` available vs. PATH mangled. Document in test comment.

```rust
#[cfg(test)]
mod tests {
    #[test]
    #[ignore] // requires tidepool-extract; run via `cargo nextest run preflight -- --ignored`
    fn succeeds_when_extract_on_path() { /* ... */ }

    #[test]
    fn fails_with_actionable_message_when_missing() {
        // Temporarily clear PATH; confirm error message contains install hint.
    }
}
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] preflight: tidepool-extract availability check with actionable diagnostic"
jj new
```
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Compile-time benchmark harness

**Verifies:** informs runtime architecture decisions; not a gating AC.

**Files:**
- Create: `crates/pattern_runtime/benches/compile_time.rs` (criterion or raw timing)
- Modify: `crates/pattern_runtime/Cargo.toml` — add `[dev-dependencies] criterion = ...` if using criterion

**Implementation:**

Measure real cold + warm compile times for representative program sizes. Informs whether the "compile once per session, run many" architecture is sufficient or whether we need further optimisation.

```rust
// Cold: clear ~/.cache/tidepool/ first, measure first compile_haskell.
// Warm: compile once, clear only the JIT machine, measure compile_haskell → JitEffectMachine::compile.
// Hot run: one JitEffectMachine::run round-trip on `pure "hello"`.

// Three program sizes:
//   - 10 lines (trivial)
//   - 100 lines (realistic minimal agent)
//   - 500 lines (complex agent with multiple effect imports)
```

Report median + p99 for each. Target bands (guidance, not hard requirements):
- trivial cold: <3s
- trivial warm: <500ms
- realistic cold: <5s
- realistic warm: <1s
- hot run: <50ms

**Commit:**
```bash
jj describe -m "[pattern-runtime] compile-time bench harness (informational)"
jj new
```
<!-- END_TASK_6 -->
<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 7-10) -->
<!-- START_TASK_7 -->
### Task 7: Haskell-side SDK module tree

**Verifies:** contributes to AC2.1, AC2.2, AC2.3, AC2.9.

**Files:**
- Create: `crates/pattern_runtime/haskell/Pattern/Memory.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Message.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Display.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Shell.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/File.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Sources.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Mcp.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Time.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Log.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Ipc.hs`
- Create: `crates/pattern_runtime/haskell/Pattern/Spawn.hs`
- Create: `crates/pattern_runtime/haskell/README.md` (explains the module tree)

**Implementation guidance:**

Each module defines its GADT-based effect algebra per freer-simple. Canonical shape (see `/home/orual/Projects/PatternProject/tidepool/examples/tide/haskell/Effects.hs`):

```haskell
-- Pattern/Time.hs
{-# LANGUAGE GADTs #-}
module Pattern.Time where

import Control.Monad.Freer (Eff, Member, send)

data Time a where
  Now   :: Time Integer       -- nanoseconds since epoch
  Sleep :: Integer -> Time () -- sleep ns (handler may decline for long values)

now :: Member Time effs => Eff effs Integer
now = send Now

sleep :: Member Time effs => Integer -> Eff effs ()
sleep ns = send (Sleep ns)
```

For stub namespaces (`mcp`, `ipc`, `spawn`), declare the GADT but have the Rust-side handler return an error:

```haskell
-- Pattern/Mcp.hs
{-# LANGUAGE GADTs #-}
module Pattern.Mcp where
import Control.Monad.Freer (Eff, Member, send)

-- Stubbed. Rust handler returns NotImplemented. Future: plugin-system plan.
data Mcp a where
  Call :: String -> String -> Mcp ()

call :: Member Mcp effs => String -> String -> Eff effs ()
call server method = send (Call server method)
```

**File by file:**

- `Memory.hs` — `Read BlockHandle`, `Write BlockHandle Content`, `Append BlockHandle Content`, `Search Query`, `Recall Handle`, `Archive Handle`.
- `Message.hs` — `Ask Request` (returns post-streaming `(MessageContent, Usage)`; Phase 4 wires the streaming provider client underneath), `Send Caller Body`, `Reply MessageId Body`, `Notify ChannelId Body`.
- `Display.hs` — **fully implemented in Phase 3**. One-way forward-to-output-surface effect. `Chunk ChunkPayload`, `Final MessageContent`. The chunk stream from `Message.Ask` flows through this effect so display/UX layers can render partial output in realtime. Handler is a broadcast-style dispatcher with registered subscribers (see Task 10's display handler impl).
- `Shell.hs` — stubbed in phase 3. `Execute Command`, `Spawn Command`, `Kill Pid`, `Status Pid`.
- `File.hs` — stubbed in phase 3. `Read Path`, `Write Path Content`, `List Path`.
- `Sources.hs` — stubbed in phase 3. `Stream Name`, `Subscribe Name Cb`, `List`.
- `Mcp.hs` — stubbed.
- `Time.hs` — fully implemented.
- `Log.hs` — fully implemented. `Info Msg`, `Warn Msg`, `Error Msg`, `Debug Msg`.
- `Ipc.hs` — stubbed.
- `Spawn.hs` — stubbed.

**Streaming + effect-shape note:** From the agent Haskell program's perspective, `Message.Ask` is a one-shot: it calls the effect, gets back a fully-assembled `(MessageContent, Usage)` after streaming completes internally. The agent doesn't iterate chunks in Haskell. What the user/UX sees as "streaming output" happens on the Rust side — while `Message.Ask` is blocked awaiting the provider's stream, the MessageHandler forwards incoming chunks to the `Display` effect's registered subscribers (CLI bin, future UX layers). Agent programs that want to react mid-stream (e.g., kick off a subagent on seeing specific text) register a Display subscriber rather than trying to iterate chunks at the Haskell level.

Why one-shot at the agent level:
- Keeps the freer-simple effect algebra clean (Haskell programs don't wrangle chunk streams).
- Subagent synchronous-interface-but-streamed-internals (per design discussion) maps naturally: the subagent's `Message.Ask` is one-shot from the subagent's POV, but display subscribers registered by the parent agent see chunks in real time and can trigger parallel work.
- CLI / display / telemetry layers pluggable and independent of agent logic.

**Step 1:** Write each .hs file. Keep the shapes tight — one algebra per file, smart constructors for each variant.

**Step 2:** README at `crates/pattern_runtime/haskell/README.md` explains:
- Why the modules live here (source of truth; Rust handler enums mirror this shape).
- That `SdkLocation::Directory` (Task 11) resolves to this directory by default via `CARGO_MANIFEST_DIR`.
- Constructor-name parity: Haskell variant name ⇄ Rust `#[core(name = "...")]` attribute must match.

**Step 3:** Write `crates/pattern_runtime/haskell/Pattern/Prelude.hs` that re-exports the common subset (Time, Log, Memory, Message, Display) for ergonomic imports in agent programs.

**Commit:**
```bash
jj describe -m "[pattern-runtime] haskell SDK module tree: 11 effect algebras (10 + display) + prelude re-exports"
jj new
```
<!-- END_TASK_7 -->

<!-- START_TASK_8 -->
### Task 8: Rust-side effect request enums (FromCore)

**Verifies:** AC2.2, AC2.3, AC2.9.

**Files:**
- Create: `crates/pattern_runtime/src/sdk.rs` (module root)
- Create: `crates/pattern_runtime/src/sdk/requests/` directory
  - `mod.rs`, `memory.rs`, `message.rs`, `display.rs`, `shell.rs`, `file.rs`, `sources.rs`, `mcp.rs`, `time.rs`, `log.rs`, `ipc.rs`, `spawn.rs`

**Implementation:**

One request enum per effect namespace, deriving `FromCore` (from `tidepool-bridge-derive`). Variant names must match Haskell constructor names byte-for-byte.

```rust
// sdk/requests/time.rs
use tidepool_bridge::FromCore;

#[derive(Debug, FromCore)]
pub enum TimeReq {
    #[core(name = "Now")]
    Now,
    #[core(name = "Sleep")]
    Sleep(i64),
}
```

```rust
// sdk/requests/log.rs
use tidepool_bridge::FromCore;

#[derive(Debug, FromCore)]
pub enum LogReq {
    #[core(name = "Info")]  Info(String),
    #[core(name = "Warn")]  Warn(String),
    #[core(name = "Error")] Error(String),
    #[core(name = "Debug")] Debug(String),
}
```

```rust
// sdk/requests/display.rs
//
// The Display effect is broadcast-style: the Haskell agent emits one-shot
// envelopes describing observable output. Subscribers registered Rust-side
// receive them in realtime. See sdk/handlers/display.rs for subscriber shape.
use tidepool_bridge::FromCore;

#[derive(Debug, FromCore)]
pub enum DisplayReq {
    /// A partial chunk during a streaming provider response. Forwarded to
    /// every registered subscriber as-is.
    #[core(name = "Chunk")]
    Chunk(String), // simplified payload; real shape carries the ChunkKind enum serialized
    /// Final assembled content for the turn's Message.Ask. Fired once,
    /// after the provider stream completes.
    #[core(name = "Final")]
    Final(String),
    /// Agent-visible note (typing indicator, tool-call progress, etc.) that
    /// isn't part of the LLM response stream. Subscribers decide whether
    /// to render.
    #[core(name = "Note")]
    Note(String),
}
```

Shape per file analogous. Stub namespaces (`mcp`, `ipc`, `spawn`, `shell`, `file`, `sources`) still define the enums — they're needed so the handler compiles — but the handler bodies return `NotImplemented` errors (Task 9).

**Step 1:** Write each `sdk/requests/*.rs` file.

**Step 2:** `sdk/requests/mod.rs` re-exports all enums.

**Step 3:** Add a compile-time test that every Haskell constructor has a matching Rust variant, and vice versa:

```rust
#[cfg(test)]
mod parity {
    // Read each .hs file at test time, parse constructor names, assert they
    // match the Rust enum variants. Catches drift early.
    // If a haskell module parser is too much, do it by hand per module with a table.
}
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] SDK request enums (FromCore) + parity test against haskell constructors"
jj new
```
<!-- END_TASK_8 -->

<!-- START_TASK_9 -->
### Task 9: Rust-side effect handlers — stubs for future-scope namespaces

**Verifies:** AC2.9.

**Files:**
- Create: `crates/pattern_runtime/src/sdk/handlers/mod.rs`
- Create: `crates/pattern_runtime/src/sdk/handlers/{shell,file,sources,mcp,ipc,spawn}.rs`

**Implementation:**

For each stub namespace, implement `EffectHandler` that returns a clear "not implemented" diagnostic:

```rust
// sdk/handlers/mcp.rs
use pattern_core::error::RuntimeError;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use crate::sdk::requests::McpReq;

#[derive(Default)]
pub struct McpHandler;

impl EffectHandler for McpHandler {
    type Request = McpReq;

    fn handle(&mut self, req: McpReq, _cx: &EffectContext) -> Result<tidepool_eval::Value, EffectError> {
        // Construct a descriptive error. freer-simple side will surface it.
        Err(EffectError::custom(format!(
            "Pattern.Mcp.{:?} is not implemented in v3 foundation \
             (phase: plugin-system plan). Agent code should not call MCP \
             effects in v3-foundation-scope programs.",
            req
        )))
    }
}
```

Stubs cover: `shell`, `file`, `sources`, `mcp`, `ipc`, `spawn`. Each has a message identifying:
- the phase / plan that will implement it,
- which effect was called,
- guidance for the agent program author (don't call this yet).

**Step 1:** Write six stub handlers per the pattern.

**Step 2:** Test each: construct handler + request + invoke; assert the error body matches expected phrasing.

**Commit:**
```bash
jj describe -m "[pattern-runtime] stub SDK handlers: shell/file/sources/mcp/ipc/spawn with actionable not-implemented errors (AC2.9)"
jj new
```
<!-- END_TASK_9 -->

<!-- START_TASK_10 -->
### Task 10: Rust-side effect handlers — time, log, and display (fully implemented)

**Verifies:** AC2.2, AC2.3. Also enables the streaming display plumbing Phase 4 / 6 consume.

**Files:**
- Create: `crates/pattern_runtime/src/sdk/handlers/time.rs`
- Create: `crates/pattern_runtime/src/sdk/handlers/log.rs`
- Create: `crates/pattern_runtime/src/sdk/handlers/display.rs`

**`time.rs` implementation:**

```rust
use jiff::Timestamp;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;
use crate::sdk::requests::TimeReq;

#[derive(Default)]
pub struct TimeHandler;

impl EffectHandler for TimeHandler {
    type Request = TimeReq;

    fn handle(&mut self, req: TimeReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            TimeReq::Now => {
                // jiff::Timestamp is an explicit UTC instant with nanosecond precision.
                // as_nanosecond() returns i128 (jiff's range exceeds i64); narrow to i64
                // for the Haskell Int wire format. try_from panics only past year 2262.
                let ns: i64 = i64::try_from(Timestamp::now().as_nanosecond())
                    .expect("timestamp fits in i64 nanos until year 2262");
                cx.respond(ns) // ToCore<i64> produces Value::Lit(Literal::LitInt(ns))
            }
            TimeReq::Sleep(ns) => {
                // Very short sleeps only — we don't want the handler to block the JIT loop.
                // For ns > 100ms, return an error and let agent use the Rust-side scheduler
                // via a different effect when we build one.
                const MAX_SLEEP_NS: i64 = 100_000_000;
                if ns > MAX_SLEEP_NS {
                    return Err(EffectError::custom(format!(
                        "Pattern.Time.Sleep {ns} exceeds in-handler limit {MAX_SLEEP_NS}ns; \
                         use scheduler effect (future)"
                    )));
                }
                std::thread::sleep(std::time::Duration::from_nanos(ns as u64));
                cx.respond(()) // ToCore<()> produces the Haskell unit Value
            }
        }
    }
}
```

Rationale: `jiff::Timestamp::now()` gives an explicit wall-clock UTC instant with nanosecond precision; `.as_nanosecond()` returns nanos-since-epoch as `i128`, narrowed to `i64` for the GHC `Int` wire format (`Literal::LitInt(i64)`). Handlers return via `cx.respond(rust_value)` which uses the `ToCore` trait from `tidepool_bridge` — handlers don't construct `Value` variants manually. `Sleep` is bounded — long sleeps would block the JIT caller thread (`std::thread::sleep` + `std::time::Duration` is correct here; we're doing a short stopwatch sleep, not manipulating a wall-clock instant).

**`log.rs` implementation:**

```rust
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tracing::{debug, error, info, warn};
use crate::sdk::requests::LogReq;

/// Log handler. Writes via tracing so Rust-side subscribers (tests, telemetry)
/// observe agent-originated log events.
#[derive(Default)]
pub struct LogHandler {
    /// Optional span name so correlated turns can be grouped. Set by Session.
    pub session_id: Option<String>,
}

impl EffectHandler for LogHandler {
    type Request = LogReq;

    fn handle(&mut self, req: LogReq, cx: &EffectContext) -> Result<tidepool_eval::Value, EffectError> {
        let sid = self.session_id.as_deref().unwrap_or("unknown");
        match req {
            LogReq::Debug(msg) => debug!(session = sid, source = "agent", "{msg}"),
            LogReq::Info(msg)  => info!( session = sid, source = "agent", "{msg}"),
            LogReq::Warn(msg)  => warn!( session = sid, source = "agent", "{msg}"),
            LogReq::Error(msg) => error!(session = sid, source = "agent", "{msg}"),
        }
        cx.respond(()) // Haskell unit via ToCore
    }
}
```

**`display.rs` implementation:**

```rust
use std::sync::{Arc, RwLock};
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use crate::sdk::requests::DisplayReq;

/// Subscriber to Display events. Implementors forward chunks/final/notes
/// to output surfaces: stdout (CLI), telemetry, test capture, etc.
///
/// Subscribers run synchronously on the effect dispatch thread. Work that
/// might block (e.g., writing to a remote telemetry sink) should push onto
/// a tokio mpsc and return immediately.
pub trait DisplaySubscriber: Send + Sync {
    fn on_event(&self, event: &DisplayEvent);
}

#[derive(Debug, Clone)]
pub enum DisplayEvent {
    /// Incremental chunk during a streaming provider response.
    Chunk(String),
    /// Terminal assembled content for the turn's message ask. Fires once.
    Final(String),
    /// Agent-visible note (typing indicator, tool-call progress, etc.).
    Note(String),
}

/// Broadcast-style handler: every registered subscriber receives every event.
#[derive(Default, Clone)]
pub struct DisplayHandler {
    subscribers: Arc<RwLock<Vec<Arc<dyn DisplaySubscriber>>>>,
}

impl DisplayHandler {
    pub fn new() -> Self { Self::default() }

    /// Register a subscriber. Returns a token that can be used to deregister
    /// if needed (Phase 3 doesn't implement deregistration; CLI lifecycle is
    /// one-shot).
    pub fn subscribe(&self, subscriber: Arc<dyn DisplaySubscriber>) {
        self.subscribers.write().unwrap().push(subscriber);
    }
}

impl EffectHandler for DisplayHandler {
    type Request = DisplayReq;

    fn handle(&mut self, req: DisplayReq, cx: &EffectContext) -> Result<tidepool_eval::Value, EffectError> {
        let event = match req {
            DisplayReq::Chunk(s) => DisplayEvent::Chunk(s),
            DisplayReq::Final(s) => DisplayEvent::Final(s),
            DisplayReq::Note(s)  => DisplayEvent::Note(s),
        };
        let subs = self.subscribers.read().unwrap();
        for s in subs.iter() { s.on_event(&event); }
        cx.respond(()) // Haskell unit via ToCore
    }
}
```

**How MessageHandler drives Display:** Phase 4's MessageHandler (the real one, once pattern_provider is wired) holds a clone of the session's `DisplayHandler`. When processing a `Message.Ask` effect, MessageHandler calls into `AnthropicProviderClient::complete` (streaming), forwards each `CompletionChunk` to `display_handler.handle(DisplayReq::Chunk(...))`, and when the stream ends, emits `DisplayReq::Final(assembled_content)`. The Haskell agent program sees only the assembled return value; human/UX subscribers see real-time chunks. Phase 6's CLI implements `DisplaySubscriber` for the terminal.

**Step 1:** Implement time, log, and display handlers.

**Step 2:** Unit tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tidepool_eval::Value;

    #[test]
    fn time_now_returns_current_nanos() {
        use tidepool_repr::Literal;

        let mut h = TimeHandler::default();
        let before = i64::try_from(jiff::Timestamp::now().as_nanosecond()).unwrap();
        let v = h.handle(TimeReq::Now, &EffectContext::for_test()).unwrap();
        let after = i64::try_from(jiff::Timestamp::now().as_nanosecond()).unwrap();
        match v {
            Value::Lit(Literal::LitInt(n)) => {
                assert!(n >= before && n <= after);
            }
            other => panic!("expected Value::Lit(LitInt), got {:?}", other),
        }
    }

    #[test]
    fn log_info_is_observed_via_tracing() {
        // Attach a tracing subscriber that captures events.
        // Dispatch LogReq::Info; assert the subscriber saw the event with
        // session= / source= fields.
    }
}
```

The tracing-subscriber capture test may require `tracing-test` or similar — add as a dev-dep if needed.

**Commit:**
```bash
jj describe -m "[pattern-runtime] time + log handlers fully implemented (AC2.2, AC2.3)"
jj new
```
<!-- END_TASK_10 -->
<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (tasks 11-13) -->
<!-- START_TASK_11 -->
### Task 11: `SdkLocation` enum + Directory-mode resolver

**Verifies:** contributes to AC2.1 (SDK must be findable at runtime); AC2.9 for the unimplemented modes.

**Files:**
- Create: `crates/pattern_runtime/src/sdk/location.rs`

**Implementation:**

```rust
//! SDK location resolution. Phase 3 implements Directory mode only; Embedded
//! and Auto are declared for API stability but return a todo! with clear
//! guidance to use Directory mode.

use pattern_core::error::RuntimeError;
use std::path::PathBuf;

/// Where Pattern finds its Haskell SDK modules at runtime.
#[derive(Debug, Clone)]
pub enum SdkLocation {
    /// Read `.hs` files from a directory on disk at runtime.
    ///
    /// The sole Phase 3 implementation. Default path is
    /// `concat!(env!("CARGO_MANIFEST_DIR"), "/haskell")`, overridable via
    /// `PATTERN_SDK_DIR`. Edits to SDK modules take effect on the next
    /// `Session::open` without a Pattern rebuild.
    Directory(PathBuf),

    /// Extract embedded `.hs` files (via `include_str!`) to a temp dir at
    /// Session open. Self-contained distribution; no external files needed.
    ///
    /// TODO: not yet implemented — phase: post-foundation SDK-distribution plan.
    Embedded,

    /// Disk-first, embedded fallback. `strict: true` requires disk and
    /// embedded contents to match exactly, catching drift.
    ///
    /// TODO: not yet implemented — phase: post-foundation SDK-distribution plan.
    Auto { directory: PathBuf, strict: bool },
}

impl Default for SdkLocation {
    fn default() -> Self {
        // Resolve in order: PATTERN_SDK_DIR env override, then CARGO_MANIFEST_DIR baked at build.
        let base = std::env::var("PATTERN_SDK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/haskell")));
        Self::Directory(base)
    }
}

impl SdkLocation {
    /// Resolve to a concrete directory suitable for passing to
    /// `tidepool_runtime::compile_haskell(include=)`.
    pub fn resolve(&self) -> Result<PathBuf, RuntimeError> {
        match self {
            Self::Directory(p) => {
                if !p.is_dir() {
                    return Err(RuntimeError::SdkNotFound {
                        path: p.clone(),
                        hint: "Set PATTERN_SDK_DIR or ensure crates/pattern_runtime/haskell exists".into(),
                    });
                }
                Ok(p.clone())
            }
            Self::Embedded => todo!(
                "SdkLocation::Embedded not yet implemented — \
                 phase: post-foundation SDK-distribution plan. \
                 Use SdkLocation::Directory or the Default (PATTERN_SDK_DIR env)."
            ),
            Self::Auto { .. } => todo!(
                "SdkLocation::Auto not yet implemented — \
                 phase: post-foundation SDK-distribution plan. \
                 Use SdkLocation::Directory."
            ),
        }
    }
}
```

**Step 1:** Write location.rs. Add `RuntimeError::SdkNotFound { path: PathBuf, hint: String }` to `pattern_core::error::runtime` (likely missing; check and add with `#[non_exhaustive]` considerations).

**Step 2:** Unit tests:
- Default resolves to existing `CARGO_MANIFEST_DIR/haskell` → Ok.
- Directory with non-existent path → Err(SdkNotFound) with hint.
- Embedded → panics (test with `#[should_panic(expected = "Embedded not yet implemented")]`).
- Auto → panics similarly.

**Commit:**
```bash
jj describe -m "[pattern-runtime] SdkLocation enum: Directory implemented, Embedded+Auto todo (AC2.9-adjacent)"
jj new
```
<!-- END_TASK_11 -->

<!-- START_TASK_12 -->
### Task 12: Handler bundle + dispatch shape

**Verifies:** contributes to AC2.1; makes AC2.2, AC2.3, AC2.9 observable end-to-end.

**Files:**
- Create: `crates/pattern_runtime/src/sdk/bundle.rs`

**Implementation:**

```rust
//! Bundle all 11 SDK handlers into a single DispatchEffect via frunk::HList.
//! Handler position in the list maps to the effect tag in Haskell code.
//!
//! ORDERING IS SEMANTIC. If the Haskell `agent` program declares
//! `Eff '[Memory, Message, Display, Shell, File, Sources, Mcp, Time, Ipc, Log, Spawn]`,
//! this bundle must list handlers in that same order. A parity test in Task 8
//! catches mismatches between the Haskell declaration and Rust bundle.

use frunk::HList;
use crate::sdk::handlers::{
    DisplayHandler, FileHandler, IpcHandler, LogHandler, McpHandler,
    MemoryHandler, MessageHandler, ShellHandler, SourcesHandler, SpawnHandler,
    TimeHandler,
};

// For clarity, wrap the HList in a newtype so callers see a single opaque bundle.
pub type SdkBundle =
    HList![MemoryHandler, MessageHandler, DisplayHandler, ShellHandler, FileHandler,
           SourcesHandler, McpHandler, TimeHandler, IpcHandler, LogHandler, SpawnHandler];

pub fn default_bundle() -> SdkBundle {
    frunk::hlist![
        MemoryHandler::default(),
        MessageHandler::default(),
        DisplayHandler::default(),
        ShellHandler::default(),
        FileHandler::default(),
        SourcesHandler::default(),
        McpHandler::default(),
        TimeHandler::default(),
        IpcHandler::default(),
        LogHandler::default(),
        SpawnHandler::default(),
    ]
}
```

**Step 1:** Define `MemoryHandler` and `MessageHandler` as stubs too for now — Phase 5 makes Memory real; the Message handler body becomes real once pattern_provider exists (Phase 4) — but we need the types for the bundle to type-check. Stub bodies emit `EffectError::custom("<Namespace> handler is stubbed in phase 3 — Phase 4/5 wires real backing")`.

**Step 2:** The `default_bundle()` function constructs an instance with `Default` defaults. Sessions that need custom handler state (e.g., `LogHandler::session_id`) build bundles directly.

**Step 3:** Integration test:

```rust
#[tokio::test]
async fn bundle_handles_time_and_log_effects() {
    // 1. Load a simple Haskell program that calls Time.now then Log.info.
    // 2. Compile, instantiate SessionMachine, build bundle with named LogHandler.
    // 3. Run; assert the final Value is (), tracing-captured logs see the info line.
}
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] SDK handler bundle (frunk HList of 11 handlers) + default constructor"
jj new
```
<!-- END_TASK_12 -->

<!-- START_TASK_13 -->
### Task 13: Hello-world integration test

**Verifies:** AC2.1, AC2.2, AC2.3.

**Files:**
- Create: `crates/pattern_runtime/tests/hello_world.rs`
- Create: `crates/pattern_runtime/tests/fixtures/hello.hs`

**Implementation:**

**`fixtures/hello.hs`:**

```haskell
{-# LANGUAGE DataKinds #-}
module Main where

import Control.Monad.Freer (Eff)
import Pattern.Prelude
import qualified Pattern.Time as Time
import qualified Pattern.Log as Log

agent :: Eff '[Time.Time, Log.Log] ()
agent = do
  t <- Time.now
  Log.info ("hello from haskell; epoch ns = " <> show t)
```

Exact `Eff '[..]` signature depends on which effects the agent calls; the bundle must include them. For this test, only Time + Log — but to keep the main bundle type stable, the test uses a reduced bundle (or the default bundle and ignores unused handler slots; freer-simple is happy with spare handlers).

**`tests/hello_world.rs`:**

```rust
use pattern_runtime::{SdkLocation, Session, SessionMachine};

#[tokio::test]
async fn hello_world_runs_end_to_end() {
    // Preflight: skip test if tidepool-extract unavailable, with a diagnostic
    // print pointing at the installation docs.
    if pattern_runtime::preflight::check().is_err() {
        eprintln!("skipping hello_world: tidepool-extract unavailable");
        return;
    }

    let source = include_str!("fixtures/hello.hs");
    let sdk = SdkLocation::default().resolve().unwrap();

    // Compile.
    let program = pattern_runtime::tidepool::compile_program(
        source,
        "agent",
        &[&sdk],
    )
    .unwrap();

    // Warm the JIT.
    let mut machine = SessionMachine::new(program, 32 * 1024 * 1024 /* 32 MiB */).unwrap();

    // Bundle with tracing-test subscriber so Log.info is observable.
    let mut bundle = /* build SdkBundle */ ();
    let user_ctx = /* Session-scoped user context */ ();

    let result = machine.run(&mut bundle, &user_ctx).unwrap();
    // Haskell unit `()` rendered as Value::Con(unit_dcid, []). Use the
    // `FromCore` impl for Rust `()` rather than hand-matching the DataConId:
    //   <() as tidepool_bridge::FromCore>::from_value(&result, machine.table()).unwrap();
    // which will error if the returned value isn't unit.
    <() as tidepool_bridge::FromCore>::from_value(&result, machine.table()).unwrap();

    // Assert tracing captured a log line containing "hello from haskell".
    // (Using tracing-test or similar.)
}
```

**Step 1:** Write the .hs fixture.

**Step 2:** Write the Rust test.

**Step 3:** `cargo nextest run -p pattern_runtime --test hello_world`. Passes when tidepool-extract is available; skips gracefully when not.

**Commit:**
```bash
jj describe -m "[pattern-runtime] hello-world integration test (AC2.1, AC2.2, AC2.3)"
jj new
```
<!-- END_TASK_13 -->
<!-- END_SUBCOMPONENT_C -->

<!-- START_SUBCOMPONENT_D (tasks 14-16) -->
<!-- START_TASK_14 -->
### Task 14: `Session` and `AgentRuntime` impls

**Verifies:** AC2.1, AC2.10.

**Files:**
- Create: `crates/pattern_runtime/src/session.rs`
- Create: `crates/pattern_runtime/src/runtime.rs`

**Implementation:**

**`session.rs`:**

```rust
//! Concrete Session impl backed by Tidepool.
//!
//! Lifecycle:
//! 1. `TidepoolSession::open(persona, sdk_location)` — preflight, compile, warm JIT.
//! 2. Repeat: `session.step(turn_input)` — run the JIT with turn input threaded
//!    through effect handlers, collect turn output.
//! 3. `session.checkpoint()` / `session.restore()` — event-log based (Task 15).

use pattern_core::{
    error::RuntimeError,
    traits::Session,
    types::{SessionSnapshot, TurnInput, TurnOutput},
};
use crate::{
    sdk::{SdkLocation, SdkBundle, default_bundle},
    tidepool::{CompiledProgram, SessionMachine, compile_program},
};

/// Session-scoped context threaded into `machine.run()` as the `user` param
/// that tidepool-effect hands to each EffectHandler. Holds session-long state
/// handlers may need to read.
pub struct SessionContext {
    /// Timeout budget for `step()` calls. Persona-configurable.
    budget: crate::timeout::Budget,
    /// Shared handle to pattern_core::memory storage. Phase 5's MemoryHandler
    /// uses this to service memory effects.
    memory_store: std::sync::Arc<dyn pattern_core::traits::MemoryStore>,
    /// Shared handle to the provider client. Phase 4's MessageHandler uses
    /// this for LLM calls; forwarded by reference when the handler fires.
    provider: std::sync::Arc<pattern_provider::AnthropicProviderClient>,
    /// Persona snapshot for identity / config lookup.
    persona: PersonaSnapshot,
    /// Shared cancellation flag (Phase 3 Task 16 two-path cancellation).
    /// Set by the watchdog when soft-cancel fires; checked by each handler.
    cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SessionContext {
    pub fn from_persona(
        persona: &PersonaSnapshot,
        memory_store: &std::sync::Arc<dyn pattern_core::traits::MemoryStore>,
        provider: &std::sync::Arc<pattern_provider::AnthropicProviderClient>,
    ) -> Self {
        Self {
            budget: persona.budget.unwrap_or_default(),
            memory_store: memory_store.clone(),
            provider: provider.clone(),
            persona: persona.clone(),
            cancellation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    pub fn budget(&self) -> crate::timeout::Budget { self.budget }
    pub fn cancellation(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.cancellation.clone()
    }
    pub fn memory_store(&self) -> std::sync::Arc<dyn pattern_core::traits::MemoryStore> {
        self.memory_store.clone()
    }
    pub fn provider(&self) -> std::sync::Arc<pattern_provider::AnthropicProviderClient> {
        self.provider.clone()
    }
}

pub struct TidepoolSession {
    machine: SessionMachine,
    bundle: SdkBundle,
    session_id: String,
    checkpoint_log: Vec<CheckpointEvent>,
    /// Session-long context threaded into effect handlers.
    ctx: SessionContext,
    /// Set to true when hard-abandonment fires (Phase 3 Task 16 two-path
    /// cancellation). Subsequent `step()` calls short-circuit to
    /// `RuntimeError::SessionPoisoned` without touching the machine.
    poisoned: bool,
    /// Shared handle to the session's DisplayHandler. Exposed via `display()`
    /// so callers (CLI, tests, future UX) can register subscribers after
    /// session open. The same handler is also held by MessageHandler inside
    /// the bundle; both clones share the same subscriber list (Arc<RwLock>).
    display_handle: crate::sdk::handlers::DisplayHandler,
}

impl TidepoolSession {
    /// Return a clone of the session's DisplayHandler. Cheap — it's an
    /// Arc-shared subscriber list. Subscribers registered via this handle
    /// receive events from the MessageHandler's clone too.
    pub fn display(&self) -> crate::sdk::handlers::DisplayHandler {
        self.display_handle.clone()
    }
}

impl Session for TidepoolSession {
    fn step(&mut self, input: TurnInput) -> Result<TurnOutput, RuntimeError> {
        // 1. Seed the bundle's stateful handlers with turn input (TurnContextHandler gets messages,
        //    MemoryHandler gets a handle to the store, LogHandler gets session_id).
        self.bundle.seed_for_turn(input)?;
        // 2. Wrap `self.machine.run` in the timeout harness (Task 16).
        // 3. Collect effect-log entries into self.checkpoint_log.
        // 4. Extract TurnOutput from handler state (e.g., pending messages collected by
        //    MessageHandler during the run).
        let _value = crate::timeout::run_bounded(
            &mut self.machine,
            &mut self.bundle,
            &self.ctx,
            self.ctx.budget(),
        )?;
        self.bundle.take_turn_output()
    }

    fn checkpoint(&self) -> Result<SessionSnapshot, RuntimeError> { /* Task 15 */ todo!("phase: 3; AC: AC2.4") }
    fn restore(&mut self, snapshot: SessionSnapshot) -> Result<(), RuntimeError> { /* Task 15 */ todo!("phase: 3; AC: AC2.4") }
}

impl TidepoolSession {
    pub fn open(
        persona: PersonaSnapshot,
        sdk: &SdkLocation,
        memory_store: std::sync::Arc<dyn pattern_core::traits::MemoryStore>,
        provider: std::sync::Arc<pattern_provider::AnthropicProviderClient>,
    ) -> Result<Self, RuntimeError> {
        crate::preflight::check()?;
        let sdk_dir = sdk.resolve()?;
        let program = compile_program(&persona.program, "agent", &[&sdk_dir])?;
        let machine = SessionMachine::new(program, persona.nursery_size.unwrap_or(32 * 1024 * 1024))?;
        let session_id = persona.new_session_id();
        // Clone Arcs before moving into SessionContext so we retain handles
        // for the inline bundle construction below.
        let ctx = SessionContext::from_persona(&persona, &memory_store, &provider);
        // Construct the handler bundle inline, seeding session-scoped state
        // (LogHandler.session_id, MemoryHandler with store handle, etc.)
        // on the relevant handlers. `default_bundle()` from Task 12 with the
        // log handler overridden.
        use crate::sdk::handlers::*;
        // MessageHandler gets a clone of the DisplayHandler so it can forward
        // stream chunks to display subscribers during provider streaming.
        // A third clone lives on TidepoolSession itself so callers can
        // register subscribers via `session.display()` after open.
        let display = DisplayHandler::new();
        let bundle = frunk::hlist![
            MemoryHandler::new(ctx.memory_store(), ctx.cancellation()),
            MessageHandler::new(ctx.provider(), display.clone(), ctx.cancellation()),
            display.clone(),
            ShellHandler::default(),
            FileHandler::default(),
            SourcesHandler::default(),
            McpHandler::default(),
            TimeHandler::default(),
            IpcHandler::default(),
            LogHandler { session_id: Some(session_id.clone()) },
            SpawnHandler::default(),
        ];
        Ok(Self {
            machine,
            bundle,
            session_id,
            checkpoint_log: vec![],
            ctx,
            poisoned: false,
            display_handle: display,
        })
    }
}
```

**`runtime.rs`:**

```rust
//! Concrete AgentRuntime implementation.
//! Owns the SdkLocation and spawns TidepoolSession instances.

use pattern_core::traits::AgentRuntime;
use pattern_core::types::PersonaSnapshot;
use pattern_core::error::RuntimeError;
use crate::session::TidepoolSession;
use crate::sdk::SdkLocation;

pub struct TidepoolRuntime {
    sdk: SdkLocation,
    memory_store: std::sync::Arc<dyn pattern_core::traits::MemoryStore>,
    provider: std::sync::Arc<pattern_provider::AnthropicProviderClient>,
}

impl TidepoolRuntime {
    pub fn new(
        sdk: SdkLocation,
        memory_store: std::sync::Arc<dyn pattern_core::traits::MemoryStore>,
        provider: std::sync::Arc<pattern_provider::AnthropicProviderClient>,
    ) -> Self {
        Self { sdk, memory_store, provider }
    }
}

#[async_trait::async_trait]
impl AgentRuntime for TidepoolRuntime {
    type Session = TidepoolSession;

    async fn open_session(&self, persona: PersonaSnapshot) -> Result<Self::Session, RuntimeError> {
        // Offload compile to blocking pool — GHC subprocess shouldn't block async runtime.
        let persona_cloned = persona.clone();
        let sdk = self.sdk.clone();
        let memory_store = self.memory_store.clone();
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || TidepoolSession::open(persona_cloned, &sdk, memory_store, provider))
            .await
            .map_err(|e| RuntimeError::SessionOpenFailed { source: e.to_string() })?
    }

    async fn shutdown(&self) -> Result<(), RuntimeError> {
        // Nothing session-level to release; individual sessions drop their machines.
        Ok(())
    }
}
```

**Step 1:** Implement both files.

**Step 2:** Integration test in `tests/session_lifecycle.rs`:
- open → step (once) → drop. Assert preflight + compile + single run path works.
- open → step (twice) → drop. Assert the second step does not trigger a recompile (observe via bench / instrumentation).

**Step 3:** Concurrency test (AC2.10):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_sessions_are_isolated() {
    let runtime = TidepoolRuntime::with_default_sdk();
    let futs: Vec<_> = (0..4).map(|i| {
        let rt = &runtime;
        async move {
            let mut s = rt.open_session(PersonaSnapshot::test_agent(i)).await.unwrap();
            for _ in 0..3 {
                let input = TurnInput::test(i);
                let out = s.step(input).unwrap();
                assert_eq!(out.session_tag(), i);
            }
        }
    }).collect();
    futures::future::join_all(futs).await;
}
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] Session + AgentRuntime impls backed by Tidepool (AC2.1, AC2.10)"
jj new
```
<!-- END_TASK_14 -->

<!-- START_TASK_15 -->
### Task 15: Event-log checkpoint / restore

**Verifies:** AC2.4.

**Files:**
- Create: `crates/pattern_runtime/src/checkpoint.rs`
- Modify: `crates/pattern_runtime/src/session.rs` — wire checkpoint recording into run loop

**Implementation:**

```rust
//! Event-log checkpoint: record (EffectRequest, Value) exchanges during a turn.
//! Restore: fresh compile + replay the log, then continue past the cursor.
//! Relies on tidepool JIT determinism given identical CBOR + effect responses.

use pattern_core::types::SessionSnapshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointEvent {
    pub tag: u32,
    pub request_cbor: Vec<u8>,
    pub response_cbor: Vec<u8>,
    pub turn: u64,
    pub sequence: u64,
}

pub struct CheckpointLog {
    events: Vec<CheckpointEvent>,
    current_turn: u64,
    current_seq: u64,
}

impl CheckpointLog {
    pub fn record(&mut self, tag: u32, req: &tidepool_eval::Value, resp: &tidepool_eval::Value) {
        // Serialise req/resp via tidepool_repr CBOR. Append to self.events.
        todo!("phase: 3; AC: AC2.4")
    }
    pub fn snapshot(&self) -> SessionSnapshot { /* serialize events + turn state */ todo!() }
    pub fn replay(events: Vec<CheckpointEvent>) -> ReplayingBundle { todo!() }
}
```

**Recording hook:** Each handler, after producing its response, calls `session.checkpoint_log.record(tag, &req_as_value, &response)` before returning. Implementation detail: handlers take a `&mut CheckpointLog` through EffectContext or via a shared Arc<Mutex<_>>. Prefer the latter — EffectContext in tidepool is designed around this.

**Replay:** On `Session::restore(snapshot)`, decode events. Construct a `ReplayingBundle` that wraps the real handlers: for each effect invocation, look up the next-in-sequence event, return its recorded response (verifying the request matches). When the log is exhausted, fall through to real handlers.

**Round-trip test (AC2.4):**

```rust
#[tokio::test]
async fn checkpoint_restore_roundtrip_is_deterministic() {
    // 1. Open session, run 3 turns, checkpoint after turn 2.
    // 2. Open fresh session, restore from snapshot.
    // 3. Run turn 3 again. Assert output matches original turn 3.
}
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] event-log checkpoint + restore (AC2.4)"
jj new
```
<!-- END_TASK_15 -->

<!-- START_TASK_16 -->
### Task 16: Two-path cancellation harness (soft cancel + hard abandon)

**Verifies:** AC2.5, AC2.6 (plus session-recoverability beyond literal AC text).

**Files:**
- Create: `crates/pattern_runtime/src/timeout.rs`

**Design rationale:**

Tidepool has no public interrupt API (verified during Phase 3 research). The naïve approach — "timeout fires, abandon the blocking thread, poison the session" — is correct as a last-resort escape hatch but dumps all timeouts into unrecoverable territory. That's the wrong default: most real timeouts happen during LLM calls (the slowest effect — agent is yielding, just waiting on network). Those are cooperatively recoverable.

Phase 3 ships **two paths, with the soft path as the happy case**:

1. **Soft cancellation (common case — agent is yielding via effects, just taking too long):**
   - Watchdog fires after wall-clock or CPU budget exceeded
   - Sets `SessionContext.cancellation` atomic flag to `true`
   - Every EffectHandler's `handle()` method checks the flag first thing; if set, returns `EffectError::Cancelled`
   - JIT sees the effect error, propagates through `freer-simple`, `JitEffectMachine::run()` returns `JitError::Effect(EffectError::Cancelled)`
   - `run_bounded` maps to `RuntimeError::Timeout { wall_ms, cpu_ms, path: CancelPath::Soft }`
   - **Session remains usable** — the machine is still held by the still-running blocking task but that task finishes cleanly (the JIT stopped at the cancelled effect, machine.run returned normally, spawn_blocking future resolves). Caller can checkpoint/restore or start a fresh turn.

2. **Hard abandonment (rare — agent is in pure compute with no effect yields for N CPU-seconds):**
   - If watchdog has observed **zero effect invocations** for `hard_abandon_threshold` CPU-seconds beyond the budget, soft cancel is assumed stuck
   - Abandon the blocking thread (it keeps running in the background; tokio forgets about it)
   - Set `TidepoolSession.poisoned = true`
   - Return `RuntimeError::Timeout { wall_ms, cpu_ms, path: CancelPath::HardAbandon }`
   - All subsequent `session.step()` calls short-circuit to `RuntimeError::SessionPoisoned` without touching the machine (the detached thread still owns it)
   - Caller opens a fresh session (cheap — compile is cached, new JIT machine ~50ms to instantiate)

**Timeout budget pauses during I/O-bound effect handlers:**

"Agent is processing" for Pattern means "inside an effect handler waiting for something outside the JIT." The timeout budget measures time the JIT is actually running compute, not wall-clock including I/O waits. When a handler is awaiting an HTTP response from Anthropic's endpoint, the JIT thread is blocked waiting for the handler to return — CPU time doesn't tick up (thread is idle), and we want wall-clock to also pause so slow-but-normal LLM responses don't wrongly trigger soft-cancel.

Implementation: handlers announce entry/exit via `SessionContext.enter_handler()` / `exit_handler()` which increment/decrement an `in_effect_handler` counter on the shared context. The watchdog checks this counter: when `> 0`, do not accumulate budget consumption. HTTP-level timeouts are owned by the `reqwest::Client` configuration (default 120s for streaming bodies, 60s for non-streaming; set in `pattern_provider` Phase 4).

This naturally gives the right behaviour:
- Slow LLM call (say, 45 seconds of streaming) — handler is active; budget paused; no timeout
- Slow network (HTTP client 120s timeout fires) — handler returns error; budget resumes; JIT sees error and either retries or propagates
- Agent stuck in a tight Haskell compute loop — no handler is active; budget accumulates; soft-cancel flag set on next effect invocation; if no effects fire, hard-abandon after threshold

**Implementation:**

```rust
//! Two-path cancellation harness for Tidepool execution.
//!
//! Tidepool has no public interrupt API. Pattern's approach:
//! 1. Soft cancel via shared atomic flag checked by every effect handler.
//! 2. Hard abandon (last resort) when no effect yields observed for long enough.
//!
//! Budget consumption pauses while the JIT is inside an effect handler
//! (handler owns its own timeout, typically for I/O). Budget counts
//! time-in-JIT-compute, not wall-clock-including-I/O.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use pattern_core::error::RuntimeError;

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Wall-clock budget for time-in-JIT-compute. Default 30s per turn.
    pub wall: Duration,
    /// CPU budget for time-in-JIT-compute. Default 10s per turn.
    pub cpu: Duration,
    /// When no effect invocations observed for this long beyond the cpu budget,
    /// escalate to hard-abandon. Default: 2× cpu budget.
    pub hard_abandon_threshold: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        let cpu = Duration::from_secs(10);
        Self {
            wall: Duration::from_secs(30),
            cpu,
            hard_abandon_threshold: cpu * 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum CancelPath {
    /// Soft cancel fired at an effect boundary; session recoverable.
    Soft,
    /// Hard abandon fired — blocking thread detached; session poisoned.
    HardAbandon,
}

/// Counter shared with every effect handler. Handlers increment on entry,
/// decrement on exit. The watchdog reads this to determine whether budget
/// should accumulate.
pub struct HandlerGate(AtomicU32);

impl HandlerGate {
    pub fn new() -> Self { Self(AtomicU32::new(0)) }
    pub fn enter(&self) { self.0.fetch_add(1, Ordering::SeqCst); }
    pub fn exit(&self) { self.0.fetch_sub(1, Ordering::SeqCst); }
    pub fn in_handler(&self) -> bool { self.0.load(Ordering::SeqCst) > 0 }
}

pub async fn run_bounded<U, H>(
    machine: &mut crate::tidepool::SessionMachine,
    handlers: &mut H,
    user: &U,
    budget: Budget,
    cancellation: Arc<AtomicBool>,
    gate: Arc<HandlerGate>,
) -> Result<(tidepool_eval::Value, Option<CancelPath>), RuntimeError>
where
    H: tidepool_effect::DispatchEffect<U>,
{
    // Spawn the JIT on the blocking pool; it will run until machine.run returns
    // (normal completion, soft cancel, or real JIT error).
    let jit_handle = {
        let cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            // SessionMachine::run dispatches effects; handlers check
            // `cancellation` and return EffectError::Cancelled if set.
            machine.run(handlers, user)
        })
    };

    // Watchdog: samples CPU every 100ms, tracks no-yield window, sets
    // cancellation flag when budgets exhausted, aborts when hard-abandon
    // threshold crosses.
    let watchdog = {
        let cancellation = cancellation.clone();
        let gate = gate.clone();
        let budget = budget;
        tokio::spawn(async move {
            let start = Instant::now();
            let mut jit_cpu_accumulated = Duration::ZERO;
            let mut jit_wall_accumulated = Duration::ZERO;
            let mut last_sample = Instant::now();
            let mut last_effect_seen = Instant::now();

            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let now = Instant::now();
                let interval = now.duration_since(last_sample);
                last_sample = now;

                // Only accumulate if NOT inside an effect handler.
                if !gate.in_handler() {
                    jit_wall_accumulated += interval;
                    #[cfg(target_os = "linux")]
                    { jit_cpu_accumulated += sample_thread_cpu().unwrap_or(interval); }
                    #[cfg(not(target_os = "linux"))]
                    { jit_cpu_accumulated += interval; /* fallback: count as CPU if not linux */ }
                }

                // Check primary budget.
                if jit_wall_accumulated >= budget.wall || jit_cpu_accumulated >= budget.cpu {
                    if !cancellation.load(Ordering::SeqCst) {
                        cancellation.store(true, Ordering::SeqCst);
                        tracing::info!(
                            wall_ms = jit_wall_accumulated.as_millis() as u64,
                            cpu_ms = jit_cpu_accumulated.as_millis() as u64,
                            "soft cancel fired; JIT will exit at next effect boundary",
                        );
                    }
                    // Track how long since soft-cancel was set without JIT exiting.
                    if now.duration_since(last_effect_seen) > budget.hard_abandon_threshold {
                        return CancelOutcome::HardAbandon {
                            wall_ms: jit_wall_accumulated.as_millis() as u64,
                            cpu_ms: jit_cpu_accumulated.as_millis() as u64,
                        };
                    }
                } else {
                    // Budget still healthy; note when we last saw an effect
                    // boundary (gate went high → low) to aid hard-abandon logic.
                    if gate.in_handler() { last_effect_seen = now; }
                }
            }
        })
    };

    // Race JIT completion vs. watchdog hard-abandon signal.
    tokio::select! {
        result = jit_handle => {
            // JIT returned. Either normally, via soft cancel (EffectError::Cancelled),
            // or via a real JIT error.
            let value = result
                .map_err(|e| RuntimeError::JoinError { source: e.to_string() })?
                .map_err(crate::tidepool::error_map::map_jit_error)?;
            let path = if cancellation.load(Ordering::SeqCst) { Some(CancelPath::Soft) } else { None };
            // Reset cancellation + stop watchdog for next turn.
            cancellation.store(false, Ordering::SeqCst);
            watchdog.abort();
            Ok((value, path))
        }
        outcome = watchdog => {
            // Watchdog fired hard-abandon before JIT could cooperate.
            match outcome {
                Ok(CancelOutcome::HardAbandon { wall_ms, cpu_ms }) => {
                    // Detach the blocking task; we can't stop it but we stop
                    // waiting on it.
                    drop(jit_handle);
                    Err(RuntimeError::Timeout {
                        wall_ms, cpu_ms,
                        path: CancelPath::HardAbandon,
                    })
                }
                Err(_) => Err(RuntimeError::WatchdogFailure),
            }
        }
    }
}

enum CancelOutcome {
    HardAbandon { wall_ms: u64, cpu_ms: u64 },
}

#[cfg(target_os = "linux")]
fn sample_thread_cpu() -> Option<Duration> {
    // Read /proc/self/task/<tid>/stat; sum utime+stime in jiffies; convert to Duration.
    // Returned delta since last call is what the caller accumulates.
    // Implementation detail left to executor — tracked per-thread via a TLS cache.
    todo!("phase: 3; AC: AC2.6 — CPU sampling on linux")
}
```

**Handler-side integration:**

Every EffectHandler's `handle()` (in `crates/pattern_runtime/src/sdk/handlers/*.rs`) gains two bookends:

```rust
fn handle(&mut self, req: Self::Request, cx: &EffectContext) -> Result<Value, EffectError> {
    // First: check cancellation flag.
    if cx.session_context().cancellation().load(Ordering::SeqCst) {
        return Err(EffectError::Cancelled);
    }
    // Second: announce we're entering handler work (budget pauses).
    cx.session_context().handler_gate().enter();
    let result = self.handle_inner(req, cx);
    cx.session_context().handler_gate().exit();
    result
}
```

Where `handle_inner` is the handler-specific logic that actually does the work (HTTP calls for MessageHandler, `jiff::Timestamp::now()` for TimeHandler, etc.). This pattern is duplicated across all 11 handlers; a macro or trait-helper can reduce boilerplate. Light-weight handlers (TimeHandler, LogHandler, DisplayHandler) may skip the gate since their work is instantaneous; gating is primarily for I/O-bound ones.

**Add to `pattern_core::error::RuntimeError`:**

```rust
pub enum RuntimeError {
    // ... existing variants ...
    Timeout {
        wall_ms: u64,
        cpu_ms: u64,
        path: crate::CancelPath, // re-exported from pattern_runtime; or simply duplicate the enum
    },
    SessionPoisoned { reason: String }, // set by hard-abandon path
    JoinError { source: String },
    WatchdogFailure,
}
```

**Session-side poisoning:**

In `TidepoolSession::step`, before calling `run_bounded`:

```rust
fn step(&mut self, input: TurnInput) -> Result<TurnOutput, RuntimeError> {
    if self.poisoned {
        return Err(RuntimeError::SessionPoisoned {
            reason: "previous turn hard-abandoned due to runaway compute without effect yields".into(),
        });
    }
    self.bundle.seed_for_turn(input)?;
    match crate::timeout::run_bounded(
        &mut self.machine,
        &mut self.bundle,
        &self.ctx,
        self.ctx.budget(),
        self.ctx.cancellation(),
        self.ctx.handler_gate(),
    ).await {
        Ok((_value, None)) => self.bundle.take_turn_output(),
        Ok((_value, Some(CancelPath::Soft))) => {
            // Session remains usable; return a soft-timeout error the caller can retry.
            Err(RuntimeError::Timeout {
                wall_ms: self.ctx.budget().wall.as_millis() as u64,
                cpu_ms: self.ctx.budget().cpu.as_millis() as u64,
                path: CancelPath::Soft,
            })
        }
        Err(RuntimeError::Timeout { path: CancelPath::HardAbandon, .. }) => {
            self.poisoned = true;
            Err(RuntimeError::Timeout {
                wall_ms: self.ctx.budget().wall.as_millis() as u64,
                cpu_ms: self.ctx.budget().cpu.as_millis() as u64,
                path: CancelPath::HardAbandon,
            })
        }
        Err(e) => Err(e),
    }
}
```

**Tests:**

```rust
// tests/timeout.rs

#[tokio::test]
async fn soft_cancel_recovers_session(/* ... */) {
    // Agent program: long loop that DOES call ctx.time.now between iterations.
    // Budget: 200ms wall. Expect RuntimeError::Timeout { path: Soft }.
    // Then: same session.step() with a fresh turn input succeeds.
}

#[tokio::test]
async fn hard_abandon_poisons_session(/* ... */) {
    // Agent program: tight Haskell compute loop (e.g., sum [1..huge]) with
    // NO effect invocations. Budget: 200ms wall, 500ms hard_abandon_threshold.
    // Expect RuntimeError::Timeout { path: HardAbandon } after ~700ms.
    // Subsequent session.step() returns SessionPoisoned.
}

#[tokio::test]
async fn slow_llm_handler_does_not_trigger_timeout(/* ... */) {
    // Agent program: calls ctx.message.send (LLM). MessageHandler in test
    // uses a mock that sleeps for 5s before returning.
    // Budget: wall 2s, cpu 1s. Handler pauses budget — no timeout fires.
    // Assert session.step returns normally.
}

#[tokio::test]
async fn http_client_timeout_surfaces_as_handler_error_not_session_timeout(/* ... */) {
    // Agent program: ctx.message.send. MessageHandler uses a reqwest client
    // with 200ms timeout pointed at a wiremock that delays 2s.
    // Expect: MessageHandler returns ProviderError::HttpTimeout; agent's
    // step returns that error (or whatever the Haskell program does with it).
    // Crucially: NOT a pattern-side RuntimeError::Timeout.
}

#[tokio::test]
#[cfg(target_os = "linux")]
async fn cpu_budget_counted_while_jit_runs(/* ... */) {
    // Haskell program: non-yielding compute. Budget: cpu 100ms.
    // Assert Timeout fires, cpu_ms >= 100.
}
```

AC2.5 pass criterion: soft-path kill fires before 1.5× wall budget when agent IS yielding via effects. AC2.6 pass criterion: hard-path or soft-path returns `RuntimeError::Timeout` with cpu_ms populated on Linux.

**Commit:**
```bash
jj describe -m "[pattern-runtime] two-path cancellation: soft cancel via effect-flag + hard abandon for runaway compute (AC2.5, AC2.6)

Session stays recoverable after soft cancel. Hard abandon poisons session.
Budget pauses while handler is active — HTTP-bound effects don't spuriously
trigger timeout. Watchdog-hard-abandon writes to TidepoolSession.poisoned."
jj new
```
<!-- END_TASK_16 -->
<!-- END_SUBCOMPONENT_D -->

<!-- START_SUBCOMPONENT_E (tasks 17-20) -->
<!-- START_TASK_17 -->
### Task 17: Effect-overflow handling (AC2.7)

**Verifies:** AC2.7.

**Files:**
- Modify: `crates/pattern_runtime/src/tidepool/error_map.rs` — ensure `EffectError` with ResponseTooLarge maps to `RuntimeError::EffectOverflow`
- Create: `crates/pattern_runtime/tests/effect_overflow.rs`

**Implementation:**

Tidepool enforces a 10K-node limit on effect responses (`tidepool-codegen/src/jit_machine.rs:171-173`). When exceeded, `JitError::Effect(e)` surfaces with a ResponseTooLarge detail. Map it.

Test:

```rust
// Haskell program calls an effect that asks for a huge response.
// Test handler returns a 50K-node Value. Expect RuntimeError::EffectOverflow.
```

**Commit:**
```bash
jj describe -m "[pattern-runtime] effect-overflow surfacing (AC2.7)"
jj new
```
<!-- END_TASK_17 -->

<!-- START_TASK_18 -->
### Task 18: GHC crash surfacing (AC2.8)

**Verifies:** AC2.8.

**Files:**
- Create: `crates/pattern_runtime/tests/ghc_crash.rs`
- Modify: `crates/pattern_runtime/src/session.rs` — set session-unusable flag on RuntimeCrashed

**Implementation:**

Construct a test that:
1. Opens a session.
2. Runs a Haskell program known to cause a JIT signal (e.g., segfault via bad stack manipulation — or mock the error by injecting a mapped error variant).
3. Asserts `step` returns `RuntimeError::RuntimeCrashed`.
4. Asserts subsequent `step` calls also error with "session unusable" (separate variant — add `RuntimeError::SessionPoisoned` or reuse RuntimeCrashed).

If we can't reliably trigger a signal from a test program, stub it via the error-map layer: inject a fake `JitError::Signal` into the mapping path to exercise the Pattern-side flow.

**Commit:**
```bash
jj describe -m "[pattern-runtime] GHC/JIT crash surfacing + session poisoning (AC2.8)"
jj new
```
<!-- END_TASK_18 -->

<!-- START_TASK_19 -->
### Task 19: Stub effect hang-free verification (AC2.9)

**Verifies:** AC2.9.

**Files:**
- Create: `crates/pattern_runtime/tests/stub_effects.rs`

**Implementation:**

For each stubbed namespace (shell/file/sources/mcp/ipc/spawn):
- Write a minimal Haskell program that calls the effect.
- Run via session.
- Assert the call returns a `RuntimeError` (or `EffectError` bubbled up via JitError) with the namespaced "not implemented" message, within <100ms wall.

Catches silent-hang regressions per AC2.9.

**Commit:**
```bash
jj describe -m "[pattern-runtime] stub-effect not-implemented surfacing tests (AC2.9)"
jj new
```
<!-- END_TASK_19 -->

<!-- START_TASK_20 -->
### Task 20: Minimal `time` + `log` end-to-end tests (AC2.2, AC2.3)

**Verifies:** AC2.2, AC2.3 (explicit tests beyond the hello-world test in Task 13).

**Files:**
- Extend: `crates/pattern_runtime/tests/hello_world.rs` or create `crates/pattern_runtime/tests/time_log_effects.rs`

**Implementation:**

Targeted tests:

**Time.Now:**

```haskell
agent :: Eff '[Time.Time] Integer
agent = Time.now
```

Rust side: run, capture the integer, assert it's within ±1s of `jiff::Timestamp::now().as_nanosecond()` before/after invocation.

**Log.Info:**

```haskell
agent :: Eff '[Log.Log] ()
agent = Log.info "structured-log-assertion-marker"
```

Rust side: attach tracing-test subscriber, run, assert the subscriber captured an event containing the marker string and `source=agent`.

**Commit:**
```bash
jj describe -m "[pattern-runtime] targeted time + log effect tests (AC2.2, AC2.3)"
jj new
```
<!-- END_TASK_20 -->
<!-- END_SUBCOMPONENT_E -->

<!-- START_SUBCOMPONENT_F (tasks 21-23) -->
<!-- START_TASK_21 -->
### Task 21: Zero-warning compile + lint

**Verifies:** cleanliness gate; required for phase close.

**Step 1:**

```bash
cargo check -p pattern_runtime 2>&1 | tee /tmp/phase3-check.log
! grep -q 'warning:' /tmp/phase3-check.log && echo "cargo check clean" || { echo FAIL; exit 1; }

cargo clippy -p pattern_runtime --all-features --all-targets -- -D warnings 2>&1 | tee /tmp/phase3-clippy.log
```

**Step 2:** Fix all warnings. Do NOT `#[allow]` without a comment explaining why.

**Step 3:** `cargo doc -p pattern_runtime --no-deps` — zero warnings.

**Step 4:** `cargo test --doc -p pattern_runtime` — all doctests pass.

**Commit:**
```bash
jj describe -m "[pattern-runtime] phase 3 close: zero warnings on check, clippy, doc"
jj new
```
<!-- END_TASK_21 -->

<!-- START_TASK_22 -->
### Task 22: Audit script pass

**Verifies:** AC1.7–AC1.10 (carried forward from Phase 2's audit script).

**Step 1:**

```bash
bash scripts/audit-rewrite-state.sh
```

All new code in `pattern_runtime` lands in its final home; no fate markers needed unless there's in-flight code. The audit checks:
- any `MOVING TO:` / `REPLACED BY:` markers in pattern_runtime (should be none — this is destination code)
- every `todo!()` has phase + AC reference
- no commented-out code

**Step 2:** Fix any violations. Run until clean.

**Commit:**
```bash
jj describe -m "[pattern-runtime] audit pass: AC1.7-AC1.10 clean post-phase-3"
jj new
```
<!-- END_TASK_22 -->

<!-- START_TASK_23 -->
### Task 23: Final Phase 3 verification

**Verifies:** all of AC2.*.

**Step 1:** Full test suite.

```bash
cargo nextest run -p pattern_runtime 2>&1 | tee /tmp/phase3-tests.log
```

All integration tests pass (those gated on tidepool-extract availability skip gracefully if the binary isn't installed; CI must have it).

**Step 2:** AC enumeration — each AC2.* case has at least one passing test:

| AC | Test |
|---|---|
| AC2.1 | `tests/hello_world.rs::hello_world_runs_end_to_end` |
| AC2.2 | `tests/time_log_effects.rs::time_now_returns_current_epoch` |
| AC2.3 | `tests/time_log_effects.rs::log_info_observable_via_tracing` |
| AC2.4 | `tests/checkpoint.rs::checkpoint_restore_roundtrip_is_deterministic` |
| AC2.5 | `tests/timeout.rs::wall_clock_timeout_fires` |
| AC2.6 | `tests/timeout.rs::cpu_timeout_fires` (Linux only) |
| AC2.7 | `tests/effect_overflow.rs::oversized_response_fails` |
| AC2.8 | `tests/ghc_crash.rs::ghc_crash_poisons_session` |
| AC2.9 | `tests/stub_effects.rs::*` (one per stubbed namespace) |
| AC2.10 | `tests/session_lifecycle.rs::concurrent_sessions_are_isolated` |

**Step 3:** `just pre-commit-all` passes.

**Commit:**
```bash
jj describe -m "[pattern-runtime] phase 3 complete: tidepool FFI + minimal runtime + 11 handlers + timeout + checkpoint

AC2.1 hello-world run: PASS (tests/hello_world.rs)
AC2.2 time.now roundtrip: PASS (tests/time_log_effects.rs)
AC2.3 log.info via tracing: PASS (tests/time_log_effects.rs)
AC2.4 checkpoint/restore roundtrip: PASS (tests/checkpoint.rs)
AC2.5 wall-clock timeout: PASS (tests/timeout.rs)
AC2.6 CPU timeout (linux): PASS (tests/timeout.rs)
AC2.7 effect overflow: PASS (tests/effect_overflow.rs)
AC2.8 GHC crash: PASS (tests/ghc_crash.rs)
AC2.9 stub namespace errors: PASS (tests/stub_effects.rs)
AC2.10 concurrent session isolation: PASS (tests/session_lifecycle.rs)

Known limitation (tracked in post-foundation dep-hardening plan):
- tidepool path deps during rewrite; convert to git-rev once foundation lands.
- No tidepool interrupt API; timeout wrapper detaches runaway threads.
- SdkLocation::Embedded and Auto are declared but todo!; see post-foundation SDK-distribution plan."
jj new
```
<!-- END_TASK_23 -->
<!-- END_SUBCOMPONENT_F -->

---

## Phase 3 "Done when" checklist

- [ ] `pattern_runtime` has tidepool path deps wired; `cargo check -p pattern_runtime` compiles
- [ ] FFI wrapper modules exist: `tidepool/{compile,machine,error_map}`
- [ ] `SdkLocation` enum with Directory implemented; Embedded + Auto declared with `todo!` carrying phase/AC refs
- [ ] 11 Haskell SDK modules in `crates/pattern_runtime/haskell/Pattern/` plus a `Prelude`
- [ ] 11 Rust effect handlers: `time`, `log`, and `display` fully implemented; `memory` + `message` stubbed (filled by Phases 4/5); `shell`/`file`/`sources`/`mcp`/`ipc`/`spawn` stubbed with actionable not-implemented errors
- [ ] `TidepoolSession::display()` accessor returns a cheap clone of the session's DisplayHandler for caller-side subscriber registration (CLI, tests, future UX)
- [ ] Handler bundle (`frunk::hlist![...]`) assembled and type-checks
- [ ] `TidepoolSession` + `TidepoolRuntime` implement Phase 2's `Session` + `AgentRuntime` traits
- [ ] Event-log checkpoint + deterministic restore round-trip works
- [ ] External wall-clock + CPU timeout harness with known-limitation note re: tidepool interrupts
- [ ] flake.nix integrates `tidepool-extract`; README + pattern_runtime/CLAUDE.md document setup
- [ ] Preflight check produces actionable diagnostics when `tidepool-extract` is missing
- [ ] Compile-time benchmark harness captures cold/warm/hot numbers for three program sizes
- [ ] All AC2.* cases have passing tests (with Linux-only gating on AC2.6)
- [ ] `cargo check`, `cargo clippy -- -D warnings`, `cargo doc` all zero-warning
- [ ] `bash scripts/audit-rewrite-state.sh` passes
- [ ] `just pre-commit-all` passes

## What this phase deliberately does NOT do

- Does not implement real `memory`, `message`, `shell`, `file`, `sources` handlers — Phase 4 (message backing), Phase 5 (memory rendering) own those.
- Does not attempt to upstream or fork tidepool to expose the step/resume primitives. Noted as post-foundation opportunity.
- Does not implement `SdkLocation::Embedded` or `Auto`. Declared for API stability; filled later.
- Does not implement an MCP handler or plugin surface. Stub only.
- Does not wire `pattern_runtime` into a CLI or server binary — Phase 6 smoke test builds the minimal driver.
- Does not touch `pattern_core` beyond adding a `RuntimeError::SdkNotFound` variant if that's missing post-Phase-2.
- Does not optimise the timeout harness beyond "it catches runaways per the ACs." Real cancellation needs upstream tidepool support.
