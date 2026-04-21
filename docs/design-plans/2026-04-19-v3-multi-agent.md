# Pattern v3 Multi-Agent System Design

## Summary
<!-- TO BE GENERATED after body is written -->

## Definition of Done

Plan 3 in the Pattern v3 rewrite sequence. Builds spawn primitives, coordination patterns, a capability-based permission system, and the fronting persona concept on top of Plans 1 (memory rework) and 2 (task+skill blocks). The plan is done when:

### Spawn primitives

- Three spawn modes implemented in `pattern_runtime` via a fully functional `spawn.rs` handler (replacing the current stub):
  - **Ephemeral** — short-lived worker with configurable costume, read/write block access, authority level (Advisory/Gating), timeout, and writeBack handles. No persistent identity; attributed to parent in logs.
  - **Fork** — snapshots parent's logical session state + memory refs. Gets its own runtime session with isolation appropriate to expected duration (short-lived forks may use in-memory CRDT branching; long-lived forks use jj workspaces). Resolution options: await_result, merge_back, discard, promote-to-sibling, checkpoint/rollback.
  - **Sibling** — distinct persona with own identity, memory root, history. Structured spawn config with relationship type, coordination group, shared blocks with per-block permissions.
- Sub-spawns inside ephemeral/fork die when parent resolves (implicit lifetime rule).
- `ctx.spawn.ephemeral(...)`, `ctx.spawn.fork(...)`, `ctx.spawn.sibling(...)` SDK surface fully functional.

### Fork isolation

- Fork isolation model determined by expected fork duration (or explicit override):
  - Short-lived forks: lightweight isolation without jj workspace overhead
  - Long-lived forks: jj workspace in the same repo (cheap, shared commit store, independent working copies, namespaced bookmarks)
- Fork resolution (merge_back) handles memory diffs correctly via loro CRDT merge or jj commit merge depending on isolation model.
- Promote-to-sibling converts a fork into a new persistent persona, inheriting memory state at promotion point.

### Coordination patterns (reworked)

- Two coordination substrates, appropriate to different multi-agent shapes:
  1. **Task-based delegation** — for ephemeral agent work. Spawn agent, assign task via `ctx.tasks.*`, agent completes task and dies. Pipeline and round-robin patterns map here (chain workers, distribute work items). Tasks are also available as async structured requests between any agents (persistent or ephemeral) — "hey, do this when you get to it" with structure and feedback.
  2. **Fronting/routing** — for persistent persona coordination. Supervisor pattern = permanently fronting agent that routes incoming messages to specialists. Direct addressing of any agent still possible. Co-fronting supported.
- Old coordination pattern enum/types from staging either rebuilt on these substrates or explicitly deprecated with rationale.
- Coordination groups track membership, relationships, and routing rules.

### Capability-based permission system

- `CapabilitySet` per agent/session defines what SDK effects are available.
- Two-layer gating:
  1. **Effect visibility** — which effects are even compiled into the agent's sandbox SDK for a given persona/project/context. Effects not in the set are invisible (not "denied", just absent).
  2. **Runtime approval** — for effects that are available but gated. Agent requests use, human approves one-shot, indefinitely, or scoped to project. Existing `PermissionBroker` infrastructure wired in for this.
- Scope: per-persona + per-project configurable. Directory permissions, shell command gating, and MCP server access fold into the capability model (or as sub-schemas).
- Tasks flow through the capability system but default permissive.
- Memory ACL (`MemoryPermission`) remains as-is; capability system layers on top for spawn/tool/MCP/shell gating.

### Fronting persona

- Runtime tracks current fronting set (usually one; co-fronting allowed).
- Supervisor-as-permanent-front is a natural expression of the fronting concept — supervisor fronts, routes to specialists, specialists can be addressed directly.
- `ctx.caller = Human(user_id) | Agent(persona_id)` discriminant in all effect handlers.
- Human invocations use fronting persona's Ctx (workspace, project mount, memory handles inherited).
- Human short-circuits permission/policy gate by virtue of being human.
- Invocations logged and visible in fronting persona's turn history.

### Identity authorization

- New-identity siblings require authorization via either:
  - Capability flag on spawner (`can_spawn_new_identities`, default off)
  - Draft state: fresh-identity spawns default to "draft" (exist but can't take actions until user promotes)
- Adopting/waking existing personas doesn't need authorization.
- Ephemeral/fork unaffected (no new identity created).

### Discovery

- Agent/group registry for "who's in my constellation / who's on project X".
- Sibling spawn auto-registers in the registry.

### Testing

- Deterministic tests for all spawn modes (ephemeral lifecycle, fork isolation + merge, sibling identity)
- Coordination pattern integration tests (task-based delegation end-to-end, fronting/routing with message delivery)
- Capability system unit tests (visibility filtering, approval flow, scope inheritance)
- Fork-merge memory consistency tests (loro CRDT merge, jj workspace merge)
- No live-model dependency in CI paths

### Explicitly OUT OF SCOPE (deferred to Plan 4 or later)

- Plugin system, MCP inverted surface, iroh-rpc transport
- Trust enforcement code paths for plugin-installed skills (Plan 4 consumes capability system)
- Hook lifecycle system
- TUI rendering of fronting state (pattern_cli territory)
- WASM transport, plugin marketplace
- Cross-device constellation coordination

### Context

This is the fourth design plan in the Pattern v3 rewrite sequence. Builds on:

- `docs/design-plans/2026-04-16-v3-foundation.md` (foundation — Tidepool runtime, provider, three-segment cache)
- `docs/design-plans/2026-04-19-v3-memory-rework.md` (Plan 1 — pattern_memory crate, rusqlite, fs-canonical storage, MemoryScope, modes A/B/C)
- `docs/design-plans/2026-04-19-v3-task-skill-blocks.md` (Plan 2 — TaskList + Skill block subtypes, `ctx.tasks.*` and `ctx.skills.*` SDK surfaces)
- `docs/plans/2026-04-16-rewrite-v3-design-draft.md` §4 (subagent primitives brainstorm)

Plan 4 follows: `v3-extensibility` — CC-compatible plugin system, MCP inverted surface, iroh-rpc transport, trust enforcement

## Acceptance Criteria
<!-- TO BE GENERATED and validated before glossary -->

## Glossary
<!-- TO BE GENERATED after body is written -->
