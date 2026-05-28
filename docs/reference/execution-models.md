# Execution Models for Code-Act LLM Agents

This document surveys programmatic execution models for LLM agents that write and execute code in sandboxes instead of emitting JSON tool calls. The goal is to inform Pattern's rust rewrite, where agents generate and run code as their primary action mechanism.

## Executive Summary

Code-based execution models fundamentally change the agent capability ceiling: instead of constrained tool calls, agents write real code that composes tools, implements control flow, and expresses multi-step reasoning directly. This eliminates formatting overhead, enables better error recovery, and empirically yields higher success rates (up to 20% improvement). Trade-offs center on sandbox design, resource limits, and the execution primitive (Python interpreter, Deno, AST interpretation, etc.).

## 1. Code-Act: Unified Executable Actions (Foundational Work)

**Paper**: [Executable Code Actions Elicit Better LLM Agents](https://arxiv.org/abs/2402.01030) (ICML 2024)  
**Authors**: Xingyao Wang, Yangyi Chen, Lifan Yuan, Yizhe Zhang, Yunzhu Li, Hao Peng, Heng Ji  
**Official Implementation**: [xingyaoww/code-act](https://github.com/xingyaoww/code-act)

### Core Problem Addressed

Traditional LLM agent systems constrain actions to JSON tool calls:
- Fixed action space (only predefined tools available)
- No composition—each tool invocation is independent
- Format overhead—LLM must generate structured text, parser validates it
- Cascading errors—one malformed call breaks the interaction

Code-Act consolidates actions into a **unified code-based action space**: agents write executable Python, the environment executes it and returns results.

### Execution Architecture

```
LLM Agent generates Python code
    ↓
Code sent to execution engine (Jupyter Kernel Gateway in Docker)
    ↓
Individual Docker container executes code (per chat session)
    ↓
Stdout/stderr captured, returns to agent as observation
    ↓
Next iteration uses results to refine code
```

**Key mechanism**: Agents can observe execution results mid-turn and revise code. This enables dynamic error recovery—catch an exception, adjust parameters, retry. No round-trip delay needed between observation and next action.

### Benchmarks and Performance

**Datasets tested**:
- **API-Bank**: 264 tasks requiring tool use on 53 real-world APIs
- **M3ToolEval**: 82 human-curated tasks with complex multi-tool composition; problems require intricate coordination across multiple tools

**Performance vs. baselines**:
- **+20% absolute improvement** in success rate over JSON-based and text action formats
- **30% fewer actions** required to complete complex tasks (better planning via code)
- **17 LLMs evaluated**: From GPT-4 to open models (Llama, Mistral)

**Failure modes identified** (from API-Bank analysis):
- API hallucination (agents invoke non-existent API endpoints): 61.4%
- Incorrect retrieval (missing required API Search step): Common in GPT-4
- Format errors (incorrect parameter types): Reduced significantly with CodeAct
- Omitted calls (forgetting to invoke required tools): Less common with CodeAct

### Tool Chaining and Multi-Step Reasoning

The paper introduces **M3ToolEval** to measure complex tool composition. Key findings:

- **Tool composition strength**: Code naturally enables sequential dependencies—result from Tool A feeds into Tool B's parameters
- **Control flow**: Conditionals, loops, error handling emerge naturally in code, absent in JSON
- **Data flow**: Variables bind results, reducing hallucinated intermediate values
- **State persistence**: Within a code block, prior results are available; no need to re-invoke equivalent queries

Example capability gap: A task requiring "search for API docs, parse response, extract endpoint, call endpoint with parsed value" requires structured chaining impossible with pure tool calling.

### Limitations and Trade-offs

1. **Execution environment overhead**: Container startup per session adds latency
2. **Resource constraints**: Timeout, memory limits must be enforced (no published bounds in paper)
3. **Python-centric**: Limited to Python ecosystem; tooling integration via Python only
4. **Partial observability**: Agents see stdout/stderr, not structured tool responses (unless code formats them)
5. **Library availability**: No mention of sandboxing what Python packages are available

## 2. Phoebe: Deno-Based Sandbox Execution

**Repository**: [haileyok/phoebe](https://github.com/haileyok/phoebe)  
**Focus**: Trust & Safety agent (content moderation, investigation)

### Architecture and Permissions Model

Phoebe runs agent-generated TypeScript code in a **sandboxed Deno runtime** with extremely restrictive permissions:

**Denied by default**:
- No file system access
- No network access (local or remote)
- No NPM packages
- No subprocess spawning

**How tools access external systems**: Network requests and external APIs are invoked from **Python code outside the sandbox**. The agent writes TypeScript that calls exposed functions; those functions are implemented in Python and make network calls. This maintains the security boundary—agent code cannot directly access network.

### Resource Limits

- **Execution timeout**: 60 seconds per code block
- **V8 heap memory**: 256 MB limit
- **Code size**: 50,000 character maximum per block
- **Output size**: 1 MB limit for captured stdout/stderr

These are strict resource constraints, necessary because Phoebe runs in a production trust & safety pipeline.

### Stdlib and Tool Injection

Tools are exposed as a **global object** available to TypeScript code. Example shape (inferred from use cases):

```typescript
// Available in agent's TypeScript context
const tools = {
  query: (sql: string) => Promise<Array<object>>,
  createRule: (rule: object) => Promise<{ id: string }>,
  searchDomain: (domain: string) => Promise<object>,
  // ... investigation tools
};

// Agent code
const results = await tools.query("SELECT * FROM events WHERE ...");
const rule = await tools.createRule({ pattern: results[0].pattern });
```

The key design decision: **TypeScript code composes synchronously; async results are awaited**. This differs from one-shot tool calling—the agent can loop:

```typescript
for (const event of events) {
  const analysis = await tools.analyze(event);
  if (analysis.severity > threshold) {
    await tools.createRule({ pattern: analysis.pattern });
  }
}
```

### Tool Chaining Benefits

The original motivation for Phoebe's code-based approach:

> "When the agent knows it wants results from three separate SQL queries, it can group all three in a single execute_code block."

This eliminates the round-trip problem: with JSON tool calling, three tool calls = three turns (plus parsing overhead). With code execution, the agent issues all three queries, processes results, and decides next steps in a single turn.

### Security Boundary

Phoebe maintains security by **separating concerns**:

1. **Agent's TypeScript code**: Constrained, sandboxed, no network
2. **Tool implementations** (Python): Outside sandbox, can do anything
3. **Access control**: Agent's code privileges depend entirely on what tools are exposed

The agent cannot escalate privileges beyond the tools it's given—network access requires a tool that provides it, and that tool's implementation enforces Phoebe's policies.

## 3. AgentScript: AST-Based Execution (TypeScript)

**Repository**: [AgentScript-AI/agentscript](https://github.com/AgentScript-AI/agentscript)  
**Key Innovation**: Abstract Syntax Tree interpretation instead of direct code execution

### Why AST Instead of Direct Execution?

Traditional sandboxes (Deno, containers) provide **isolation**. AgentScript prioritizes **observability and resumability**:

The LLM-generated code is **not executed directly**. Instead:

1. Code is parsed into an AST
2. AST is interpreted in a custom runtime
3. Execution can be **paused at any statement or tool call**
4. State is serializable to a database
5. Execution can be resumed later from checkpoint

This enables:
- **Human-in-the-loop workflows**: Pause execution, human reviews decision, resumes
- **State persistence**: Save entire execution state (variables, call stack) to disk
- **Resumability**: Recover from failures by replaying from last checkpoint
- **Enhanced observability**: Track which tool calls fired, their order, their results

### Supported Code Subset

Not full JavaScript. Intentionally restricted to enforce agent focus:

**Supported**:
- Variable declarations and assignments
- Function calls (tool invocations)
- Basic object/array operations
- Console operations for logging

**Explicitly excluded**:
- Regular expressions (unnecessary for tool orchestration)
- Complex control flow initially (if/loops planned but not released)
- Template literals (use simple string formatting)
- Arrow functions
- Anything requiring computation vs. orchestration

The philosophy: **LLM should express orchestration, not computation**. If computation is needed, expose a tool for it.

### Tool Injection and Signatures

Tools are defined as an object:

```typescript
const tools = {
  addToDate: (date: Date, days: number) => Date,
  summarizeData: summarizeData({ model }),
  linear: {
    searchIssues: searchIssues({ model, linear })
  }
};
```

The LLM receives **detailed function signatures** (parameters, return types, descriptions). Tools can be namespaced (e.g., `linear.searchIssues`), matching real-world API structure.

### Limitations

1. **Subset of JavaScript only**: No loops, conditionals yet (under development)
2. **No external runtime needed**: Positive for control, but means agents can't use arbitrary libraries
3. **Single-function semantics**: Each statement is observed; no truly complex logic possible yet

## 4. Deno Embedding in Rust (deno_core)

**Crate**: [deno_core](https://docs.rs/deno_core/)  
**Documentation**: [Official Rust docs](https://docs.rs/deno_core/latest/deno_core/), [Embedding guide](https://deno.land/manual@v1.29.3/advanced/embedding_deno)

### Overview

`deno_core` is a Rust crate providing V8 bindings and abstractions for embedding a JavaScript runtime in Rust. It's what Deno itself is built on—suitable for agents, sandboxing, and custom runtimes.

### Key Components

**JsRuntime**: Main abstraction, manages a V8 isolate + event loop

```rust
let runtime = JsRuntime::new(Default::default());
let result = runtime.execute_script("script.js", code)?;
```

**V8 Isolates**: Each JsRuntime has its own isolated V8 heap, GC, and JavaScript context. Multiple isolates can run in parallel without sharing memory.

**Startup and Snapshots**:
- Cold start cost: ~50-100ms per isolate (V8 initialization)
- **Snapshots**: Pre-serialized V8 heap can be restored instantly; used by Deno to reduce startup latency
- `JsRuntimeForSnapshot` is used to create snapshots; requires one-time per-app build step

### Permission Model

Deno's permission system is **ops-based**: Native Rust functions (ops) are exposed to JavaScript. Each op can be gated by Deno's permission system.

**Permission categories**:
- File system (`--allow-read`, `--allow-write`)
- Network (`--allow-net`)
- Environment variables (`--allow-env`)
- Subprocess (`--allow-run`)
- System information (`--allow-sys`)

**Granularity**: Permissions can be:
- Whole category: `--allow-net` (all network)
- Specific resource: `--allow-read=/home/user/data` (only one file)

**In deno_core**: Permissions are customizable via `RuntimeOptions`. You can:
1. Expose only specific ops to JavaScript
2. Implement custom permission checking logic
3. Use Deno's built-in permission system if desired

### Exposing Rust Functions to JavaScript

**Ops** are the mechanism. Example (Deno source):

```rust
// Rust function
fn op_read_file(state: &mut OpState, path: String) -> Result<Vec<u8>, AnyError> {
    std::fs::read(&path).map_err(|e| e.into())
}

// Register with runtime
deno_core::extension!(my_ext, ops=[op_read_file]);

let mut runtime = JsRuntime::new(RuntimeOptions {
    extensions: vec![my_ext::init_ops()],
    ..Default::default()
});

// JavaScript can now call: Deno.core.ops.op_read_file("/path")
```

**Tool exposure pattern for agents**:
- Define ops for each tool (e.g., `op_query_api`, `op_list_files`)
- Agent's JavaScript code calls ops
- Each op enforces its own security policies

### Startup Cost and Alternatives

**deno_core startup**:
- Cold start: 50-100ms per isolate
- With snapshot: <1ms (pre-loaded heap image)
- Memory per isolate: 5-10 MB baseline

**Alternatives to deno_core**:

| Engine | Pros | Cons |
|--------|------|------|
| **V8 direct** (via rusty_v8) | Maximum performance, full control | Unsafe Rust, high complexity, no stdlib |
| **boa** (Rust-native JS engine) | Memory safe, no FFI, Rust-friendly | Slower than V8, partial ES6 support, no snapshot support |
| **QuickJS** (via rquickjs) | Smaller footprint than V8, fast startup | Less complete JS spec, weaker optimization |
| **deno_core** | Best ergonomics for Rust integration, proven production use, permission system | Slower startup (unless snapshot), heavier than alternatives, V8 memory use |

**Recommendation for agent execution**:
- **deno_core** if you want Deno's permission model and easy ops-based tool injection
- **QuickJS** if startup latency is critical and you're okay with incomplete JS support
- **V8 direct** only if you need maximum performance and can handle unsafe Rust

## 5. ExoMonad: Haskell-Based Agent Orchestration (Limited Information)

**Author**: Inanna Malick  
**Blog**: [recursion.wtf](https://recursion.wtf/) (Rust tag)  
**Related Project**: Tidepool (Haskell-in-Rust runtime)

### What We Know

**Disclaimer**: ExoMonad's exact architecture is not publicly documented in detail. The following is inferred from indirect references and the author's blog.

ExoMonad is described as a **radically reconfigurable agent orchestration system** that:

- Replaces the "swarm of agents PRing main" model with a **tree of worktrees** (Git-based isolation)
- Hooks into Claude Agent Teams' messaging bus (agents running different LLM backends appear as team members)
- Stitches together multiple LLM providers (Claude, Gemini, Kimi, Letta Code, Copilot) using their existing binaries
- Can be reconfigured for different purposes (original: PR orchestration, but flexible)

**Example use case**: Inanna used ExoMonad to implement a new **Haskell compiler backend in Rust** over ~700 PRs, demonstrating agent coordination at scale.

### Tidepool Connection

Tidepool is a **lazily evaluated Haskell-in-Rust runtime** built using ExoMonad (in ~2 weeks, <50% of Claude Max + Gemini Ultra subscription):

- Native interop between Haskell and Rust
- Uses Cranelift JIT directly on Haskell compiler's Core IR (instead of WASM)
- Execution model: Haskell code executes as agents coordinate its building

This suggests ExoMonad's execution model is **generalist—it can coordinate different execution substrates** (Haskell, Rust, other languages) rather than being tied to a specific one.

### Inferred Architecture

Based on available information:

1. **Coordination layer**: Agents communicate via a messaging bus (Claude Agent Teams API)
2. **Worktree isolation**: Each agent works in its own Git worktree; changes are coordinated via PRs
3. **Multi-model**: Agents can run different LLM backends; system treats them uniformly
4. **Reconfigurable execution**: The execution model (what agents do, what tools they have) is customizable

**Execution model likely involves**:
- Code generation (agents write code, similar to Code-Act)
- Distributed execution (agents run in parallel on different worktrees)
- Synchronization via Git (PRs as the atomic unit of coordination)
- LLM abstraction (supports multiple backends via standard interfaces)

### Why This Matters for Pattern

ExoMonad demonstrates that **agent execution can be decoupled from a single runtime**. Instead of "agents write Python in a Docker container," the pattern is "agents write code in their native language, and that code executes in a language-appropriate runtime." This is valuable if Pattern needs to support multi-language agent codebases.

### Information Gaps

- Exact code execution primitive (subprocess? JIT? Interpreted?)
- Tool exposure mechanism (how agents access capabilities)
- Error recovery strategy
- Resource limits and timeout model

If you need these details, contact Inanna Malick directly ([GitHub](https://github.com/inanna-malick), [Twitter](https://twitter.com/inanna_malick), or [recursion.wtf](https://recursion.wtf/)).

## 6. Tool Chaining and Error Recovery Patterns

### Tool Chaining Models

**One-shot tool calling** (traditional):
```
Agent → Tool 1 (returns result A) → new turn → Agent → Tool 2 (uses A) → new turn
```

**Code-based chaining** (Code-Act, Phoebe, AgentScript):
```
Agent writes code:
  result_a = await tool_1()
  result_b = await tool_2(result_a.field)
  result_c = tool_3(result_a, result_b)
  
All executed in single turn, all results available immediately
```

**Benefits of code-based**:
- Reduces round-trip latency (multiple tools in one turn)
- Better data flow (results are variables, not re-parsed text)
- Natural composition (nested function calls, sequential dependencies)
- Intermediate results don't need to be hallucinated by next iteration

### Error Recovery

**Code-Act approach** (Python in Jupyter):
```python
try:
    api_response = requests.get(endpoint, params)
    data = api_response.json()
except Exception as e:
    # Agent observes error, next turn can adjust parameters
    # e.g., if 401, try with different auth; if timeout, retry
```

Agent observes the exception, analyzes it, and **in the next turn** writes corrected code. This is one-shot recovery per execution cycle.

**Phoebe approach** (Deno TypeScript):
```typescript
try {
  const result = await tools.query(sql);
  // Process result
} catch (e) {
  // Try alternative approach
  const fallback = await tools.query(alternative_sql);
}
```

Both approaches rely on the **agent's reasoning** to recover. The execution environment doesn't auto-retry or provide structured error handling.

### Multi-Step Reasoning in Code

The **M3ToolEval** benchmark from Code-Act measures this. Example task structure:

```
1. Search for API documentation
2. Parse response to extract endpoint URL
3. Extract required parameters from docs
4. Validate user input against parameters
5. Call endpoint with validated input
6. Parse response and format for user
```

With tool calling, each step = tool call. With code:

```python
docs = api_search("service")
endpoint = parse_docs(docs)['endpoint_url']
params = validate_params(user_input, parse_docs(docs)['required'])
result = call_endpoint(endpoint, params)
return format_result(result)
```

The code naturally expresses the dependency chain. The agent can see the intermediate values. If step 2 fails (malformed response), the agent can inspect it and adjust step 3.

### Streaming and Cancellation

**Not addressed in surveyed systems**. Current implementations are synchronous (code block executes, returns result). Streaming outputs (token-by-token results) and cancellation mid-execution are not documented.

**Pattern's rewrite should consider**:
- Can agent code emit results progressively?
- Can agent code be interrupted? How does cleanup happen?
- What's the interaction model—fire-and-forget, or does agent see intermediate outputs?

## 7. Recommendations for Pattern's Rust Rewrite

### Choice of Execution Primitive

**Option A: deno_core + TypeScript**
- ✅ Proven in production (Phoebe)
- ✅ Permission model maps naturally to agent capabilities
- ✅ Good startup cost with snapshots
- ✅ Cross-platform
- ⚠️ V8 memory overhead (~5-10 MB per isolate)
- ⚠️ Slower than some alternatives

**Option B: QuickJS (via rquickjs)**
- ✅ Minimal memory footprint
- ✅ Fast startup (no snapshot needed)
- ✅ Simple FFI
- ⚠️ Incomplete JavaScript spec (may limit future flexibility)
- ⚠️ Fewer examples in agent systems

**Option C: Subprocess-based (Python interpreter for Code-Act model)**
- ✅ Familiar to many agents (Python ecosystem)
- ✅ Inherent isolation (separate process)
- ⚠️ Startup cost per execution
- ⚠️ IPC overhead
- ✅ Best for containerized deployments

**Recommendation**: Start with **deno_core + TypeScript** if you want permission-based tool gating and smooth Rust integration. Use **Code-Act's Jupyter model** if you already have Python infrastructure and can accept container-per-session overhead.

### Tool Exposure Architecture

Learn from Phoebe and AgentScript:

1. **Define tool interface in agent language** (TypeScript types for deno_core agents)
2. **Implement tools in Rust** (custom ops or external functions)
3. **Gate each tool's invocation** (permission system or explicit checks)
4. **Return structured results** (JSON-serializable, not string stdout)

Example architecture:

```rust
// Agent's view (TypeScript)
const tools = {
  list_tasks: async () => Task[],
  update_task: async (id: TaskId, update: TaskUpdate) => Task,
  query_db: async (sql: string) => QueryResult,
};

// Rust implementation
impl PatternAgent {
    fn register_tools(runtime: &mut JsRuntime) {
        runtime.op_async::<fn_list_tasks>("pattern::list_tasks");
        runtime.op_async::<fn_update_task>("pattern::update_task");
        // ... gate by agent's role/permissions
    }
}
```

### Resource Limits and Timeouts

Borrow Phoebe's model:

- **Execution timeout**: Configurable, typically 30-60 seconds for responsive agents
- **Memory limit**: Enforce via OS (cgroup/limits) or V8 heap limit
- **Code size limit**: Reject code blocks over N characters (prevents DoS)
- **Output size limit**: Truncate stdout/stderr returned to agent

Make these **configurable per agent** (power users get higher limits).

### State and Resumability

Unlike AgentScript (which emphasizes resumability), most deployed systems use **stateless execution**:

- Each code block is independent
- Results are communicated back to agent (which may store them in memory blocks)
- No automatic checkpointing

**Pattern's rewrite should decide early**:
- Do agents need resumable execution? (Required for human-in-the-loop workflows)
- If yes, adopt AST-based interpretation (similar to AgentScript)
- If no, simpler to use deno_core or subprocess-based approach

### Error Propagation and Observability

Ensure agents see:

1. **Structured error information** (not just stack traces)
   - Error type (TimeoutError, PermissionDenied, RuntimeError, etc.)
   - Line number in agent code
   - Attempt count (for retries)

2. **Execution trace** (for debugging)
   - Which tools were called, in what order
   - Arguments and return values (up to size limit)
   - Timing of each tool call

3. **Resource usage** (for capacity planning)
   - Execution time
   - Memory peak
   - Number of tool calls

Make this observable for both the agent (for dynamic recovery) and operators (for monitoring).

## References

1. [Code-Act Paper (arXiv 2402.01030)](https://arxiv.org/abs/2402.01030)
2. [xingyaoww/code-act](https://github.com/xingyaoww/code-act) — Official implementation
3. [haileyok/phoebe](https://github.com/haileyok/phoebe) — Deno-based trust & safety agent
4. [AgentScript-AI/agentscript](https://github.com/AgentScript-AI/agentscript) — AST-based execution
5. [deno_core Documentation](https://docs.rs/deno_core/) — Rust V8 bindings
6. [Deno Embedding Guide](https://deno.land/manual@v1.29.3/advanced/embedding_deno)
7. [boa JavaScript Engine](https://github.com/boa-dev/boa) — Rust-native alternative
8. [recursion.wtf](https://recursion.wtf/) — Inanna Malick's blog (ExoMonad context)
9. [Tidepool Heavy Industries](https://tidepool.leaflet.pub/) — Haskell-in-Rust runtime
10. [Cage4Deno: A Fine-Grained Sandbox for Deno Subprocesses](https://dl.acm.org/doi/fullHtml/10.1145/3579856.3595799) — Advanced Deno sandboxing

## Appendix: Comparison Table

| Aspect | Code-Act (Python) | Phoebe (Deno) | AgentScript (AST) | deno_core | ExoMonad |
|--------|---|---|---|---|---|
| **Language** | Python | TypeScript | TypeScript | JavaScript/TypeScript | Multi-language |
| **Execution Model** | Jupyter in Docker | V8 Isolate | Custom AST Interpreter | V8 Isolate | Agent-coordinated |
| **Startup Cost** | ~500ms (container) | ~10-50ms (isolate) | ~1-5ms (AST) | ~10-50ms | Varies |
| **Tool Model** | Python imports | TypeScript functions | Object methods | Ops | Language-specific |
| **Resumability** | No | No | Yes (by design) | Optional (custom) | Yes (worktree-based) |
| **Permission Model** | Container limits | Deno permissions | AST constraints | Custom ops | Git-based isolation |
| **Production Ready** | Yes | Yes | Alpha | Yes (via Deno) | Yes |
| **Tool Chaining** | Sequential, native | Parallel + sequential | Sequential | Native | Native |
| **Error Recovery** | Observe & retry | Try/catch or observe & retry | On-statement pause | Observe & retry | Agent-driven |

