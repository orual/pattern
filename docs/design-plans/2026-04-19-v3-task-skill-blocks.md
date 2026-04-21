# Pattern v3 Task + Skill Block Subtypes Design

## Summary

Plan 2 extends Pattern's `pattern_memory` crate — the CRDT-backed, file-canonical memory system from Plan 1 — with two new block subtypes: `TaskList` for agent work tracking and `Skill` for reusable prompt content. Both are additive extensions to the existing `BlockSchema` enum; no new crate boundary is introduced, and no changes are made to the `MemoryStore` trait or the underlying subscriber/mount/scope infrastructure.

`TaskList` blocks store structured task items with dependency edges directly in a Loro CRDT document, serialized canonically to KDL on disk. A SQLite index (`tasks` + `task_edges` tables) is derived from Loro state by the existing per-document subscriber worker on each commit, enabling efficient filtered queries and BFS graph traversal without touching canonical storage. Two new SDK effect surfaces — `ctx.tasks.*` (eight methods) and `ctx.skills.*` (five methods: `list`, `get_metadata`, `load`, `search`, `get_usage_stats`) — expose these capabilities to agent programs via the same freer-monad algebra established in Plan 1. `Skill` blocks follow a simpler path: markdown body plus YAML frontmatter serialized to `.md` files, parsed via the `saphyr` YAML library using a hand-written AST visitor (consistent with Plan 1's approach to KDL). Skills are discoverable through the existing FTS5 block search and loaded explicitly into an agent's turn context by injecting a `[skill:loaded]` pseudo-message into the conversation's segment-2 history. Trust tier assignment (first-party, project-local, ad-hoc, plugin-installed-reserved) is surfaced as metadata but not enforced — enforcement is deferred to Plan 4.

## Definition of Done

Ships `Task` and `Skill` as new `BlockSchema` variants on top of Plan 1's pattern_memory foundation. The plan is done when:

### Block schema extensions

- `BlockSchema::TaskList` — multi-task container; each task item has `subject`, `description`, `activeForm`, `status`, `owner`, `blocks` (list of BlockRef — outgoing edges only; reverse direction is derived from the `task_edges` index, not stored on the item), `metadata` (JSON), `comments` (inline list of `{author, timestamp, text}`)
- `BlockSchema::Skill` — markdown body + YAML frontmatter; frontmatter parsed to typed `SkillMetadata` (trust_tier, description, keywords, optional structured hook fields)
- Both added to the existing enum (Text / Map / List / Log / Composite) without disrupting existing variants
- Canonical file formats: TaskList → `.kdl`; Skill → `.md` with YAML frontmatter
- KDL ↔ LoroValue converter extended to handle TaskList's nested structure
- md+frontmatter ↔ LoroValue converter implemented for Skill

### Task infrastructure

- Existing `tasks` + `coordination_tasks` tables repurposed as the Task block INDEX (populated by the subscriber on TaskList block writes)
- New `task_edges` index table for the task dependency graph (edges are the outgoing `blocks` field on each task item in loro — this table is derived; reverse direction is queried, not stored)
- `ctx.tasks.*` SDK surface: `create_task`, `update_task`, `list_tasks` (with filters), `query_graph` (deps traversal), `link` (add edge), `unlink` (remove edge), `transition_status`
- Full cross-block dependency graph as a primitive — edges can reference any block; meaning emerges from use; runtime imposes no topology limits in v1

### Skill infrastructure

- Trust tiers: `first-party` (pattern's own), `project-local` (in `<mount>/skills/` or block-owned by project scope), `ad-hoc` (other); `plugin-installed` tier value reserved in the enum but no code path assigns it in this plan
- Skill loading is always explicit: agent calls `ctx.skills.load(handle)` to inject content; `ctx.skills.list()` for discovery; `ctx.skills.get_metadata(handle)` for structured hook access
- NO auto-mode: runtime does not preload or auto-attach skills based on context
- Hybrid SDK: load-into-context is the default use; `get_metadata` exposes typed frontmatter fields for programmatic use

### Fate markers + scope

- Existing unused task queries at `pattern_db/src/queries/task.rs` absorbed into the new Task block index queries; removed or rewired per the repurposing
- `coordination_tasks` table retention or deprecation decided in brainstorming; retain if it serves a distinct coordination concept, deprecate if redundant

### Testing (per-phase, deterministic-preferred)

- Round-trip property tests for TaskList ↔ KDL
- Round-trip tests for Skill ↔ md+YAML frontmatter
- Graph-edge consistency: write task with edges, verify loro has edges in task fields AND sqlite index matches
- Scope enforcement: TaskList + Skill blocks respect MemoryScope isolate_from_persona policy like any block
- Skill trust tier stored + surfaced correctly; no enforcement path beyond surfacing (intentional for this plan)

### Explicitly OUT OF SCOPE (deferred to future plans)

- Plugin-installed trust tier code paths (Plan 4: `v3-plugins-mcp-iroh`)
- Auto-loading / context-based skill selection
- Subagent coordination on task graphs (Plan 3: `v3-subagents`)
- Task orchestration or execution beyond status transitions
- v2 → v3 migration of any existing task data
- Graph topology limits (deferred; add if observed problems)
- Skill versioning / dependency resolution

### Context

This is the third design plan in the Pattern v3 rewrite sequence. Builds on:

- `docs/design-plans/2026-04-16-v3-foundation.md` (foundation — Tidepool runtime, provider, three-segment cache)
- `docs/design-plans/2026-04-19-v3-memory-rework.md` (Plan 1 — pattern_memory crate, rusqlite, fs-canonical storage, MemoryScope, modes A/B/C)
- `docs/plans/2026-04-16-rewrite-v3-design-draft.md` §3 (memory system brainstorm) and §4 (subagent primitives, referenced for forward compat)

Future v3 plans follow this one:

- Plan 3: `v3-subagents` — ephemeral/fork/sibling primitives, fork-as-jj-workspace, coordination patterns rewired (may exercise the task graph primitive from this plan)
- Plan 4 (if scope permits): `v3-plugins-mcp-iroh` — CC-compatible plugin system (assigns the `plugin-installed` trust tier reserved in this plan)

## Acceptance Criteria

### v3-task-skill-blocks.AC1: TaskList schema + KDL round-trip

- **v3-task-skill-blocks.AC1.1 Success:** `BlockSchema::TaskList { default_owner, default_status, display_limit }` exists and is exported from `pattern_core::types::memory_types`
- **v3-task-skill-blocks.AC1.2 Success:** `TaskItem`, `TaskStatus`, `TaskComment`, `BlockRef`, `TaskItemId` types exist with documented fields
- **v3-task-skill-blocks.AC1.3 Success:** Property test (proptest) confirms round-trip equivalence: generate arbitrary TaskList with nested items + edges + comments → serialize to KDL → parse back → LoroValue matches original
- **v3-task-skill-blocks.AC1.4 Success:** BlockRef parses both `(block)"<handle>"` and `(block)"<handle>#<item_id>"` forms
- **v3-task-skill-blocks.AC1.5 Failure:** Malformed BlockRef annotation (e.g., `(block)""` or missing typed annotation) produces `KdlConversionError` with file:line reference
- **v3-task-skill-blocks.AC1.6 Edge:** Empty TaskList (zero items) round-trips cleanly; self-referential edge (`A.blocks = [A]`) round-trips cleanly
- **v3-task-skill-blocks.AC1.7 Edge:** Item reordering via `LoroMovableList` preserves item ids across round-trip
- **v3-task-skill-blocks.AC1.8 Success:** `TaskItemId::parse("")` returns `TaskItemIdError::Empty`; `TaskItemId::new()` produces a valid Snowflake string (base32-encoded Mastodon-style via the workspace `new_snowflake_id` generator)
- **v3-task-skill-blocks.AC1.9 Success:** Two concurrent agents calling `TaskItemId::new()` produce distinct ids (Snowflake collision-resistant by construction via ferroid's atomic generator)

### v3-task-skill-blocks.AC2: Task block index tables + migration

- **v3-task-skill-blocks.AC2.1 Success:** The task-block-index migration (filename determined at execution time against the sibling memory-rework plan's migration tree layout — typically `0014_task_block_index.sql` in flat layout or `memory/XX_task_block_index.sql` in subtree layout) applies cleanly to a fresh DB
- **v3-task-skill-blocks.AC2.2 Success:** Migration round-trip test: fixture DB with pre-migration `tasks` rows migrates; pre-existing columns preserved; new columns added with defaults
- **v3-task-skill-blocks.AC2.3 Success:** `coordination_tasks` table dropped; no remaining references in active code paths
- **v3-task-skill-blocks.AC2.4 Success:** `task_edges` table created with `source_block`, `source_item NOT NULL`, `target_block`, `target_item NULL`; unique expression index over `COALESCE(target_item, '<block>')` serves as the effective primary key
- **v3-task-skill-blocks.AC2.5 Failure:** Attempting to insert a duplicate edge (same source_block + source_item + target_block + target_item) is rejected by the unique constraint
- **v3-task-skill-blocks.AC2.6 Edge:** Column drop of `priority` from `tasks` does not break any remaining queries (verified by `cargo check -p pattern-db`); indexes on `coordination_tasks` are dropped BEFORE the table drop so no DROP INDEX failures occur
- **v3-task-skill-blocks.AC2.7 Edge:** Block-level target reference (target_item NULL) and item-level reference to a hypothetical empty string id cannot collide because `source_item NOT NULL` rejects empty-string ids at the newtype level before insert reaches sqlite

### v3-task-skill-blocks.AC3: Subscriber reconciliation for TaskList blocks

- **v3-task-skill-blocks.AC3.1 Success:** Writing a TaskList block with 5 items + 3 edges triggers subscriber reconciliation; 5 rows in `tasks`, 3 edge rows in `task_edges` match loro state. The single-source-of-truth edge model stores each edge exactly once on the source task; reverse direction is queried, not stored.
- **v3-task-skill-blocks.AC3.2 Success:** Deleting a task item removes the corresponding `tasks` row + all edges referencing it from `task_edges`
- **v3-task-skill-blocks.AC3.3 Success:** Modifying a task's `blocks` field (add / remove edge) updates `task_edges` in the same transaction
- **v3-task-skill-blocks.AC3.4 Failure:** Intentionally failing a query mid-subscriber-reconcile (e.g., simulated db lock) rolls back the full transaction; neither `tasks` nor `task_edges` shows half-applied state
- **v3-task-skill-blocks.AC3.5 Failure:** If subscriber panics during reconcile, supervisor restarts worker + `metrics::counter!("memory.sync_worker.restart")` increments
- **v3-task-skill-blocks.AC3.6 Edge:** Subscriber reconcile is idempotent — running it twice with no loro state change produces no rows changed
- **v3-task-skill-blocks.AC3.7 Edge:** Concurrent edits by two agents (one adds edge A→B, other removes edge C→D on a different task) both apply cleanly; loro CRDT merges; subscriber reconciles to final state

### v3-task-skill-blocks.AC4: `ctx.tasks.*` SDK surface methods

- **v3-task-skill-blocks.AC4.1 Success:** `create_task` writes a new task item to the target block; returned `TaskItemId` matches the item's id in loro; subsequent `list_tasks` includes it
- **v3-task-skill-blocks.AC4.2 Success:** `update_task` patches specified fields; unspecified fields unchanged; `updated_at` refreshed
- **v3-task-skill-blocks.AC4.3 Success:** `transition_status` changes status field; `updated_at` refreshed; if new status is `Completed`, optional `completed_at` set (if block schema tracks it)
- **v3-task-skill-blocks.AC4.4 Success:** `link(A, B)` adds one edge in A's loro doc (A.blocks += B) as a single atomic commit; `task_edges` has exactly one row (source=A, target=B) after subscriber reconciles; the reverse direction (tasks that block B) is queryable via `task_edges WHERE target_block+target_item = B` — no separate reverse row needed
- **v3-task-skill-blocks.AC4.5 Success:** `unlink(A, B)` removes the entry from A.blocks in a single loro commit; `task_edges` row deleted by subscriber on next reconcile
- **v3-task-skill-blocks.AC4.5b Edge:** `link(A, B)` where A and B are in different TaskList blocks is atomic — only A's block is modified, so there's no cross-document commit coordination required
- **v3-task-skill-blocks.AC4.6 Success:** `add_comment(task, text)` appends `TaskComment { author: current_agent, timestamp: now, text }` to the task's comments list
- **v3-task-skill-blocks.AC4.7 Failure:** `update_task` on a nonexistent `BlockRef` returns `MemoryError::TaskNotFound` with the offending ref in the error message
- **v3-task-skill-blocks.AC4.8 Edge:** `link(A, A)` (self-edge) is allowed — graph topology is unconstrained in v1

### v3-task-skill-blocks.AC5: `list_tasks` + `query_graph`

- **v3-task-skill-blocks.AC5.1 Success:** `list_tasks(block=Some(h), TaskFilter::default())` returns all tasks in block h as TaskViews
- **v3-task-skill-blocks.AC5.2 Success:** `list_tasks(block=None, ...)` returns tasks from all scope-visible TaskList blocks
- **v3-task-skill-blocks.AC5.3 Success:** `list_tasks` with `status: Some([InProgress, Blocked])` filters to those statuses; `owner: Some(agent)` filters to owner; `has_blockers: Some(true)` filters to tasks with at least one blocked_by edge; `keyword` filters via FTS5
- **v3-task-skill-blocks.AC5.4 Success:** `query_graph(root, GraphQuery { direction, depth, max_nodes })` returns BFS slice respecting all three params; `Forward` follows outgoing edges, `Reverse` follows incoming (queried via target_block lookup), `Both` combines; nodes + edges consistent (every edge endpoint appears in nodes)
- **v3-task-skill-blocks.AC5.5 Success:** Scope enforcement — agent with `CoreOnly` isolation sees only project-scope tasks via `list_tasks`; persona-scope TaskList blocks are invisible
- **v3-task-skill-blocks.AC5.6 Failure:** `query_graph` with `depth: Some(0)` returns only the root node, no edges
- **v3-task-skill-blocks.AC5.6b Success:** `query_graph` respects `max_nodes` cap (default 1000 if None); when cap hit, `truncated: true` in result and BFS frontier dropped
- **v3-task-skill-blocks.AC5.7 Edge:** Graph traversal on a cyclic subgraph terminates at the depth or max_nodes limit (whichever hit first); visited-set tracking prevents re-enqueueing
- **v3-task-skill-blocks.AC5.8 Edge:** Runaway graph test: create 10,000 tasks with long chain + branching edges; `query_graph(root, GraphQuery::default())` returns ≤1000 nodes with `truncated: true`; traversal completes in bounded time (spawn_blocking thread not pinned for longer than a second)

### v3-task-skill-blocks.AC6: Skill schema + md+YAML frontmatter round-trip

- **v3-task-skill-blocks.AC6.1 Success:** `BlockSchema::Skill { expected_keys }` exists and is exported
- **v3-task-skill-blocks.AC6.2 Success:** Round-trip property test: arbitrary `SkillMetadata` + arbitrary markdown body serialize to .md+frontmatter, parse back, LoroValue matches original
- **v3-task-skill-blocks.AC6.3 Success:** Saphyr parses valid YAML frontmatter without panics; unknown keys preserved in the loro state for round-trip even if not in `SkillMetadata`
- **v3-task-skill-blocks.AC6.4 Success:** `SkillMetadata.hooks` preserves nested structure as `serde_json::Value` through round-trip
- **v3-task-skill-blocks.AC6.5 Failure:** Malformed YAML frontmatter (syntax error, missing required `name`) produces `SkillParseError` with file location
- **v3-task-skill-blocks.AC6.6 Failure:** Missing frontmatter delimiters (`---`) rejects with a clear error
- **v3-task-skill-blocks.AC6.7 Edge:** Frontmatter with all-optional fields (only `name` + `trust_tier` set) parses correctly with None / empty defaults

### v3-task-skill-blocks.AC7: Trust tier assignment

- **v3-task-skill-blocks.AC7.1 Success:** Skill loaded from pattern_runtime's SDK resource directory → `trust_tier == FirstParty`
- **v3-task-skill-blocks.AC7.2 Success:** Skill loaded from `<mount>/skills/foo.md` → `trust_tier == ProjectLocal`
- **v3-task-skill-blocks.AC7.3 Success:** Skill block created at runtime via `MemoryStore::put_block` → `trust_tier == AdHoc`
- **v3-task-skill-blocks.AC7.4 Success:** Frontmatter declaring `trust_tier: "plugin-installed"` on a file loaded from `<mount>/skills/` preserves the `PluginInstalled` value on round-trip (no overwrite to `ProjectLocal`)
- **v3-task-skill-blocks.AC7.5 Success:** Loading a skill with declared `PluginInstalled` tier increments `metrics::counter!("skill.plugin_installed_tier_without_plugin_system")` and logs a warning
- **v3-task-skill-blocks.AC7.6 Edge:** A skill with invalid `trust_tier` string value in frontmatter (e.g., `"foo"`) surfaces a parse error; does NOT silently default to `AdHoc`

### v3-task-skill-blocks.AC8: `ctx.skills.*` SDK surface methods

- **v3-task-skill-blocks.AC8.1 Success:** `list()` enumerates all Skill-schema blocks visible in current scope; returns `SkillInfo` with correct handles + metadata summary
- **v3-task-skill-blocks.AC8.2 Success:** `get_metadata(handle)` on a Skill block returns typed `SkillMetadata` with hook fields preserved as `serde_json::Value`
- **v3-task-skill-blocks.AC8.3 Success:** `get_metadata(handle)` on a non-Skill block returns `None`
- **v3-task-skill-blocks.AC8.4 Success:** `search(query)` returns matching `SkillInfo` via FTS5 over skill name + description + keywords + body; relevance-ranked
- **v3-task-skill-blocks.AC8.5 Failure:** `load(handle)` on a non-existent block returns `MemoryError::BlockNotFound`
- **v3-task-skill-blocks.AC8.6 Failure:** `load(handle)` on a non-Skill block returns `SkillError::NotASkill(handle)`

### v3-task-skill-blocks.AC9: Skill `load` behavior + metadata updates

- **v3-task-skill-blocks.AC9.1 Success:** `load(handle)` injects a `[skill:loaded]` marker + skill body + `[skill:loaded:end]` marker as a pseudo-message in segment 2 of the current turn's composed model request (snapshot-tested)
- **v3-task-skill-blocks.AC9.2 Success:** Loaded skill persists in segment 2 for subsequent turns; segment 1 cache is not invalidated by load
- **v3-task-skill-blocks.AC9.3 Success:** `load` updates the Skill block's `SkillUsageStats` (last_used, last_used_by, use_count++) in the `skill_usage_stats` sqlite table ONLY; the canonical `.md` file is NOT re-emitted and stays content-hash-stable across loads. Verify by computing file hash before and after load calls — hash unchanged. (Design deviation from earlier draft: stats are sqlite-only, not in the LoroDoc — usage is per-local-install observability, not replicated content.)
- **v3-task-skill-blocks.AC9.3b Success:** Post-load, `get_metadata` returns typed `SkillMetadata` without `last_used` (stats are separate); `get_usage_stats(handle)` returns fresh `SkillUsageStats`
- **v3-task-skill-blocks.AC9.4 Success:** Multiple loads of different skills in the same turn produce multiple [skill:loaded] markers; order preserved
- **v3-task-skill-blocks.AC9.5 Edge:** Loading the same skill twice in the same turn produces two [skill:loaded] markers (no dedup in v1, by design)
- **v3-task-skill-blocks.AC9.6 Edge:** Skill usage stats update does NOT cause VCS dirtiness — `git status` (or `jj status`) in a Mode A mount shows no pending changes after 100 skill loads

### v3-task-skill-blocks.AC10: End-to-end smoke + scope enforcement

- **v3-task-skill-blocks.AC10.1 Success:** Smoke test at `crates/pattern_memory/tests/task_skill_smoke.rs` passes deterministically in CI: creates mount, TaskList with cross-block edges, Skill with frontmatter, exercises full `ctx.tasks.*` + `ctx.skills.*` surface, asserts composed request reflects all state
- **v3-task-skill-blocks.AC10.2 Success:** Mock ProviderClient in the smoke test produces deterministic output (no live model dependency in CI)
- **v3-task-skill-blocks.AC10.3 Success:** Scope enforcement smoke — TaskList + Skill blocks in project scope with `Full` isolation are invisible to persona-default sessions; `list_tasks` and `ctx.skills.list` both respect the policy
- **v3-task-skill-blocks.AC10.4 Failure:** Any step in the smoke flow failing produces a clear error identifying which step and which assertion
- **v3-task-skill-blocks.AC10.5 Edge:** Smoke test runs concurrently with other `pattern-memory` integration tests without shared-state interference (each uses its own temp-dir fixture)
- **v3-task-skill-blocks.AC10.6 Success (Plan 1 interop):** External `.kdl` edit reconciliation — test externally edits a TaskList `.kdl` file (adds a new task item via text editor write), notify-watcher fires, loro CRDT merge imports the change, subscriber emits the canonical file again, `tasks` + `task_edges` index rows reflect the added item
- **v3-task-skill-blocks.AC10.7 Success (Plan 1 interop):** Quiesce + commit cycle — call `quiesce()` on the mount containing task and skill blocks, `memory.db` reaches canonical state (WAL truncated), `jj commit` (or host VCS commit) produces a clean commit containing `.kdl` + `.md` files + `memory.db` snapshot; task index state preserved across restart-from-checkpoint
- **v3-task-skill-blocks.AC10.8 Success (cross-block-type search):** FTS5 search spanning a Text block, TaskList block, and Skill block returns results from all three with correct BM25 scoring; no block type is silently excluded

## Glossary

- **BlockSchema**: The Rust enum that determines how a memory block is interpreted, serialized, and queried. Existing variants include `Text`, `Map`, `List`, `Log`, `Composite`; this plan adds `TaskList` and `Skill`.
- **BlockRef**: A typed reference to either a whole block (by `BlockHandle`) or a specific item within a block (block + `TaskItemId`). Encoded in KDL as a typed annotation: `(block)"handle"` or `(block)"handle#item-id"`.
- **BlockHandle**: A stable string identifier for a memory block, scoped within a mount.
- **TaskItem**: A single work record inside a `TaskList` block, carrying subject, status, owner, dependency edges, comments, and freeform metadata.
- **TaskItemId**: A `SmolStr` alias serving as a stable identifier for a `TaskItem` within its containing block, preserved across list reorders.
- **TaskStatus**: Enum of task lifecycle states: `Pending`, `InProgress`, `Blocked`, `Completed`, `Cancelled`. `Blocked` is distinct from `Pending` to surface stuck work explicitly.
- **SkillMetadata**: Typed representation of a Skill block's YAML frontmatter, including name, trust tier, description, keywords, and an opaque `hooks` JSON value for author-defined structured data.
- **SkillTrustTier**: Enum classifying a skill's provenance: `FirstParty` (shipped with the runtime), `ProjectLocal` (in a mount's `skills/` directory or project scope), `AdHoc` (agent-created at runtime), `PluginInstalled` (reserved for Plan 4, no assignment code path in this plan).
- **MemoryScope**: A per-session policy that controls which memory blocks are visible to an agent. The `isolate_from_persona` policy (e.g., `CoreOnly`, `Full`) prevents persona-scoped blocks from leaking into project-scoped or cross-agent sessions.
- **mount**: A filesystem root registered with the pattern_memory system representing a project or workspace. Blocks are stored under `<mount>/blocks/` and skills under `<mount>/skills/`.
- **persona**: A named agent identity with its own scoped memory blocks, isolated from other personas according to `MemoryScope` policy.
- **sync_worker**: A per-document background task (from Plan 1) that reacts to Loro commit events: exports canonical files, updates FTS5/vector indexes, and (new in Plan 2) reconciles the task block index tables.
- **Loro / LoroDoc**: A Rust CRDT library providing per-document state containers (`LoroDoc`, `LoroMap`, `LoroMovableList`, `LoroText`) with deterministic merge semantics under concurrent edits. Canonical truth for all memory block content.
- **LoroMovableList**: A Loro container type for ordered lists that supports item-level moves (reordering) without rewriting all list members, preserving stable item identities across priority reshuffles.
- **CRDT (Conflict-free Replicated Data Type)**: A data structure with a merge operation that is associative, commutative, and idempotent, enabling multiple concurrent editors to converge to the same state without coordination. Loro provides CRDT semantics for memory blocks.
- **KDL**: A human-friendly document language (kdl.dev) used as the canonical on-disk format for `TaskList` blocks. Supports typed annotations (`(block)"..."`) used to encode `BlockRef` values.
- **YAML frontmatter**: A YAML metadata block at the top of a Markdown file, delimited by `---` lines, used as the canonical format for `Skill` block metadata.
- **saphyr**: A pure-Rust YAML 1.2 parser (AST-based) selected as the frontmatter parser for Skill blocks. Chosen over `serde_yaml` (unmaintained) and `serde_yml` (AI-assisted fork with quality concerns).
- **pattern_memory**: The crate introduced in Plan 1 that provides the `MemoryStore` trait, block types, Loro-primary storage, subscriber topology, and file-canonical I/O. Plan 2 builds entirely within this crate.
- **rusqlite**: The synchronous SQLite client used for the derived index tables (`tasks`, `task_edges`, FTS5). Called from async context via `spawn_blocking`.
- **FTS5**: SQLite's full-text search extension, used for keyword-based filtering of tasks (`subject`/`description`) and skills (name/description/keywords/body).
- **BFS (breadth-first search)**: The graph traversal algorithm used by `query_graph` to walk the `task_edges` table up to a caller-specified depth, with a visited set to terminate on cycles.
- **spawn_blocking**: Tokio's mechanism for running synchronous (blocking) code on a dedicated thread pool, used to call rusqlite from async SDK handlers without stalling the async executor.
- **insta**: A Rust snapshot testing library used to assert that FTS5 output and skill `[skill:loaded]` pseudo-messages remain stable across code changes.
- **proptest**: A Rust property-based testing library used to generate arbitrary `TaskList` and `SkillMetadata` values and assert round-trip equivalence through serialization.
- **three-segment cache layout**: Pattern v3 foundation's model-request composition scheme: segment 1 is the stable system prompt (cached), segment 2 is conversation history (updated across turns), segment 3 is the current turn's live input. Skill `load` injects content into segment 2.
- **freer-monad effect algebra**: The SDK pattern used in Pattern's agent runtime: agent programs are expressed as sequences of typed effect requests (e.g., `Pattern.Tasks.create_task`) interpreted by Rust handlers, without directly calling Rust from Haskell. Plan 2 adds `Pattern.Tasks` and `Pattern.Skills` algebras.
- **fate marker**: A code comment convention (e.g., `// REPLACED BY: queries/task.rs`) used to mark transitional code that exists between implementation phases, ensuring each phase boundary is auditable.
- **`[skill:loaded]` marker**: A synthetic delimiter injected into segment 2 of the model request when `ctx.skills.load(handle)` is called, wrapping the skill's markdown body so the agent can identify loaded skill boundaries in its context.
- **task_edges**: A SQLite table derived from each Loro task-item's `blocks` field (outgoing only; single-source-of-truth edge model). Stores directional edges between `BlockRef` pairs for efficient graph queries. Fully derived — Loro is canonical. Reverse direction ("who blocks me") is computed by querying `task_edges WHERE target_block+target_item = X`, not stored separately.

## Architecture

Plan 2 adds two new `BlockSchema` variants on top of Plan 1's pattern_memory crate: `TaskList` for agent work tracking with a cross-block dependency graph, and `Skill` for reusable prompt content with trust-tier metadata. Both are schema-level extensions — no new crate, no new storage backend, no changes to the MemoryStore trait. The existing subscriber topology, mount model, MemoryScope, and file-canonical storage from Plan 1 apply unchanged.

### TaskList schema

A TaskList block contains zero or more task items. Each item is a fine-grained work record with status, owner, dependency edges, metadata, and inline comments. Multiple tasks per block (rather than one task per block) keeps block count bounded for typical workloads and matches the natural grouping agents use (a project phase's tasks, a review checklist's items, a coordination handoff's work units).

```rust
pub enum BlockSchema {
    // ... existing Text / Map / List / Log / Composite
    TaskList {
        default_owner: Option<AgentId>,
        default_status: Option<TaskStatus>,
        display_limit: Option<usize>,  // visible-in-context cutoff
    },
    Skill { expected_keys: Vec<String> },
}

pub struct TaskItem {
    pub id: TaskItemId,              // newtype; never empty
    pub subject: String,              // imperative ("Fix login timeout")
    pub description: String,          // markdown body
    pub active_form: Option<String>,  // present-continuous ("Fixing login timeout")
    pub status: TaskStatus,
    pub owner: Option<AgentId>,
    /// Outgoing edges ONLY. This task blocks the referenced targets.
    /// `blocked_by` is NOT stored — it's a derived view computed from
    /// other tasks' `blocks` fields via the task_edges index.
    pub blocks: Vec<BlockRef>,
    pub metadata: serde_json::Value,
    pub comments: Vec<TaskComment>,   // inline, append-mostly
    pub created_at: Timestamp,        // jiff
    pub updated_at: Timestamp,
}

pub struct BlockRef {
    pub block: BlockHandle,
    pub task_item: Option<TaskItemId>,  // None = block-level reference
}

pub enum TaskStatus {
    Pending,
    InProgress,
    Blocked,    // distinct from Pending; surfaces stuck tasks
    Completed,
    Cancelled,
}

pub struct TaskComment {
    pub author: AgentId,
    pub timestamp: Timestamp,
    pub text: String,
}

/// Newtype wrapping SmolStr. Constructor validates non-empty. Generated
/// via the workspace's ferroid-backed Mastodon-style Snowflake generator
/// (base32-encoded, time-ordered, lexicographically sortable, collision-
/// resistant across concurrent multi-agent creates via an atomic counter).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaskItemId(SmolStr);

impl TaskItemId {
    pub fn new() -> Self { /* delegates to pattern_core::types::ids::new_snowflake_id */ }
    pub fn parse(s: &str) -> Result<Self, TaskItemIdError> {
        if s.is_empty() { return Err(TaskItemIdError::Empty); }
        Ok(Self(s.into()))
    }
}
```

**LoroDoc layout** for TaskList blocks:

```
LoroDoc (root = Map)
├── schema: "task-list"     (discriminator for converter dispatch)
├── default_status: "pending"
├── display_limit: 20
└── items: LoroMovableList  (reorderable, one entry per task item)
   ├── LoroMap (t-1): { subject, description, status, owner, blocks, metadata, comments, ... }  // no blocked_by — derived from task_edges
   └── LoroMap (t-2): ...
```

`LoroMovableList` enables priority-reshuffle without rewriting the whole list. Each TaskItem is a LoroMap sub-container; subscribers to the root doc observe TaskItem-level changes at the granularity loro provides. Reordering preserves item ids — edges reference items by id, not position.

**Single-source-of-truth edge model:** Edges live only on the SOURCE task's `blocks` field. The inverse relation (what blocks task X) is computed by the subscriber into `task_edges` and exposed via queries. This makes `link` and `unlink` atomic — a single LoroDoc commit on the source block, regardless of whether the target is in the same or a different TaskList block. Cross-block edge consistency becomes trivial because there's only one place to write.

**KDL canonical file** (example at `<mount>/blocks/working/my-task-list.kdl`):

```kdl
task-list default_status="pending" display_limit=20 {
    item id="0197f2a1-7c84-7e4e-b3f8-a1..." status="in-progress" owner="@orual" {
        subject "Fix login timeout bug"
        description "The auth flow drops session after 30s idle."
        active_form "Fixing login timeout bug"
        metadata { priority "high"; estimated_hours=2.5; }
        // This task blocks two other tasks in this same block.
        blocks (block)"my-task-list#0197f2a1-7c84-..." (block)"my-task-list#0197f2a1-7c85-..."
        comments {
            entry author="@reviewer" timestamp="2026-04-19T14:20:00Z" {
                text "Possibly related to the clock-skew issue last week."
            }
        }
    }
    item id="0197f2a1-7c84-7e4e-b3f8-a2..." status="pending" {
        subject "Update auth documentation"
        // No `blocks` field here — "blocked by t-1" is derived from t-1's outgoing edges.
    }
}
```

Note: task item ids shown above are base32-encoded Mastodon-style Snowflakes produced by the workspace's `new_snowflake_id` generator. Only `blocks` (outgoing) appears in the canonical KDL; "who blocks me" is queried from the `task_edges` index. This matches the LoroValue shape and keeps the canonical file stable under edge operations.

BlockRef encoding in KDL: typed annotation `(block)` with a string value of either `"<block_handle>"` (block-level reference) or `"<block_handle>#<task_item_id>"` (item-specific reference). The converter parses both forms into `BlockRef { block, task_item }`.

### Skill schema

A Skill block is reusable prompt content — a checklist, a workflow description, an invocation template. Agents load skills explicitly (no auto-mode) to inject the skill's markdown body into their current turn's context. Skill metadata includes a trust tier and optional structured hooks for programmatic use.

```rust
pub struct SkillMetadata {
    pub name: String,
    pub trust_tier: SkillTrustTier,
    pub description: Option<String>,
    pub keywords: Vec<String>,
    /// Skill-author-defined hooks (e.g., checklist, workflow params).
    /// Preserved as opaque JSON; exposed via ctx.skills.get_metadata.
    pub hooks: serde_json::Value,
}

/// Runtime-captured usage stats. Stored ONLY in a dedicated sqlite
/// table (`skill_usage_stats`, handle-keyed). NOT in the LoroDoc and
/// NOT serialized to the canonical `.md` frontmatter. Rationale:
/// usage stats are per-local-install observability (how often *this*
/// runtime has loaded a skill), not replicated content — divergent
/// counts between nodes shouldn't merge via CRDT. Keeping stats out
/// of the LoroDoc also avoids any "skip this subtree" emitter
/// carve-out and any coordination-item on sibling plans for a new
/// LoroDoc mutation hook on MemoryStore.
pub struct SkillUsageStats {
    pub last_used: Option<Timestamp>,
    pub last_used_by: Option<AgentId>,
    pub use_count: u64,
}

pub enum SkillTrustTier {
    /// Pattern-owned, shipped with the runtime's SDK resources.
    FirstParty,
    /// Lives in <mount>/skills/ or is block-owned by project scope.
    ProjectLocal,
    /// Reserved for Plan 4 plugin installation flow.
    /// NO CODE PATH assigns this in Plan 2.
    PluginInstalled,
    /// Agent-created or runtime-inserted; unknown/untrusted origin.
    AdHoc,
}
```

**Canonical file format** (example at `<mount>/skills/review-checklist.md`):

```markdown
---
name: "review-checklist"
trust_tier: "project-local"
description: "Checklist for reviewing PRs against the house style guide."
keywords: [review, pr, checklist, style]
hooks:
  checklist:
    - "Types match the contract"
    - "Tests cover the new behavior"
  workflow:
    entry_criteria: "PR has passed CI + self-review"
---

# Review Checklist

Use this skill when reviewing code changes against the house style guide.

## Before starting
- Confirm the PR has tests that verify the acceptance criteria
- Check that the implementation follows patterns established in CLAUDE.md
```

**LoroDoc layout** for Skill blocks:

```
LoroDoc (root = Map)
├── schema: "skill"
├── metadata: LoroMap (name, trust_tier, description, keywords, hooks)
├── extras:   LoroMap (unknown frontmatter keys, preserved for round-trip)
└── body:     LoroText (markdown content — agent-editable as text)
```

Usage stats (`last_used`, `last_used_by`, `use_count`) live in a separate sqlite table `skill_usage_stats`, NOT in the LoroDoc. See the "Skill usage stats separation" section for rationale.

**YAML parser choice**: frontmatter parsing uses `saphyr` (pure Rust, actively maintained; `serde_yaml` is unmaintained as of 2026, and community `serde_yml` forks have surfaced AI-assisted-porting concerns the user has flagged as problematic). Saphyr produces an AST; we map it to `SkillMetadata` directly without going through serde derives, similar to the hand-written LoroValue↔KdlDocument converter pattern from Plan 1.

**Saphyr AST → SkillMetadata visitor architecture:**

```
parse(file_bytes) -> SkillFile
  1. Split frontmatter from body by locating `---` delimiters
     at file start + next `---` terminator line
  2. Parse frontmatter chunk via saphyr::parser::Parser → Yaml AST
  3. Walk AST with a hand-written visitor:
     - Required keys: `name` (must be non-empty String), `trust_tier`
       (must parse into SkillTrustTier); absent or wrong-type → SkillParseError
     - Optional keys: `description` (String or null), `keywords` (Sequence
       of Strings, default empty), `hooks` (any Yaml value, stored as
       serde_json::Value after Yaml → JSON conversion)
     - Unknown keys: preserved into LoroMap via a generic Yaml → LoroValue
       converter. Ensures AC6.3 round-trip fidelity even for
       skill-author-defined extensions.
     - Type mismatches (e.g., keywords: String instead of Sequence): typed
       SkillParseError with the offending key + file location
  4. Body is the remainder of the file bytes after the terminator `---` +
     leading newline, stored as LoroText verbatim.
```

Unknown-key preservation uses a generic Yaml → LoroValue converter similar to the LoroValue↔KdlDocument converter from Plan 1. The converter implements Yaml scalar / sequence / mapping / null → LoroValue variant (`I64`/`Double`/`Bool`/`String`/`List`/`Map`/`Null`) mappings directly; YAML's `!!binary` tag produces `LoroValue::Binary` (consistent with the Binary handling policy from Plan 1). Unknown keys land under a `loro_extra_metadata` LoroMap submap inside the Skill block's metadata, preserved on round-trip.

**Trust tier assignment**:

- `FirstParty` — assigned at SDK init to skills loaded from pattern_runtime's SDK resource directory
- `ProjectLocal` — assigned to skills loaded from `<mount>/skills/` or to Skill blocks in project-scoped storage
- `AdHoc` — assigned to skills created at runtime (e.g., an agent writes a new Skill block directly into memory, or a skill arrives via message attachment)
- `PluginInstalled` — variant value reserved in the enum. Reading a .md file whose frontmatter declares `trust_tier: "plugin-installed"` preserves the value on round-trip but emits a warning via `metrics::counter!("skill.plugin_installed_tier_without_plugin_system")`; Plan 4 will validate provenance and legitimately assign this tier

No runtime enforcement beyond surfacing the tier in `SkillInfo`. Agents query via `ctx.skills.get_metadata(handle)` and make their own decisions. Explicit loading (no auto-mode) means the agent always chooses which skills enter context.

### SDK surfaces

`ctx.tasks.*` is a new agent-facing effect surface with eight methods; `ctx.skills.*` adds five. All handlers wrap the sync `MemoryStore` from Plan 1 plus the new task index query paths and the `skill_usage_stats` sqlite table.

```haskell
-- ctx.tasks.*
create_task       :: BlockHandle -> TaskSpec -> Effect TaskItemId
update_task       :: BlockRef -> TaskPatch -> Effect ()
transition_status :: BlockRef -> TaskStatus -> Effect ()
-- link/unlink write ONLY to source.blocks (single loro commit, atomic).
link              :: BlockRef -> BlockRef -> Effect ()   -- source, target
unlink            :: BlockRef -> BlockRef -> Effect ()
list_tasks        :: Maybe BlockHandle -> TaskFilter -> Effect [TaskView]
-- query_graph returns a bounded BFS slice. max_nodes caps total node
-- count to prevent runaway traversal of large or cyclic graphs.
query_graph       :: BlockRef -> GraphQuery -> Effect GraphSlice
add_comment       :: BlockRef -> Text -> Effect ()

-- ctx.skills.*
list              :: Effect [SkillInfo]
get_metadata      :: BlockHandle -> Effect (Maybe SkillMetadata)
load              :: BlockHandle -> Effect ()
search            :: Query -> Effect [SkillInfo]
-- get_usage_stats reads the sqlite-only skill_usage_stats table.
-- Stats are separate from SkillMetadata because they're per-local-install
-- observability, not replicated content.
get_usage_stats   :: BlockHandle -> Effect SkillUsageStats
```

Rust-side shapes for the task surface:

```rust
pub struct TaskSpec {
    pub subject: String,
    pub description: String,
    pub active_form: Option<String>,
    pub status: Option<TaskStatus>,      // defaults to block's default_status or Pending
    pub owner: Option<AgentId>,
    pub metadata: serde_json::Value,
    // edges NOT set on creation — use link() to add after
}

pub struct TaskPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub status: Option<TaskStatus>,
    pub owner: Option<Option<AgentId>>,  // Some(None) clears owner
    pub metadata: Option<serde_json::Value>,
}

pub struct TaskFilter {
    pub status: Option<Vec<TaskStatus>>,     // any-of match
    pub owner: Option<AgentId>,
    pub has_blockers: Option<bool>,          // true = only blocked tasks
    pub keyword: Option<String>,             // FTS5 substring match on subject/description
}

pub struct TaskView {
    pub block_ref: BlockRef,
    pub subject: String,
    pub status: TaskStatus,
    pub owner: Option<AgentId>,
    pub blocker_count: usize,
    pub blocks_count: usize,
}

pub struct GraphQuery {
    /// Direction to traverse from the root.
    pub direction: Direction,
    /// Maximum hops from root. None = runtime default (16).
    pub depth: Option<u32>,
    /// Maximum total nodes returned. None = runtime default (1000).
    /// Exceeding this cap truncates the BFS frontier; truncated=true in result.
    pub max_nodes: Option<u32>,
}

pub enum Direction {
    /// Follow outgoing edges (tasks THIS task blocks).
    Forward,
    /// Follow incoming edges (tasks that block THIS task).
    /// Reverse direction is answered by the query layer — no reverse
    /// edges stored in loro; sqlite index queries by target_block/target_item.
    Reverse,
    /// Both forward and reverse; nodes reached by either direction.
    Both,
}

pub struct GraphSlice {
    pub nodes: Vec<BlockRef>,
    /// Edges always stored source → target; direction reflects the
    /// traversal used to reach a node, not a separate "kind".
    pub edges: Vec<(BlockRef, BlockRef)>,
    /// True if `max_nodes` was hit and traversal was truncated.
    pub truncated: bool,
}

pub struct SkillInfo {
    pub handle: BlockHandle,
    pub name: String,
    pub description: Option<String>,
    pub trust_tier: SkillTrustTier,
    pub keywords: Vec<String>,
    pub last_used: Option<Timestamp>,
}
```

**Skill load semantics**: `ctx.skills.load(handle)` retrieves the Skill block's body and injects it into the current turn's composed model request as a synthesized message in segment 2 (history), wrapped with a clear marker:

```
[skill:loaded]  name="review-checklist"  trust_tier="project-local"

# Review Checklist
...skill body content...

[skill:loaded:end]
```

The skill content persists in segment 2 for subsequent turns — it's a real history message once loaded, not ephemeral. The agent can call `load` multiple times; each adds a new segment-2 pseudo-message. Plan 1's three-segment cache layout applies: segment 2 invalidates on additions but segment 1 stays cached. Runtime captures `last_used` + `last_used_by` + `use_count` in the `skill_usage_stats` sqlite table on each load (not in the LoroDoc or canonical file).

### Task index tables

Plan 2's storage additions are index-only — Plan 1's canonical-content-in-loro pattern applies unchanged. The existing `tasks` table (sitting unused since the initial schema) is repurposed as the Task block index, extended to carry block-provenance columns. A new `task_edges` table derives from task-item fields for efficient graph queries.

```sql
-- migrations/memory/0012_task_block_index.sql (in Plan 1's split migration tree)

-- Drop coordination_tasks indexes BEFORE dropping the table they reference.
DROP INDEX IF EXISTS idx_tasks_status;        -- index on coordination_tasks
DROP INDEX IF EXISTS idx_tasks_assigned;      -- index on coordination_tasks

-- Drop coordination_tasks: its columns are a strict subset of the new tasks-as-index schema,
-- and its "coordination" framing will be rebuilt on task blocks in Plan 3 (v3-subagents).
DROP TABLE coordination_tasks;

-- Extend existing tasks table
ALTER TABLE tasks ADD COLUMN block_handle TEXT;         -- the TaskList block containing this task
ALTER TABLE tasks ADD COLUMN task_item_id TEXT;         -- task id within the block (Snowflake, base32-encoded)
ALTER TABLE tasks ADD COLUMN owner_agent_id TEXT;       -- owner (new; distinct from legacy agent_id)
ALTER TABLE tasks ADD COLUMN comments_json TEXT NOT NULL DEFAULT '[]';
CREATE INDEX idx_tasks_block ON tasks(block_handle, task_item_id);
CREATE INDEX idx_tasks_owner ON tasks(owner_agent_id, status);

-- Drop unused columns that don't map to TaskItem.
-- `priority` has no index, foreign key, or trigger in 0001_initial.sql (verified) so DROP COLUMN is safe.
ALTER TABLE tasks DROP COLUMN priority;

-- Single-direction edges table (derived from loro task `blocks` fields).
-- task_item_id is guaranteed non-empty by the TaskItemId newtype, so we can use it
-- directly in the PK without COALESCE gymnastics. NULL distinguishes block-level refs.
CREATE TABLE task_edges (
    source_block TEXT NOT NULL,
    source_item  TEXT NOT NULL,                         -- never NULL — source is always a task item
    target_block TEXT NOT NULL,
    target_item  TEXT                                   -- NULL = block-level target reference
    -- No `kind` column — all edges are `blocks`; reverse direction is queried by swapping source/target.
    -- No `created_at` — derived tables don't invent timestamps; query from source block's updated_at if needed.
);
-- A plain rowid table (NOT `WITHOUT ROWID`) because SQLite requires an explicit
-- PRIMARY KEY on WITHOUT ROWID tables, and our natural key includes a nullable
-- `target_item` that cannot be a straight PRIMARY KEY column (NULL ≠ NULL under
-- PK constraints). The unique expression index below provides the dedup guarantee.
CREATE UNIQUE INDEX idx_task_edges_pk ON task_edges(
    source_block, source_item, target_block, COALESCE(target_item, '<block>')
);
CREATE INDEX idx_task_edges_source ON task_edges(source_block, source_item);
CREATE INDEX idx_task_edges_target ON task_edges(target_block, target_item);
```

**Uniqueness explanation:** The expression-index key is `source_block + source_item + target_block + COALESCE(target_item, '<block>')`. `source_item` is guaranteed non-empty by the `TaskItemId` newtype (validated at construction; empty strings rejected). `target_item` uses `'<block>'` sentinel for block-level references because `'<block>'` is not a valid Snowflake/base32 id — collision impossible by construction.

Any remaining references to `coordination_tasks` in `pattern_db/src/queries/coordination.rs` are either absorbed into `queries/task.rs` (if they serve the new task model) or removed with a `// REPLACED BY: queries/task.rs` fate marker.

### Subscriber extension for TaskList blocks

The per-doc `sync_worker` from Plan 1 already emits canonical files + updates FTS5/vector indexes on block commits. Plan 2 extends the worker's dispatch: when the committed block has schema `TaskList`, the worker additionally reconciles the `tasks` and `task_edges` index tables against the block's current loro state.

```
sync_worker loop for TaskList blocks (new dispatch):
  on commit event:
    1. export .kdl file                              (existing)
    2. update FTS5 row for the block                 (existing)
    3. queue vector re-embed if hash changed         (existing)
    4. NEW: diff loro task items vs tasks rows;
            upsert changed items, delete removed
    5. NEW: diff THIS block's task items' `blocks` fields vs
            task_edges rows where source_block = this_block;
            upsert + delete to match. Only outgoing edges from
            tasks in this block are touched.
    6. heartbeat
```

Steps 4 and 5 run inside a single `rusqlite::Transaction` — task rows and edge rows commit or revert together. Because edges are stored single-direction (source-side only), reconciling one block only affects `task_edges` rows WHERE source_block = this block. Edges originating from other blocks aren't touched, so there's no race between subscribers operating on different blocks.

If the subscriber is lagged, queries against the index see stale state for up to the debounce window (50ms). Same eventual-consistency property as any Plan 1 block type.

Skill blocks do NOT get a new index table — they're discoverable via the existing block search (FTS5 over name / description / keywords / body) using the standard `MemoryStore::search` surface. `SkillInfo` is constructed from block metadata at query time.

### Graph semantics

Edges are first-class loro state on each task item's `blocks` field (outgoing only). The `task_edges` table is a derived view. Two design consequences:

- **Meaning emerges from use.** Runtime doesn't constrain what an edge means beyond the directional relation ("source blocks target" / "source is blocked by target"). Edges can reference any block type: a task blocked by a text-block design document, a task blocking a skill block until it's reviewed, tasks blocking other tasks (the common case). Agents interpret the semantics.
- **No topology limits in v1.** No max depth, no max in/out-degree, no cycle detection. Rely on natural bounds (memory limits, agent good sense, eventual query-cost observation) for this plan. If observed problems accumulate, add limits in a follow-up plan informed by real usage data.

### Relationship to pattern-nd (human task tracking)

These agent-oriented `TaskList` blocks are for agent work tracking — coordination, plan progress, handoff state. They are NOT for tracking a human user's personal to-do list. That concern (especially in the ADHD-support context pattern was originally built for) is a separate design problem handled later, likely in the `pattern-nd` plugin territory. The TaskList schema and `ctx.tasks.*` surface are narrowly scoped to agent use.

## Existing Patterns

**Preserved patterns**:

- **pattern_memory crate architecture** from Plan 1 is the substrate. `BlockSchema` extension is the intended extensibility axis; Task and Skill land as additions without new crate boundaries.
- **Loro-primary storage with derived sqlite indexes** from Plan 1 applies unchanged. Task index + edges table are derived; loro is canonical.
- **Per-doc subscriber topology** from Plan 1 extends to TaskList dispatch without structural changes. The lazy-spawn + lifecycle-tied-to-doc + bounded-channel + supervisor-watchdog properties carry through.
- **KDL serialization** from Plan 1 is the canonical format for structured block content. TaskList uses KDL; the converter extends cleanly to the TaskList shape via typed `(block)` annotations.
- **Markdown + YAML frontmatter** is a canonical format already established for Plan 2 (locked during Plan 1 brainstorming). Skill blocks use this format.
- **MemoryScope isolate_from_persona policy** from Plan 1 applies to TaskList and Skill blocks automatically — they're blocks like any other; scope routing respects them without new code.
- **SDK effect pattern** (`Pattern.Memory`, `Pattern.Diagnostics` from Plan 1) extends to `Pattern.Tasks` and `Pattern.Skills` via the same freer-simple algebra + Rust handler pattern.
- **Existing `tasks` + `coordination_tasks` db schema**: the existing-but-unused tables in `0001_initial.sql` inform the index shape. The user's framing — "there's already some tasks stuff IN the db schema that's not used, that helps the surface" — motivates the repurposing rather than greenfield design.
- **Pattern v3 foundation's three-segment cache layout**: Skill `load` injects content into segment 2 as a pseudo-message, matching the Plan 1 pattern for `[memory:updated]` / `[memory:written]` markers.

**Divergences from current code**:

- New `BlockSchema` variants added. Existing call sites that match on `BlockSchema` (rendering, validation) need updating — most are in `pattern_memory` itself; known-affected files listed per-phase.
- `coordination_tasks` table dropped. Any in-flight code referencing it is removed or rewired.
- YAML parser dep added (saphyr). New to the workspace; verify it doesn't conflict with other transitive deps.

**Patterns not applicable**:

- **Plugin-installed skill provenance** (trust tier validation): not a current pattern because plugin-installed is reserved for Plan 4. Enum value present; no code path constructs or validates it.
- **Auto-loading skills based on context** (retrieval-augmented-style): not applicable in this plan. Explicit load only. Future exploration in Plan 4 or a dedicated plan informed by observed skill usage patterns.

## Implementation Phases

Five phases. Sequential dependency chain. Each ships its own tests as part of its DoD per project guidance — no standalone "testing phase" at the end.

<!-- START_PHASE_1 -->
### Phase 1: TaskList schema + KDL serialization

**Goal:** `BlockSchema::TaskList` variant in place; all TaskItem types defined; KDL converter extended to handle the TaskList shape with round-trip fidelity.

**Components:**
- Add `BlockSchema::TaskList { default_owner, default_status, display_limit }` variant to `pattern_core::types::memory_types`
- Types: `TaskItem`, `TaskStatus`, `TaskComment`, `BlockRef`, `TaskItemId` (SmolStr alias)
- Extend `pattern_memory/src/fs/kdl.rs` LoroValue↔KdlDocument converter to handle TaskList's nested structure: `task-list` node with `item` children, typed `(block)` entries for `BlockRef`, nested `metadata` + `comments` subtrees
- `BlockRef` KDL representation: `(block)"<handle>"` for block-level, `(block)"<handle>#<item_id>"` for item-specific
- **Ensure `BlockSchema` carries `#[non_exhaustive]`** — forces all downstream match sites to have a catch-all arm, so adding new variants fails LOUDLY at compile time at every site that needs updating (not silently panicking at runtime). Verify the attribute is present; add if missing.
- Update all `BlockSchema` match sites to handle the new `TaskList` variant (and, via Phase 4, `Skill`). Investigation prior to implementation enumerates the full match-site surface — grep `match .*BlockSchema` across all active crates; expected sites include `pattern_memory::document` rendering, `pattern_memory::cache` validation, `pattern_core::types` serde impls, `pattern_runtime::sdk::requests::memory` GADT bridge, `pattern_runtime::sdk::handlers::memory` dispatch. A complete site list is produced during Phase 1 execution and added to the implementation plan.

**Dependencies:** None in this plan; Plan 1 must be landed (pattern_memory crate exists)

**Done when:**
- `cargo check --workspace` passes
- Round-trip property tests (proptest) for TaskList ↔ KDL: generated arbitrary TaskList with nested items + edges + comments round-trips through .kdl → LoroValue → .kdl with content-equal output
- Specific edge-case tests: empty TaskList (zero items), TaskList with item containing all-default-null fields, TaskList with item referencing its own block (self-edge), TaskList with movable-list reorder preserving item ids across round-trip
- BlockRef parsing: both `"<handle>"` and `"<handle>#<item>"` forms parse; malformed forms produce clear errors
- Covers: `v3-task-skill-blocks.AC1.*`
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Task block index tables + subscriber extension

**Goal:** `tasks` + `task_edges` index tables populated by the subscriber on TaskList block commits. `coordination_tasks` deprecated.

**Components:**
- Migration `crates/pattern_db/migrations/memory/0012_task_block_index.sql`: extend `tasks` table with block-provenance columns; add `comments_json` column; drop `priority`; create `task_edges` table; drop `coordination_tasks`
- Remove `pattern_db/src/queries/coordination.rs`; absorb any query forms still needed into `queries/task.rs`
- Rewrite `queries/task.rs` against the extended schema: `upsert_task_row`, `delete_task_row`, `upsert_task_edges`, `delete_task_edges`, `list_tasks_filtered`, `query_task_graph_bfs`
- `FromSql` / `ToSql` impls for `TaskStatus` (text column)
- `from_row` impls for `TaskRow`, `TaskEdgeRow`
- Extend `pattern_memory/src/subscriber/task.rs` sync_worker dispatch: on TaskList block commit, after the existing emit + FTS5 + vector work, reconcile `tasks` + `task_edges` inside a single `rusqlite::Transaction`
- Subscriber scope-awareness: TaskList writes respect `MemoryScope::isolate_from_persona` like any block write

**Dependencies:** Phase 1 (TaskList schema + KDL round-trip working)

**Done when:**
- `cargo check --workspace` passes
- `cargo nextest run -p pattern-db` passes all existing + new task-index tests
- Migration round-trip test: fixture DB with pre-migration rows in `tasks` migrates cleanly; old `coordination_tasks` data is discarded with a documented note
- Subscriber integration test: write a TaskList with 5 items + 3 edges, assert `tasks` rows match loro items; delete an item, assert row removed; modify an edge, assert `task_edges` reflects change
- Atomicity test: intentionally fail mid-subscriber-reconcile, assert partial transaction rolled back (neither tasks nor edges show the half-applied state)
- FTS5 task-content snapshot test (insta): representative task subjects + descriptions produce stable BM25 output
- Scope enforcement test: TaskList block written in a project with `CoreOnly` persona isolation respects the policy (write succeeds in project scope, doesn't bleed into persona)
- Covers: `v3-task-skill-blocks.AC2.*`, `v3-task-skill-blocks.AC3.*`
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: `ctx.tasks.*` SDK surface

**Goal:** Agent programs can create / query / mutate tasks and traverse the dependency graph via the new SDK module.

**Components:**
- Haskell SDK module `Pattern.Tasks` in `pattern_runtime`'s SDK resource directory: types (`TaskSpec`, `TaskPatch`, `TaskFilter`, `TaskView`, `GraphSlice`, `Direction`); effect algebra entries for 8 methods
- `pattern_runtime/src/sdk/requests/tasks.rs`: GADT-bridge variants for each method
- `pattern_runtime/src/sdk/handlers/tasks.rs`: Rust handlers wrapping sync `MemoryStore` + `task_edges` queries
- `query_graph` BFS traversal implementation: starts from the given `BlockRef`, walks `task_edges` up to `Depth` hops in the specified direction, returns `GraphSlice { nodes, edges }`
- `list_tasks` filter implementation: translates `TaskFilter` into `SELECT ... FROM tasks WHERE ...` with FTS5 join when `keyword` is set
- `list_tasks` with `block=None`: scope-aware search across all TaskList blocks visible to the caller's `MemoryScope`

**Dependencies:** Phase 2 (index tables + subscriber populating them)

**Done when:**
- `cargo check --workspace` passes
- Unit tests for each SDK method against an in-memory `TestMemoryStore` fixture: create_task returns a valid item id, the task appears in list_tasks and in the index; update_task patches fields correctly; link / unlink add / remove edges atomically and reflect in `task_edges`
- Graph traversal test: construct a chain of 5 tasks connected via `blocks` edges; `query_graph(task_5, Direction::Reverse, unlimited)` returns all 5 nodes + 4 edges; `query_graph(task_5, Direction::Reverse, depth=2)` returns 3 nodes + 2 edges
- Edge consistency test: after `link(A, B)`, querying A shows B in its outgoing `blocks` list AND `task_edges WHERE target=B` returns A; removing the edge via `unlink(A, B)` removes both the loro list entry and the derived index row
- Scope enforcement test: agent with `CoreOnly` isolation sees only project-scope tasks via `list_tasks`; persona-scope tasks hidden
- `add_comment` appends to the task's comment list and shows up in the index's `comments_json`
- Covers: `v3-task-skill-blocks.AC4.*`, `v3-task-skill-blocks.AC5.*`
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Skill schema + md+YAML frontmatter

**Goal:** `BlockSchema::Skill` variant in place; markdown+YAML frontmatter round-trips to loro state; trust tier assignment implemented.

**Components:**
- Add `BlockSchema::Skill { expected_keys }` variant to the enum
- Types: `SkillMetadata`, `SkillTrustTier` (with `PluginInstalled` reserved variant)
- Cargo dep: add `saphyr` for YAML parsing (no serde_yaml; no serde_yml)
- `pattern_memory/src/fs/markdown_skill.rs`: md+frontmatter ↔ LoroValue converter. Body parses to `LoroText`; frontmatter YAML parses via saphyr to an AST and is mapped to `SkillMetadata` by a hand-written visitor (matching the LoroValue↔KdlDocument pattern from Plan 1)
- Trust tier assignment logic (`pattern_memory/src/skill.rs::assign_trust_tier`): inspects block provenance — from SDK resource dir → `FirstParty`; from `<mount>/skills/` or project scope → `ProjectLocal`; from runtime agent action → `AdHoc`; explicit `PluginInstalled` declaration in frontmatter preserved on round-trip but emits `metrics::counter!("skill.plugin_installed_tier_without_plugin_system")` warning
- Update `pattern_memory::document` rendering to handle the Skill schema

**Dependencies:** None on other Plan 2 phases (orthogonal to Tasks); Plan 1 landed

**Done when:**
- `cargo check --workspace` passes
- Round-trip property test for Skill blocks: arbitrary metadata + arbitrary markdown body round-trips through .md+frontmatter → LoroValue → .md+frontmatter with exact content equality
- Frontmatter parse edge-case tests: all-optional-null metadata, metadata with nested hooks, keywords list, unknown keys preserved in the loro state for round-trip even if not in `SkillMetadata`
- Trust tier assignment tests:
  - skill loaded from SDK resource dir → `FirstParty`
  - skill loaded from `<mount>/skills/foo.md` → `ProjectLocal`
  - skill block created at runtime via `MemoryStore::put_block` → `AdHoc`
  - skill with frontmatter declaring `trust_tier: "plugin-installed"` → preserved + metrics counter incremented
- Malformed frontmatter (invalid YAML, missing required keys) produces typed error pointing to the offending file + line
- FTS5 skill-content snapshot test: skill name + description + body produce stable search output
- Scope enforcement test: Skill block in project scope with `Full` isolation is invisible to persona-default sessions
- Covers: `v3-task-skill-blocks.AC6.*`, `v3-task-skill-blocks.AC7.*`
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: `ctx.skills.*` SDK surface + full integration smoke

**Goal:** Agents can list / get_metadata / load / search / get_usage_stats skills. End-to-end smoke validates the full Plan 2 feature surface.

**Components:**
- Haskell SDK module `Pattern.Skills`: types (`SkillInfo`, re-exported `SkillMetadata` + `SkillTrustTier` + `SkillUsageStats`); effect algebra entries for 5 methods
- `pattern_runtime/src/sdk/requests/skills.rs`: GADT-bridge variants
- `pattern_runtime/src/sdk/handlers/skills.rs`: Rust handlers wrapping `MemoryStore::search` (for `list` / `search`) + direct block reads (for `get_metadata` / `load`) + `pattern_db::queries::skill_usage::get_usage_stats` (for `get_usage_stats`)
- `load` handler: fetches Skill block's body via `MemoryStore::get_block`, emits a `[skill:loaded]` pseudo-message into the current turn's message-history-in-progress (segment 2 of the foundation plan's cache layout); records usage via a direct `pattern_db::queries::skill_usage::record_usage(tx, handle, agent, now)` call against the `skill_usage_stats` sqlite table — no LoroDoc mutation, no canonical-file write
- `list` handler: enumerates all Skill-schema blocks in scope via `MemoryStore::list_blocks(filter: BlockFilter::schema("skill"))`, constructs `SkillInfo` (joining `skill_usage_stats` for `last_used`) from metadata
- `search` handler: delegates to `MemoryStore::search(SearchScope::Schema(BlockSchemaKind::Skill))` with keyword query
- `get_usage_stats` handler: direct sqlite read via `pattern_db::queries::skill_usage::get_usage_stats`
- End-to-end smoke test deliverable: `crates/pattern_memory/tests/task_skill_smoke.rs` exercising both surfaces with a scripted mock-provider agent

**Dependencies:** Phase 4 (Skill schema landed)

**Done when:**
- `cargo check --workspace` passes
- SDK method unit tests: `list` returns expected SkillInfo for all skills in scope; `get_metadata` returns typed metadata with hook fields preserved; `search` returns matching skills by keyword; `load` injects correct [skill:loaded] marker into segment 2; `get_usage_stats` returns fresh `SkillUsageStats` from the sqlite table
- Snapshot test (insta) for `load`-produced pseudo-message: known skill → known marker + body content in composed request
- `last_used` + `last_used_by` + `use_count` recorded in the `skill_usage_stats` sqlite table on each `load` call; subsequent `get_usage_stats(handle)` returns fresh values (separate from `get_metadata`, which returns only author-defined content)
- **End-to-end smoke test passes deterministically in CI:** create a project mount, create a TaskList block with 5 tasks and 3 cross-block edges, call `ctx.tasks.create_task` + `update_task` + `transition_status` + `link` + `list_tasks` + `query_graph`, create a Skill block with structured frontmatter hooks, call `ctx.skills.list` + `get_metadata` + `load`, assert the composed model request contains the expected task state + loaded skill content; mock provider for deterministic output
- Scope enforcement smoke: skills in project scope with `Full` isolation don't appear in persona-scoped `list`
- Covers: `v3-task-skill-blocks.AC8.*`, `v3-task-skill-blocks.AC9.*`, `v3-task-skill-blocks.AC10.*`
<!-- END_PHASE_5 -->

## Execution Mode Recommendation

**Recommendation: Collaborative.**

Reasoning:

- **Novel SDK surfaces**: `ctx.tasks.*` and `ctx.skills.*` are new agent-facing effect algebras. Shape decisions (method signatures, error cases, effect ordering) benefit from human checkpoint — once these ship, downstream plans (especially Plan 3 subagents) will use them as primitives.
- **Graph semantics are deliberately under-specified in v1**. No runtime limits. This is a defensible choice but warrants human validation during implementation if real-world graphs surface problems (cycles that cause perf issues, depth blowups).
- **YAML parser introduction**: adding `saphyr` as a new workspace dep. User preference on YAML handling was explicit (no serde_yaml / serde_yml). Picking and validating saphyr during Phase 4 benefits from human check.
- **Trust tier semantics**: the split between "surfaces tier but doesn't enforce" (this plan) and "enforces via permission surface" (Plan 4) is a subtle policy line. The `PluginInstalled` reserved variant needs careful scope-discipline during implementation to avoid accidentally adding enforcement that should wait.
- **Smaller than Plan 1**: 5 phases, narrower scope. Not huge but substantial enough that autonomous execution would miss feedback-worthy junctions.

Not fully autonomous-safe due to the novel surface decisions; not small enough for Light-scope execution.

## Additional Considerations

**YAML parser choice: saphyr**

Pattern avoids `serde_yaml` (unmaintained as of 2026) and `serde_yml` (AI-assisted port with quality concerns). Saphyr is a pure-Rust YAML 1.2 parser with active maintenance. It parses to an AST; Plan 2's frontmatter handler walks that AST to populate `SkillMetadata` via a hand-written visitor, matching the LoroValue↔KdlDocument approach from Plan 1 (no serde derives for the canonical types). This choice is locked during Phase 4's implementation but confirmed in the plan here.

**Graph topology under concurrent writes**

Edges are loro state, stored single-direction on the source task's `blocks` field. Concurrent edge additions by multiple agents merge via loro's CRDT list semantics. In the rare case where two agents simultaneously `link(A, B)` and `unlink(A, B)`, loro's deterministic merge lands one outcome; the subscriber reconciles the index accordingly. No additional synchronization needed at the SDK level.

**Single-source-of-truth edge model**

An edge `A blocks B` is stored ONLY on A's task item (`A.blocks += B`). B does NOT have a `blocked_by` field in the loro data — the reverse relation is computed at query time by scanning `task_edges WHERE target_block = B_block AND target_item = B_item`. This eliminates the cross-document commit atomicity problem: `link(A, B)` is a single commit on A's LoroDoc regardless of whether B is in the same or a different TaskList block. If A and B are in different blocks, only A's block is modified; B's block is not touched by the edge operation. Subscribers reconcile independently without racing.

**TaskItemId generation**

`TaskItemId` wraps SmolStr with a constructor that forbids empty strings. `TaskItemId::new()` delegates to the workspace's `new_snowflake_id` generator (ferroid's Mastodon-style Snowflake, base32-encoded, lexicographically sortable). Two agents independently calling `create_task` on the same block at the same wall-clock millisecond will produce distinct ids — ferroid's `AtomicSnowflakeGenerator` disambiguates within a millisecond via its atomic sequence counter.

**Graph traversal cap**

`ctx.tasks.query_graph` enforces both a depth limit (default 16 hops) and a node count cap (default 1000 nodes). Exceeding the node cap truncates the BFS frontier and sets `truncated: true` in the result; callers can re-query with a different root or larger cap. This prevents runaway traversal of pathological graphs (agent loops creating tasks in cycles, dense dependency meshes) from pinning a `spawn_blocking` thread indefinitely. Defaults configurable per-mount via `.pattern.kdl` in a future enhancement; hardcoded in Plan 2 for simplicity.

**Skill usage stats separation**

`SkillMetadata` (author-defined content) and `SkillUsageStats` (runtime-captured usage) are separate types and separate storage. `SkillMetadata` serializes to the canonical `.md` frontmatter (replicated content). `SkillUsageStats` lives ONLY in a dedicated `skill_usage_stats` sqlite table (handle-keyed, per-local-install observability). This separation has two rationales: (1) usage stats aren't replicated content — divergent counts between nodes shouldn't merge via CRDT; (2) keeping stats out of the LoroDoc and canonical file means the `.md` content hash stays stable across loads, so the Plan 1 echo-suppression path never sees a spurious change and the file stays VCS-clean across any number of agent loads. Earlier drafts put `SkillUsageStats` partly in the LoroDoc; that forced a "skip this subtree" emitter carve-out plus a new MemoryStore mutation hook. The sqlite-only approach sidesteps both.

**Skill `load` caching**

Skills loaded mid-turn join segment 2 of the model request. Subsequent turns see the skill in history (it's a real message now). If the agent re-loads the same skill later, a second [skill:loaded] marker appears in segment 2 — no deduplication in v1. Agents can manage this explicitly via `list` + `get_metadata` checks before calling `load`. Future optimization: a `loaded_skills` set in session state that `load` consults to skip re-injection. Not in scope.

**Relationship to human task tracking (pattern-nd)**

TaskList blocks and `ctx.tasks.*` are scoped to agent work tracking. Human-user-facing task management (ADHD support, calendar integration, external reminders) is a separate design concern handled by the `pattern-nd` plugin territory or its successor. Plan 2's primitives can potentially be reused by pattern-nd, but pattern-nd's UI, notification behavior, and scheduling model are out of scope here.

**`coordination_tasks` deprecation**

The existing `coordination_tasks` table (initial schema, since unused) is dropped in Phase 2's migration. Its columns (description / assigned_to / status / priority) are a strict subset of the new tasks-as-index schema. The "coordination" framing was agent-delegation work that Plan 3 (v3-subagents) will rebuild on task blocks — specifically, delegation becomes `link(parent_task, child_task)` with `owner` set to the delegated subagent. No data migration needed (table was unused); drop is clean.

**Testing philosophy reminder**

Per project guidance at `.orual/design-plan-guidance.md`: tests live inside each phase's DoD. There is no standalone "testing phase" at the end of this plan. The end-to-end smoke test is a deliverable of Phase 5 (the last feature-delivering phase) alongside Phase 5's own SDK-surface tests. FTS5 and scope regression tests land in the phases that add the relevant features (Phase 2 for tasks, Phase 4 for skills).

**Intermediate code-state policy (carryover)**

Fate markers apply to any transitional code. Cruft (undefined fate, commented-out code, orphaned `unimplemented!()`) fails the intermediate-state audit at each phase boundary. Port-list doc updated if `coordination_tasks` deprecation introduces any transitional state across phases.
