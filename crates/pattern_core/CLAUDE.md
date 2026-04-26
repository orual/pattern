# CLAUDE.md - Pattern Core

⚠️ **CRITICAL WARNING**: DO NOT run `pattern` CLI or test agents during development!
Production agents are running. CLI commands will disrupt active agents.

Last verified: 2026-04-26

Core agent framework, memory trait definitions, tools, and coordination system for Pattern's multi-agent ADHD support. The `MemoryStore` trait is defined here; the canonical implementation (`MemoryCache`) lives in `pattern_memory`.

## Current status
- Loro CRDT memory, Jacquard ATProto client.
- Shell tool implemented with PTY backend and security validation.
- Phase 5 complete: message attachment model, batch-anchored snapshots,
  turn round-trip recording, `TurnInput::continuation` flow.
- v3-memory-rework complete: `MemoryStore` desynced (28->19 methods),
  `MemoryCache` + `SharedBlockManager` extracted to `pattern_memory`,
  `IsolatePolicy` + consolidation types added to `types/memory_types`.
- v3-TUI complete (2026-04-23): `TurnEvent` and related turn-sink types
  gained `Serialize`/`Deserialize` so the daemon can fan events out over
  IRPC via `WireTurnEvent`. The unified `MemoryError` variant set here is
  the canonical error type — duplicates were folded in during v3-TUI
  stabilisation.

## Tool System Architecture

Following Letta/MemGPT patterns with multi-operation tools:

### Core Tools
1. **context** - Operations on context blocks
   - `append`, `replace`, `archive`, `load_from_archival`, `swap`

2. **recall** - Long-term storage operations
   - `insert`, `append`, `read`, `delete`
   - Full-text search with FTS5 BM25 scoring

3. **search** - Unified search across domains
   - Supports archival_memory, conversations, all
   - Domain-specific filters and limits

4. **send_message** - Agent communication
   - Routes through AgentMessageRouter
   - Supports CLI, Group, Discord, Queue endpoints

5. **shell** - Command execution via PTY
   - Operations: `execute`, `spawn`, `kill`, `status`
   - Implemented via the Phase 3 ProcessManager + LocalPtyBackend in pattern_runtime; per-session session context.
   - Permission validation via `CommandValidator` trait
   - Blocklist for dangerous commands (rm -rf /, etc.)
   - Three permission levels: `ReadOnly`, `ReadWrite`, `Admin`

### Implementation Notes
- Each tool has single entry point with operation enum
- Tool usage rules bundled with tools via `usage_rule()` trait
- ToolRegistry automatically provides rules to context builder
- Archival labels included in context for intelligent memory management

## Message and turn types

### Message attachments (`types/message.rs`)

Messages carry optional `attachments: Vec<MessageAttachment>` — pattern-level
metadata that renders onto the wire at compose-time but is NOT stored in the
`ChatMessage`. Keeps the conversational record clean while the wire still gets
ephemeral context reminders (memory snapshots). Attachments are only set on
batch-initiating user messages.

Key types:
- `MessageAttachment::BatchOpeningSnapshot { kind, block_names, blocks, edited_blocks }` —
  carries either a Full memory dump or a Delta since a prior batch.
- `SnapshotKind::Full | Delta { since_batch }` — determines rendering scope.
- `RenderedBlock { label, block_type, rendered: Option<Arc<str>>, content_hash }` —
  frozen snapshot of one memory block. `rendered=None` means "tracked but silent"
  (hash present for delta detection, content suppressed on wire).
- `SnapshotSelection { include_types, include_labels, exclude_labels }` —
  policy for which blocks appear in snapshots. Default: Core + Working.

### Turn types (`types/turn.rs`)

- `TurnInput` — one wire-level activation. First turn carries caller messages;
  subsequent turns use `TurnInput::continuation(batch_id, agent_id)` (empty
  messages — prior turn's tool_result lives in TurnHistory).
- `TurnOutput.messages` — full round-trip: `[assistant_msg]` on EndTurn,
  `[assistant_msg, tool_result_msg]` on ToolUse. The tool_result message is a
  `ChatRole::Tool` synthesised by `orchestrate` after dispatch.
- `TurnOutput.tool_results()` — accessor that reconstructs `Vec<ToolResult>`
  by walking the inlined tool_result message. NOT a stored field.
- `ToolResponse.content` is `serde_json::Value` (not String). The `new()`
  constructor wraps as `Value::String` for back-compat; `new_content()` accepts
  raw Value.
- `StepReply` aggregates N wire turns from one `Session::step`.

### Message router

- Each agent has its own router (not singleton).
- Database queuing provides natural buffering.
- Call chain prevents infinite loops.
- Anti-looping: 30-second cooldown between rapid messages.

### Endpoints
- **CliEndpoint**: Terminal output
- **GroupEndpoint**: Coordination pattern routing
- **DiscordEndpoint**: Discord integration
- **QueueEndpoint**: Database persistence (stub)
- **BlueskyEndpoint**: ATProto posting

## Architecture Overview

### Key Components

1. **Agent System** (`agent/`, `context/`)
   - Base `Agent` trait with memory and tool access
   - DatabaseAgent using `pattern-db`
   - AgentType enum with feature-gated ADHD variants

2. **Memory System** (`memory/` + `traits/memory_store.rs` + `types/memory_types/`)
   - `MemoryStore` trait: sync (no async), 19 methods (consolidated from 28).
   - `StructuredDocument` remains here (trait signature dependency).
   - Consolidation types: `BlockFilter`, `BlockMetadataPatch`, `UndoRedoOp`,
     `UndoRedoDepth`, `MemorySearchScope`, `IsolatePolicy`.
   - `IsolatePolicy` enum (`None`, `CoreOnly`, `Full`) governs scope routing
     between persona and project memory in `pattern_memory::scope`.
   - **Canonical implementation** (`MemoryCache`, `SharedBlockManager`) lives
     in `pattern_memory`. `pattern_core` must never depend on `pattern_memory`
     (enforced by trybuild compile-fail test).

3. **Tool System** (`tool/`)
   - Type-safe `AiTool<Input, Output>` trait
   - Dynamic dispatch via `DynamicTool`
   - Thread-safe `ToolRegistry` using DashMap

4. **Coordination** (`coordination/`)
   - All patterns implemented and working
   - Type-erased `Arc<dyn Agent>` for group flexibility
   - Message routing and response aggregation

5. **Database** (`../pattern_db`)
   - SQLite embedded databases

## Common Patterns

### Creating a Tool
```rust
#[derive(Debug, Clone)]
struct MyTool;

#[async_trait]
impl AiTool for MyTool {
    type Input = MyInput;   // Must impl JsonSchema + Deserialize + Serialize
    type Output = MyOutput; // Must impl JsonSchema + Serialize

    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does something useful" }

    async fn execute(&self, params: Self::Input) -> Result<Self::Output> {
        // Implementation
    }
}
```

### Error Handling
```rust
// Use specific error variants with context
return Err(CoreError::tool_not_found(name, available_tools));
return Err(CoreError::memory_not_found(&agent_id, &block_name, available_blocks));
```

### Notable RuntimeError variants (v3 foundation cycle)

- `RuntimeError::SharedBlockRefNotSupported` — persona TOML references a
  shared block ID at seed time; shared-block refs are rejected early with
  a clear diagnostic rather than silently failing downstream.
- `RuntimeError::CompactionInternalError` — wraps unexpected failures
  inside the compaction pipeline so they don't propagate as generic errors.

### BlockCreate and permission

`BlockCreate` gained a `permission: Option<MemoryPermission>` field with
a `with_permission()` builder. Persona TOML `permission = "read_only"`
now actually takes effect at block creation time, threaded through
`MemoryCache::create_block` and `InMemoryMemoryStore::create_block`.

### PersonaSnapshot — capability + policy fields (v3-multi-agent Phase 1)

`PersonaSnapshot.enabled_tools` and its `with_enabled_tools()` builder
were retired in earlier phases; capability control returned in
v3-multi-agent Phase 1 via two new `PersonaSnapshot` fields:

- `capabilities: Option<CapabilitySet>` — when `Some`, restricts which
  effects the agent's prelude exposes at compile time. `None` means
  "full power" (back-compat for personas that pre-date capability
  scoping). Threaded through `TidepoolSession::open_with_agent_loop`
  and into `pattern_runtime::sdk::preamble::build_for(caps)`.
- `policy_rules: Vec<PolicyRule>` — KDL-loaded policy rules carrying
  `Precedence::KdlConfig`. Layered over `pattern_runtime::policy::rust_defaults()`
  at session open via `merge_policies`.

KDL persona files now accept `capabilities { effects { ... } flags { ... } }`
and `policy { rule "name" effect="..." action="..." { matcher "..." pattern="..." } }`
blocks; `pattern_runtime::persona_loader` parses and converts them.

## Capability + permission system (v3-multi-agent Phase 1)

### `capability` module

Pure-data types backing the runtime's capability/policy machinery.
`pattern_core` defines the language; concrete enforcement (prelude
filtering, handler gating) lives in `pattern_runtime`.

- `CapabilitySet { categories: BTreeSet<EffectCategory>, flags: BTreeSet<CapabilityFlag> }`
  — an agent's permission scope. `CapabilitySet::all()` is the
  back-compat "full power" default.
- `EffectCategory` — `#[non_exhaustive]` enum aligned with
  `pattern_runtime::sdk::bundle::CANONICAL_EFFECT_ROW` (15 live SDK
  effects: `Memory, Search, Recall, Tasks, Skills, Message, Display,
  Time, Log, Shell, File, Mcp, Spawn, Diagnostics, Port`;
  `Sources` and `Rpc` removed in v3-sandbox-io Phase 4 and replaced
  by the unified `Port` effect; `Wake` is forward-reserved but not
  yet wired as an SDK effect row entry).
  `pattern_runtime` carries a `canonical_row_matches_effect_category_implemented_set`
  cross-check test to prevent drift.
- `CapabilityFlag` — orthogonal flags (`SpawnNewIdentities`,
  `WakeConditionRegistration`, `FrontingControl`) that gate runtime
  behaviours not mappable to a single effect category.
- `CapabilityError` — surfaces escalation attempts and missing
  category/flag denials.
- `CapabilityParseError` — `FromStr` errors for `EffectCategory` /
  `CapabilityFlag` (used by KDL parsing).

### `capability::policy` submodule

- `PolicyRule` — `{ effect, matcher, action, precedence }`. Construct
  via `PolicyRule::new(...)`; the struct is `#[non_exhaustive]`.
- `PolicyMatcher` — `Always | ShellCommand { pattern } | FilePath { pattern } | Scope(PermissionScope)`. Glob semantics: `*` and `?` only.
- `PolicyAction` — `Allow | RequireApproval { reason } | Deny { reason }`.
- `Precedence` — `RustDefault < KdlConfig < RuntimeOverride`. Higher
  weight wins ties.
- `PolicySet::evaluate(effect, &PolicyContext)` — returns the action
  of the highest-precedence matching rule; falls through to `Allow`
  when no rule matches (policy is opt-in; the broker is the gate of
  last resort).
- `PolicyContext<'a>` — runtime carrier passed to `evaluate`:
  `Shell { command }`, `FileWrite { path, content }`, `Generic`.

### `permission` module — per-runtime broker

Rebuilt in v3-multi-agent Phase 1. **No global singleton** — each
`TidepoolSession` constructs its own `PermissionBroker`. Key changes
from the pre-v3 shape:

- `PermissionGrant.expires_at: Option<jiff::Timestamp>` (was
  `chrono::DateTime`).
- `PermissionDecisionKind::ApproveForDuration(jiff::Span)` (was
  `std::time::Duration`).
- `PermissionScope` gained `FileWrite { path: String }` for
  path-granular file-write grants.
- Approve-for-scope and approve-for-duration caches keyed
  `(agent_id, scope)` — per-agent isolation is **load-bearing**;
  the broker's `request` argument list carries `agent_id` directly.
- Origin-aware request: `request(... origin: &MessageOrigin, ...)`.
  Partner-bypass predicate `MessageOrigin::bypasses_permission_gate()`
  short-circuits the broker when the *immediate dispatcher* is a
  Partner. Dispatch origin is set by `pattern_runtime::agent_loop::drive_step`
  to `Author::Agent(self)` per orchestrate iteration — so partner-bypass
  does NOT fire from autonomous agent activity even on Partner-
  activated turns. Only explicit direct-execution paths (none in
  Phase 1) override the slot to a Partner origin.
- Timeout cleanup: `pending` and `pending_info` maps are pruned on
  timeout; no leaks across many aborted requests.
- Injected clock: `PermissionBroker::with_clock(now_fn)` for
  deterministic duration-cache tests.

**Ephemerality contract**: grants live in RAM only via the broker's
`scope_cache`. There is no "load grants from disk" path. KDL holds
*rules* (declarative); grants stay session-scoped (imperative). Module
docstring spells this out as load-bearing for handler-level locked
invariants — see `pattern_runtime::sdk::handlers::file` for the
config-KDL shape guard that depends on this property.

### `MessageOrigin::bypasses_permission_gate()`

Predicate added to `types::origin::MessageOrigin` that returns `true`
for `Author::Partner(_)` and `false` for everyone else. The broker
calls it on the *immediate dispatcher* origin (read from
`SessionContext::current_dispatch_origin`), not the activating turn's
origin. See `pattern_runtime::CLAUDE.md` for the dispatch-origin
discipline that keeps this safe.

### Port trait (v3-sandbox-io Phase 4)

External-service ports use the unified `Port` trait at `traits/port.rs` —
one `id()`, one `metadata()`, one `subscribe()`, one `call()`, plus a
`library()` for optional Haskell wrapper code spliced into the agent's
prelude. Ports register with the runtime's `PortRegistry` (concrete impl
lives in `pattern_runtime`) at boot. Per-port capability gating via
`CapabilitySet::has_port(port_id)` filters which ports each agent sees
in `Pattern.Port.List` and which it can `Call`/`Subscribe`. See
`crates/pattern_runtime/CLAUDE.md` for the registry + dispatcher actor
implementation details.

## Identifier Types

All identifiers (`AgentId`, `MessageId`, `BatchId`, `TurnId`, etc.) are
[`smol_str::SmolStr`] type aliases defined in `types/ids.rs`. There is
no newtype ceremony and no compile-time distinction between kinds:
aliases exist only for signature readability.

Two minting functions:

- `new_id()` — 32-char unhyphenated UUID-v4 string. Use for unordered
  identifiers (agent IDs, tool-call IDs, session IDs).
- `new_snowflake_id()` — monotonic timestamp-based ID. Use for
  identifiers that must sort by creation time (`BatchId`, `TurnId`,
  message position keys). Thread-safe; blocks briefly only if the
  per-ms sequence counter is exhausted (65k/ms).

Convention: `BatchId` and `TurnId` use snowflakes; `MessageId` and
`AgentId` use UUIDs. The crate-root doctest teaches `new_snowflake_id`
for `TurnId`.

Rationale: the previous `define_id_type!` macro generated newtypes
with prefixed-UUID displays, `Display`/`FromStr`/`from_uuid`/`generate`
impls, and per-type validation errors. In practice nothing relied on
the type-level distinctness — DB query types enforced row shape,
serde tags handled wire-format discrimination, and the newtypes just
added ceremony. SmolStr is cheap to clone (Arc-sharing for >22 bytes)
and interop is straightforward.

## Performance Notes
- SmolStr inlines strings ≤ 22 bytes, shares via Arc beyond that
- CompactString (used for non-id string fields) inlines ≤ 24 bytes
- DashMap shards internally for concurrent access
- ToolContext via Arc<AgentRuntime> for cheap cloning
- Database operations are non-blocking with optimistic updates

## Embedding Providers
- **Candle (local)**: Pure Rust with Jina models (512/768 dims)
- **OpenAI**: text-embedding-3-small/large
- **Cohere**: embed-english-v3.0
- **Ollama**: Stub only - TODO

**Known Issues**: BERT models fail in Candle (dtype errors), use Jina models instead.

## Testing

### Test Utilities (`tool/builtin/test_utils.rs`)
Shared test infrastructure for tool testing:
- `MockToolContext`: Implements `ToolContext` for tool testing
- `MockToolContextBuilder`: Fluent builder for configurable test contexts
- `create_test_context_with_agent()`: Quick setup for simple tests
- `create_test_agent_in_db()`: Helper for FK constraint satisfaction

### Running Tests
```bash
# All pattern-core tests
cargo nextest run -p pattern-core

# Shell tool tests specifically
cargo nextest run -p pattern-core shell

# PTY tests may skip in CI (no PTY available)
```
