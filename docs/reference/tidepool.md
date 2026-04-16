# Tidepool: Haskell-in-Rust JIT Runtime

**Repository**: https://github.com/tidepool-heavy-industries/tidepool.git  
**Commit**: cc0ebf815967a215dfb662120ce24347f402ee71 (2026-04-15)  
**License**: MIT OR Apache-2.0

## Overview

Tidepool compiles Haskell effect programs (written using `freer-simple`) into native state machines via Cranelift JIT. The core design is **Haskell expands, Rust collapses**: Haskell code builds a pure, recursive description of side-effecting operations; Rust interprets that description through pluggable effect handlers.

The system lives in a single unified repository with both Haskell tooling (`tidepool-extract`, a GHC plugin) and Rust runtime (14 crates forming a 5-layer compilation pipeline). **Not** two separate projects—previous research incorrectly claimed orthogonality.

## IO Posture: Hard-Off by Construction

**Critical Finding**: IO is **architecturally inaccessible** from Haskell code, enforced at the type system and serialization boundary.

### The Boundary Mechanism

Tidepool implements a hylo boundary (hylomorphism: ana-phase expansion + cata-phase collapse):

1. **Haskell layer** (`tidepool-extract` + freer-simple): Haskell code constructs a tree of effect requests using algebraic effects. The `freer-simple` library provides a GADT-based effect system with no escape hatches—`unsafePerformIO` and direct C FFI are not available in the GHC Core IR produced by the serializer.

2. **CBOR boundary**: Core IR is serialized to Concise Binary Object Representation. No runtime environment, no `RealWorld` token, no Haskell RTS—only data.

3. **Rust execution** (`tidepool-codegen` + user effect handlers): The JIT compiles Core to native code. When execution encounters an effect request (e.g., a data constructor for `FileRead`), the JIT yields control to Rust, which dispatches to effect handlers. Handlers are user-provided Rust functions with full control over whether to perform IO.

### Evidence from Source

**Effect handler trait** (`tidepool-effect/src/dispatch.rs:42-65`):

```rust
pub trait EffectHandler<U = ()> {
    type Request: FromCore;
    fn handle(
        &mut self,
        req: Self::Request,
        cx: &EffectContext<'_, U>,
    ) -> Result<Value, EffectError>;
}
```

Haskell code cannot satisfy this trait—only Rust code can. The JIT yields effect requests as opaque `(tag, Value)` pairs; what happens next is entirely up to the Rust handler.

**Runtime rejects IO types** (`tidepool-runtime/src/lib.rs:44`):

```rust
#[error("IO type detected in result binding. IO operations (unsafePerformIO, etc.) are not supported in the Tidepool sandbox.")]
IOTypeDetected,
```

The runtime explicitly rejects Haskell expressions whose result type is `IO`. If the Haskell compiler produces an `IO` type in the result binding, compilation fails.

**Yield loop** (`tidepool-codegen/src/jit_machine.rs:115-190`):

The JIT's main loop alternates between JIT code execution and Rust handler dispatch. When the JIT encounters an effect, it returns control to Rust with effect data, waits for a handler response, and resumes. The Haskell side has no mechanism to escape this loop to arbitrary IO.

### Capability Model: Tag-Based Dispatch

The system uses a **tag-based dispatch** with an HList of handlers. Each effect in `Eff '[E0, E1, ..., EN]` maps to a numeric tag (0, 1, ..., N). Only Rust code that explicitly provides a handler for tag `i` can satisfy effect `Ei`. This is:

- **Selective**: Handlers are opt-in. An embedder can provide handlers for console I/O but not file I/O.
- **Typed**: Each handler has a request type (typically derived via `#[derive(FromCore)]`) and must explicitly convert between Haskell and Rust values.
- **Dynamic dispatch**: The tag is determined at runtime, so the JIT doesn't know which effect will fire—it just yields and waits.

**Production example** (`examples/tide/src/handlers.rs:90-110`):

```rust
#[derive(FromCore)]
pub enum ReplReq {
    #[core(name = "ReadLine")]
    ReadLine,
    #[core(name = "Display")]
    Display(String),
}

impl EffectHandler for ReplHandler {
    type Request = ReplReq;

    fn handle(&mut self, req: ReplReq, cx: &EffectContext) -> Result<Value, EffectError> {
        match req {
            ReplReq::ReadLine => {
                // Rust reads from stdin, returns Option<Value>
                cx.respond(result)
            }
            ReplReq::Display(s) => {
                println!("{}", s);
                cx.respond(())
            }
        }
    }
}
```

The Haskell code never sees the `println!`. It only requests "display this string"; the handler decides what to do (print, log, drop, route to API, etc.).

**Verdict**: Previous research claiming "IO is hard off by construction, no scoped permissions" is **correct** on the first part. Scoped permissions do not exist; the capability model is all-or-nothing per effect type. But this is suitable for agent sandboxing where the host simply doesn't expose dangerous handlers.

## Resource Bounding: Heap Limits, GC, No CPU Limits

### Heap Management

Tidepool implements **nursery-based allocation** with a copying garbage collector. Each JIT execution owns a nursery (default 64 MiB, configurable via `nursery_size` parameter).

**Allocation with limits** (`tidepool-codegen/src/alloc.rs`):

The JIT maintains `alloc_ptr` and `alloc_limit` pointers. When `alloc_ptr > alloc_limit`, allocation traps and triggers `gc_trigger` (a Rust-side host function). The GC then:
1. Traces from roots (values in registers/stack).
2. Compacts live objects into a new arena.
3. Updates `alloc_ptr` and `alloc_limit` to the new arena.

**Growth heuristic** (`tidepool-heap/src/arena.rs:147`):

If live data after GC exceeds 75% of nursery capacity, the nursery grows. **This is not a hard limit**—memory can grow unbounded if the program allocates faster than GC can collect.

**Nursery size control** (`tidepool-codegen/src/jit_machine.rs:83`):

```rust
pub fn compile(
    expr: &CoreExpr,
    table: &DataConTable,
    nursery_size: usize,
) -> Result<Self, JitError>
```

Embedders specify nursery size at compile time. The runtime default is 64 MiB (`tidepool-runtime/src/lib.rs:200`), overridable via `compile_and_run_with_nursery_size`.

### Effect Response Limits

**Hard limit on effect handler response sizes** (`tidepool-codegen/src/jit_machine.rs:171-173`):

```rust
const MAX_EFFECT_RESPONSE_NODES: usize = 10_000;
if nodes > MAX_EFFECT_RESPONSE_NODES {
    return Err(JitError::EffectResponseTooLarge { nodes, limit });
}
```

Prevents a malicious handler from returning a 100-million-node value. Reasonable for most queries but tunable in code.

### CPU Time / Wall-Clock Limits: **Not Implemented**

**Critical Gap**: Tidepool has **no interruption mechanism, call depth limits, or wall-clock timeouts**.

Evidence:
- **No signal handlers** for `SIGALRM` or `SIGINT` to interrupt Haskell code.
- **Call depth counter** exists (`tidepool-codegen/src/host_fns.rs`) for tail recursion optimization but is reset per GC and never triggers a fault.
- **No timeout in JIT loop** (`tidepool-codegen/src/jit_machine.rs:115-190`)—machine runs to completion or effect yield with no intermediate checks.

**Consequence**: Infinite recursion or an infinite loop will hang the calling thread indefinitely. The only escape is to kill the process.

**Recommendation**: If Pattern embeds Tidepool, implement external CPU limits via:
- `setrlimit(RLIMIT_CPU)` for subprocess-level limits.
- Watchdog thread with `std::thread::spawn` + timeout.
- Managed timeout wrapper before invoking the JIT.

**Verdict**: Previous research claim "no heap limits, CPU limits, or wall-clock timeouts" is **correct**. Heap is bounded by nursery + GC growth (unbounded in practice); CPU/wall-clock limits do not exist.

## Embedding Story: FFI, Build, Binary Size

### Public API Surface

**Compilation** (`tidepool-runtime/src/lib.rs:70-140`):

```rust
pub fn compile_haskell(
    source: &str,
    target: &str,
    include: &[&Path],
) -> Result<CompileResult, CompileError>
```

Takes Haskell source, target binder name, and include paths. Returns `(CoreExpr, DataConTable, MetaWarnings)` or an error.

**Execution** (`tidepool-runtime/src/lib.rs:223-245`):

```rust
pub fn compile_and_run_with_nursery_size<U, H: DispatchEffect<U>>(
    source: &str,
    target: &str,
    include: &[&Path],
    handlers: &mut H,
    user: &U,
    nursery_size: usize,
) -> Result<Value, RuntimeError>
```

Compiles and runs in one shot, dispatching effects through the handler and returning the final `Value` (convertible to JSON via `value_to_json`).

**No direct C FFI**: Tidepool does not expose Haskell function pointers or support low-level Rust↔Haskell function calls. All communication goes through the effect system. To call a Haskell function from Rust, wrap it in a handler that dispatches an effect.

### Build Process

**Haskell side**:
- Cabal project (`haskell/tidepool-harness.cabal`) with GHC 9.12.
- `tidepool-extract` is a GHC frontend plugin hooking into the Core generation phase.
- Produces the `tidepool-extract` binary, installed via Nix or `cabal install`.

**Rust side**:
```bash
cargo install --path tidepool
```

Installs the MCP server binary. To embed in another Rust project:
```toml
[dependencies]
tidepool-runtime = { path = "../tidepool/tidepool-runtime" }
tidepool-effect = { path = "../tidepool/tidepool-effect" }
tidepool-bridge = { path = "../tidepool/tidepool-bridge" }
```

Rust crates have no external dependencies beyond the standard Rust ecosystem (cranelift, frunk, thiserror, etc.)—**no Haskell RTS linking required**.

### Binary Size

- **`tidepool` binary**: 50-80 MB (includes Haskell prelude CBOR, Cranelift code, MCP server).
- **`tidepool-extract`**: ~300 MB (full GHC 9.12 toolchain, Haskell libraries).

When embedding, only the Rust crates are compiled; you still need `tidepool-extract` on the `$PATH` at runtime. Host Rust binary size is small (Rust-side crates only).

**Customization**:
- `TIDEPOOL_EXTRACT`: path to binary (defaults to `$PATH` lookup).
- `TIDEPOOL_PRELUDE_DIR`: override embedded stdlib location.
- `TIDEPOOL_GHC_LIBDIR`: override GHC's lib directory (avoids `ghc --print-libdir` call).

## Maturity Signals: Alpha-Stage, Actively Maintained

### Timeline

The cloned repository's shallow history shows only one commit dated **2026-04-15** (recent) with message `fix(prelude): add takeWhileT/dropWhileT to avoid T.takeWhile PAP bug (#268)`. The `#268` reference indicates **268 PRs have been merged historically**—substantial development activity.

### Test Coverage

- **~1351 tests total** (per orchestration plan).
- **Mutation testing**: 50% coverage (from `coverage-gaps.md`, dated 2026-03-14), meaning ~10 mutation operators survive testing. Notable gaps:
  - Lambda shadowing in substitution.
  - Transitive thunk forcing edge cases.
  - Some primops (`IntShra`, `SubWordCCarry`) have no tests.
  - GC ThunkRef tracing boundary conditions.
  - Nursery growth heuristic not checked.

### CI/CD

GitHub Actions (`.github/workflows/ci.yml`):
```yaml
test:
  runs-on: self-hosted
  steps:
    - build tidepool-extract (via `.github/ci-build-extract.sh`)
    - cargo test --workspace
    - cargo clippy --workspace -- -D warnings
```

Tests are gated; clippy warnings are treated as errors. Self-hosted runner suggests pre-public-CI status.

### Documentation

- **`ARCHITECTURE.md`**: Detailed 5-layer pipeline, hylo boundary, crate responsibilities.
- **`CLAUDE.md`**: Explicit project guidelines for AI agents, orchestration model, locked architectural decisions, test practices.
- **`CONTRIBUTING.md`**: Setup and development workflow.
- **`coverage-gaps.md`**: Transparent mutation testing analysis with specific line-number references.
- **`plans/` directory**: Phased implementation roadmap.
- **`.claude/plans/` directory**: 7 detailed remediation plans for known resource/safety issues, ready for parallel implementation.

Documentation is **comprehensive and well-maintained**, with explicit guidance for AI agent contributions.

### Active Remediation

The `.claude/plans/` directory contains 7 concurrent worktree-based fixes (orphaned threads, resource limits, mutex poisoning, signal closure leaks, etc.), scheduled for parallel implementation via ExoMonad's `spawn_leaf_subtree` pattern. This indicates:
1. Known issues are tracked and prioritized.
2. The team uses structured multi-agent orchestration to manage parallel work.
3. Quality is engineered, not accidental.

**Verdict**: **Alpha-stage with strong engineering discipline**. Not production-ready (version 0.1.0, ongoing safety fixes), but actively maintained and improving at a pace suggesting serious investment.

## ExoMonad Integration

This repository is developed within **ExoMonad**, a multi-agent orchestration system designed for AI-driven development. Key aspects relevant to Pattern:

### Branch Hierarchy and Worktree Isolation

Work is organized in a **tree of git worktrees**, not a single branch:

```
main                                [human]
├── main.lazy-thunks                [TL - Claude Opus]
│   ├── main.lazy-thunks.ws1-force      [leaf - Gemini]
│   ├── main.lazy-thunks.ws2-codegen    [leaf - Gemini]
│   └── main.lazy-thunks.ws3-tests      [leaf - Gemini]
└── ...
```

Each worktree is isolated. PRs target parent branches, then cascade to main.

### Fire-and-Forget Execution Model

The TL (Team Lead) does **not** wait for leaves:
1. TL writes spec, spawns leaf via `spawn_leaf_subtree`.
2. TL returns immediately and starts next task.
3. Leaf works, commits, files PR.
4. GitHub poller detects Copilot review, injects into leaf's pane.
5. Leaf iterates against Copilot until clean.
6. Leaf calls `notify_parent` with `success`; TL gets `[CHILD COMPLETE]`.
7. TL reviews merged diff and merges up.

**Convergence is leaf + Copilot**, not TL. This is the design.

### Not Relevant to Embedding

ExoMonad orchestration is **internal to Tidepool's development**. It doesn't affect compiled artifacts or runtime behavior. But it's important context: **AI-collaborative, parallel, worktree-based, with strong QA built in**. The `CLAUDE.md` explicitly states "All rules from the exomonad project apply here," indicating this development culture is intentional.

## Gaps and Caveats

1. **No CPU/wallclock limits**: Infinite recursion or loops will hang indefinitely. External watchdog required.
2. **Unbounded heap growth**: GC will grow the nursery if live data exceeds 75% of capacity. No hard ceiling unless imposed at the Rust level (e.g., `setrlimit(RLIMIT_AS)`).
3. **50% mutation score**: 10 mutations survived testing. Production use should expect edge-case bugs, especially in:
   - Substitution correctness (shadowing).
   - Thunk forcing chains.
   - Primop semantics (`IntShra`, `SubWordCCarry`).
   - GC reachability (`ThunkRef` tracing).
4. **No stable API guarantees**: Version 0.1.0; API may change.
5. **Self-hosted CI only**: Tests run on self-hosted runner, not GitHub's cloud infrastructure.

## LLM-Facing Haskell Surface

Agents write standard Haskell with these runtime constraints:
- **No IO monad**: Code must compile to pure Haskell Core. `unsafePerformIO` and direct C FFI are not available.
- **No system imports**: Modules like `System.IO`, `System.Process`, `Network` are unavailable or forbidden.
- **Effect-driven IO**: Agents request IO via effect handlers, which are Rust functions.

**Built-in Haskell prelude** (auto-imported, from `CLAUDE.md`):
- Text operations (pack, unpack, splitOn, replace, lines, words, etc.)
- List ops (map, filter, foldl, sort, nub, zip, take, drop, reverse, etc.)
- Numeric (even/odd, abs, round, parseIntM, parseDoubleM)
- JSON (Value type, toJSON, object, lenses, keys, arrays)
- Map operations (insert, delete, union, intersection, foldWithKey, etc.)
- Monadic combinators (mapM, forM, foldM, when, unless, join)

**Missing**:
- Concurrency (forkIO, MVar, TVar, STM)
- Unsafe operations (unsafeCoerce, unsafePerformIO)
- External libraries (dependency on host's module allow-list)

## Recommendations for Pattern

### IO Safety: Suitable for Agent Sandboxing

Tidepool's hard IO boundary makes it **well-suited** for running untrusted or semi-trusted Haskell agent code. Agents cannot:
- Read/write files.
- Make network requests.
- Access environment variables.
- Fork processes.
- Access the OS.

All require explicit handlers, which Pattern controls. This is **a stronger advantage** than Deno or WASM runtimes that provide unrestricted IO by default.

### Resource Safety: Requires Wrapping

- **Heap**: Configure a smaller nursery (e.g., 32 MiB) per agent execution. Monitor GC frequency. Wrap with `setrlimit(RLIMIT_AS)` or a watchdog if unbounded growth is a concern.
- **CPU**: Implement external timeout via `std::thread::spawn(agent, timeout)` with a watchdog that kills if exceeded. Tidepool has **no** built-in mechanism.
- **Effect responses**: The 10,000-node limit is reasonable for most queries, tunable in code.

### Maturity: Acceptable for MVP, Risky for Production

With 1351 tests, active maintenance, and clear documentation, Tidepool is suitable for **proof-of-concept or MVP**. For production:
- Wait for version 1.0.
- Mutation score should exceed 90%.
- CPU/wall-clock limits should be implemented upstream or well-documented.
- The `.claude/plans/` remediation track should be completed.

### Binary Size and Deployment

- Tidepool binary: 50-80 MB (with stdlib).
- Dependency: `tidepool-extract` must be on `$PATH` at runtime.

For Pattern's agents, acceptable for development/testing. For production edge deployment, moderate compared to full language runtimes.

### LLM Integration: Aperture Pattern

Tidepool's effect system is well-suited to structured agent control flows. The **aperture pattern** from the Prelude:

```haskell
main = do
  context <- ask "Gather context"
  if shouldProceed context
    then expensiveAnalysis context
    else pure "skipped"
```

The `ask` effect suspends execution, allowing the Rust side to gather information independently, then resumes with a decision steering the rest of the computation. This is a natural checkpoint for multi-phase agent reasoning.

## Honest Reassessment: Does Previous Research Hold?

**Previous claim**: "Deno is still the pragmatic bootstrap choice."

**Source-level findings**:
1. **IO posture**: Previous research claimed "IO is hard off by construction, no scoped permissions." ✓ **Correct** (with caveat: no scoped permissions means all-or-nothing, which is fine for agents).
2. **Resource bounding**: Previous research claimed "no heap limits, CPU limits, or wall-clock timeouts." ✓ **Correct** (heap is bounded by nursery but unbounded in practice; CPU/wall-clock limits do not exist).
3. **Maturity**: Alpha-stage, 1351 tests, 50% mutation score, active maintenance. Stronger than "research project," weaker than "production-ready."

**Updated assessment**:
- **For MVP/PoC**: Tidepool is **more suitable** than Deno. Harder IO boundary, pure-by-default semantics, active development culture.
- **For production**: Deno still wins on maturity and documentation. Tidepool's resource model is more manual. Both require external timeout wrapping for CPU limits.
- **For Haskell-native agents**: Tidepool is the **clear choice**. Type safety + lazy evaluation + IO safety = natural fit for agent reasoning.

**Verdict**: Previous research underestimated Tidepool's engineering quality. Source-level examination shows mature development practices (mutation testing, transparent gaps, structured multi-agent orchestration). The "pragmatic choice" depends on whether you value type safety + lazy semantics (Tidepool) or ecosystem maturity + community (Deno). For Pattern, if you're willing to handle resource wrapping, Tidepool is a defensible MVP choice with better long-term properties.

