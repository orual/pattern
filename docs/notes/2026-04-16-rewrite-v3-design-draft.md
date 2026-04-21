# Pattern Rewrite v3 — Design Draft

**Status**: brainstorm complete, not yet formalized as a design-plan. Produced through extended collaborative brainstorming; captures all decisions reached during Phase 3 of the design process. Clean up into a proper design-plan format as a follow-up.

**Date**: 2026-04-16
**Working branch target**: `rewrite-v3` (not to reuse existing `rewrite` which is in conflict state)

---

## 0. Reading this document

Decisions in this doc were reached through explicit iterative refinement with the user. Each section reflects a landed-on architectural choice, generally with alternatives discussed and rejected. Deferred enhancements (things noted as worth doing later but not v1) are captured at the end.

Reference research docs this depends on live under `docs/reference/`:

- `claude-code-ecosystem.md` — OAuth flow, tool architecture, rommie-code patches
- `execution-models.md` — phoebe, code-act, AgentScript, exomonad/Tidepool overview
- `memory-and-personas.md` — Letta/Anthropic memory, loro, jj-lib
- `other-agents-and-proxies.md` — codex, opencode, chainlink, crosslink, gateways
- `local-tooling.md` — orual-plugins and rust-genai fork details
- `cosa.md` — cosa AST interpreter
- `constellation.md` — Numina-Systems/constellation (pattern+phoebe TS synthesis)
- `tidepool.md` — Tidepool Haskell-in-Rust runtime source-grounded findings
- `exomonad.md` — ExoMonad orchestration (separate from Tidepool)
- `task-and-issue-tracking.md` — chainlink, crosslink, exomonad tracking layer
- `jj-workspaces.md` — jj workspace isolation levels, jj-lib embedding
- `oauth-and-detection.md` — source-grounded OAuth flow and detection posture

---

## 1. Goals & scope

Pattern v3 rewrites the multi-agent ADHD support system into a layered design:

1. A **general-purpose persistent-persona agent runtime** usable as the substrate for:
   - pattern-the-ADHD-support-system (one deployment/profile on top of the runtime)
   - a Claude Code-replacement coding tool with persistent agent personas (another deployment on top)
   - future uses built on the same runtime primitives

2. **Social integrations (Discord, ATProto/Bluesky, etc.) move out of the core** and become external utilities delivered as plugins. Pattern's core stops knowing about specific platforms.

3. Substantial changes to:
   - **execution model** — shift from tool-call/respond loops to code-executing-in-sandbox (code-act / programmatic execution)
   - **memory surfacing** — fix long-context attention / cache-breakpoint issues with the current "blocks rendered into system prompt" design by using pseudo-messages in recent context for changes
   - **subagent model** — first-class fork / ephemeral / sibling primitives with well-defined coordination
   - **plugin system** — CC-compatible plugin format with pattern-specific extensions
   - **storage** — filesystem-primary memory with jj for history, loro for concurrent-edit CRDT, SQLite for indexes

4. **Scope caveats**:
   - Claude subscription OAuth preservation is **load-bearing** (user-paid, personal use, not a service to abuse)
   - Detection-avoidance is about sending honest identification, not impersonation
   - ToS posture prefers session-pickup of claude-code's existing session (coresident tool pattern) over running pattern's own PKCE flow
   - Nothing sacred — this is effectively a from-scratch rewrite, with selective porting of surviving patterns

---

## 2. Substrate & execution

### 2.1 Runtime substrate

**Chosen**: **Tidepool** (Haskell-in-Rust runtime from `github.com/tidepool-heavy-industries/exomonad/tree/main/haskell`).

**Alternatives considered**:
- **Deno (phoebe-style)** — battle-tested with multiple production references (phoebe, constellation). Rejected for bootstrap because the cosa-endgame migration becomes a much bigger redesign from Deno than from Haskell. Kept as documented fallback.
- **cosa directly** — fork-friendly AST interpreter, close semantic match. Not yet mature enough; targeted for Phase 2 migration post-v1.
- **Tidepool** — closer semantic match to cosa (both functional, monadic, temporal). Alpha-stage but actively developed (268 PRs, 1,351 tests, CI with clippy-as-errors, active maintainer).

**Known risks (document in design-plan)**:
- N=1 external consumer (pattern is the second)
- 0.1.0, API churn expected — pin version + thin adapter
- 50% mutation score on Tidepool's own tests (actively remediated by team)
- No CPU/wall-clock timeout — we add external wrapping at FFI boundary
- No resource bound beyond 64 MiB nursery default and 10,000-node effect response limit
- LLM Haskell fluency good but more verbose than TS — compound token cost over long-running personas
- Debug path through GHC→FFI rougher than v8→deno_core
- No production phoebe/constellation-style reference for the Haskell path — we port concepts from TS, not code

### 2.2 Deno fallback

**Documented as a sibling design-plan**: `docs/design-plans/NNN-runtime-substrate-deno-fallback.md` (to be written). Specifies the Deno variant of Sections 2-3 in enough detail that if the Haskell spike fails, the pivot is "implement the alternate doc" not "redesign from scratch."

### 2.3 Effect system

**Chosen**: `freer-simple` (Tidepool's default).

Alternatives (`polysemy`, `effectful`, `bluefin`) deferred to post-v1 reconsideration if there are concrete ergonomic/perf complaints.

### 2.4 AgentRuntime trait

The substrate-swappable boundary. Lives in `pattern_core` (traits-only crate, see §8). Designed around cosa-like properties (per-statement-level observability, cheap fork, reifiable env) even though Haskell is the bootstrap — so the trait stays valid when cosa lands.

**Shape** (Haskell-flavored pseudocode):

```haskell
class AgentRuntime r where
  instantiate :: PersonaSnapshot -> Program -> r -> IO Session
  hostCapabilities :: r -> CapabilitySet

class Session s where
  step :: s -> IO StepResult
  checkpoint :: s -> IO EnvSnapshot
  restore :: PersonaSnapshot -> EnvSnapshot -> IO s
  fork :: s -> IO s
  interrupt :: InterruptReason -> s -> IO InterruptResult

data StepResult
  = Continue
  | YieldForHost HostCall
  | Paused PauseReason
  | Completed TurnOutput
  | Error ExecutionError
```

### 2.5 Agent SDK surface

Exposed to agent code (Haskell for now, cosa later). Stays **small and deliberate** — most "features" build on top as utilities, not SDK primitives.

**Philosophical posture**: capabilities the persona uses every turn are **first-class in the SDK hierarchy**, not hidden behind a generic `tool(name, args)` dispatcher. Dynamic tool dispatch exists only as the escape hatch for plugin-registered tools unknown at SDK compile time. Matches how humans write programs in any language — you don't wrap stdlib calls in a dispatch layer.

**Logical hierarchy** (each language expresses this according to its own syntax — Haskell via module prefixes / record accessors, TS via dot chaining if Deno fallback is taken, cosa via modules-once-we-add-module-support):

```
ctx
  agent_id / workspace_id / project / caller       identity primitives
                                                    (caller = Agent(persona_id) | Human(user_id), see §7)

  memory                                            persistent block operations
    read(handle) / write(handle, content) / append(handle, content)
    search(query) / recall(handle) / archive(handle)

  message                                           agent/human communication (was send_message tool)
    send(to, content)                               via MessageRouter
    reply(content)                                  to current caller
    notify(user, content)                           to human user

  shell                                             command execution, PTY-backed
    execute(cmd) / spawn(cmd) / kill(pid) / status(pid)

  file                                              filesystem access, permission-gated
    read(path) / write(path, content) / list(path)

  sources                                           data source access (registered DataStreams)
    stream(id) / subscribe(id, handler) / list()

  spawn                                             subagent primitives (see §4)
    ephemeral(...) / fork(...) / sibling(...)

  mcp                                               MCP, inverted surface (see §5.4)
    call(server, method, args) / list_servers() / introspect(server)

  time                                              temporal primitives
    now() / sleep(dur) / schedule(when, program)
                                                    (first-class via freer-simple TimeEffect; cosa native)

  ipc                                               external service communication
    call(service, payload)

  tool                                              dynamic tool dispatch — ESCAPE HATCH ONLY
    call(name, args) / list()
                                                    for plugin-registered / unknown-at-compile-time tools

  log / parallel / checkpoint                       runtime primitives
```

**Mapping from pattern's existing tool taxonomy**:
- `context` tool (append/replace/archive/load_from_archival/swap) → `ctx.memory.*`
- `recall` tool (insert/append/read/delete) → `ctx.memory.archive` + `ctx.memory.recall`
- `search` tool (unified) → `ctx.memory.search`
- `send_message` tool → `ctx.message.{send,reply,notify}`
- `shell` tool → `ctx.shell.{execute,spawn,kill,status}`

Agent code is **full language**, not a restricted subset. IO is hard-off by Tidepool's construction; effects requested via freer-simple flow through Rust-side handlers (which enforce permission/policy). Sandboxing is effect-based, not syntactic.

### 2.6 Checkpoint & resumability

- Checkpoints happen at SDK-call granularity (between awaits / effect requests) — cheap, no heap snapshot required, just the logical session state the runtime tracks.
- Hard interrupts (e.g., tight loop in agent code): handled by Tidepool's `EffectResponse` node limit + external CPU timeout wrapper we add at FFI boundary.
- Fork = snapshot of logical session state; new runtime session spawned with same program+ctx; namespaced jj workspace attached.

### 2.7 Porting phoebe/constellation primitives

These are all TS/Deno references. **Ported to Haskell, not forked**. Specific primitives worth porting:

- resource metering patterns (phoebe's approach)
- network allowlist enforcement (phoebe)
- compaction pipeline shape (constellation — extensively, see §6)
- reflexion system (constellation)
- subconscious background processing (constellation)
- DataSource registry abstraction (constellation)
- rate-limiting with per-provider token buckets (constellation)

---

## 3. Memory system

### 3.1 Storage layers

Four-layer composition, each doing what it's best at:

| Layer | Purpose | Tech |
|---|---|---|
| **Canonical store** | human-legible source of truth per block | markdown files |
| **CRDT merge layer** | concurrent-write conflict resolution | loro (in-process) |
| **History** | versioning, branch/fork/merge | jj-lib or jj CLI wrap |
| **Indexes** | FTS, vector, message log, auth | sqlite (FTS5 + sqlite-vec) |

### 3.2 Block schema

Existing types retained and two added:

- `Text` — sequential human-readable content, fs-native
- `Log` — append-mostly with `display_limit`, fs-native
- `Map` — key-value structured, loro+sqlite
- `List` — ordered, loro+sqlite
- `Composite` — nested structure, loro+sqlite
- **`Task`** (new) — specialized with enforced lifecycle (status transitions, deps, assignment). SDK exposes `ctx.tasks.*` methods that mutate task-blocks correctly. Structured-queryable via sqlite indexes (by status/owner/deps).
- **`Skill`** (new) — instructions/prompt content loaded on-demand by handle. Trust-tagged by source (first-party / project-local / plugin-installed / ad-hoc). Same block infrastructure handles permissions, sharing, history, pseudo-message-on-change.

### 3.3 Three-tier context model

From MemGPT/Letta/constellation consensus:

- **Core** — always rendered in system prompt. Small. Persona-level identity and current-focus content. `~/.pattern/personas/<id>/core/*`
- **Working** — referenced by handle, loaded on demand. When written, pseudo-message-surfaced in recent context. `<persona|project>/working/*`
- **Archival** — searchable but not context-loaded by default. Hybrid FTS+vector retrieval via `ctx.memory.search`. Never lost; compaction moves things here, not away.

### 3.4 Memory positioning & cache architecture

**Two problems the current design hits**:

1. **Attention**: current pattern's "stale blocks" issue is NOT staleness — prompts rebuild every cycle. The actual failure is either cache-breakpoint positioning hiding updates behind the cached prefix, or long-context attention deprioritizing content at the prompt head.
2. **Cache efficiency**: blocks embedded in the system prompt mean any block edit busts the entire prefix cache, including base instructions. Costly for personas that edit memory frequently.

**Proposed design (tentative — pending claude-code review, see §11)**: pull blocks out of the system prompt entirely and position them as a pre-latest-turn pseudo-turn with their own cache breakpoint. Three cache segments:

```
[segment 1] system prompt: identity + base instructions + tool descriptions  ← very stable
[cache_control: ephemeral]                                                     marker 1

[segment 2] historical message stream (turns, embedded pseudo-messages)       ← stable once committed
[cache_control: ephemeral]                                                     marker 2

[segment 3] current block state as pseudo-turn before latest user msg         ← changes on block edit
[cache_control: ephemeral]                                                     marker 3

[fresh]     latest user turn + in-progress tool results                       ← uncached
```

**Block edit impact**: busts segment 3 forward only (block content + latest turn). Segments 1 and 2 stay cached. Segment 1 alone is often 10-50KB of tokens; preserving its cache across block edits is a substantial savings.

**How block content surfaces**:
- Segment 3 is a synthesized pseudo-turn just before the latest user message. Header like `[memory:current_state]` then rendered Core blocks + Working-blocks-in-context with their content.
- Framed clearly so the agent treats it as persistent-memory-as-of-this-turn, not conversational content.

**Block CHANGES between turns** surface as pseudo-messages **in segment 2** (historical stream), not segment 3:
```
[memory:updated] block 'current_focus' was modified: <diff or new content>
[memory:written] agent X wrote to shared block 'tasks': <content>
```
Same pattern handles subagent writes, cross-agent awareness, compaction observability, and fork-merge events. Changes get baked into history as it rolls forward.

**Attention property preserved**: blocks still end up near the recent end of context (just before latest user turn) where attention lives per the Section 2.4 analysis. Cache optimization complements the attention fix rather than compromising it.

**Four-breakpoint budget**: Anthropic's prompt caching allows up to 4 `cache_control: ephemeral` markers per request. This design uses 3; one spare for future extensions (e.g., plugin-injected-skills section).

**Compaction interaction**: when compaction runs, archived batches roll out of segment 2 and summary + pseudo-messages roll in. Segment 2 cache busts but segment 1 stays hit. Segment 3 rebuilds naturally on next turn.

**Required research before locking this design** (see §11):
- Examine how claude-code itself positions cache breakpoints in its requests. Claude-code has load-bearing smart cache usage insights even if pattern's use case differs substantially. Check `services/api/` in local claude-code clone. Validate or revise the three-segment layout based on what claude-code does.
- Also worth cross-checking rommie-code's cache handling for multi-provider variations.

### 3.5 Storage location modes

Two primary modes plus one advanced:

- **Mode A — in-repo, host-VCS-owned**: `<project-repo>/.pattern/shared/` committed normally, host VCS (git/jj/whatever) is the history of record. Pattern adds NO history layer on top. Shared with collaborators. Default for shared project work.
- **Mode B — separate, pattern-jj-tracked**: `~/.pattern/projects/<project-id>/`, pattern-jj owns history, content NOT in host repo. Symlink to the directory from project may exist for agent-path convenience. Private by construction. Default for solo / private work.
- **Mode C (advanced, documented)** — sidecar pattern-jj repo whose working copy IS `<host-repo>/.pattern/shared/`. Two VCSes over same files. Fragile (jj-in-git-subdir untested by jj team but lock-free design suggests safety). Gitignore `.jj/`, `jj workspace update-stale` after host git operations.

### 3.6 Persona vs project scopes

Two orthogonal scopes:

- **Persona-level memory** — persona's own identity + cross-context knowledge. Always separate, always pattern-jj-tracked. Never lives in any project repo.
- **Project-level memory** — knowledge/tasks/decisions tied to a specific project. Per-project config picks Mode A or B.

**`isolate_from_persona` flag** on project attachment: `none | core-only | full`. "Full" gives the persona fresh-eyes context (identity only, no memory carryover). Useful for unbiased review or new-project onboarding.

**Project-scoped personas**: a persona can be `scope: project:<id>` — exists only in that project's context, travels with the codebase (if Mode A) or stays private (if Mode B).

### 3.7 jj workspaces and forks

- Subagent forks spawn as **jj workspaces in the same repo** (option b per jj-workspaces research): cheap, shared commit store, independent working copies, lock-free.
- Bookmarks namespaced: `<agent-id>/<task-id>` or `<workspace-id>/<branch>`. No collisions.
- Colocation with git: **ON by default for Mode A/C**, N/A for Mode B (no host git present).
- Full-isolation forks (separate repo clones) reserved for "genuinely needs full isolation" edge cases.

### 3.8 jj integration

**Wrap jj CLI first, migrate to pinned jj-lib later**. CLI is known-stable; jj-lib has no formal stability guarantee and would churn. Pattern's hot path isn't jj-heavy so CLI wrapping works indefinitely if needed.

Thin adapter ~15-30 functions: workspace add/list/forget/update-stale, commit, log, bookmark set/delete, merge, restore. Lives as an internal module of `pattern_memory`, not a separate crate.

### 3.9 Project utilities (NOT skills)

Concrete invokable code, not prompt content. Two layers:

1. **Sandbox-native code** (preferred) at `.pattern/shared/lib/*.hs` — Haskell (or cosa later), imports `Ctx`, uses standard effects, subject to sandbox permission model. Sandbox loader treats as regular importable modules. Gets effect-typing, checkpointing, interrupt, permission uniformity.
2. **External scripts** at `.pattern/shared/scripts/*.sh` — justfiles, shell scripts, human-invokable glue. Native utils can call these via `ctx.shell`. Two layers compose cleanly.

`.pattern/shared/` directory shape:

```
.pattern/shared/
  lib/         # sandbox-native utilities
  scripts/     # shell / justfile / human-invokable glue
  skills/      # prompt-content blocks loaded on demand
  personas/    # project-scoped personas (scope=project)
  setup/       # init/attach/fork hook handlers (sandbox-native)
  plugins/     # project-scoped installed plugins (optional)
```

### 3.10 Isolation primitives

Runtime provides minimal identity: `ctx.agent_id`, `ctx.workspace_id`, `ctx.project`. Anything more (port reservation, db namespacing, dev-server isolation) is built as project-scoped native utilities, not baked into runtime.

Rationale: project-specific isolation patterns vary; keeping them as code rather than runtime features means they're inspectable, editable, and each project can tune them. Helpers accumulate naturally. Intersects with constellation's "skills retrieval via semantic search" — project-local helpers are the searchable pool.

---

## 4. Subagent primitives

Three spawn modes, all first-class, per-spawn selection. No "lifetime" config — implicit rule: sub-spawns inside ephemeral/fork die when parent resolves.

### 4.1 Ephemeral (with richer options)

Short-lived worker, returns result, dies. Expanded for code-review-with-authority and similar patterns:

```haskell
ctx.spawn.ephemeral $ EphemeralConfig
  { program
  , costume   :: Maybe PersonaTemplate      -- style/prompt, not persistent identity
  , readBlocks :: [BlockHandle]             -- what it can see of parent's memory
  , authority :: Authority                   -- Advisory | Gating
  , timeout   :: Maybe Duration
  , writeBack :: [BlockHandle]              -- blocks it can write on completion
  }
```

- `Advisory` — parent does whatever with result (classic)
- `Gating` — ephemeral returns `Verdict::Approve | Reject | RequestChanges`; parent's next action blocks on this

No persistent identity; attributed to parent's persona in logs. Rollback = parent's turn rolls back.

### 4.2 Fork

Snapshots parent's logical session state + memory refs + current turn state. New runtime session; own jj workspace with namespaced bookmark. Can diverge, checkpoint independently.

Resolution options:
- `fork.await_result()` — block parent until fork completes
- `fork.merge_back(strategy)` — merge memory diffs; jj handles commit merge; triggers fork-merge compaction
- `fork.discard()` — abandon branch
- `fork.promote(persona_id)` — fork becomes new sibling persona, inherits memory state at current point
- `fork.checkpoint()` / `fork.rollback_to(checkpoint_id)` — independent timeline operations

### 4.3 Sibling

Distinct persona with own identity, memory root, history.

**Structured spawn config**:

```haskell
ctx.spawn.sibling $ SiblingConfig
  { persona       :: Either PersonaId PersonaNewConfig
  , relationship  :: Relationship  -- Peer | Specialist | Observer | Twin | Supervisor
  , group         :: Maybe GroupConfig -- which coordination group they join
  , pattern       :: CoordinationPattern -- inherited or explicit
  , sharedBlocks  :: [(BlockHandle, Perms)]
  }
```

Coordination via existing pattern_core coordination patterns (supervisor/pipeline/voting/roundrobin/sleeptime/dynamic), not ad-hoc.

### 4.4 Identity authorization

New-identity siblings (fresh persona_id) need authorization. Two mechanisms:

- **Capability flag on spawner**: `can_spawn_new_identities`, default off
- **Draft state**: fresh-identity spawns default to "draft" — exist but can't take actions until user explicitly promotes to active

Adopting/waking existing persona doesn't need authorization. Ephemeral/fork unaffected (no new identity).

### 4.5 Human as caller

Human invocation (via TUI slash command / REPL) parallels agent invocation:

- Same runtime pathway
- Context: **fronting persona's `Ctx`**, not a stripped HumanCtx. Workspace, project mount, memory handles inherited.
- `ctx.caller = Human(user_id) | Agent(persona_id)` — utilities can branch if they care
- Permission bypass: human short-circuits policy gate by virtue of being human
- Observability: invocation logged and visible in fronting persona's turn history ("human ran util X")

**Fronting persona** concept becomes first-class:
- Runtime tracks current fronting set (usually one; co-fronting allowed)
- TUI displays active persona
- `/util:X` uses fronting default; `/util:X @persona-name` overrides

**Audience tiers** for utilities (`@agent_only | @human_only | @both`; default `@both`):
- Author responsibility, not runtime paternalism
- `@agent_only` invokable by humans via `/debug util:X` override
- Matches CC slash command convention

### 4.6 Discovery

Via existing `AgentGroup`/`GroupMember` registry. Sibling spawn auto-registers. Personas query registry for "who's in my constellation / who's on project X."

---

## 5. Plugin layer

### 5.1 Manifest format

Claude Code plugin-compatible `.claude-plugin/plugin.json`. Pattern-specific fields under `pattern` namespace:

```json
{
  "name": "...",
  "commands": [...],  // → pattern utils (@both or @human_only)
  "agents": [...],    // → pattern agents (default ephemeral)
  "skills": [...],    // → pattern Skill blocks
  "hooks": [...],     // → lifecycle hook bindings
  "mcpServers": [...],// → pattern_mcp handles
  "pattern": {
    "persona_mode": true,
    "declared_effects": [...],
    "trust_level": "plugin",
    "transport": "pipe" | "mcp" | "iroh_rpc" | "wasm",
    "iroh": { "allowed_remote": false, "discovery": "local" }
  }
}
```

Unknown fields silently ignored by stock CC hosts; unknown pattern fields tolerated. Bidirectional non-breaking.

### 5.2 Loader translation

- CC agent (YAML with model/prompt/tools) → pattern agent, default `spawn_mode: ephemeral`; opt into persona via `pattern.persona_mode`
- CC skill (markdown + frontmatter) → pattern `Skill` block, trust-tagged by source
- CC command → pattern util with audience tier per declaration
- CC hooks (`SessionStart`, `PostToolUse`, etc.) → pattern lifecycle hooks via alias table:

| CC event | Pattern event | Caveat |
|---|---|---|
| `SessionStart` | `persona.attach` (interactive surface) | not daemon startup |
| `SessionEnd` | `persona.detach` by default; prefer `compaction.cycle.end` for close-out semantics | pattern has no discrete sessions |
| `UserPromptSubmit` | `turn.before` (any caller) | ≈identical |
| `PreToolUse`/`PostToolUse` | `tool.before`/`tool.after` | identical |

Pattern-native lifecycle events also available for plugins that want precise semantics:
`persona.attach.<project>`, `persona.detach`, `turn.before/after`, `tool.before/after`, `memory.write`, `fork.spawn`, `fork.resolve`, `compaction.cycle.start/end`, `plugin.install/uninstall`.

### 5.3 Plugin transports (tiered)

| Tier | Transport | Use case | v1 |
|---|---|---|---|
| **1** | stdin/stdout pipe | CC compat commands, one-shot tools | yes |
| **2** | MCP (stdio/SSE/streamable-http) via rmcp | CC-standard plugins, docs-as-blocks surface, resource subscriptions for data streams | yes |
| **3** | **iroh-rpc** over QUIC | richer bidirectional integration, persistent event streams, cross-device constellation coordination | v1 if atproto needs it; otherwise v1.5 |
| **4** | WASM component model via wasmtime | in-process plugins, CPU-heavy work | v3+ |

**Iroh-rpc rationale**: same protocol for local (QUIC-over-loopback) and remote (iroh NAT traversal), cryptographic per-plugin auth via node identity, enables cross-device constellation as native capability.

Transport selection per plugin via `pattern.transport` field. One plugin can expose multiple (MCP for tools + iroh-rpc for a DataStream).

### 5.4 MCP: inverted surface (context-explosion mitigation)

**Standard MCP clients explode context**: each server's tools flood the agent's tool list. Pattern inverts:

- Agents see a **single primitive**: `ctx.mcp.call(server, method, args)` and discovery (`ctx.mcp.list_servers`, `ctx.mcp.introspect`)
- Tool documentation materialized as **searchable memory blocks** at `mcp/<server>/tools/<tool>.md` on server-load. Hybrid FTS+vector search, loaded on demand, never implicitly in context.
- Agents wrap frequently-used MCP calls in native-code project utilities at `.pattern/shared/lib/` — accumulate a typed MCP wrapper library per project.

**pattern_mcp needs** (current implementation is incomplete):
- Full MCP client: tools/resources/prompts list and call
- Server lifecycle: spawn/connect/introspect/disconnect/restart
- Introspection → doc-block materialization on server-load
- Per-server scoped permissions (plugin-install = trust boundary; user override per-server)
- Transports: stdio + SSE + streamable-HTTP

Pattern_mcp lives as a module of `pattern_runtime`, not a separate crate. Tightly coupled to SDK + tool dispatch.

### 5.5 Trust model

| Artifact | Trust source |
|---|---|
| Plugin install | User's explicit install IS the trust decision |
| Plugin skills | Trusted once plugin installed |
| Plugin utilities (`@both`) | Run in sandbox with standard permissions; plugin can't bypass |
| Plugin MCP servers | Separate processes; same MCP permissions as CC |
| Plugin hooks | Fire in standard pathway; can't bypass runtime gates |
| Ad-hoc skill (non-plugin source) | Body-redact + user-enable flow on first use |

### 5.6 Directory layout

```
~/.pattern/plugins/<plugin-id>/               # global
<project>/.pattern/shared/plugins/<plugin-id>/ # project-scoped (mode A, committed)
<project>/.pattern/private/plugins/<plugin-id>/ # project-scoped private (gitignored)
```

Load precedence: project > global > ambient. Collisions warned at install.

### 5.7 Plugin capabilities

Plugins can register:
- Agents (with persona_mode opt-in)
- Skills (Skill blocks)
- Commands (utils)
- Hooks (lifecycle bindings)
- MCP servers
- **DataStream implementations** (for data sources — atproto firehose, etc.)
- **MessageRouter endpoints** (for delivery destinations — new post to Bluesky, send Discord message, etc.)

All registration via `pattern_plugin` loader, bound at load time.

---

## 6. Compaction & context management

Pattern's distinctive property: compaction is **NOT "replace history with summary."** It's **archive-older-keep-recent + optionally-produce-summary**. Post-compaction context = system prompt + blocks + (optional summary of archived) + **active batches verbatim**. Recent history preserved in original form; older stuff condensed; nothing truly lost (archived batches go to recall storage, FTS+vector-searchable).

### 6.1 v1 (build this round)

**Four strategies retained** (from current pattern):

- `Truncate { keep_recent }`
- `RecursiveSummarization { chunk_size, model, prompt }` — accumulates summaries across passes, clips head+tail of summaries when recursing
- `ImportanceBased { keep_recent, keep_important }` — heuristic-or-LLM scoring
- `TimeDecay { compress_after_hours, min_keep_recent }`

**Batch-based integrity**: compression operates on `MessageBatch`es, not individual messages. Incomplete batches never archived. Tool-call sequences stay intact.

**Distinctive summarization prompt** retained (lines 1046-1060 of current compression.rs):
> preserve: novel insights, unique terminology, relationship evolution patterns, crisis response validations, architectural discoveries
> condense: repetitive status updates, routine sync confirmations
> prioritize: things affecting future interactions
> remove: duplicate info, play-by-plays of routine events

**v1 improvements**:

- **Structured summaries with recall-handles** — "tell me more about X from last week" becomes cheap (summary section points at archived batch by handle)
- **Post-compaction pseudo-message observability** — agent sees: `[compaction] archived N batches covering period X, searchable via recall('<handle>')`
- **Per-persona configurable clip params + importance keywords** — hardcoded `clip_archive_summary(4, 8)` and generic keyword list become per-persona config
- **Fork-merge-triggered compaction** — when a fork resolves back, parent's newly-absorbed context auto-compacts before next turn
- **Pseudo-message pattern reused** — memory writes during compaction become pseudo-messages in next turn's context

### 6.2 Deferred (later enhancements)

- Strategy composition / layered filters (can't currently express "time-decay AND importance AND keep-last-N")
- Agent-triggered compaction points ("good stopping spot, compact now")
- Background/idle compaction with hysteresis (constellation's approach)
- Rolling summary with incremental updates (not regen-from-scratch)
- Per-block freshness signals weighted into keep/archive decisions
- Learned importance signals (replace heuristic)
- Compaction-as-persona-skill (persona has opinions about what's worth keeping)

---

## 7. Provider layer

### 7.1 Auth resolution (three-tier)

Resolved in order:

1. **Anthropic claude-agent-sdk-style session pickup** — read `~/.claude/session.json` (exact path per claude-code source). If valid + unexpired, use it. Claude-code's own refresh keeps it fresh. ToS-cleanest path.
2. **Pattern-owned PKCE flow** (fallback) — full PKCE per `oauth-and-detection.md`. Client ID `9d1c250a-...`, scopes `user:inference` etc., token stored in keyring (our own creds), refresh on 5-min buffer.
3. **API key** — `ANTHROPIC_API_KEY` env or config. Non-subscription, per-token billing.

**Asymmetric storage**:
- **Our credentials**: keyring primary, JSON fallback. Uniform cross-platform.
- **Claude Code session pickup**: **always check JSON path** (`~/.claude/session.json`) regardless of keyring state — claude-code writes JSON on linux even when keyring present. Missing keyring entry tells us nothing about whether a usable session exists.

### 7.2 rust-genai rebase

- Rebase onto current upstream (fork is behind; missing adaptive thinking, 1M-context Opus/Sonnet 4.6/4.7, newer beta headers)
- **Strip to auth-only patches**: session-pickup, PKCE, refresh handling. Everything else is upstream's.
- Possibly upstream the auth work (separate conversation with jeremychone)

### 7.3 Rate limiting

- Per-provider token buckets (constellation pattern): tokens-per-minute and tokens-per-day caps
- Per-persona phase budgets (crosslink inspiration): persona in focused phase can have hard budget cap; phase completion releases remaining
- Bucket exhaustion: queue request, retry with jitter, surface visible delay

### 7.4 Request shaping (detection resilience)

Pluggable `RequestShaper` trait; default shaper = "honest pattern identification":
- `x-app: pattern`
- Real User-Agent (`pattern/<version>`)
- Session tracking UUID
- Real model/timing

**"You are Claude Code" prefix slot**: cosmetic (no server-side rejection found). Pattern fills it with its own persona text (per rommie-code proof-of-concept: same structural slot, different content). Not impersonation.

Shaper configurable at runtime — if Anthropic changes detection tomorrow, config update handles it without redeploy. Note: server-side detection is unknowable without empirical testing; never bake fixed detection-avoidance into code.

### 7.5 Provider-session UUID (façade for continuous internal model)

Pattern internally has no discrete sessions. Provider-level session headers need something:
- Per-persona-attach-lifetime UUID
- Rotates on `compaction.cycle.end` by default (provider sees "new session" at compaction boundaries)
- Rotates on `persona.detach` (definitive end)
- Overridable by plugin or user config

### 7.6 Multi-provider routing

- Per-persona primary + fallbacks
- Per-task routing hints (rommie-code pattern: summarization → cheap model; sub-agents → different provider)
- Implementable at provider layer, surfaced to agents as `ctx.model(role: "summarize") -> ProviderHandle` rather than baking model IDs
- Cross-provider correlation via persona-attach-id

### 7.7 Observability

Every provider request emits structured log:
- `persona_id, workspace_id, provider, model, token_counts(in,out), cost_if_billed, duration, shaper_config_version, session_uuid`

Feeds persona budget tracking, pattern-wide cost reporting, auth debugging.

### 7.8 pattern_auth dissolved

- Provider auth (Anthropic OAuth, API keys, session pickup, keychain) → merged into `pattern_provider`
- ATProto + Jacquard → `pattern-atproto` plugin (ships ATProto client, firehose data source, auth storage, posting tools self-contained)
- Discord auth → `pattern-discord` plugin
- No shared credential-storage crate: each consumer uses `keyring` crate directly. Extract only if three crates end up with near-identical wrappers.

### 7.9 pattern_core de-platformed

- `data_source/bluesky/` → `pattern-atproto` plugin
- `BlueskyEndpoint` router destination → `pattern-atproto` (registered as plugin-provided endpoint)
- Any ATProto types leaking into core pushed out
- Pattern_core ends up genuinely platform-agnostic

---

## 8. Crate topology

### 8.1 Core crates (framework)

```
pattern_core             traits + types only (AgentRuntime, MemoryStore, ProviderClient,
                         McpClient, MessageRouter, DataStream, Block, errors, enums).
                         No logic. Everyone imports.

pattern_runtime          agent loop, Tidepool FFI + SDK, spawn primitives, tool dispatch +
                         registry, coordination patterns, MCP client (folded in),
                         generic DataStream impls (process/PTY, file-watcher, http-poll,
                         timer/heartbeat). Imports pattern_core traits; wires concrete
                         impls from memory/provider/db at bind time.

pattern_memory           block schema, three-tier model, mode A/B/C, pseudo-message
                         emission, jj-integration (internal module). Implements
                         MemoryStore trait.

pattern_provider         LLM provider client (rebased rust-genai), auth (three-tier),
                         rate limiting, request shaping, session-uuid façade, multi-
                         provider routing, keychain wrapping. Implements ProviderClient
                         trait.

pattern_db               sqlite with FTS5 + sqlite-vec + hybrid search. Message log,
                         task/skill indexes, block metadata, agent/group registry.
                         Schema migrations owned here.

pattern_plugin           CC-compatible loader, manifest parsing, translation, transport
                         adapters (pipe/mcp/iroh-rpc in v1; wasm later), hook lifecycle,
                         trust tier management, DataStream + MessageRouter endpoint
                         registration from plugins.
```

### 8.2 Surface crates (user-facing)

```
pattern_cli              TUI/REPL builders, fronting-persona concept, slash command
                         dispatch, human-caller entry point
pattern_api              typed HTTP contracts
pattern_server           backend API server
```

### 8.3 Plugins (shipped as CC-compat plugin directories)

```
pattern-atproto          ATProto client (Jacquard), firehose DataStream, posting
                         endpoint, auth storage, social tools
pattern-discord          Discord bot, token storage, message endpoint
pattern-nd               ADHD-specific tools/personas (kept name, converted from crate
                         to plugin format). Lower priority than core rebuild.
pattern-popup            GUI popup tool (integrates popup-mcp as MCP server plugin)
```

### 8.4 Retired

```
pattern_auth             dissolved into pattern_provider + plugins
pattern_surreal_compat   removed entirely
```

### 8.5 Dependency graph

```
pattern_core ← { runtime, memory, provider, db, plugin, cli, server, plugins }
pattern_memory  → pattern_core, pattern_db
pattern_provider→ pattern_core
pattern_runtime → pattern_core + pattern_memory + pattern_provider + pattern_db
pattern_plugin  → pattern_core + pattern_runtime + pattern_memory
pattern_cli     → pattern_plugin (+ concretes)
plugins         → pattern_core + whatever core facilities they need
```

No circular deps. pattern_core at root (trait-only), concrete crates fan out, plugin + surface crates wire them.

### 8.6 Boundary rules

- pattern_core never imports platform-specific symbols
- Plugins depend on core facilities, not on sibling plugins (use IPC / shared blocks / routed messages)
- pattern_runtime's SDK surface is the contract; everything else builds on what SDK exposes
- Credentials flow through pattern_provider or per-plugin creds-helpers using `keyring` directly
- DataStream + MessageRouter endpoints registered dynamically by plugins at load time

---

## 9. Migration path

### 9.1 Physical layout

**New long-lived branch in current workspace, not new tree.**

- Name: `rewrite-v3` (avoid existing `rewrite` in conflict state)
- Branches from `main` at tagged checkpoint (`tag: pre-rewrite-v3`)
- Old code stays visible on `main`; bugfixes land on main during rewrite
- Rewrite-v3 → main via merge when done; old-world crates deleted in the merge commit

### 9.2 Branch discipline

**Compile-clean NOT required during heavy demolition phases**:

- Forcing compile invites shim-and-stub pollution explicitly unwanted
- Branch WILL not compile for stretches; that's fine — we're rewriting, not bisecting bug fixes
- **Excise-don't-stub**: deleted code goes away, not replaced with `unimplemented!()`
- If code X references deleted code Y and X is also being rewritten, delete X in same pass
- If X survives but needs Y's replacement, comment out with TODO pointing to port-list until replacement lands

### 9.3 Workspace manipulation pattern

- `members = ["crates/*"]` in workspace `Cargo.toml` narrowed to explicit list
- **Currently-being-worked-on crate**: stays in `members`
- **Not-yet-touched crates that reference rewritten stuff**: removed from `members` until their turn
- **Already-rewritten-and-working crates**: stay in `members`
- **Retired crates**: directory deleted in dedicated commits once responsibilities migrated
- `members` grows as rewrite progresses

### 9.4 Port-list tracking

Living doc at `docs/plans/rewrite-v3-portlist.md`:
- Which crates currently excluded from `members`
- Which are in-flight
- Which are done
- Which are retired
- Single source of truth for "work remaining"

### 9.5 Data migration

Existing deployments have per-constellation SQLite state. One-shot migrator binary:

```
pattern-migrate-v1-to-v3 --old-db <path> --target-dir ~/.pattern/ [--dry-run]
```

Mapping:

| Old | New |
|---|---|
| `memory_blocks` rows | Markdown files in persona/project mount + sqlite index rows |
| `memory_block_checkpoints` | jj commits in relevant pattern-jj repo |
| `archival_entries` | Recall storage (moved to new pattern_db) |
| `messages` | Unchanged schema; pattern_db absorbs |
| `agents` / `agent_groups` / `group_members` | Pattern_db registry (schema modernized) |
| `shared_block_agents` | Pattern_db shared-block table |
| `auth.db` Anthropic OAuth | Keychain entries via pattern_provider |
| `auth.db` ATProto sessions | pattern-atproto plugin's own store |
| `auth.db` Discord tokens | pattern-discord plugin's own store |

Idempotent. `--dry-run` prints intended actions. Validates round-trip before success.

### 9.6 Rollout phases

Exact gates tuned during implementation.

1. **Phase 0 — scaffold**: branch created, crate skeletons, trait definitions in pattern_core, port-list started
2. **Phase 1 — runtime + provider**: Tidepool embedding working, minimal agent loop with hello-world, pattern_provider three-tier auth. **Milestone: pattern program calls an LLM.**
3. **Phase 2 — memory + persistence**: pattern_memory + pattern_db complete, fs+loro+jj+sqlite wired, block read/write/search end-to-end. **Milestone: persona retains state across restarts.**
4. **Phase 3 — spawn primitives + coordination**: ephemeral/fork/sibling, fork-as-jj-workspace, coordination patterns ported. **Milestone: persona spawns reviewer, fork merges back.**
5. **Phase 4 — plugin layer**: CC-compat loader, MCP, iroh-rpc (if atproto needs), hook lifecycle. **Milestone: CC plugin loads and works.**
6. **Phase 5 — socials as plugins**: pattern-atproto + pattern-discord migrated, firehose works. **Milestone: Discord + ATProto via plugin boundaries.**
7. **Phase 6 — migrator + rollout**: migrator written, tested non-destructively, TUI/CLI polished, Deno-fallback doc completed. **Milestone: migrate existing deployment to v3.**
8. **Phase 7 — merge to main**: `rewrite-v3` → `main`, tag `v2.0.0`, old crates deleted, production cutover.

### 9.7 Parallel-deployment during rollout

Production pattern runs from `main` (old world) while `rewrite-v2` develops. Migrator runs against copy of production first, verified, then for real.

### 9.8 Rollback plan

Migrator leaves old SQLite databases untouched. If v2 has catastrophic issue post-cutover, stop v2, restart v1 from old DBs, file bug. New fs-based state at `~/.pattern/` coexists with old DBs without conflict.

### 9.9 Scope containment

- No speculative refactoring of old code on `main` — only bugfixes
- No API compat between worlds — they're explicitly different
- No feature additions during rewrite — v2 ships the design we designed
- Deferred-enhancements list stays deferred

---

## 10. Deferred enhancements (preserved for later consideration)

Preserved from brainstorming to avoid losing ideas even if not in v1.

### Compaction
- Strategy composition / layered filters
- Agent-triggered compaction points
- Background/idle compaction with hysteresis
- Rolling summary with incremental updates
- Per-block freshness signals weighted into decisions
- Learned importance signals (replace heuristic)
- Compaction-as-persona-skill

### Runtime
- cosa-native backend (Phase 2, post-v1; AgentRuntime trait designed to accommodate)
- Mid-statement pause and resumability (free on cosa; not built on Tidepool unless demonstrated need)
- Alternate effect systems (`polysemy`, `effectful`, `bluefin`) if ergonomic complaints accumulate
- WASM transport for plugins (tier 4)

### Memory
- Mode C hardened (jj-in-git-subdir with documented best practices as default-usable)
- Structured query over block content beyond FTS (graph queries, relational patterns)
- Skill-as-block generalization (other on-demand-content types via same mechanism)

### Provider
- Upstream rust-genai auth patches (negotiate with jeremychone)
- Additional providers (Anthropic primary; OpenAI/Gemini/local via rebased rust-genai; others per need)
- Per-persona model preferences with cost-aware routing

### Plugins
- Native iroh-rpc for cross-device constellation coordination (v1.5 if not v1)
- WASM component-model transport (v2+)
- Plugin marketplace / discovery

### Constellation-inspired
- Reflexion system (self-review turns as first-class pattern)
- Subconscious system (background processing during idle)
- Skills retrieval via semantic search (beyond just being a block type)
- Rate limiting enhancements (per-model buckets, adaptive)

### Identity & personas
- Metacog-style cognitive-state tools (research-grade, observe if matures)
- Agent File (.af) interop for persona export/import

---

## 11. Open questions / risks (document in design-plan)

- **Tidepool maturity**: 50% mutation score, alpha-stage, N=1 external consumer. If blocks Phase 1 badly, Deno fallback exists.
- **MCP context-explosion design is novel**: inverted surface (call primitive + docs-as-blocks) not proven at scale. May need iteration during Phase 4.
- **Iroh-rpc as plugin transport**: good fit, but plugin author learning curve is real. Documentation needs to be thorough.
- **jj-lib vs CLI wrapping**: CLI is stable; lib is not. Design-plan stays on CLI for v1. Migrate to lib if perf dictates.
- **Detection resilience is adversarial**: server-side rules are unknowable. Design assumes shaper-config changes are cheap to ship.
- **"You are Claude Code" prefix**: rommie-code proves content-replacement works. Pattern does same. Grey area; user has judged this acceptable for their own subscription use.
- **Data migration completeness**: loro snapshot format translation from old DB to new fs+sqlite must be lossless. Test extensively against real deployments before production cutover.
- **Cache breakpoint positioning** (§3.4): three-segment layout is tentative. Required research task before Phase 2 memory work lands:
    - examine claude-code source (`~/Git_Repos/claude-code/services/api/` and related) for how it positions `cache_control` markers, what segments it treats as stable vs. volatile, and any edge cases around tool results + cache
    - cross-check rommie-code's patches for multi-provider cache handling variations
    - validate or revise the three-segment design based on findings; document resulting decision inline in §3.4 with a "verified: YYYY-MM-DD" stamp

---

## 12. Next steps

1. Clean up this draft into proper design-plan format at `docs/design-plans/NNN-pattern-rewrite-v2.md`
2. Write Deno-fallback sibling design-plan at `docs/design-plans/NNN-runtime-substrate-deno-fallback.md`
3. Start port-list at `docs/plans/rewrite-v2-portlist.md`
4. Phase 0 work: create `rewrite-v2` branch from tagged `pre-rewrite-v2`, scaffold crate skeletons, `pattern_core` trait definitions
5. Begin Phase 1 (runtime + provider)

---

*End of draft.*
