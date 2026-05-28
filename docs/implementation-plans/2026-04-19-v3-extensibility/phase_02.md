# v3-extensibility Phase 2: Hook lifecycle system

**Goal:** Open string-tag `HookEvent` struct + tag catalog of constants + per-tag payload types + glob-based `HookBus` dispatcher with two-tier (daemon + per-session) wiring. Wire emit calls at every documented hookable point in Pattern's existing subsystems. CC event alias map (28 entries, with variant-specific targets) for plugin-load-time translation.

**Architecture:** All hook surface lives in a new `pattern_core::hooks` module. `HookEvent` is a `#[non_exhaustive]` struct, NOT a closed enum — adding a new hook point doesn't change the trait. Tag strings are exposed as `pub const` items in `pattern_core::hooks::tags::*` so emit sites and subscribers reference the same identifiers; raw string literals are reserved for plugin-emitted custom tags. Tags are hierarchical (`<domain>.<event>` or `<domain>.<sub>.<event>` or `<domain>.<event>.<variant>`) so subscribers can use globset patterns to match broad (`task.*`) or narrow (`task.transitioned.done`) semantics. Per-tag payload structs carry typed data for known events; subscribers deserialize lazily via `HookEvent::try_payload::<T>()`. `HookSemantics::{Blocking, Notification}` is set by the emitter — not the subscriber — because the emitter knows whether it can wait. `HookBus` is a per-session actor-style dispatcher: subscribers register with a `HookFilter` (compiled glob) and a callback; emit walks filters and dispatches to matches. Blocking events `await` all subscriber responses with a per-event timeout; notification events fire-and-forget. CC alias mapping is applied at plugin-load time inside the CC adapter (Phase 3+) — the bus itself only knows pattern-native tags.

**Tech Stack:** `serde_json` for payload, `smol_str::SmolStr` for tags, `tokio` for async dispatch, **`globset` 0.4 as a new direct workspace dep** for tag glob matching, `parking_lot::RwLock` for filter registry.

**Scope:** 2 of 7 phases.

**Codebase verified:** 2026-04-27.

---

## Codebase verification findings

**~68 emit sites inventoried.** The list below is the authoritative reference for Phase 2 wiring tasks (5-7 group these by domain). Each `emit` call references a `tag` constant from Phase 2 Task 2's catalog. Notification semantics unless flagged `[blocking]`.

**Turn loop** — `crates/pattern_runtime/src/agent_loop.rs`:
- L189-250 `orchestrate` entry: emit `turn.before` [blocking]
- L242-244 before tool dispatch: emit `tool.before` [blocking]
- L244-251 after tool dispatch: emit `tool.after` (or `tool.failed` on error)
- L216-218 terminal stop: emit `turn.stop`
- L254-310 turn assembly complete: emit `turn.after.{success,failure}`

**SDK handlers** — `crates/pattern_runtime/src/sdk/handlers/*.rs`:
- `memory.rs:141-147` Memory.Get: emit `memory.read`
- `memory.rs:190-250` Memory.Put/Create/Append/Replace: emit `memory.write`
- `memory.rs:300+` Memory.GetShared: emit `memory.shared.read`
- `shell.rs:142-180` Execute (policy gate): emit `shell.execute.before` [blocking]
- `shell.rs:155-200` Execute return: emit `shell.execute.after`
- `shell.rs:210-230` Spawn: emit `shell.spawn`
- `shell.rs:240-260` Kill: emit `shell.kill`
- `tasks.rs:113-119` Create: emit `task.created`
- `tasks.rs:127-133` Transition: emit `task.transitioned.{done,in_progress,blocked,canceled}`
- `tasks.rs:141-147` Link: emit `task.linked`
- `tasks.rs:134-140` AddComment: emit `task.commented`
- `search.rs:72-150` SearchReq dispatch: emit `search.query`
- `recall.rs:73-120` Search: emit `recall.search`; `recall.rs:120-140` Insert: emit `recall.inserted`
- `file.rs:100-200` Open: emit `file.opened`; `:150-180` Read: emit `file.read`; `:100-120` Write (gate): emit `file.write` [blocking]; `:280-320` Watch: emit `file.watched`
- `port.rs:88-200` Call (dispatcher entry): emit `port.called` [blocking]; `:200-250` Call return: emit `port.call.after`; `:250-290` Subscribe: emit `port.subscribed`
- `spawn.rs:114-212` ephemeral: emit `spawn.ephemeral.start`; on exit `spawn.ephemeral.exit`
- `spawn.rs:564-650` sibling: emit `spawn.sibling.{existing,new.active,new.draft}`
- `spawn.rs:341-450` fork: emit `fork.spawned.{lightweight,persistent}`
- `spawn.rs:270-340` fork_op MergeBack: emit `fork.merged.{lightweight,persistent}`; Promote: `fork.promoted`; Discard: `fork.discarded.{lightweight,persistent}`
- `wake.rs:137-164` Register: emit `wake.condition.registered`
- `message.rs:74-180` Send: emit `message.sent`
- `skills.rs:435-480` Load: emit `skill.loaded`; on redaction: `skill.body_redacted`

**Compaction** — `crates/pattern_runtime/src/compaction.rs`:
- L91-150 gate evaluation: emit `compaction.cycle.start`
- L150-200 strategy dispatch: emit `compaction.strategy.fired`
- L200-250 post-strategy: emit `compaction.cycle.end`
- L200-220 after `rotate_session_uuid`: emit `provider.session.rotated`

**Persona / fronting** — `crates/pattern_runtime/src/{persona_loader,session,fronting_dispatch}.rs`:
- session open: emit `persona.attached`
- session drop: emit `persona.detached`
- `fronting_dispatch.rs:70-100` FrontingSet mutation: emit `fronting.rotated`

**Wake** — `crates/pattern_runtime/src/wake/`:
- evaluator fire: emit `wake.condition.fired`

**Mailbox** — `crates/pattern_runtime/src/mailbox.rs`:
- L100-130 push: emit `mailbox.enqueued` and `message.received`
- drain loop: emit `mailbox.drained`

**Permissions** — `crates/pattern_runtime/src/permission.rs:86-120`:
- request_sync entry: emit `permission.requested` [blocking]
- on grant: emit `permission.granted`
- on deny/timeout: emit `permission.denied`

**Agent registry / constellation** — `crates/pattern_runtime/src/agent_registry.rs` and `crates/pattern_runtime/src/sdk/handlers/constellation.rs`:
- L150-200 register_active/draft: emit `constellation.persona.registered`
- L200-250 promote (Draft→Active): emit `constellation.persona.promoted`
- constellation.rs:200+ relate command: emit `constellation.persona.related`

**File / process / port infrastructure**:
- `file_manager/manager.rs` DirWatcher external change: emit `file.external.edit`
- `file_manager/manager.rs` LoroSyncedFile conflict: emit `file.conflict`
- `process_manager/manager.rs` Spawn: emit `process.spawn`
- `process_manager/manager.rs` exit observation: emit `process.exit`
- `process_manager/manager.rs` Kill: emit `process.killed`
- `port_registry/registry.rs` register: emit `port.registered`
- `port_registry/registry.rs` unregister: emit `port.unregistered`
- `port_registry/dispatcher.rs` event drain: emit `port.event`

**Provider** — `crates/pattern_provider/src/streaming.rs`:
- stream open: emit `provider.stream.start`
- stream close: emit `provider.stream.end`
- usage event: emit `provider.tokens.reported`

**Server mounts (daemon-scoped bus)** — `crates/pattern_server/src/server.rs`:
- `get_or_mount_project`: emit `mount.opened`
- ProjectMount drop: emit `mount.closed`

Full count: **~68 emit sites** across 14 subsystems. Tasks 5-7 group these by domain for efficient batched wiring.

**Pre-existing infrastructure verified:**
- ✓ `pattern_core::traits` and `pattern_core::types` are established home directories for cross-cutting trait + type modules; `pattern_core::hooks` parallels.
- ✓ `tokio::sync::mpsc` + `parking_lot::RwLock` are the standard concurrency primitives.
- ✓ `globset 0.4` transitively present via `notify`. Adding direct workspace pin in Task 1.
- ✓ Each SDK handler today follows a uniform shape (read context, gate, dispatch, return). Emit-sites slot in cleanly at gate-pre and post-dispatch points.
- ✓ `compaction.rs:91-250` has clear cycle-start/cycle-end seams.
- ✓ `pattern_server::server` already maintains a per-mount `ProjectMount` shared across sessions; bus instance attaches there for daemon-level events.

---

## Acceptance Criteria Coverage

This phase implements and tests:

### v3-extensibility.AC4: Hook lifecycle

- **v3-extensibility.AC4.1 Success:** `turn.before` hook fires before turn processing begins; hook can return a modification (e.g., prepend content) that affects the turn
- **v3-extensibility.AC4.2 Success:** `tool.before` hook fires before tool dispatch; hook can return `HookResponse::Block` to prevent tool execution
- **v3-extensibility.AC4.3 Success:** `memory.write` hook fires after a memory write completes; hook receives block handle and change summary; return value ignored (notification)
- **v3-extensibility.AC4.4 Success:** CC alias mapping: hook registered as `PreToolUse` fires on `tool.before` events; hook registered as `SessionStart` fires on `persona.attach`
- **v3-extensibility.AC4.5 Failure:** Hook execution exceeds timeout; hook treated as returning no response; event proceeds; warning logged
- **v3-extensibility.AC4.6 Failure:** Blocking hook attempts to call an effect not in the runtime's capability set; hook's effect denied (hooks respect capability gates)
- **v3-extensibility.AC4.7 Edge:** Multiple hooks registered for the same event fire in registration order; all complete before event proceeds (blocking) or all fire independently (notification)

---

## Tasks

<!-- START_SUBCOMPONENT_A (tasks 1-4) -->

<!-- START_TASK_1 -->
### Task 1: `pattern_core::hooks` core types

**Verifies:** None (infrastructure).

**Files:**
- Create: `crates/pattern_core/src/hooks.rs` (module root: re-exports + submodule declarations).
- Create: `crates/pattern_core/src/hooks/event.rs` (`HookEvent`, `HookEventMetadata`, `HookSemantics`, `HookResponse`).
- Create: `crates/pattern_core/src/hooks/filter.rs` (`HookFilter`, glob compilation).
- Modify: `crates/pattern_core/src/lib.rs` — `pub mod hooks;` declaration.
- Modify: workspace `Cargo.toml` — add `globset = "0.4"` to `[workspace.dependencies]`.
- Modify: `crates/pattern_core/Cargo.toml` — add `globset = { workspace = true }`.

**Implementation:**

`hooks.rs` re-exports the public surface:

```rust
//! Hook event lifecycle system.
//!
//! Open string-tag dispatch — adding a hook point is non-breaking.
//! See `tags::*` for the catalog of well-known events emitted by Pattern itself.

pub mod cc_aliases;
pub mod event;
pub mod filter;
pub mod tags;
pub mod payloads;

pub use event::{HookEvent, HookEventMetadata, HookResponse, HookSemantics};
pub use filter::{HookFilter, HookFilterError};
```

`event.rs` defines the central types:

```rust
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookEvent {
    pub tag: SmolStr,
    pub payload: serde_json::Value,
    pub metadata: HookEventMetadata,
    pub semantics: HookSemantics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HookEventMetadata {
    pub session_id: Option<SmolStr>,
    pub agent_id: Option<SmolStr>,
    pub batch_id: Option<SmolStr>,
    pub partner_id: Option<SmolStr>,
    pub mount_id: Option<SmolStr>,
    pub origin_author_kind: Option<SmolStr>, // "Partner" | "Agent" | "System" | "Plugin"
    pub emitted_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HookSemantics {
    Blocking,
    Notification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HookResponse {
    Continue,
    Block { reason: SmolStr },
    Modify(serde_json::Value),
}

impl HookEvent {
    /// Lazy typed-payload deserialization. Returns `Err` if the payload doesn't
    /// match the requested type. Subscribers should match on `event.tag` first
    /// before deserializing — wrong-tag-and-type pairs are a logic error.
    pub fn try_payload<'de, T: Deserialize<'de>>(&'de self) -> Result<T, serde_json::Error> {
        T::deserialize(&self.payload)
    }
}
```

`filter.rs` wraps `globset::Glob`:

```rust
use globset::{Glob, GlobMatcher};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct HookFilter {
    pattern: String,
    matcher: GlobMatcher,
}

impl HookFilter {
    /// Build a filter from a tag glob pattern. Examples:
    /// - `"turn.before"` (literal)
    /// - `"task.*"` (any task event)
    /// - `"task.transitioned.*"` (any task transition)
    /// - `"**"` (firehose; matches every event)
    pub fn new(pattern: impl Into<String>) -> Result<Self, HookFilterError> { /* compile glob */ }

    pub fn matches(&self, tag: &str) -> bool {
        self.matcher.is_match(tag)
    }

    pub fn pattern(&self) -> &str { &self.pattern }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HookFilterError {
    #[error("invalid hook filter pattern {pattern:?}: {source}")]
    InvalidGlob { pattern: String, #[source] source: globset::Error },
}
```

**Verification:**
Run: `cargo check -p pattern-core`
Expected: clean build.

**Commit:** `[meta] [pattern-core] add hooks module — HookEvent + HookFilter types + globset dep`
<!-- END_TASK_1 -->

<!-- START_TASK_2 -->
### Task 2: Tag catalog and per-tag payload structs

**Verifies:** None (infrastructure for emit + subscribe sites).

**Files:**
- Create: `crates/pattern_core/src/hooks/tags.rs` (catalog constants).
- Create: `crates/pattern_core/src/hooks/payloads.rs` (per-tag payload structs).

**Implementation:**

`tags.rs` exposes every tag Pattern emits as a `pub const &str`. Grouped by domain:

```rust
//! Catalog of well-known hook event tags.
//!
//! Plugins emit custom tags as raw strings under their plugin-id namespace
//! (`<plugin-id>.<event>`). Pattern internals use the constants here so emit
//! and subscribe sites stay in sync.

// Turn / tool dispatch
pub const TURN_BEFORE: &str = "turn.before";
pub const TURN_AFTER_SUCCESS: &str = "turn.after.success";
pub const TURN_AFTER_FAILURE: &str = "turn.after.failure";
pub const TURN_STOP: &str = "turn.stop";
pub const TOOL_BEFORE: &str = "tool.before";
pub const TOOL_AFTER: &str = "tool.after";
pub const TOOL_FAILED: &str = "tool.failed";
pub const TOOL_BATCH_AFTER: &str = "tool.batch.after";

// Memory
pub const MEMORY_READ: &str = "memory.read";
pub const MEMORY_WRITE: &str = "memory.write";
pub const MEMORY_SHARED_READ: &str = "memory.shared.read";
pub const MEMORY_BLOCK_COMMITTED: &str = "memory.block.committed";
pub const MEMORY_EXTERNAL_EDIT: &str = "memory.external.edit";
pub const MEMORY_CONFLICT: &str = "memory.conflict";

// File / shell / port
pub const FILE_OPENED: &str = "file.opened";
pub const FILE_READ: &str = "file.read";
pub const FILE_WRITE: &str = "file.write";
pub const FILE_WATCHED: &str = "file.watched";
pub const FILE_EXTERNAL_EDIT: &str = "file.external.edit";
pub const FILE_CONFLICT: &str = "file.conflict";
pub const SHELL_EXECUTE_BEFORE: &str = "shell.execute.before";
pub const SHELL_EXECUTE_AFTER: &str = "shell.execute.after";
pub const SHELL_SPAWN: &str = "shell.spawn";
pub const SHELL_KILL: &str = "shell.kill";
pub const SHELL_EXIT: &str = "shell.exit";
pub const PROCESS_SPAWN: &str = "process.spawn";
pub const PROCESS_EXIT: &str = "process.exit";
pub const PROCESS_KILLED: &str = "process.killed";
pub const PORT_CALLED: &str = "port.called";
pub const PORT_CALL_AFTER: &str = "port.call.after";
pub const PORT_SUBSCRIBED: &str = "port.subscribed";
pub const PORT_EVENT: &str = "port.event";
pub const PORT_REGISTERED: &str = "port.registered";
pub const PORT_UNREGISTERED: &str = "port.unregistered";

// Tasks / search / recall / skills / message
pub const TASK_CREATED: &str = "task.created";
pub const TASK_TRANSITIONED: &str = "task.transitioned";
pub const TASK_TRANSITIONED_DONE: &str = "task.transitioned.done";
pub const TASK_TRANSITIONED_IN_PROGRESS: &str = "task.transitioned.in_progress";
pub const TASK_TRANSITIONED_BLOCKED: &str = "task.transitioned.blocked";
pub const TASK_TRANSITIONED_CANCELED: &str = "task.transitioned.canceled";
pub const TASK_LINKED: &str = "task.linked";
pub const TASK_COMMENTED: &str = "task.commented";
pub const SEARCH_QUERY: &str = "search.query";
pub const RECALL_SEARCH: &str = "recall.search";
pub const RECALL_INSERTED: &str = "recall.inserted";
pub const SKILL_LOADED: &str = "skill.loaded";
pub const SKILL_BODY_REDACTED: &str = "skill.body_redacted";
pub const MESSAGE_SENT: &str = "message.sent";
pub const MESSAGE_RECEIVED: &str = "message.received";

// Spawn / fork
pub const SPAWN_EPHEMERAL_START: &str = "spawn.ephemeral.start";
pub const SPAWN_EPHEMERAL_EXIT: &str = "spawn.ephemeral.exit";
pub const SPAWN_SIBLING_EXISTING: &str = "spawn.sibling.existing";
pub const SPAWN_SIBLING_NEW_ACTIVE: &str = "spawn.sibling.new.active";
pub const SPAWN_SIBLING_NEW_DRAFT: &str = "spawn.sibling.new.draft";
pub const FORK_SPAWNED_LIGHTWEIGHT: &str = "fork.spawned.lightweight";
pub const FORK_SPAWNED_PERSISTENT: &str = "fork.spawned.persistent";
pub const FORK_MERGED_LIGHTWEIGHT: &str = "fork.merged.lightweight";
pub const FORK_MERGED_PERSISTENT: &str = "fork.merged.persistent";
pub const FORK_PROMOTED: &str = "fork.promoted";
pub const FORK_DISCARDED_LIGHTWEIGHT: &str = "fork.discarded.lightweight";
pub const FORK_DISCARDED_PERSISTENT: &str = "fork.discarded.persistent";

// Wake / mailbox
pub const WAKE_CONDITION_REGISTERED: &str = "wake.condition.registered";
pub const WAKE_CONDITION_FIRED: &str = "wake.condition.fired";
pub const MAILBOX_ENQUEUED: &str = "mailbox.enqueued";
pub const MAILBOX_DRAINED: &str = "mailbox.drained";

// Permissions
pub const PERMISSION_REQUESTED: &str = "permission.requested";
pub const PERMISSION_GRANTED: &str = "permission.granted";
pub const PERMISSION_DENIED: &str = "permission.denied";

// Persona / fronting / constellation
pub const PERSONA_ATTACHED: &str = "persona.attached";
pub const PERSONA_DETACHED: &str = "persona.detached";
pub const FRONTING_ROTATED: &str = "fronting.rotated";
pub const CONSTELLATION_PERSONA_REGISTERED: &str = "constellation.persona.registered";
pub const CONSTELLATION_PERSONA_PROMOTED: &str = "constellation.persona.promoted";
pub const CONSTELLATION_PERSONA_RELATED: &str = "constellation.persona.related";

// Compaction / provider
pub const COMPACTION_CYCLE_START: &str = "compaction.cycle.start";
pub const COMPACTION_STRATEGY_FIRED: &str = "compaction.strategy.fired";
pub const COMPACTION_CYCLE_END: &str = "compaction.cycle.end";
pub const PROVIDER_SESSION_ROTATED: &str = "provider.session.rotated";
pub const PROVIDER_STREAM_START: &str = "provider.stream.start";
pub const PROVIDER_STREAM_END: &str = "provider.stream.end";
pub const PROVIDER_TOKENS_REPORTED: &str = "provider.tokens.reported";

// Plugins / mounts / instructions / config
pub const PLUGIN_INSTALL: &str = "plugin.install";
pub const PLUGIN_UNINSTALL: &str = "plugin.uninstall";
pub const PLUGIN_ENABLE: &str = "plugin.enable";
pub const PLUGIN_DISABLE: &str = "plugin.disable";
pub const MOUNT_OPENED: &str = "mount.opened";
pub const MOUNT_CLOSED: &str = "mount.closed";
pub const INSTRUCTIONS_LOADED: &str = "instructions.loaded";
pub const CONFIG_CHANGED: &str = "config.changed";
pub const CWD_CHANGED: &str = "cwd.changed";

// Misc / CC-mapped
pub const PROMPT_EXPANSION: &str = "prompt.expansion";
pub const NOTIFICATION_EMIT: &str = "notification.emit";
pub const AGENT_IDLE: &str = "agent.idle";
pub const MCP_ELICITATION: &str = "mcp.elicitation";
pub const MCP_ELICITATION_RESULT: &str = "mcp.elicitation.result";
```

`payloads.rs` defines a typed payload struct per tag. Each derives `Serialize + Deserialize + Debug + Clone`. Examples:

```rust
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TurnBeforePayload {
    pub batch_id: SmolStr,
    pub model_id: SmolStr,
    pub message_count: u32,
    pub estimated_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolBeforePayload {
    pub tool_call_id: SmolStr,
    pub tool_name: SmolStr,
    pub arguments_summary: String, // truncated/redacted JSON snippet
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MemoryWritePayload {
    pub block_label: SmolStr,
    pub scope: MemoryScopeWire,
    pub write_kind: MemoryWriteKind, // Create | Replace | Append | Delete
    pub content_hash_before: Option<[u8; 8]>,
    pub content_hash_after: [u8; 8],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MemoryWriteKind { Create, Replace, Append, Delete }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MemoryScopeWire { Persona, Project, Shared }

// ... payload struct per tag, ~30 total
```

Define a payload struct for each tag from the catalog. The full enumeration is mechanical; task-implementor walks the catalog list and produces one struct per tag, drawing field shapes from the existing context types in each subsystem.

Add a `pub fn payload_type_name(tag: &str) -> Option<&'static str>` helper for diagnostics:

```rust
pub fn payload_type_name(tag: &str) -> Option<&'static str> {
    match tag {
        tags::TURN_BEFORE => Some("TurnBeforePayload"),
        tags::TOOL_BEFORE => Some("ToolBeforePayload"),
        tags::MEMORY_WRITE => Some("MemoryWritePayload"),
        // ... full table
        _ => None,
    }
}
```

**Testing:**
Tests must verify the catalog → payload mapping is consistent: write a unit test that walks every catalog constant, calls `payload_type_name`, asserts `Some(_)` (catches typos / missed entries during catalog edits).

**Verification:**
Run: `cargo check -p pattern-core` and `cargo nextest run -p pattern-core hooks::tags`
Expected: builds clean; consistency test passes.

**Commit:** `[pattern-core] add hook tag catalog + per-tag payload structs`
<!-- END_TASK_2 -->

<!-- START_TASK_3 -->
### Task 3: CC alias map (28 events → Pattern tags, with variant targets)

**Verifies:** AC4.4 (foundation; full integration in Task 8).

**Files:**
- Create: `crates/pattern_core/src/hooks/cc_aliases.rs`.

**Implementation:**

```rust
//! Translation table from CC plugin hook event names to Pattern hook tags.
//!
//! Applied at plugin load time by the CC adapter (Phase 3+) — the bus itself
//! only knows pattern-native tags. Variant-specific entries (e.g., `TaskCompleted`
//! → `task.transitioned.done`) leverage Pattern's hierarchical tag namespacing
//! so CC subscribers receive only the events that match the original CC semantics.

use crate::hooks::tags;

pub const ALIASES: &[(&str, &str)] = &[
    ("PreToolUse",          tags::TOOL_BEFORE),
    ("PostToolUse",         tags::TOOL_AFTER),
    ("PostToolUseFailure",  tags::TOOL_FAILED),
    ("PostToolBatch",       tags::TOOL_BATCH_AFTER),
    ("PermissionRequest",   tags::PERMISSION_REQUESTED),
    ("PermissionDenied",    tags::PERMISSION_DENIED),
    ("UserPromptSubmit",    tags::TURN_BEFORE),
    ("UserPromptExpansion", tags::PROMPT_EXPANSION),
    ("SessionStart",        tags::PERSONA_ATTACHED),
    ("SessionEnd",          tags::PERSONA_DETACHED),
    ("Stop",                tags::TURN_AFTER_SUCCESS),
    ("StopFailure",         tags::TURN_AFTER_FAILURE),
    ("SubagentStart",       tags::SPAWN_EPHEMERAL_START),
    ("SubagentStop",        tags::SPAWN_EPHEMERAL_EXIT),
    ("TaskCreated",         tags::TASK_CREATED),
    ("TaskCompleted",       tags::TASK_TRANSITIONED_DONE),
    ("Notification",        tags::NOTIFICATION_EMIT),
    ("TeammateIdle",        tags::AGENT_IDLE),
    ("InstructionsLoaded",  tags::INSTRUCTIONS_LOADED),
    ("ConfigChange",        tags::CONFIG_CHANGED),
    ("CwdChanged",          tags::CWD_CHANGED),
    ("FileChanged",         tags::FILE_EXTERNAL_EDIT),
    ("WorktreeCreate",      tags::FORK_SPAWNED_PERSISTENT),
    ("WorktreeRemove",      tags::FORK_DISCARDED_PERSISTENT),
    ("PreCompact",          tags::COMPACTION_CYCLE_START),
    ("PostCompact",         tags::COMPACTION_CYCLE_END),
    ("Elicitation",         tags::MCP_ELICITATION),
    ("ElicitationResult",   tags::MCP_ELICITATION_RESULT),
];

/// Translate a CC event name to a Pattern tag, if recognized.
pub fn translate_cc(cc_name: &str) -> Option<&'static str> {
    ALIASES.iter().copied().find_map(|(cc, pat)| (cc == cc_name).then_some(pat))
}
```

**Testing:**
- Round-trip test for every alias: walk the table, call `translate_cc`, assert result matches the right-hand side.
- Negative test: `translate_cc("UnknownCcEvent")` returns `None`.
- Coverage check: all 28 documented CC events appear in the table.

**Verification:**
Run: `cargo nextest run -p pattern-core hooks::cc_aliases`
Expected: passes.

**Commit:** `[pattern-core] add CC hook event alias table`
<!-- END_TASK_3 -->

<!-- START_TASK_4 -->
### Task 4: `HookBus` dispatcher with glob-based subscription registry

**Verifies:** AC4.5, AC4.7, plus AC4.1/AC4.2/AC4.3 dispatch foundation (consumed by emit-site tasks 5-7).

**Files:**
- Create: `crates/pattern_core/src/hooks/bus.rs`.
- Modify: `crates/pattern_core/src/hooks.rs` — declare `pub mod bus;` and re-export.

**Implementation:**

`HookBus` lives in `pattern_core` (trait-only-ish — it carries a small executor, but per project guidance "sufficiently shared/fundamental code can live in core" — see updated CLAUDE.md). Parking-lot-locked filter registry, mpsc-channel subscribers.

```rust
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tracing::{warn, debug};

use super::event::{HookEvent, HookResponse, HookSemantics};
use super::filter::HookFilter;

pub type SubscriptionId = u64;

#[derive(Debug)]
pub struct HookBus {
    inner: Arc<RwLock<BusInner>>,
    blocking_timeout: Duration,
}

#[derive(Debug, Default)]
struct BusInner {
    next_id: SubscriptionId,
    subs: Vec<Subscription>, // Vec preserves registration order — AC4.7.
}

#[derive(Debug)]
struct Subscription {
    id: SubscriptionId,
    filter: HookFilter,
    sender: SubscriberSender,
}

#[derive(Debug)]
enum SubscriberSender {
    Blocking { tx: mpsc::Sender<BlockingDelivery> },
    Notification { tx: mpsc::Sender<HookEvent> },
}

#[derive(Debug)]
struct BlockingDelivery {
    event: HookEvent,
    reply: oneshot::Sender<HookResponse>,
}

impl HookBus {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(5))
    }

    pub fn with_timeout(blocking_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BusInner::default())),
            blocking_timeout,
        }
    }

    /// Subscribe to events matching `filter`. The returned receiver delivers
    /// blocking deliveries (with reply channel) for blocking events; a separate
    /// `subscribe_notifications` is used for notification-only subscribers.
    pub fn subscribe_blocking(&self, filter: HookFilter)
        -> (SubscriptionId, mpsc::Receiver<BlockingDelivery>) { /* ... */ }

    pub fn subscribe_notifications(&self, filter: HookFilter)
        -> (SubscriptionId, mpsc::Receiver<HookEvent>) { /* ... */ }

    pub fn unsubscribe(&self, id: SubscriptionId) -> bool { /* remove from Vec; returns whether found */ }

    /// Emit a notification event. Walks filters in registration order, fires
    /// `try_send` to each match. Slow subscribers that fill their buffer drop
    /// silently — debug-log on drop.
    pub fn emit(&self, event: HookEvent) {
        debug_assert_eq!(event.semantics, HookSemantics::Notification);
        let inner = self.inner.read();
        for sub in &inner.subs {
            if !sub.filter.matches(&event.tag) { continue; }
            if let SubscriberSender::Notification { tx } = &sub.sender {
                if tx.try_send(event.clone()).is_err() {
                    debug!(sub_id = sub.id, tag = %event.tag, "notification subscriber drop (buffer full or closed)");
                }
            }
        }
    }

    /// Emit a blocking event. Walks filters in registration order. For each match,
    /// sends through the blocking channel and `await`s the response with timeout.
    /// Returns the first `HookResponse::Block` if any subscriber blocks; otherwise
    /// the last `HookResponse::Modify(...)` if any modifies; otherwise `Continue`.
    pub async fn emit_blocking(&self, event: HookEvent) -> HookResponse {
        debug_assert_eq!(event.semantics, HookSemantics::Blocking);
        let mut response = HookResponse::Continue;
        let subs_snapshot: Vec<_> = self.inner.read().subs.iter()
            .filter_map(|s| match &s.sender {
                SubscriberSender::Blocking { tx } if s.filter.matches(&event.tag) =>
                    Some((s.id, tx.clone())),
                _ => None,
            })
            .collect();

        for (sub_id, tx) in subs_snapshot {
            let (reply_tx, reply_rx) = oneshot::channel();
            let delivery = BlockingDelivery { event: event.clone(), reply: reply_tx };
            if tx.send(delivery).await.is_err() { continue; } // subscriber dropped

            match timeout(self.blocking_timeout, reply_rx).await {
                Ok(Ok(HookResponse::Block { reason })) => return HookResponse::Block { reason },
                Ok(Ok(HookResponse::Modify(v))) => { response = HookResponse::Modify(v); }
                Ok(Ok(HookResponse::Continue)) => {}
                Ok(Err(_)) => warn!(sub_id, tag = %event.tag, "blocking subscriber dropped reply channel"),
                Err(_)     => warn!(sub_id, tag = %event.tag, timeout_ms = ?self.blocking_timeout.as_millis(), "blocking hook timed out; proceeding"),
            }
        }
        response
    }
}
```

**Testing:**

Tests must verify each AC listed above:
- AC4.5: Register a blocking subscriber that never replies; emit blocking event; assert it returns `HookResponse::Continue` after the configured timeout; assert a `tracing::warn` line was produced.
- AC4.7 blocking: Register two blocking subscribers; have them record the order in which they receive deliveries; assert it matches registration order.
- AC4.7 notification: Same as above for notification path.
- Subscribe / unsubscribe round-trip: emit, observe delivery; unsubscribe; emit again, observe no delivery to that subscriber.
- Glob match: register `task.*`; emit `task.transitioned.done`; assert delivery. Register `task.transitioned.done` exact; emit `task.transitioned.in_progress`; assert NO delivery to that subscriber.

**Verification:**
Run: `cargo nextest run -p pattern-core hooks::bus`
Expected: all bus tests pass.

**Commit:** `[pattern-core] add HookBus dispatcher with glob filters and blocking timeout`
<!-- END_TASK_4 -->

<!-- END_SUBCOMPONENT_A -->

<!-- START_SUBCOMPONENT_B (tasks 5-7) -->

<!-- START_TASK_5 -->
### Task 5: Wire emit calls — turn loop and SDK handlers

**Verifies:** AC4.1, AC4.2, AC4.3 (emit foundation; subscribers tested in Task 8).

**Files:** ~30 modify-targets. The full inventory lives in the scratchpad; key entry points:
- Modify: `crates/pattern_runtime/src/agent_loop.rs` — emit `turn.before`, `turn.after.{success,failure}`, `turn.stop`, `tool.before`, `tool.after`, `tool.failed` at the documented sites.
- Modify: `crates/pattern_runtime/src/sdk/handlers/memory.rs` — emit `memory.read`, `memory.write`, `memory.shared.read`.
- Modify: `crates/pattern_runtime/src/sdk/handlers/file.rs` — emit `file.opened`, `file.read`, `file.write`, `file.watched`.
- Modify: `crates/pattern_runtime/src/sdk/handlers/shell.rs` — emit `shell.execute.before/after`, `shell.spawn`, `shell.kill`, `shell.exit`.
- Modify: `crates/pattern_runtime/src/sdk/handlers/port.rs` — emit `port.called`, `port.call.after`, `port.subscribed`, `port.event` (port.event is also fired from registry; emit handler-side for caller-visible variant).
- Modify: `crates/pattern_runtime/src/sdk/handlers/tasks.rs` — emit `task.created`, `task.transitioned.{done,in_progress,blocked,canceled}`, `task.linked`, `task.commented`.
- Modify: `crates/pattern_runtime/src/sdk/handlers/search.rs`, `recall.rs`, `skills.rs`, `message.rs`, `spawn.rs`, `wake.rs` — corresponding emits.

**Implementation:**

Add a `HookBus` field on `SessionContext` (gated behind `Option` only during transition; default to a session-owned instance in the standard `open_with_agent_loop`). Trait `HasHookBus { fn hook_bus() -> &Arc<HookBus> }` implemented for `SessionContext` (returns the live bus) and `()` (returns a global empty bus that drops every emit). `()` keeps the test-double path closed-by-default.

Each emit call is a small inline helper:

```rust
// Pattern at every emit site:
let bus = cx.user().hook_bus();
let payload = serde_json::to_value(MemoryWritePayload {
    block_label: block.label.clone(),
    scope: scope.into_wire(),
    write_kind: MemoryWriteKind::Replace,
    content_hash_before: prev_hash,
    content_hash_after: new_hash,
})?;
bus.emit(HookEvent {
    tag: tags::MEMORY_WRITE.into(),
    payload,
    metadata: build_metadata(cx),
    semantics: HookSemantics::Notification,
});
```

For blocking events (`turn.before`, `tool.before`, `shell.execute.before`, `permission.requested`, `file.write` (blocking due to policy)), use `emit_blocking` and act on the response:

```rust
match bus.emit_blocking(event).await {
    HookResponse::Block { reason } => return Err(EffectError::Handler(format!("blocked by hook: {}", reason))),
    HookResponse::Modify(payload_modification) => { /* fold into turn input */ }
    HookResponse::Continue => {}
}
```

A `pub(crate) fn build_metadata(cx: &EffectContext<'_, SessionContext>) -> HookEventMetadata` helper extracts session_id, agent_id, batch_id, partner_id, mount_id, origin_author_kind from context. Lives in `pattern_runtime::hooks::metadata`.

**Testing:**
End-to-end coverage of these emit sites comes from Task 8's integration suite, which subscribes a test tap and emits-then-observes events at each site. Per-site unit-test coverage is not required at this granularity — the integration tests cover the contract.

For safety: a single representative unit test per handler asserting "emit fires when expected, doesn't fire when handler error-paths short-circuit" is the minimum bar.

**Verification:**
Run: `cargo check --workspace` (must build); `cargo nextest run -p pattern-runtime` (existing tests still pass).

**Commit:** `[pattern-runtime] wire hook emit calls in turn loop + SDK handlers`
<!-- END_TASK_5 -->

<!-- START_TASK_6 -->
### Task 6: Wire emit calls — cross-cutting subsystems

**Verifies:** AC4-foundation continued.

**Files:**
- Modify: `crates/pattern_runtime/src/compaction.rs` — emit cycle.start, strategy.fired, cycle.end, provider.session.rotated.
- Modify: `crates/pattern_runtime/src/persona_loader.rs` and `crates/pattern_runtime/src/session.rs` — emit persona.attached, persona.detached.
- Modify: `crates/pattern_runtime/src/fronting_dispatch.rs` — emit fronting.rotated.
- Modify: `crates/pattern_runtime/src/mailbox.rs` — emit mailbox.enqueued, mailbox.drained.
- Modify: `crates/pattern_runtime/src/permission.rs` — emit permission.requested (blocking), permission.granted, permission.denied.
- Modify: `crates/pattern_runtime/src/agent_registry.rs` — emit constellation.persona.registered, constellation.persona.promoted.
- Modify: `crates/pattern_runtime/src/sdk/handlers/constellation.rs` — emit constellation.persona.related (uses Phase 6 of v3-multi-agent's relate handler).

**Implementation:**

Same emit-helper pattern as Task 5. Permission broker is the only blocking site in this batch — everything else is notification.

For persona attach/detach in `session.rs`, the bus instance is constructed during session open and destroyed in the session's drop path. A `Drop` impl on `SessionContext` emits `persona.detached` synchronously via `emit` (notification — fire-and-forget, no async needed in Drop).

**Testing:** Integration coverage in Task 8.

**Verification:**
Run: `cargo nextest run --workspace` — all existing tests pass.

**Commit:** `[pattern-runtime] wire hook emit calls in cross-cutting subsystems`
<!-- END_TASK_6 -->

<!-- START_TASK_7 -->
### Task 7: Wire emit calls — infrastructure layers

**Verifies:** AC4-foundation continued.

**Files:**
- Modify: `crates/pattern_runtime/src/file_manager/manager.rs` — emit file.external.edit, file.conflict.
- Modify: `crates/pattern_runtime/src/process_manager/manager.rs` — emit process.spawn, process.exit, process.killed.
- Modify: `crates/pattern_runtime/src/port_registry/registry.rs` — emit port.registered, port.unregistered.
- Modify: `crates/pattern_runtime/src/port_registry/dispatcher.rs` — emit port.event (the actual subscription bridge).
- Modify: `crates/pattern_runtime/src/wake/registry.rs` and `crates/pattern_runtime/src/wake/custom.rs` — emit wake.condition.registered, wake.condition.fired.
- Modify: `crates/pattern_provider/src/streaming.rs` (or the canonical streaming entrypoint) — emit provider.stream.start, provider.stream.end, provider.tokens.reported. (`provider.session.rotated` already covered in Task 6 from compaction.)
- Modify: `crates/pattern_server/src/server.rs` — emit mount.opened, mount.closed at `get_or_mount_project` and ProjectMount drop sites.

**Implementation:**

The daemon-level mount events use a daemon-scoped `HookBus` instance held by `DaemonServer`. Per-session bus instances forward events tagged with `mount.*` upward to the daemon bus via a one-way `tokio::sync::mpsc` to keep cross-session telemetry coherent. This forwarding edge is implemented as a default subscriber on each per-session bus that re-emits matching tags onto the daemon bus.

`pattern_provider` doesn't currently depend on `pattern_core`'s hook module; verify the dep chain is clean. If it isn't, the provider emits via a callback closure passed into the request runtime, not by direct `HookBus` reference. Task-implementor picks the cleaner shape based on what compiles.

**Testing:** Integration coverage in Task 8.

**Verification:**
Run: `cargo nextest run --workspace` — all existing tests pass; mount.opened/closed observable from server-level subscriber in Task 8 tests.

**Commit:** `[pattern-runtime] [pattern-server] [pattern-provider] wire hook emit calls in infrastructure layers`
<!-- END_TASK_7 -->

<!-- END_SUBCOMPONENT_B -->

<!-- START_SUBCOMPONENT_C (task 8) -->

<!-- START_TASK_8 -->
### Task 8: `pattern_server` bus integration + telemetry tap + integration tests

**Verifies:** AC4.1, AC4.2, AC4.3, AC4.4, AC4.5, AC4.6, AC4.7 (all end-to-end).

**Files:**
- Modify: `crates/pattern_server/src/server.rs` — daemon-level `HookBus` field on `DaemonServer`; per-session bus wired via `SessionContext::with_hook_bus`.
- Create: `crates/pattern_server/src/hook_telemetry.rs` — debug-level telemetry subscriber. Logs each event at `debug!(target: "pattern.hooks", ...)`; provides feature-gated diagnostic dump on `HookBus::dump()` for tests and operators.
- Create: `crates/pattern_runtime/tests/hook_lifecycle.rs` — integration tests covering AC4.1-AC4.7.

**Implementation:**

Server integration: at `get_or_open_session`, construct a `HookBus`, wire into `SessionContext`, register a default telemetry subscriber that logs every event at debug. The daemon-level bus also gets a default subscriber that fans out `mount.*` events to all currently-connected TUI clients via the existing `WireTurnEvent` channel (using a new `WireTurnEvent::HookEvent { tag, payload_json }` variant scoped to mount-level events; per-session events stay session-internal in Phase 2).

Integration test outline:

1. **AC4.1 turn.before with Modify response.** Build a session, register a blocking subscriber on `tags::TURN_BEFORE` that responds `HookResponse::Modify({"prepend": "[debug] "})`. Drive a turn through the agent loop with a mock provider; assert the prepended content appears in the turn's first user message.

2. **AC4.2 tool.before with Block response.** Register a blocking subscriber on `tags::TOOL_BEFORE` that responds `HookResponse::Block { reason: "denied for testing" }`. Trigger a tool dispatch; assert the handler returns `EffectError::Handler` containing the block reason; assert no actual tool execution occurred.

3. **AC4.3 memory.write notification.** Register a notification subscriber on `tags::MEMORY_WRITE`; trigger a `Memory.Put` via the SDK handler; assert the subscriber received exactly one `HookEvent` with the expected payload (label, scope, write_kind, hashes); assert the handler completed without waiting for the subscriber.

4. **AC4.4 CC alias.** Register a subscriber via `cc_aliases::translate_cc("PreToolUse")` (resolves to `tag::TOOL_BEFORE`); trigger a tool.before event; assert delivery. Repeat for `SessionStart` → `persona.attached`. (Full CC adapter consumes the alias map in Phase 3+; this tests the table is sound.)

5. **AC4.5 timeout.** Use `HookBus::with_timeout(Duration::from_millis(50))`. Register a blocking subscriber that sleeps 200ms before replying. Emit a blocking event; assert `emit_blocking` returns `HookResponse::Continue` after ~50ms. Use `tracing-test` to assert the warn-level log fired.

6. **AC4.6 capability gate respected.** Register a subscriber whose response is `HookResponse::Modify(...)` containing a payload that *would* (in a hypothetical broken-bus implementation) bypass the capability gate. Verify the subscriber's modify *does NOT* let an effect through that the runtime's `CapabilitySet` denies — the bus modifies the event payload, but the subsequent effect dispatch still goes through `policy::evaluate(...)` which returns `Deny`. Assert `EffectError::Handler` with the policy-deny prefix.

7. **AC4.7 ordering blocking.** Register subs A, B, C on `tags::TURN_BEFORE`; have each push their id into a shared `Vec`; emit; assert `[A, B, C]`. Repeat for notification.

8. **AC4.7 ordering notification.** Same shape; emit a notification event; assert all three subs received the event.

9. **Multi-bus separation.** Per-session buses don't cross-deliver: register subscriber X on session 1's bus, emit on session 2's bus, assert X receives no event. Mount-level events fan out from per-session bus to daemon bus correctly.

**Testing:**
Tests must verify each AC listed above. Subscriber callbacks use `tokio::sync::oneshot` to surface assertion-level events to the test harness.

**Verification:**
Run: `cargo nextest run -p pattern-runtime --test hook_lifecycle` and `cargo nextest run --workspace`.
Expected: all hook integration tests pass; existing test suite unaffected.

**Commit:** `[pattern-runtime] [pattern-server] integrate HookBus into sessions + telemetry tap + AC4 integration suite`
<!-- END_TASK_8 -->

<!-- END_SUBCOMPONENT_C -->

---

## Phase done-when checklist

- [ ] `pattern_core::hooks` module compiles with `HookEvent`, `HookFilter`, `HookBus`, tags catalog, payloads catalog, CC alias map.
- [ ] `globset` is a direct workspace dep.
- [ ] All 68+ emit sites from the inventory wire calls to the bus.
- [ ] `HookBus::emit_blocking` honours timeout, returns `Continue` on miss, warns at `tracing::warn` level.
- [ ] `HookBus::emit` (notification) does not block the emitter even with full subscriber buffers; drops are debug-logged.
- [ ] Per-session and daemon-level buses are wired in `pattern_server`. Mount-level events forwarded session→daemon.
- [ ] Default telemetry subscriber attached at session open.
- [ ] All AC4 cases pass under `cargo nextest run -p pattern-runtime --test hook_lifecycle`.
- [ ] `cargo nextest run --workspace` green; no existing tests regress.
- [ ] `cargo fmt` + `cargo clippy --all-features --all-targets` clean.

---

## Notes for executor

- **Do NOT add a closed enum for `HookEvent`.** The shape is `tag: SmolStr` + payload. Adding a new hook point should be a one-line emit-site change plus a one-line catalog constant — never a trait or enum modification.
- **Match emit-site payloads to existing context types.** Don't invent payload fields the surrounding handler doesn't already produce — payload structs in `payloads.rs` are documentation of what we *can* emit, not a wishlist.
- **CC alias table is data, kept in sync with the catalog.** The catalog test in Task 2 (`payload_type_name`) plus the alias round-trip test in Task 3 catch desyncs. Adding a new tag means adding a payload type AND optionally adding a CC alias entry if it maps.
- **`emit_blocking` is `async`. Notification `emit` is sync.** This is the public contract — emit-sites in async functions can use both; sync-only paths can only use `emit`. If a sync site needs blocking semantics (e.g., the eval-worker thread), it goes through the existing `RouterBridge`/`PermissionBridge` pattern — those bridges can wrap `emit_blocking` internally.
- **Per project guidance: no shims, no commented-out code.** If wiring a particular subsystem's emit-site reveals a missing context field (e.g., `mount_id` not threaded through to a handler), thread it through fully rather than passing `None` and writing a comment. Surface a design question if the threading is genuinely complex.
- **First subscriber matters.** The Task 8 telemetry tap is the dogfood path. If the tap can't trivially observe what each emit site is producing, the metadata struct is wrong — fix it now, don't paper over.
- **`HookBus` is not optional in real sessions.** The `()` shim's empty bus is for unit tests only. `SessionContext::open_with_agent_loop` always wires a real bus.
