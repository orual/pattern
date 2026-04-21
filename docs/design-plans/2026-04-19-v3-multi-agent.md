# Pattern v3 Multi-Agent System Design

## Summary

Pattern's multi-agent system gives each AI agent in the constellation the ability to spawn, communicate with, and coordinate alongside other agents — turning what was a single-agent framework into a network of cooperating personas. This plan builds three capabilities on top of the existing session and memory infrastructure: a spawn system with three distinct modes (ephemeral workers, memory-forked variants, and fully independent sibling personas), a message-passing mailbox so agents can talk to each other and be woken by conditions rather than only by human input, and a fronting/routing layer that determines which persona a human is actually talking to at any given moment. The agent registry ties these together by tracking the full constellation of personas, their relationships, and their current status.

The approach deliberately avoids building a monolithic coordination abstraction. Instead, two composable primitives handle the two distinct coordination problems: task-based delegation (using ephemeral spawn plus the task system from Plan 2) for short-lived parallel work, and the fronting set with a routing table for persistent persona coordination. Patterns like "supervisor routes to specialists" or "fan-out across workers" emerge from configuring these primitives rather than from a dedicated coordination type. Permission and trust enforcement are handled by a two-layer capability system — effect categories are filtered out of an agent's Haskell compilation environment entirely if not granted, and runtime approval (via a per-instance permission broker) gates individual sensitive operations. The implementation is designed to be testable without a live language model, using a mock provider throughout.

## Definition of Done

Plan 3 in the Pattern v3 rewrite sequence. Builds spawn primitives, coordination patterns, a capability-based permission system, and the fronting persona concept on top of Plans 1 (memory rework) and 2 (task+skill blocks). The plan is done when:

### Spawn primitives

- Three spawn modes implemented in `pattern_runtime` via a fully functional `spawn.rs` handler (replacing the current stub):
  - **Ephemeral** — short-lived worker with configurable costume (prompt/style template, not persistent identity), CapabilitySet (inherited from parent, can only restrict), and timeout. No persistent identity; attributed to parent in logs.
  - **Fork** — snapshots parent's logical session state + memory refs. Gets its own runtime session with isolation appropriate to expected duration (short-lived forks use in-memory CRDT branching; long-lived forks use jj workspaces). CapabilitySet inherited from parent (can only restrict). Resolution options: await_result, merge_back, discard, promote-to-sibling, checkpoint/rollback.
  - **Sibling** — distinct persona with own identity, memory root, history. Structured spawn config with relationship type and shared blocks. CapabilitySet from the sibling's own persona config (not inherited from spawner).
- Sub-spawns inside ephemeral/fork die when parent resolves (implicit lifetime rule).
- `ctx.spawn.ephemeral(...)`, `ctx.spawn.fork(...)`, `ctx.spawn.sibling(...)` SDK surface fully functional.

### Fork isolation

- Fork isolation model determined by expected fork duration (or explicit override):
  - Short-lived forks: lightweight isolation without jj workspace overhead
  - Long-lived forks: jj workspace in the same repo (cheap, shared commit store, independent working copies, namespaced bookmarks)
- Fork resolution (merge_back) handles memory diffs correctly via loro CRDT merge or jj commit merge depending on isolation model.
- Promote-to-sibling converts a fork into a new persistent persona, inheriting memory state at promotion point.

### Coordination patterns (reworked)

- Two coordination substrates as independent composable primitives:
  1. **Task-based delegation** — for ephemeral agent work. Spawn agent, assign task via `ctx.tasks.*`, agent completes task and dies. Tasks also available as async structured requests between any agents (persistent or ephemeral). Delegation patterns (round-robin, pipeline, fan-out) live as Haskell library modules and/or skills, NOT Rust types — the runtime provides spawn + tasks primitives, patterns compose them at the agent level. A starter set ships with the runtime as importable Haskell code.
  2. **Fronting/routing** — for persistent persona coordination. FrontingSet is an independent runtime primitive tracking who's fronting, with a RoutingTable for message dispatch. Supervisor pattern = permanently fronting agent that routes to specialists. Direct addressing of any agent still possible. Co-fronting supported. FrontingSet persists to DB, survives restarts.
- Old coordination pattern enum/types from staging deprecated — coordination is expressed through these two primitives, not through a unified CoordinationPattern enum.
- Concurrency limits enforced at Rust level (max N concurrent ephemeral spawns per session, configurable).

### Agent mailbox and communication

- Each agent has an inbox (tokio mpsc channel) watched by a background tokio task.
- When a message arrives and the agent isn't mid-turn, it steps with that input.
- Agent-to-agent messages flow through `ctx.message.send(to, content)` → MessageRouter → target agent's mailbox.
- Task assignment via delegation pins the task in the target agent's working memory.
- Wake conditions beyond "message received" (baseline, always present):
  - Rust primitives: `TaskTimeout(Duration)`, `TaskDependencyResolved(BlockRef)`, `BlockChanged(BlockHandle)`, `Interval(Duration)`
  - Custom Haskell programs: compiled once, run repeatedly via full effects engine. Registered via `ctx.wake.register(condition)` — capability-gated (`WakeConditionRegistration` capability, most agents don't have it).
  - When a condition fires, agent gets poked with a `WakeReason` in its TurnInput.
- Haskell wake condition evaluation model: full effects engine, compiled once and long-lived, either using the agent's existing eval worker or a dedicated one. Detailed mechanics deferred to implementation (depends on how Tidepool handles concurrent evaluation on the same compiled program).

### Capability-based permission system

- `CapabilitySet` per agent/session defines what SDK effects are available.
- Two-layer gating:
  1. **Effect visibility** — which effects are even compiled into the agent's sandbox SDK for a given persona/project/context. Implemented via Haskell prelude filtering at session open: effects not in the set have their GADT declarations excluded, so the agent's code cannot even reference them (compile-time enforcement). Effects not in the set are invisible (not "denied", just absent).
  2. **Runtime approval** — for effects that are available but gated. Policy rules from three sources: Rust defaults (conservative), KDL config per persona/project (can loosen or tighten from defaults), runtime PermissionBroker (human overrides per invocation — approve once, for scope, for duration, or deny). Agents cannot self-modify policy.
- Scope: per-persona + per-project configurable. Directory permissions, shell command gating, and MCP server access fold into the capability model (or as sub-schemas).
- Tasks flow through the capability system but default permissive.
- Memory ACL (`MemoryPermission`) remains as-is; the permission system encompasses both capability gating and memory ACL as complementary mechanisms within a unified system.
- Config file protection: writes to files that parse as pattern config KDL are always gated (shape-based detection, false positives preferred over false negatives). This is a Rust default that cannot be loosened by config.
- PermissionBroker rebuilt as per-runtime instance (not global singleton), using jiff instead of chrono.

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

### Discovery and registry

- Flat persona registry in pattern_db: id, status (active/draft/inactive), config path, capabilities, project attachments.
- Relationship edges (from, to, kind: supervisor-of, specialist-for, peer-with, observer-of).
- Named groups for organizational purposes (not a coordination mechanism — just logical grouping).
- Project-scoped groups and relationships.
- Sibling spawn auto-registers in the registry.
- Discovery via `ctx.constellation.list()`, `ctx.constellation.find(project, relationship)`.

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

### v3-multi-agent.AC1: CapabilitySet and prelude filtering

- **v3-multi-agent.AC1.1 Success:** `CapabilitySet` with `[Memory, Message, Tasks]` produces a prelude containing only those effect GADTs; `Spawn`, `Shell`, `Wake` constructors are absent from the generated Haskell source
- **v3-multi-agent.AC1.2 Success:** Agent program referencing an excluded effect (e.g., `ctx.shell.execute`) fails at Tidepool compilation with a clear "unknown constructor" error, not a runtime error
- **v3-multi-agent.AC1.3 Success:** Agent program using only included effects compiles and executes normally
- **v3-multi-agent.AC1.4 Success:** `CapabilitySet::all()` produces a prelude identical to the unfiltered `canonical_effect_decls()` output
- **v3-multi-agent.AC1.5 Failure:** Attempting to construct a CapabilitySet that adds capabilities not present in the parent's set (for ephemeral/fork) returns `CapabilityError::Escalation`
- **v3-multi-agent.AC1.6 Edge:** Empty CapabilitySet (no effects) produces a prelude with only base types and no effect constructors; agent can still compile a program that does pure computation

### v3-multi-agent.AC2: Runtime approval and policy

- **v3-multi-agent.AC2.1 Success:** Rust default policy gates destructive shell commands (`rm -rf`, `sudo`); agent with Shell capability gets `PermissionRequired` on these commands
- **v3-multi-agent.AC2.2 Success:** KDL config loosens a Rust default (e.g., allows `git push` without gating); agent executes the command without broker intervention
- **v3-multi-agent.AC2.3 Success:** KDL config tightens beyond defaults (e.g., gates all file writes, not just config files); agent gets `PermissionRequired` on any file write
- **v3-multi-agent.AC2.4 Success:** PermissionBroker approve-once allows the specific invocation; subsequent identical invocation is gated again
- **v3-multi-agent.AC2.5 Success:** PermissionBroker approve-for-scope allows all invocations matching the scope pattern until session ends
- **v3-multi-agent.AC2.6 Success:** PermissionBroker approve-for-duration allows invocations for the specified jiff duration; invocation after expiry is gated again
- **v3-multi-agent.AC2.7 Failure:** Agent attempts to write a file that parses as pattern config KDL; write is gated regardless of KDL config settings (Rust default, cannot be loosened)
- **v3-multi-agent.AC2.8 Failure:** PermissionBroker request times out (no human response); effect returns denial, not hang
- **v3-multi-agent.AC2.9 Edge:** PermissionBroker is per-runtime instance; two runtime instances have independent broker state and pending request queues

### v3-multi-agent.AC3: Ephemeral spawn

- **v3-multi-agent.AC3.1 Success:** `ctx.spawn.ephemeral(config)` creates a new TidepoolSession with a separate EvalWorker thread; the ephemeral executes its program and returns a result to the parent
- **v3-multi-agent.AC3.2 Success:** Ephemeral's CapabilitySet is a subset of parent's; prelude filtering reflects the restricted set
- **v3-multi-agent.AC3.3 Success:** Ephemeral with costume has its system prompt override set to the costume's content; persona identity remains the parent's in logs
- **v3-multi-agent.AC3.4 Success:** Ephemeral timeout fires; session is cancelled; parent receives a timeout error, not a hang
- **v3-multi-agent.AC3.5 Success:** Concurrent ephemeral count respects the configured semaphore limit; attempt to exceed returns a clear error
- **v3-multi-agent.AC3.6 Failure:** Parent session resolves (completes or errors); all child ephemeral sessions are cancelled; no orphaned EvalWorker threads remain
- **v3-multi-agent.AC3.7 Edge:** Ephemeral spawning its own ephemeral (nested); grandchild dies when child dies, child dies when parent resolves — full lifetime chain

### v3-multi-agent.AC4: Fork spawn and isolation

- **v3-multi-agent.AC4.1 Success:** `ctx.spawn.fork(ForkConfig { isolation: Lightweight, .. })` creates a session with a forked LoroDoc; parent and fork can write to their respective memory states independently
- **v3-multi-agent.AC4.2 Success:** `ctx.spawn.fork(ForkConfig { isolation: Persistent, .. })` creates a jj workspace via the jj adapter; fork's memory changes appear in the workspace's working copy
- **v3-multi-agent.AC4.3 Success:** `fork.merge_back()` on a lightweight fork imports the fork's LoroDoc state back into the parent via `LoroDoc::import()`; changes from both parent and fork are merged
- **v3-multi-agent.AC4.4 Success:** `fork.merge_back()` on a persistent fork performs jj merge + loro CRDT merge; parent's working copy reflects the merged state
- **v3-multi-agent.AC4.5 Success:** `fork.discard()` on a lightweight fork drops the forked LoroDoc; no state changes propagate to parent
- **v3-multi-agent.AC4.6 Success:** `fork.discard()` on a persistent fork runs `jj workspace forget` on the fork's workspace; bookmark deleted
- **v3-multi-agent.AC4.7 Success:** `fork.promote(persona_config)` creates a new persona config, registers as Draft in the registry, inherits the fork's memory state
- **v3-multi-agent.AC4.8 Failure:** `fork.promote()` without `SpawnNewIdentities` capability returns `CapabilityError::Denied`
- **v3-multi-agent.AC4.9 Edge:** Concurrent writes in parent and lightweight fork to the same block merge deterministically via loro CRDT semantics (both changes preserved, no data loss)
- **v3-multi-agent.AC4.10 Edge:** Persistent fork bookmark is namespaced as `<agent-id>/<task-id>`; no collision with other forks or bookmarks

### v3-multi-agent.AC5: Sibling spawn and identity authorization

- **v3-multi-agent.AC5.1 Success:** `ctx.spawn.sibling(SiblingConfig { persona: Existing(id), .. })` opens a session for the existing persona; no authorization required
- **v3-multi-agent.AC5.2 Success:** `ctx.spawn.sibling(SiblingConfig { persona: New(config), .. })` with `SpawnNewIdentities` capability creates the persona and opens its session
- **v3-multi-agent.AC5.3 Success:** `ctx.spawn.sibling(SiblingConfig { persona: New(config), .. })` without `SpawnNewIdentities` capability creates persona config as Draft; no session opened; returns the draft PersonaId
- **v3-multi-agent.AC5.4 Success:** Sibling session's CapabilitySet comes from its own persona config, not from the spawner's CapabilitySet
- **v3-multi-agent.AC5.5 Success:** Sibling auto-registers in the agent registry with specified relationship type
- **v3-multi-agent.AC5.6 Failure:** Sibling spawn referencing a nonexistent PersonaId returns `RegistryError::PersonaNotFound`
- **v3-multi-agent.AC5.7 Edge:** Draft persona appears in `ctx.constellation.list()` with `status: Draft`; calling `ctx.message.send` to a Draft persona queues the message (delivered when promoted)

### v3-multi-agent.AC6: Agent mailbox and message delivery

- **v3-multi-agent.AC6.1 Success:** `ctx.message.send(persona_id, content)` delivers to target agent's mailbox; target steps with the message as TurnInput when idle
- **v3-multi-agent.AC6.2 Success:** Message sent to a busy agent (mid-turn) is queued; delivered after current turn completes
- **v3-multi-agent.AC6.3 Success:** Task assignment via delegation pins the task's BlockRef into the target agent's memory snapshot selection; agent sees the task in its context
- **v3-multi-agent.AC6.4 Failure:** `ctx.message.send` to a nonexistent PersonaId returns `RouterError::PersonaNotFound`
- **v3-multi-agent.AC6.5 Failure:** `ctx.message.send` to a Draft persona queues successfully but does not trigger a step (no session exists)
- **v3-multi-agent.AC6.6 Edge:** Rapid sequential messages to the same agent queue correctly; all delivered in order; no message loss under concurrent sends from multiple agents

### v3-multi-agent.AC7: Wake conditions

- **v3-multi-agent.AC7.1 Success:** `TaskTimeout(30s)` condition fires after 30 seconds if the agent's active task hasn't completed; agent receives TurnInput with `WakeReason::TaskTimeout`
- **v3-multi-agent.AC7.2 Success:** `BlockChanged(handle)` condition fires when the specified block is modified (by any agent); agent receives `WakeReason::BlockChanged(handle)`
- **v3-multi-agent.AC7.3 Success:** `TaskDependencyResolved(ref)` condition fires when the referenced task transitions to Completed; agent receives `WakeReason::DependencyResolved(ref)`
- **v3-multi-agent.AC7.4 Success:** `Interval(60s)` condition fires every 60 seconds; agent receives `WakeReason::Interval`
- **v3-multi-agent.AC7.5 Failure:** `ctx.wake.register` without `WakeConditionRegistration` capability returns `CapabilityError::Denied`
- **v3-multi-agent.AC7.6 Edge:** Multiple wake conditions registered; first to fire triggers the poke; remaining conditions stay registered for future evaluation
- **v3-multi-agent.AC7.7 Edge:** Wake condition fires while agent is mid-turn; wake is queued and delivered after current turn completes (same as message queuing)

### v3-multi-agent.AC8: Fronting and routing

- **v3-multi-agent.AC8.1 Success:** FrontingSet persisted to pattern_db; after runtime restart, the same fronting set is loaded and routing resumes
- **v3-multi-agent.AC8.2 Success:** Incoming message matching a routing rule is delivered to the rule's target persona's mailbox
- **v3-multi-agent.AC8.3 Success:** Incoming message matching no routing rule is delivered to the fallback persona
- **v3-multi-agent.AC8.4 Success:** Direct addressing (`@persona-name` or explicit PersonaId) bypasses routing; delivered to named persona regardless of routing rules
- **v3-multi-agent.AC8.5 Success:** Co-fronting with two active personas: both receive copies of unrouted messages (or routing rules discriminate between them)
- **v3-multi-agent.AC8.6 Success:** `ctx.caller` is `Caller::Human(user_id)` for human-initiated turns and `Caller::Agent(persona_id)` for agent-initiated turns
- **v3-multi-agent.AC8.7 Success:** Human-as-caller uses fronting persona's SessionContext; all memory handles and project mount are the persona's
- **v3-multi-agent.AC8.8 Edge:** FrontingSet update while messages are in-flight: messages already queued use old routing; new messages use updated routing (no reprocessing)

### v3-multi-agent.AC9: Agent registry

- **v3-multi-agent.AC9.1 Success:** `ctx.constellation.list()` returns all personas visible in current scope with status, relationships, and group memberships
- **v3-multi-agent.AC9.2 Success:** `ctx.constellation.find(project, SupervisorOf)` returns personas with that relationship in that project
- **v3-multi-agent.AC9.3 Success:** Named group created with project scope; group visible only in that project's context
- **v3-multi-agent.AC9.4 Success:** Sibling spawn auto-registers with specified relationship; immediately visible in `ctx.constellation.list()`
- **v3-multi-agent.AC9.5 Failure:** Querying the registry for a nonexistent project returns an empty result, not an error
- **v3-multi-agent.AC9.6 Edge:** Draft personas appear in registry with `status: Draft`; they're discoverable but not steppable

### v3-multi-agent.AC10: End-to-end integration

- **v3-multi-agent.AC10.1 Success:** Smoke test at `crates/pattern_runtime/tests/multi_agent_smoke.rs` passes deterministically: creates two personas, one fronting as supervisor with routing rules, spawns ephemeral worker, assigns task, worker completes task, supervisor receives result, capability enforcement prevents unauthorized effects
- **v3-multi-agent.AC10.2 Success:** Mock ProviderClient; no live model dependency in CI
- **v3-multi-agent.AC10.3 Success:** Fork-and-merge flow: parent forks (lightweight), fork writes to memory, merge_back succeeds, parent sees merged state
- **v3-multi-agent.AC10.4 Success:** Haskell delegation modules (`Pattern.Delegation.RoundRobin` etc.) importable and functional in agent programs
- **v3-multi-agent.AC10.5 Failure:** Any step in the smoke flow failing produces a clear error identifying which step and which assertion
- **v3-multi-agent.AC10.6 Edge:** Smoke test runs concurrently with other `pattern-runtime` tests without shared-state interference

## Glossary

- **Tidepool**: The Haskell evaluation runtime embedded in Pattern. Each agent session has a `TidepoolSession` that compiles and runs the agent's Haskell program, mediating between the agent's code and the Rust-side effect handlers.
- **EvalWorker**: A per-session thread that drives Tidepool's Haskell evaluation. Spawning a child session requires spawning a new EvalWorker.
- **GADT (generalised algebraic data type)**: A Haskell type construct used to define the effect vocabulary available to an agent. Each effect category is declared as a GADT; filtering a GADT from the prelude makes that entire effect category invisible to agent code at compile time.
- **Prelude**: The Haskell preamble injected into every agent session, containing SDK type declarations, effect constructors, and standard imports. Capability filtering modifies this prelude before session open.
- **CapabilitySet**: The set of effect categories granted to a particular agent session. Determines both what appears in the agent's Haskell prelude (compile-time) and what is allowed at runtime dispatch.
- **Ephemeral**: A short-lived child agent session with no persistent identity. Used for parallel worker tasks. Attributed to its parent in logs; dies when the parent resolves.
- **Fork**: A child agent session that starts from a snapshot of the parent's memory state. Can be lightweight (in-memory CRDT branch) or persistent (a separate jj workspace on disk). Supports merge-back into the parent.
- **Sibling**: A distinct, independently-identified persona spawned alongside the spawning agent. Has its own memory root, turn history, and capability configuration. Can be an existing persona or a newly created one.
- **FrontingSet**: The runtime-tracked set of personas currently "fronting" — the active interface to human users. Determines who receives and responds to incoming messages. Persists across restarts.
- **RoutingTable**: The message dispatch rules within a FrontingSet. Determines which persona receives an incoming message based on pattern matching, with a fallback persona for unmatched messages.
- **Costume**: A prompt/style template override applied to an ephemeral agent. Changes how the agent presents without creating a new persistent identity.
- **loro / LoroDoc**: A CRDT library used for versioned memory blocks. `LoroDoc` is the primary document type; `LoroDoc::fork()` and `LoroDoc::import()` implement the lightweight fork/merge model.
- **CRDT (conflict-free replicated data type)**: A data structure that can be independently modified by multiple parties and always merged without conflicts, by construction.
- **jj (Jujutsu)**: A version control system used for persistent fork isolation. Each persistent fork gets its own jj workspace (an independent working copy within a shared repository), with a namespaced bookmark.
- **KDL**: A configuration file format used for persona and project configuration in Pattern. Config file writes that parse as KDL with pattern-specific keys are always gated.
- **PermissionBroker**: The runtime component that handles human approval requests for gated effects. Rebuilt from a global singleton to a per-runtime instance using the `jiff` time library.
- **jiff**: A Rust date/time library, replacing `chrono` in the rebuilt PermissionBroker.
- **BlockRef / BlockHandle**: References to memory blocks in Pattern's memory system (from Plan 1). Used in wake conditions (`BlockChanged`) and task pinning.
- **WakeReason**: A discriminant on a turn's input indicating why an agent was woken — message received, timeout fired, block changed, dependency resolved, or interval elapsed.
- **TurnInput**: The structured input passed to an agent at the start of a turn, including the triggering message or wake reason.
- **SessionContext**: The shared context structure for a session, containing workspace, project mount, memory handles, and other per-session state. Arc-shared, making child session creation cheap.
- **Persona**: A named, independently-identified agent with its own configuration, memory root, and history. The fundamental unit of agent identity in Pattern.
- **Constellation**: Pattern's term for the full set of personas available to a user or project.
- **Draft state**: A persona that has been registered and configured but not yet activated into a running session. Visible in the registry but cannot take actions until a human promotes it.
- **Promote**: The action of elevating a draft persona (or a fork) to active status, either opening a session for it or converting a fork into a new sibling persona.
- **RoundRobin / Pipeline / FanOut**: Delegation patterns expressed as importable Haskell library modules. Not built-in Rust types — they compose `ctx.spawn.ephemeral` and `ctx.tasks.*` primitives at the agent level.

## Architecture

### Capability system

Two-layer permission model unified under a single system that encompasses both effect-level gating and memory-level ACL.

**Layer 1 — effect visibility (compile-time).** `CapabilitySet` per agent defines which SDK effect categories exist in their Haskell environment. At session open, the canonical effect declarations (Haskell GADTs built by `canonical_effect_decls()` in `crates/pattern_runtime/src/tidepool/compile.rs`) are filtered based on the agent's `CapabilitySet`. Effects not in the set have their GADT constructors excluded from the prelude — Tidepool compilation fails if the agent's program references them. The agent cannot even express a call to an absent effect.

Effect categories: `Memory`, `Message`, `Tasks`, `Spawn`, `Shell`, `File`, `Mcp`, `Wake`, `Sources`, `Time`, `Log`, `Rpc`, `Scope`, `Display`. Granularity is per-category. Per-method restrictions are layer 2's domain.

**Layer 2 — runtime approval (dispatch-time).** Policy rules evaluated at effect dispatch in the Rust-side handlers (`crates/pattern_runtime/src/sdk/handlers/`). Three sources:

1. **Rust defaults** — conservative baseline. Destructive shell commands gated. Config file writes gated via shape-based detection (any file that parses as pattern config KDL triggers human approval; false positives preferred over false negatives). New identity spawns gated.
2. **KDL config** — per-persona and per-project configuration. Can loosen or tighten from Rust defaults in either direction. The human's trust decision. Loaded at session open, immutable during session.
3. **Runtime broker** — `PermissionBroker` rebuilt as a per-runtime instance (not the current global singleton at `crates/pattern_core/src/permission.rs`), using `jiff` instead of `chrono`. Human can override individual invocations: approve once, approve for scope, approve for duration, deny.

**Agents cannot self-modify policy.** Config file writes that parse as pattern config KDL are always gated — this is a Rust default that cannot be loosened, because it protects the mechanism that defines trust.

**CapabilitySet inheritance:** ephemeral/fork inherit from parent (can only restrict further). Sibling capabilities come from the sibling's own persona config (independent identity). Spawner passes a restricted CapabilitySet at spawn time.

**Memory ACL integration:** the existing `MemoryPermission` enum (ReadOnly/Partner/Human/Append/ReadWrite/Admin) and `memory_acl::check()` at `crates/pattern_core/src/memory_acl.rs` remain as-is for block-level access control. If an agent doesn't have the `Memory` capability, it can't touch blocks at all (layer 1). If it has `Memory`, the ACL determines what it can do to each specific block (layer 2). Same broker handles consent flows for both.

### Spawn primitives

Three spawn modes, all dispatched through the `SpawnHandler` at `crates/pattern_runtime/src/sdk/handlers/spawn.rs` (currently a stub returning `EffectError::Handler`).

**Ephemeral.** Spawns a new `TidepoolSession` by cloning the parent's Arc-shared `SessionContext` with per-session overrides: fresh `CancelState`, fresh `pending_messages` queue, fresh `CheckpointLog`, fresh `current_turn` counter. A new `EvalWorker` thread spawns with the same `include_paths` and SDK location. The costume (prompt/style template) is injected as the session's system prompt override. CapabilitySet filters the prelude. Timeout enforced by a tokio timer that cancels the session. Parent tracks the ephemeral's handle for lifetime management — sub-spawns inside die when parent resolves.

**Fork.** Two isolation modes, caller-chosen with runtime default based on timeout hint:

- *Lightweight* — loro CRDT branch of parent's memory state. LoroDoc forked via `LoroDoc::fork()`. No jj workspace, no disk writes for the fork's memory. New `TidepoolSession` with forked LoroDoc. Merge back via `LoroDoc::import()` from the fork's state. Dies if parent session ends. Suitable for "think about this in parallel" work with timeouts under ~60 seconds.
- *Persistent* — jj workspace in the same repo via Plan 1's jj adapter (`pattern_memory` internal module). Own working copy, namespaced bookmark (`<agent-id>/<task-id>`). Full disk state for the fork's memory. Merge back via jj merge + loro CRDT merge. Survives parent restart. Suitable for long-running divergent work.

Resolution options: `await_result()` (block parent), `merge_back(strategy)` (CRDT or jj merge), `discard()` (abandon), `promote(persona_config)` (fork becomes a new sibling persona — requires `SpawnNewIdentities` capability).

**Sibling.** Opens a new session for a distinct persona. If the persona exists (by `PersonaId`), adopts it — no special authorization. If creating a new persona identity, requires `SpawnNewIdentities` capability. Without it, the new persona is created in draft state (config file written, registered in DB as `status: draft`, no session opened). Human promotes via CLI/TUI, which opens the session.

Sibling sessions are fully independent — own EvalWorker, own TurnHistory, own CapabilitySet (from the sibling's persona config, not inherited from spawner).

**Concurrency limits.** Maximum concurrent ephemeral spawns per session, configurable in KDL config. Rust-enforced via a semaphore. Attempts to exceed the limit return a clear error suggesting the agent await existing spawns.

### Agent mailbox and communication

Each active agent session has an inbox — a `tokio::sync::mpsc` channel watched by a background tokio task (the "mailbox task"). The mailbox task:

1. Receives messages from the inbox channel.
2. Checks if the agent is mid-turn (via a shared `AtomicBool` or similar).
3. If idle, steps the session with the message as `TurnInput`.
4. If busy, queues the message for delivery after the current turn completes.

Agent-to-agent messages flow through `ctx.message.send(to, content)` → `MessageRouter` (existing at `crates/pattern_runtime/src/router.rs`) → target agent's mailbox channel. The router resolves persona IDs to active mailbox handles via the agent registry.

**Task pinning.** When an agent receives a task via delegation (or async structured request), the task's `BlockRef` is pinned into the agent's working memory — added to the set of blocks included in the agent's memory snapshot. The agent is always aware of assigned work.

**Wake conditions.** Beyond the baseline "message received" behavior:

- Rust primitive conditions: `TaskTimeout(Duration)`, `TaskDependencyResolved(BlockRef)`, `BlockChanged(BlockHandle)`, `Interval(Duration)`. Evaluated by the mailbox task via appropriate tokio primitives (timers, block change subscriptions via Plan 1's loro subscriber system, task index polling).
- Custom Haskell conditions: registered via `ctx.wake.register(condition)` (capability-gated: `WakeConditionRegistration`, not in most agents' CapabilitySet). Compiled once from the agent's Haskell codebase, run repeatedly via the full effects engine. Whether evaluation uses the agent's existing EvalWorker or a dedicated one is an implementation detail dependent on Tidepool's concurrent evaluation model.
- When a condition fires, the mailbox task delivers a `TurnInput` with a `WakeReason` discriminant, distinct from a message-triggered turn.

### Fronting and routing

`FrontingSet` is a runtime-level primitive, persisted to pattern_db, loaded on runtime start.

```rust
pub struct FrontingSet {
    pub active: Vec<PersonaId>,
    pub routing: RoutingTable,
    pub fallback: PersonaId,
}

pub struct RoutingTable {
    pub rules: Vec<RoutingRule>,
}

pub struct RoutingRule {
    pub pattern: MessagePattern,
    pub target: PersonaId,
    pub priority: u32,
}
```

The `FrontingSet` determines how incoming messages (from humans or external sources) are dispatched:
1. Evaluate routing rules in priority order against the message.
2. First matching rule's target persona receives the message.
3. No match → fallback persona receives.
4. Direct addressing (`@persona-name`) bypasses routing, delivers to named persona.

**Supervisor pattern** = supervisor persona permanently in the fronting set's active list, with routing rules that dispatch specialist topics to specialist personas. The supervisor is the fallback. This is a configuration of the fronting primitive, not a separate coordination mechanism.

**Human-as-caller.** Human invocations use the fronting persona's `SessionContext` — workspace, project mount, memory handles all inherited. `ctx.caller` discriminant is `Caller::Human(UserId)` vs `Caller::Agent(PersonaId)`. Effect handlers can branch on caller type. Human short-circuits the permission/policy gate (human IS the approver).

### Agent registry

Flat persona registry in pattern_db, extending the existing `agents` table schema:

- `persona_id`, `status` (Active, Draft, Inactive), `config_path`, `project_attachments`
- Relationship edges in a separate table: `from_persona`, `to_persona`, `kind` (SupervisorOf, SpecialistFor, PeerWith, ObserverOf)
- Named groups: `group_id`, `name`, `project_id` (optional scoping), with a membership join table
- FrontingSet state: persisted as a dedicated table or KDL config tied to the runtime instance

Discovery SDK surface: `ctx.constellation.list()`, `ctx.constellation.find(project, relationship)`, `ctx.constellation.groups()`.

Sibling spawn auto-registers with specified relationship. Draft personas appear in registry with `status: Draft` — visible but not steppable.

## Existing patterns

**Session architecture.** The design builds directly on `TidepoolSession`'s existing structure at `crates/pattern_runtime/src/session.rs`. SessionContext is Arc-shared, making child session creation cheap. The EvalWorker is per-session (thread-local), so spawning requires a new worker thread — this is already the expected model. The `checkpoint()`/`restore()` methods on the `Session` trait support fork semantics.

**Memory ACL.** The existing `MemoryPermission` enum and `memory_acl::check()` function at `crates/pattern_core/src/memory_acl.rs` are retained unchanged. The capability system layers on top rather than replacing them.

**Permission broker.** The existing `PermissionBroker` at `crates/pattern_core/src/permission.rs` provides the broadcast-request/oneshot-response pattern used for runtime approval. It is rebuilt (not reused) as a per-runtime instance with jiff, but the architectural shape is preserved.

**Message router.** The existing `MessageRouter` and endpoint pattern at `crates/pattern_runtime/src/router.rs` is extended with mailbox delivery, not replaced.

**Coordination types.** The existing coordination types in `rewrite-staging/` (CoordinationPattern enum, AgentGroup, GroupMember, etc.) and pattern_db coordination queries are deprecated. Their responsibilities split between the FrontingSet primitive (persistent routing), task-based delegation (ephemeral work), and the simplified agent registry (groups, relationships). The old `coordination_tasks` table is already dropped by Plan 2.

**Prelude generation.** Effect GADT declarations are generated by `canonical_effect_decls()` in `crates/pattern_runtime/src/tidepool/compile.rs`. Capability filtering is a new step inserted into this pipeline — filtering the list before concatenation into the prelude string.

## Implementation phases

<!-- START_PHASE_1 -->
### Phase 1: Capability system

**Goal:** `CapabilitySet` type, prelude filtering, and the runtime approval layer.

**Components:**
- `CapabilitySet` type and `EffectCategory` enum in `crates/pattern_core/src/types/` — the set of effect categories an agent can access
- Prelude filtering in `crates/pattern_runtime/src/tidepool/compile.rs` — filter `canonical_effect_decls()` output based on CapabilitySet before injecting into session
- `PolicyRule` types and evaluation in `crates/pattern_runtime/src/policy/` — Rust defaults, KDL config loading, rule evaluation at dispatch time
- `PermissionBroker` v2 in `crates/pattern_runtime/src/permission/` — per-runtime instance, jiff-based, replaces the global singleton
- Config file protection in file write handler (`crates/pattern_runtime/src/sdk/handlers/file.rs`) — shape-based detection of pattern config KDL files
- KDL config schema for per-persona and per-project capability and policy declarations

**Dependencies:** Plans 1 and 2 complete (pattern_memory with MemoryScope, BlockSchema, fs-canonical storage). Existing `PermissionBroker` and `MemoryPermission` as reference.

**Done when:** An agent session opens with a restricted CapabilitySet and cannot reference excluded effects in its Haskell program (compile-time rejection). Policy rules evaluate at dispatch time. PermissionBroker handles approval flows. Config file writes are gated by shape detection.
<!-- END_PHASE_1 -->

<!-- START_PHASE_2 -->
### Phase 2: Spawn primitives

**Goal:** Ephemeral, fork, and sibling spawn modes functional via `ctx.spawn.*`.

**Components:**
- `SpawnHandler` implementation in `crates/pattern_runtime/src/sdk/handlers/spawn.rs` — replacing the stub with real dispatch
- `EphemeralConfig`, `ForkConfig`, `SiblingConfig` types in `crates/pattern_core/src/types/` — spawn configuration with CapabilitySet inheritance
- Session cloning logic in `crates/pattern_runtime/src/session.rs` — clone SessionContext with per-session overrides (CancelState, pending_messages, CheckpointLog, turn counter)
- EvalWorker spawning in `crates/pattern_runtime/src/tidepool/` — new worker thread per child session with prelude filtered by child's CapabilitySet
- Spawn concurrency limiter — tokio semaphore, configurable max concurrent ephemerals
- Lifetime management — parent tracks child handles, sub-spawns die when parent resolves

**Dependencies:** Phase 1 (CapabilitySet, prelude filtering)

**Done when:** All three spawn modes create functional child sessions. Ephemeral agents execute programs, return results, and die. Forks snapshot parent state. Siblings open independent sessions. Capability inheritance works (children can only restrict). Concurrency limits enforced.
<!-- END_PHASE_2 -->

<!-- START_PHASE_3 -->
### Phase 3: Fork isolation and merge

**Goal:** Lightweight and persistent fork isolation modes with merge-back and promote-to-sibling.

**Components:**
- Lightweight fork isolation — `LoroDoc::fork()` for in-memory CRDT branch, `LoroDoc::import()` for merge-back
- Persistent fork isolation — jj workspace creation via Plan 1's jj adapter, namespaced bookmark, independent working copy
- Fork resolution logic — `await_result()`, `merge_back()` (CRDT merge for lightweight, jj merge + CRDT merge for persistent), `discard()`, `promote()`
- Promote-to-sibling — converts fork's memory state into a new persona config, registers as draft in registry, requires `SpawnNewIdentities` capability

**Dependencies:** Phase 2 (spawn primitives). Plan 1's jj adapter and loro integration.

**Done when:** Lightweight forks branch and merge memory state via loro. Persistent forks create jj workspaces and merge via jj + loro. Promote converts a fork to a draft sibling. Fork memory consistency tests pass (concurrent edits in parent and fork merge correctly).
<!-- END_PHASE_3 -->

<!-- START_PHASE_4 -->
### Phase 4: Agent mailbox and wake conditions

**Goal:** Agent-to-agent message delivery and condition-based wake/poke mechanism.

**Components:**
- Mailbox task — per-agent tokio task with mpsc inbox, busy detection, message queuing
- MessageRouter extension — resolve PersonaId to mailbox handle, deliver messages to inbox channel
- Task pinning — on task assignment, add BlockRef to agent's working memory snapshot selection
- Wake condition registry — `WakeCondition` enum with Rust primitives, registration via `ctx.wake.register()`
- Wake evaluation — mailbox task evaluates conditions: tokio timers for timeouts/intervals, loro subscriber hooks for BlockChanged, task index queries for TaskDependencyResolved
- WakeReason discriminant on TurnInput — distinguish message-triggered vs wake-triggered turns
- `WakeConditionRegistration` capability gating

**Dependencies:** Phase 2 (spawn primitives — mailbox needed for spawned agent communication). Plan 1 (loro subscribers for BlockChanged). Plan 2 (task index for TaskDependencyResolved).

**Done when:** Agents can send messages to each other via `ctx.message.send`. Messages queue when target is busy, deliver when idle. Task assignment pins tasks in working memory. Rust wake conditions fire correctly. Wake-triggered turns carry WakeReason. Custom Haskell condition interface defined (full implementation deferred to when Tidepool concurrent evaluation model is better understood).
<!-- END_PHASE_4 -->

<!-- START_PHASE_5 -->
### Phase 5: Fronting and routing

**Goal:** FrontingSet, RoutingTable, DB persistence, human-as-caller pathway.

**Components:**
- `FrontingSet`, `RoutingTable`, `RoutingRule` types in `crates/pattern_core/src/types/`
- FrontingSet DB persistence — schema in pattern_db, load on runtime start, save on change
- Message dispatch logic — evaluate routing rules, apply direct addressing override, fallback routing
- `Caller` enum (`Human(UserId)` / `Agent(PersonaId)`) threaded through all effect handlers
- Human-as-caller pathway — human invocations use fronting persona's SessionContext, short-circuit permission gates
- SDK surface: `ctx.fronting.set(personas)`, `ctx.fronting.route(rules)`, `ctx.fronting.current()`

**Dependencies:** Phase 4 (mailbox — routing delivers to mailboxes). Agent registry (Phase 6 — but FrontingSet can be tested with hard-coded personas before registry lands).

**Done when:** FrontingSet persists across restarts. Routing rules dispatch messages to correct personas. Direct addressing works. Human-as-caller uses fronting persona's context. Supervisor pattern (permanent front + specialist routing) works end-to-end.
<!-- END_PHASE_5 -->

<!-- START_PHASE_6 -->
### Phase 6: Agent registry and identity

**Goal:** Persona registry, relationships, named groups, discovery, identity authorization.

**Components:**
- Registry schema in pattern_db — personas table (id, status, config_path, project_attachments), relationships table (from, to, kind), groups table (id, name, project_id), group membership join table
- Registry queries — CRUD operations, discovery by project/relationship/group
- SDK surface: `ctx.constellation.list()`, `ctx.constellation.find(project, relationship)`, `ctx.constellation.groups()`
- Identity authorization — draft state for new-identity siblings (config-only, registered as Draft, no session)
- Auto-registration — sibling spawn registers with specified relationship, FrontingSet updates
- Old coordination tables/types deprecated — `coordination_tasks` already dropped by Plan 2, `agent_groups`/`group_members` schema replaced

**Dependencies:** Phase 2 (sibling spawn). Phase 5 (FrontingSet persistence).

**Done when:** Personas registered with status, relationships, and group memberships. Discovery queries work. Draft personas visible but not steppable. Sibling spawn auto-registers. Old coordination schema deprecated.
<!-- END_PHASE_6 -->

<!-- START_PHASE_7 -->
### Phase 7: Haskell delegation libraries and integration

**Goal:** Ship starter Haskell delegation patterns, task delegation libraries, end-to-end smoke test.

**Components:**
- Haskell delegation modules at `crates/pattern_runtime/src/tidepool/sdk/lib/` — `Pattern.Delegation.RoundRobin`, `Pattern.Delegation.Pipeline`, `Pattern.Delegation.FanOut` composing `ctx.spawn.ephemeral` + `ctx.tasks.*`
- End-to-end smoke test at `crates/pattern_runtime/tests/multi_agent_smoke.rs` — creates personas, spawns ephemeral workers, assigns tasks, tests fronting/routing, verifies capability enforcement, exercises fork/merge
- Integration verification — all spawn modes + mailbox + fronting + registry + capabilities compose correctly

**Dependencies:** All previous phases.

**Done when:** Haskell delegation patterns importable and functional. Smoke test exercises the full multi-agent surface deterministically (mock provider, no live model). All 7 phases' functionality composes correctly in the integration test.
<!-- END_PHASE_7 -->

## Execution mode recommendation

**Collaborative.** This plan involves substantial novel architecture (capability system, wake conditions, fronting concept) with design decisions that benefit from ongoing human judgment. The coordination model rework in particular — where task-based delegation and fronting/routing compose — is tricky territory where getting the abstractions right matters more than velocity. Human check-in points between phases will catch integration issues early.

## Additional considerations

**Tidepool concurrent evaluation.** Custom Haskell wake conditions require running Haskell code outside a full agent turn. Whether this reuses the agent's EvalWorker or needs a dedicated one depends on whether Tidepool supports concurrent evaluation on the same compiled program. The interface is designed (register, compile once, evaluate repeatedly) but the mechanics are deferred to implementation when we can prototype against actual Tidepool behavior.

**Migration from old coordination types.** The staging-era coordination types (CoordinationPattern, AgentGroup, etc.) are not ported — they're replaced by the two composable primitives. Any references to the old types in pattern_db queries or models need explicit cleanup during Phase 6. The old `coordination_tasks` table is already dropped by Plan 2.

**Config protection scope.** Shape-based detection of pattern config KDL files means any `.kdl` file with pattern-specific top-level keys (capabilities, policy, persona, etc.) triggers write gating. This may produce false positives for user KDL files that happen to share key names. The implementation should log clearly when a write is gated, including which keys triggered the detection, so false positives are diagnosable.
