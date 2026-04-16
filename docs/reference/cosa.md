# Cosa: AST-Walking Interpreter Language

## Summary

Cosa is a Turing-complete, asynchronous scripting language written in Rust (12.2K LOC across lexer, parser, AST, and evaluator). Originally designed for bioreactor control systems (OmniaBio), it features a pure AST-walking interpreter with no FFI or unsafe code. The language prioritizes readable syntax (Python-like duck-typing), first-class support for asynchronous execution via tokio, and domain-specific types (Time, Volume, FlowRate). For agent repurposing, Cosa offers a sandbox-by-construction design but exposes filesystem access without path restrictions, requires intentional capability removal, and has modest maintenance burden with stable module boundaries. Key appeal: eliminating runtime bloat (vs Deno/TypeScript) while retaining safe execution, though performance is slower than compiled languages.

---

## 1. Overview

**Repository:** https://github.com/GBC-OmniaBio-OS/cosa  
**License:** Not specified in Cargo.toml or README (unclear; recommend verifying with maintainers)  
**Version:** 0.1.0 (early stage)  
**Last commit:** July 2024, 63 commits total  
**Original domain:** Bioreactor control scripting for OmniaBio hardware (pumps, sensors, UART communication)

Cosa is intended as a text-based scripting language balancing learnability with expressiveness. It compiles to an AST, which is then interpreted via a single-pass evaluator. The primary design goal was ergonomic scheduling and timing abstractions for hardware control, not general-purpose programming, but it is sufficiently general-purpose for agent logic.

---

## 2. Language Design

### Syntax Style: Hybrid Functional-Imperative

Cosa blends functional and imperative styles with unconventional operator semantics:

- **Binding (`<-`)**: Lazy definition or binding of a value to an identifier. Introduces scoping or shadowing depending on context (lib/cosa.rs:92-95).
- **Application (`->`)**: Eager evaluation and assignment; often denotes a function return point. Crosses scope boundaries.
- **Lambda (`=>`)**: Function declaration, typically bound to an identifier (lib/cosa.rs:113).

Example from test_code.cosa:
```cosa
f <- { (x: int)->int => {(x * 2)->@int} }
map(f, [1, 2, 3, 4, 5]) -> res
```

Conditionals use `when...::...|->` (if-else) and `match...|->::` (pattern matching), where `::` is the "then" separator and `|->` denotes a case/else branch (lib/ast/mod.rs:1060+).

### Type System

**Declared but optional**: Cosa supports explicit type annotations (e.g., `x: int`) but relies heavily on type inference. Type checking is implemented (lib/evaluator/typecheck.rs, 1245 LOC) but mostly unenforced at runtime (README states "type checking built out but mostly not used yet").

**Core types** (lib/types.rs):
- **Numeric**: `Int` (i32), `Dec` (Decimal via rust_decimal for precision), `Time`, `Volume`, `FlowRate`
- **Collections**: `List`, `Map`, `Tuple`, `Enum` (restricted variants)
- **Higher-order**: `Result`, `Maybe`, custom types via enum/dict declarations
- **Advanced**: `OStream` (output stream for async data flow), `Pipe` (inter-routine communication)

**Time handling is specialized** (lib/types.rs:30-70): Timestamps bundle wall-clock (`DateTime<Utc>`) and monotonic time (`Instant`) to support both absolute scheduling and duration calculations. Custom lexing allows ergonomic syntax like `30s`, `1hr`, `100.5mL`.

### Evaluation Model: Asynchronous AST Walking

**Core interpreter** (lib/evaluator/mod.rs):
- Single `Evaluator` instance wraps a reference to an `Environment` (lib/evaluator/environment.rs:35-42).
- `eval()` is async; most expressions spawn lightweight tokio tasks via `make_eval_task()` (lib/evaluator/mod.rs:627-635) or `eval_task()` (lib/evaluator/mod.rs:636-670).
- Left and right operands of binary operations are evaluated **concurrently** (lib/evaluator/mod.rs:188-200: `join!` on parallel eval tasks).
- Lazy vs eager semantics governed by binding operator (`<-` defers, `->` forces).

**Task model** (lib/evaluator/runtime.rs):
- `Runtime` manages a task pool with task registration (15-25), garbage collection, and call routing via mpsc channels.
- Each user-defined function spawns a `fn_task()` (lib/evaluator/mod.rs:950+) that listens for messages on an input channel and publishes results on a broadcast output.
- Inter-task calls flow through `Runtime::call_ch`, enabling decoupled execution.

### Notable Primitives and Built-ins

**I/O and side effects** (lib/evaluator/builtins.rs:72-88):
- `print(data)` — output to stdout
- `read()` — stdin input
- `read_file(path)`, `write_file(path, content)`, `append_file(path, content)` — file operations (currently **unrestricted**)
- `read_serial_input()`, `write_serial_output()` — UART comms for hardware

**Scheduling and time** (lib/evaluator/builtins.rs:80-85):
- `now()` — current timestamp
- `do_at(time, fn)` — schedule function at absolute time
- `do_in(duration, fn)` — schedule after delay
- `do_every(interval, fn)` — periodic scheduling

**Functional operations** (lib/evaluator/builtins.rs:82-87):
- `map(fn, list)`, `for_each(fn, list)`, `flatten(list)` — list processing
- `while(cond, fn)` — loop abstraction
- `ok(x)`, `err(msg)`, `some(x)` — Result/Maybe constructors

**Error handling**: Built-in `Result(@T, @E)` and `Maybe(@T)` monads, operations return either wrapped values or error atoms (lib/types.rs:780-815).

---

## 3. Interpreter Architecture

### Module Layout (lib/ directory, 12.2K LOC)

| Module | Size | Purpose |
|--------|------|---------|
| cosa.rs | 330 LOC | Library root; exports AST, lexer, parser, evaluator, types |
| lexer/ | 579 LOC | Token generation using nom parser combinators |
| parser/ | 1330 LOC | Token-to-AST via nom; precedence, function binding, conditionals |
| ast/mod.rs | 1133 LOC | Expression, statement, operator, atom definitions |
| ast/functor.rs | 711 LOC | Functor trait + enum dispatch for I/O, pipes, transforms, monads |
| types.rs | 996 LOC | Data type definitions (Num, List, Map, Time, Custom) |
| evaluator/mod.rs | 1127 LOC | Main eval loop, task spawning, expression evaluation |
| evaluator/builtins.rs | 1068 LOC | 21 built-in functions (print, map, schedule, etc.) |
| evaluator/typecheck.rs | 1245 LOC | Type inference and checking (largely unused at runtime) |
| evaluator/compiler.rs | 691 LOC | Lambda/function compilation and partial application |
| evaluator/environment.rs | 435 LOC | Variable scope, parent/child environment chains |
| evaluator/operations.rs | 783 LOC | Arithmetic, boolean, comparison operations on atoms |
| evaluator/runtime.rs | 265 LOC | Tokio task pool and message routing |

### Key Extension Points

**Adding new built-ins**:
1. Define async fn in lib/evaluator/builtins.rs with signature `async fn b_name(args: Vec<Atom>) -> Result<Atom, String>`
2. Register in `BuiltinFunctions::get_builtins()` (lib/evaluator/builtins.rs:69) with `add_builtin_fn("name", BuiltinType::*, |f| Box::pin(b_name(f)))`
3. Return type wrapped in `Atom::Data()` or `Atom::Functor()` as appropriate

**Adding new data types**:
1. Define struct/enum in lib/types.rs, implement `Typed`, `DataType`, `Display` traits
2. Add case to `Data` enum and `Atom::Data()` matching
3. Implement operations in lib/evaluator/operations.rs if arithmetic/comparison needed

**Hooking side effects**:
- Current I/O (file, serial, stdout) is baked into lib/evaluator/builtins.rs. To intercept or restrict, pass a capability object or function pointer to `Evaluator::new_with_env()` or extend the `Environment` struct with a `capabilities` field.

### Environment and State Model

`Environment` (lib/evaluator/environment.rs:9-19) is immutable-outside-its-rwlock:
- `store: HashMap<Ident, Atom>` holds bound variables
- `parent: Option<Arc<RwLock<Environment>>>` enables nested scopes
- Each task or eval context gets its own environment or a child of the runtime's root environment

Mutation via `set()`, `register_ident()`, `assign()`, or lazy binding with `<-`. Scoping follows lexical rules except for `->` assignment, which climbs the parent chain until it finds the binding or stops at root.

---

## 4. Sandboxing Posture

### Safe by Construction (No Unsafe Code, No FFI)

- Zero `unsafe` blocks in lib/ (grep: no matches)
- No external FFI; only idiomatic tokio and Rust stdlib
- No dynamic loading, code generation, or reflection

### Capabilities Present by Default (Security Risk for Agents)

**Unrestricted filesystem access**:
- `read_file(path: String)` opens any path (lib/evaluator/builtins.rs:241-253)
- `write_file(path: String, content: String)` creates/truncates (lib/evaluator/builtins.rs:281-302)
- `append_file(path: String, content: String)` appends (lib/evaluator/builtins.rs:257-278)
- No path validation, sandboxing, or capability-based restrictions

**Unbounded async execution**:
- `do_at()`, `do_in()`, `do_every()` can spawn unlimited tasks on the tokio runtime
- No resource quotas, CPU time limits, or memory caps

**Serial I/O** (optional feature):
- `omniabio-comms` feature (Cargo.toml) enables hardware UART comms
- Could be disabled at compile time if not needed for agents

### Required Changes for Agent Sandboxing

1. **Path allowlists**: Modify `b_read_file()`, `b_write_file()`, `b_append_file()` to validate paths against a compile-time or runtime allowlist.
2. **Remove filesystem functions entirely**: Delete the three file I/O functions from `BuiltinFunctions::get_builtins()` if agents should not touch disk.
3. **Resource caps**: Extend `Runtime` and `Evaluator` with:
   - Task count limit (reject `do_at`/`do_in` if exceeded)
   - CPU time timeout per evaluation (spawn with `tokio::time::timeout`)
   - Memory usage tracking (auditing is manual; Rust's Rc/Arc don't track heap)
4. **Capability injection**: Pass a trait object or struct to `Evaluator` limiting which built-ins are available.

**Good news**: Boundary is clean. Built-ins are centralized; lexer/parser do not allow FFI syntax; no dynamic linking. Removing capabilities requires editing one file (builtins.rs).

---

## 5. Performance Characteristics

### Not Profiled in Shipped Code

README states "slow but safe"; no benchmarks in repo. Observable costs:

**Async overhead**: Every sub-expression may spawn a tokio task (via `make_eval_task()`), incurring:
- Channel allocation (`oneshot::channel()`)
- Task spawn (`task::spawn()`)
- Await on receiver

For small expressions, this is wasteful. Larger expressions with independent operands benefit.

**Lexing/parsing**: nom-based, single-pass, likely O(n) in source size. No incremental parsing.

**AST walking**: Direct match-on-expr in `eval_task()` (lib/evaluator/mod.rs:700-900), no bytecode or optimization. Interpreter overhead is typical for dynamic languages.

**Memory**: Heavy use of `Arc<RwLock<_>>` for shared state and channels. No generational GC; relies on Rust's reference counting. Large programs with many environments may leak memory if cycles aren't dropped.

**Suitable for**: Small agent scripts (< 1000 LOC), periodic tasks, I/O-bound workloads. Not suitable for tight loops or compute-intensive workloads.

---

## 6. Forking and Adaptation Effort

### Stability and Modularity

**Positive signals**:
- Clear separation: lexer, parser, AST, evaluator, types are distinct modules with minimal coupling.
- Evaluator entry points are public (`eval()`, `eval_body()`, `new_with_env()`; lib/evaluator/mod.rs:90-220).
- Built-in registry is centralized and extensible (lib/evaluator/builtins.rs:69-89).
- No external dependencies on bioreactor-specific domain (optional hardware feature is gated).

**Risks**:
- Early-stage project (0.1.0, 63 commits, 12 months of development). Spec and API may shift.
- Type checking is incomplete ("mostly not used yet"); runtime is lenient. Type safety cannot be relied on.
- Async model is tightly coupled to tokio; switching runtimes would be disruptive.
- No semantic versioning signal (no CHANGELOG, no stability guarantees).
- Sparse test coverage (tests.rs files exist; grep suggests unit tests but no integration test suite visible).

### Repurposing as Agent Runtime

**Effort: Low to Moderate (2-4 weeks for a minimal fork)**

1. **Copy lib/ to pattern_cosa crate** (or fork OmniaBio repo and add Pattern-specific extensions).
2. **Remove hardware feature**: Disable `omniabio-comms`, `tokio-serial`, `serial` in Cargo.toml.
3. **Add agent host functions**: Extend builtins.rs with agent-specific operations (e.g., HTTP to Pattern API, memory queries).
4. **Remove/gate filesystem I/O**: Edit `BuiltinFunctions::get_builtins()` or wrap in a feature flag.
5. **Capability-gate execution**: Wrap `Evaluator` in a struct that vets function calls against an agent's privilege set.
6. **Test**: Write integration tests for agent scripts invoking Pattern APIs.

**Effort: Higher (4-8 weeks) to fully separate and maintain**:
- Strip OmniaBio-specific types (Volume, FlowRate) unless useful for agents.
- Add proper error types (`#[non_exhaustive]` on Result, context with miette).
- Implement benchmarks to establish performance baseline.
- Add incremental parsing or bytecode compilation if performance matters.
- Write comprehensive security audit for sandbox enforcement.

---

## 7. Distinctive Characteristics

### Why Cosa Over Deno/TypeScript or Haskell

1. **No Runtime Bloat**: Cosa's interpreter is ~12K LOC, one Rust binary, no V8 engine or GHC runtime. Deno ships ~100MB; cosa ~5-10MB at worst.

2. **Async-First, Not Retrofit**: Unlike JavaScript (promises bolted on) or Python (asyncio awkward), cosa's AST and evaluator assume asynchrony. Concurrent expressions are idiomatic.

3. **Ergonomic Time Handling**: Native `Time`, `TimeDelta` types with lexer support (`30s`, `1hr`) beats JavaScript Date or Haskell's `DiffTime`.

4. **Domain-Friendly Extensibility**: Adding a new type (e.g., agent state, planning graph) is straightforward; no need to modify a type system or runtime VM.

5. **Human-Readable Syntax**: Closer to pseudocode than Lisp or Haskell; agents can write logic without functional programming expertise. Python-like duck-typing lowers barrier to entry.

6. **Sandbox by Construction**: No FFI, no `eval()`, no reflection. Restricting cosa is deleting built-ins; restricting Deno requires --allow flags and runtime policing.

### Trade-offs

- **Slower**: Interpreter, no JIT, lots of async overhead. Agent scripts will run slower than TypeScript on Deno.
- **Smaller standard library**: Cosa has 21 built-ins vs. Deno's thousands. Agents may need custom implementations of common utilities.
- **Incomplete type checking**: Type errors caught at runtime, not compile time. Less suitable for large, long-lived codebases.

---

## 8. Risks and Unknowns

### Maintenance Burden

**Low risk, short term**: OmniaBio maintains cosa (7 PRs, steady commits). Forking does not depend on upstream—copy, fork, or vendor the code.

**Medium risk, long term**:
- Early-stage project lacks semantic versioning and stability guarantees. If OmniaBio diverges (e.g., breaks AST structure), your fork must be maintained independently.
- No active community or third-party extensions. Bug fixes and performance improvements are your responsibility.

### Spec Instability

- No formal grammar or language spec. Semantics inferred from code and README comments.
- Type system is incomplete (type checking "mostly not used"). If you need static guarantees, you'll have to implement them or live with runtime errors.

### Performance Unknowns

- No published benchmarks. Agent performance on cosa vs. alternatives is empirical; measure with realistic scripts.
- Async overhead for small expressions may be significant; profiling needed.

### Security Unknowns

- File I/O functions have no sandboxing. **Agents can read/write arbitrary paths by default.**
- No review against OWASP or CWE. Potential for expression bombs (deeply nested expressions causing stack overflow), resource exhaustion (unbounded tasks), or timing attacks if agents share an evaluator.

### Testing and Observability

- No logging framework beyond tracing crate (feature-gated, not built-in). Hard to debug agent behavior in production.
- Tests exist but are sparse. No formal test plan or coverage metrics.

---

## 9. Concrete File References

### AST and Syntax Definition
- **lib/ast/mod.rs:1-100** — `Expr` enum (If, Match, Bind, Apply, Lambda, Call, etc.)
- **lib/lexer/mod.rs:20-55** — operator and symbol definitions (bind `<-`, apply `->`, lambda `=>`, etc.)
- **lib/parser/mod.rs:1-100** — parser entry point and precedence handling

### Evaluator and Execution
- **lib/evaluator/mod.rs:90-220** — `Evaluator` struct, `eval()`, `eval_body()` public methods
- **lib/evaluator/mod.rs:627-670** — `eval_task()` implementation; spawns tokio task for expression evaluation
- **lib/evaluator/runtime.rs:1-50** — `Runtime` struct, task pool management
- **lib/evaluator/builtins.rs:69-95** — `BuiltinFunctions::get_builtins()` registry; add new built-ins here

### Types and Environment
- **lib/types.rs:150-250** — `Time`, `TimeDelta`, `Timestamp` definitions
- **lib/evaluator/environment.rs:40-60** — `Environment` struct and creation methods
- **lib/evaluator/typecheck.rs:1-100** — Type inference entry point (unused at runtime)

### Examples
- **test_code.cosa** — fold, map, list operations
- **pump.cosa** — Hardware-domain example (enum, type definitions)

---

## 10. Conclusion

Cosa is a viable candidate for a Pattern agent execution language if you prioritize sandboxing, simplicity, and async semantics over performance or language maturity. The interpreter is small, modular, and safe by construction. Repurposing it for agents requires disabling file I/O, adding Pattern-specific built-ins, and establishing a capability-checking layer—doable work with moderate engineering effort.

**Proceed if**: Agent scripts are small, network I/O is rare, agent semantics align with async-first design, and you can maintain a fork independently.

**Reconsider if**: Agents must run untrusted code (seria ecosystem lacks formal safety proofs), performance is critical, or you need a language with a large standard library and active community.

For detailed implementation decisions, consult with Pattern maintainers on whether the sandbox gap (filesystem access) is acceptable, whether the async model suits agent logic, and whether performance measurements show cost-benefit over TypeScript/Deno.
