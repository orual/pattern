# Task and Issue Tracking for Multi-Agent Systems

A reference guide for persistent work-item coordination primitives designed for distributed agent systems. This document examines three implemented systems—Chainlink, Crosslink, and ExoMonad—to extract reusable patterns and architectural lessons for Pattern's hybrid subagent model.

## Executive Summary

Multi-agent systems operating at scale require explicit coordination primitives: a shared understanding of what work exists, what depends on what, who is doing what, and when work is blocked or complete. This is distinct from execution model (code-act, tool calling, etc.) and agent harness (permission system, tool exposure)—it is the "what are we building together" layer.

Three reference implementations show different approaches:

1. **Chainlink** (dollspace-gay/chainlink): Local-first SQLite issue tracker optimized for agent sessions. Emphasizes issue granularity, dependencies, and verification-driven development.

2. **Crosslink** (forecast.bio/crosslink): Phase-gated workflow orchestrator with checkpoint/resume semantics. Emphasizes long-running multi-agent builds and budget-aware scheduling.

3. **ExoMonad** (inferred from indirect sources): Worktree-based agent orchestration with Git as the coordination primitive. Emphasizes isolation, parallelism, and implicit task structure derived from branch/PR relationships.

Common themes: **work items as first-class entities, dependency graphs, state transitions, persistence across sessions, and multi-agent handoff**.

---

## 1. Chainlink: Local-First Issue Tracker for Agent Sessions

**Repository:** [github.com/dollspace-gay/chainlink](https://github.com/dollspace-gay/chainlink)  
**Language:** Rust  
**License:** MIT (inferred)  
**Storage:** SQLite, single file (`.chainlink/issues.db`), local only  
**Latest Activity:** Active as of April 2026

### What Chainlink Models

Chainlink is an **issue tracker optimized for AI agents**, not a general project management tool. Core primitives:

- **Issues**: Atomic units of work with title, description, priority, labels, status
- **Subissues**: Hierarchical decomposition (epic → feature → task)
- **Dependencies**: Blocking relationships (issue A blocks issue B)
- **Related Issues**: Non-blocking semantic links (related work, similar problems)
- **Sessions**: Context windows that preserve agent state across resumptions
- **Time Tracking**: Duration spent per issue per session
- **Milestones**: Grouping for planning and release coordination
- **Labels & Priorities**: Metadata for triage and sorting

### Inferred Data Schema

Based on CLI commands documented on the repository, Chainlink's SQLite schema likely includes:

```
issues
  id (primary key)
  title, description
  status (open | closed | blocked)
  priority (numeric)
  created_at, updated_at
  
subissues
  parent_id → issues.id
  child_id → issues.id
  order (for sequencing within parent)
  
dependencies
  blocker_id → issues.id
  blocked_id → issues.id
  
relations
  issue_id_1 → issues.id
  issue_id_2 → issues.id
  
labels
  issue_id → issues.id
  name
  
sessions
  id (primary key)
  started_at, ended_at
  active_issue_id → issues.id
  handoff_notes (text)
  
time_entries
  id (primary key)
  issue_id → issues.id
  start_time, end_time
  duration_seconds
```

**Note:** Chainlink does not publicly document its schema. The above is inferred from CLI patterns and feature descriptions.

### API Surface (CLI Commands)

**Create/Update:**
```bash
chainlink create <title> \
  [-p/--priority <num>] \
  [-d/--description <text>] \
  [--template <name>] \
  [-l/--label <label>] ...

chainlink update <id> [--title <title>] [--description <desc>] [--priority <num>]
chainlink comment <id> "<text>"
chainlink label <id> <label> / chainlink unlabel <id> <label>
```

**Lifecycle:**
```bash
chainlink close <id>
chainlink reopen <id>
chainlink delete <id> (soft delete / archive)
chainlink block <id> <blocker_id> / chainlink unblock <id> <blocker_id>
chainlink relate <id1> <id2> / chainlink unrelate <id1> <id2>
```

**Session Management:**
```bash
chainlink session start
chainlink session end [--handoff-note "<text>"]
chainlink work <id>  # Mark issue as active in current session
chainlink action "<description>"  # Log action taken
```

**Context Extraction:**
```bash
chainlink context <id>  # Full issue + dependency graph for agent consumption
chainlink list [--filter <expression>]  # Search/filter
```

### State Machine

**Issue lifecycle:**
```
CREATE → OPEN ─┬─→ CLOSED (archived)
              └─→ BLOCKED (within OPEN, explicit state)
```

**Blocking semantics:** An issue in BLOCKED state has at least one dependency relationship where the blocker is not CLOSED. Reopening a blocker implicitly unblocks dependents.

**Session state:**
```
IDLE ─→ ACTIVE (user running) ─→ ENDED
       (user loads context)
```

### Integration Shape

**For Agents:**
- **Context provider**: Chainlink exposes an MCP or file-based context provider. When an agent resumes, it loads the active issue and dependency graph.
- **Handoff notes**: Between sessions, agents can document what they attempted and why it failed. This context persists in the database.
- **Verification-Driven Development (VDD)**: Every code change should map to a Chainlink issue (and a corresponding verification step). This forces atomicity and traceability.

**For Humans:**
- **TUI**: Real-time issue tree, agent status, blocking relationships
- **Browser dashboard**: Drag-and-drop issue management, timeline visualization, team coordination

### Persistence Semantics

- **Transactional**: Updates to issues, dependencies, and time entries are ACID (SQLite transactions)
- **Multi-agent write story**: Unclear from public documentation. Likely assumes single-writer (one user's local session at a time) or uses file-level locking for concurrent writes. No evidence of CRDTs or eventual consistency.
- **Session handoff**: Designed for context resumption within a single user's workflow, not true multi-agent coordination.

### Maturity and Fit

- **Status**: Actively maintained (April 2026)
- **Community**: Used by AI agent developers, referenced in Verification-Driven Development discussions
- **For Pattern**: Strong reference for single-partner session persistence and task-driven agent loops. Less suitable for true multi-agent parallel coordination (no distributed write semantics documented).

---

## 2. Crosslink: Phase-Gated Workflow Orchestration

**Website:** [forecast.bio/crosslink](https://forecast.bio/crosslink/)  
**Language:** Unknown (likely Go or Rust, based on ecosystem)  
**License:** Proprietary / Commercial  
**Storage:** SQLite, single file (`.crosslink/issues.db`), local with optional git sync  
**Latest Activity:** Actively maintained (April 2026)

### What Crosslink Models

Crosslink extends issue tracking into **phase-gated workflow orchestration**. Core additions to Chainlink-like tracking:

- **Phases**: Discrete stages of a multi-stage workflow (design → implementation → testing → deploy)
- **Phase Gates**: Approval checkpoints or resource constraints that gate progression between phases
- **Budget Awareness**: Allocate token budgets per phase; agents track spend and respect limits
- **Checkpoint/Resume**: Snapshot agent state at phase boundaries; if interrupted, resume from checkpoint without recomputation
- **Knowledge Pages**: Shared markdown documents indexed with full-text search; automatically injected into agent context
- **Knowledge Sync**: Research done by one agent is available to all via git-backed knowledge pages

### Data Model (Inferred)

Extends the Chainlink model:

```
phases
  id, name, order
  budget_tokens (allocated)
  status (pending | active | gated | completed)
  
phase_gates
  id, phase_id
  type (approval | budget | resource_available)
  status (open | approved | blocked)
  
checkpoints
  id, phase_id
  state_snapshot (serialized agent memory/context)
  created_at
  
knowledge_pages
  id, title, content
  tags, full_text_index
  created_by_agent_id
  synced_to_git_at
```

### Coordination Model

**The Phase Gate Pattern:**

1. Design phase: Agents generate design document, save to knowledge pages
2. Phase gate: System requires review approval or validates budget allocation for next phase
3. Implementation phase: Agents spawn with the design document already in context
4. Phase gate: Acceptance testing, code review approval
5. Deploy phase: Agents coordinate production deployment

**Checkpoint/Resume Semantics:**

If an agent is interrupted mid-phase (timeout, user halt, OOM):
- State is checkpointed (agent's memory block, pending tasks, partial outputs)
- On resume, agent loads checkpoint and continues from that point
- No need to restart from beginning of phase

**Budget-Aware Scheduling:**

Crosslink tracks token spend per agent per phase. If an agent exceeds its budget:
- Lower-priority work is deferred
- Critical-path work continues with escalated budget
- Scheduling reflects economic constraints

### Integration Shape

**Multi-agent awareness:**
- Designed for coordinated swarms (multiple agents working on the same design document)
- Knowledge pages provide shared state without explicit message passing
- Git synchronization enables decentralized knowledge (useful for federated agent systems)

**UIs:**
- **TUI**: Real-time phase board, issue tree, active agents, knowledge pages, config
- **Browser dashboard**: Visual project oversight, charts, drag-and-drop, real-time monitoring

**Provider Integrations:**
- Integrates with Claude Code, Aider, Cursor, Continue.dev via context provider scripts

### Persistence Semantics

- **Transactional**: SQLite-backed, ACID
- **Multi-agent write story**: Checkpoints are per-agent (each agent has a checkpoint). Knowledge pages are shared but assume single-writer (one agent writing one page at a time) or use git merging for conflicts.
- **Git sync**: Knowledge pages are markdown files synced via git, enabling offline work and decentralized backups

### Maturity and Fit

- **Status**: Actively maintained commercial offering
- **Licensing**: Proprietary; deep integration not recommended. Integrate as external service if needed.
- **For Pattern**: Strong reference for phase-gating and checkpoint/resume. Less suitable as codebase to fork. Useful if forecast.bio offers an integration API.

---

## 3. ExoMonad: Worktree-Based Agent Orchestration

**Author:** Inanna Malick  
**References:** [recursion.wtf](https://recursion.wtf/) (blog), GitHub contacts  
**Language:** Inferred multi-language (Rust + Haskell)  
**Coordination Primitive:** Git worktrees and PRs  
**Storage:** Git repositories (distributed)  
**Status:** Research/production (limited public documentation)

### What ExoMonad Models

ExoMonad replaces the traditional "swarm of agents PRing main" model with **tree of worktrees as the coordination primitive**. Task structure is implicit in the branching/PR graph, not explicit as first-class work items.

**Core thesis**: Agents coordinate via Git, not a centralized database.

### Worktree Isolation Model

**Instead of:**
```
Agent1 → main branch (merge conflict risk, requires coordination)
Agent2 → main branch (merge conflict risk, requires coordination)
Agent3 → main branch (merge conflict risk, requires coordination)
```

**ExoMonad uses:**
```
main
 ├── agent1_task1 (worktree for agent 1's first task)
 ├── agent2_task2 (worktree for agent 2's task)
 └── agent3_task3 (worktree for agent 3's task)
     └── agent3_subtask_3a (nested worktree for decomposed work)
```

Each agent operates in its own worktree with its own branch. Merges happen via PRs with explicit coordination.

### Task Structure (Implicit)

Tasks are not explicit records in a database. Instead:

- **Task**: Corresponds to a branch and its associated PR
- **Task status**: Inferred from PR state (open, approved, merged, closed)
- **Task dependency**: Explicit PR comments or branch naming conventions (e.g., `depends-on:main`, `blocks:other-task`)
- **Task ownership**: Git commit authorship
- **Task output**: Branch contents (code changes)

### Coordination Primitives

**Claude Agent Teams messaging bus:** ExoMonad hooks into Claude Agent Teams' native multi-agent APIs. Agents running different LLM backends (Claude, Gemini, Kimi, Copilot, Letta Code) appear as team members on the same bus.

**Worktree-based isolation:** Each agent's work is protected by Git's branch model. No merge conflicts because each agent works on a separate worktree.

**PR-based handoff:** When an agent completes work, it opens a PR. Another agent (or human reviewer) can review, merge, or request changes.

### Execution Model (Inferred)

Based on Tidepool (a Haskell-in-Rust runtime built using ExoMonad):

- Agents generate code in a target language (Haskell, Rust, etc.)
- Code executes in a language-appropriate runtime (not a Docker container or sandbox)
- Agents observe execution results and iterate
- Each worktree can have different execution substrates (Haskell in one branch, Rust in another)

This suggests ExoMonad's execution model is **language-agnostic**: the coordination layer (Git + messaging bus) is decoupled from the execution substrate.

### State Machine (Inferred)

**Branch/PR lifecycle:**
```
CREATED (new worktree) → OPEN (PR open) ─┬─→ MERGED (included in main)
                                          └─→ CLOSED (rejected, abandoned)
```

**Blocking via naming or explicit PR comments:**
```
PR for feature A
  Comment: "blocks feature B"
  
PR for feature B
  Status: WAITING (because feature A is not merged yet)
```

### Multi-Agent Coordination Story

**Parallel execution:** Multiple agents work on different branches simultaneously. No coordination overhead until merge time.

**Merge coordination:** When two agents' work touches the same file, Git's merge tools detect conflicts. Agents can:
- Resolve conflicts collaboratively (via Agent Teams messaging)
- Coordinate upfront to avoid overlapping changes
- Use rebase to linearize history

**Tree of worktrees:** Complex projects naturally decompose into hierarchies:
```
main (stable)
 ├── feature/big-refactor (agent 1, long-running)
 │    ├── refactor/module-a (agent 2, subtask of refactor)
 │    └── refactor/module-b (agent 3, subtask of refactor)
 └── feature/new-api (agent 4)
```

Agents in deeper branches merge upward when ready; shallower agents can depend on deeper work.

### Information Gaps

Public documentation on ExoMonad is limited. Not determined:

- Exact code execution primitive (subprocess, JIT, interpreted)
- Tool exposure mechanism (how agents invoke capabilities)
- Error recovery strategy (if an agent's code fails, what happens?)
- Resource limits and timeout model
- Whether task metadata (issue title, assignee, priority) is stored separately or inferred entirely from Git

**Recommendation**: Contact Inanna Malick ([GitHub](https://github.com/inanna-malick)) directly for architectural details if ExoMonad is a strong reference for Pattern's design.

---

## 4. Common Patterns Across All Three

### 4.1 Work-Item Lifecycle

All three systems model some variant of:

```
CREATE → ACTIVE/OPEN → BLOCKED (optional) → COMPLETED/CLOSED
```

**Chainlink**: Explicit state (open, closed, blocked)  
**Crosslink**: Phased (implicit active state per phase)  
**ExoMonad**: Implicit (PR state maps to task state)

### 4.2 Dependency Graphs

All three can express "X must complete before Y starts":

**Chainlink**: Explicit `dependencies` table (issue A blocks issue B)  
**Crosslink**: Phase gates (phase B doesn't start until phase A completes)  
**ExoMonad**: Git branch dependencies (feature B depends on feature A being merged)

### 4.3 Checkpoint / Phase / Milestone Concepts

**Chainlink**: Implicit (sessions = execution checkpoints; milestones = grouping)  
**Crosslink**: Explicit (phases are first-class; checkpoints snapshot state at phase boundaries)  
**ExoMonad**: Implicit (Git tags or branch names act as checkpoints; PR merges are milestones)

### 4.4 Context Handoff Between Agents

**Chainlink**: Handoff notes + issue context (agent reads issue description and prior comments)  
**Crosslink**: Knowledge pages + checkpoints (shared markdown + serialized agent state)  
**ExoMonad**: Git history + PR comments + messaging bus (agents read commits and collaborate via Claude Agent Teams API)

### 4.5 Persistence Model

| System | Backend | Multi-Agent Writes | Sync Mechanism |
|--------|---------|-------------------|---|
| Chainlink | SQLite (local) | Single writer (file lock) | None (local only) |
| Crosslink | SQLite + git | Per-agent checkpoints; shared knowledge pages | Git for knowledge sync |
| ExoMonad | Git (distributed) | Per-agent worktrees | Git push/pull |

---

## 5. Pattern Application: Design Constraints

Pattern's design constraints shape which primitives are most relevant:

1. **Hybrid subagent model**: Pattern spawns ephemeral worker agents for specific tasks, with longer-lived personas coordinating.
2. **Persona persistence**: Personas maintain memory blocks (jj-backed, markdown-like) across sessions.
3. **Plugin system**: Pattern exposes integrations via MCP, so task/issue layer must be composable.
4. **Filesystem-backed memory**: Pattern uses markdown + jj for versioned, mergeable memory (similar to Crosslink's markdown knowledge pages).

### 5.1 Should Pattern Ship a Task/Issue Layer?

**For optional integrations**: Yes. If users (or plugins) need to coordinate work across multiple agents, Pattern should provide:
- A task registry that agents can query
- A way to mark work as done, blocked, or requiring followup
- A persistence layer that survives agent restarts

**For core single-persona workflows**: Debatable. A single persona may not need explicit issue tracking—it can store state in its memory block. However, exposing task primitives enables richer agent-to-agent coordination.

**Recommendation**: Build a minimal `pattern_tasks` crate that provides:
- Task creation / update / completion primitives
- Dependency tracking (task A blocks task B)
- Optional phase-gating for long-running workflows
- Storage-agnostic API (implementations can use SQLite, file-based, or hybrid)

### 5.2 Storage Alignment: SQLite vs. Markdown + jj

**Option A: SQLite only** (like Chainlink/Crosslink)
- Pros: Structured, efficient, concurrent read access
- Cons: Separate from Pattern's memory model; harder to version control

**Option B: Markdown + jj** (like Crosslink's knowledge pages)
- Pros: Aligns with Pattern's memory block model; versioned, mergeable
- Cons: Weaker at structured querying; less efficient for large dependency graphs

**Option C: Hybrid** (recommended)
- SQLite for task metadata (structured queries, indexes)
- Markdown for task description / context (aligns with memory model)
- jj backing for history and mergeability

Example schema:
```
tasks
  id (primary key)
  name, description (stored in markdown file + jj)
  status (open | blocked | done)
  
dependencies
  blocker_id → tasks.id
  blocked_id → tasks.id
```

Task descriptions live in `memory/tasks/{id}.md` (jj-backed), indexed by SQLite.

### 5.3 Optional vs. Core

**Recommendation**: Optional, but first-class in the SDK.

- **Core**: Single persona workflows don't require explicit task tracking.
- **Optional**: Multi-agent workflows, integrations, plugin systems benefit from it.
- **First-class SDK**: Make it easy for agents to create, query, and update tasks without overhead.

```rust
// Example API (ideal state)
agent.create_task("implement auth", TaskOptions {
    priority: High,
    depends_on: vec![task_id_1, task_id_2],
    ..Default::default()
}).await?;

agent.update_task(task_id, TaskUpdate {
    status: TaskStatus::Blocked,
    blocked_by: Some(task_id_blocker),
    ..Default::default()
}).await?;

agent.query_tasks(TaskFilter {
    status: TaskStatus::Open,
    depends_on_me: Some(task_id),
    ..Default::default()
}).await?;
```

### 5.4 Specific Things Worth Stealing

#### From Chainlink

- **Verification-Driven Development (VDD)**: Every line of code maps to a task and a verification step. Encode this as a first-class pattern in Pattern's agent lifecycle.
- **Session handoff notes**: When an agent pauses, it should leave a summary of what it tried and why. Pattern's memory block can store this automatically.
- **Priority + Labels**: Simple metadata model that's agnostic to domain. Use enums, not strings, to avoid typos.

#### From Crosslink

- **Phase gates and checkpoints**: For long-running agent loops, explicitly marking safe resumption points prevents wasted computation. Useful for Pattern's persona workflows.
- **Budget awareness**: Track token spend per agent per phase. Useful for cost-conscious deployments.
- **Knowledge page model**: Shared markdown + git sync is directly applicable to Pattern's memory blocks. Consider whether personas should have shared knowledge pages.

#### From ExoMonad

- **Worktree isolation**: If Pattern needs true parallel agent coordination, use jj worktrees (or git worktrees) to isolate branches. Each agent gets its own branch; no merge conflicts until coordination.
- **Language-agnostic execution**: Decouple the task coordination layer from the code execution model. Agents should be able to write code in any language and execute it appropriately.
- **Implicit task structure**: Don't force agents to explicitly create tasks. Let task structure emerge from branch/PR relationships. This reduces friction for simple workflows.

---

## 6. Synthesis: Architecture Recommendations for Pattern

### 6.1 Core Model: Hybrid Explicit + Implicit

Pattern should support both explicit and implicit task tracking:

**Explicit:**
```
// Agent A creates a task
create_task("implement feature X", depends_on: [Y, Z])
// Agent B queries tasks it can work on
query_tasks(status: open, dependencies_met: true)
// Agent B completes the task
complete_task(task_id)
```

**Implicit:**
```
// Agents work on branches
// Task structure emerges from branch/PR graph
// No explicit task API called
```

Both patterns coexist. Simple workflows use implicit structure; complex multi-agent coordination uses explicit tasks.

### 6.2 Storage: Hybrid SQLite + Markdown + jj

```
pattern_tasks/
├── db.sqlite          # Structured queries, indexes
├── tasks/
│   ├── task_1.md      # Task description, linked to memory
│   ├── task_2.md
│   └── ...
└── .jj/               # Version history of all tasks
```

**Benefits:**
- Structured querying via SQLite
- Versioning and mergeability via jj
- Aligns with Pattern's memory model
- Supports both relational and document-oriented access patterns

### 6.3 API Surface

**Core primitives:**

```rust
pub async fn create_task(
    name: &str,
    options: TaskOptions,
) -> Result<TaskId>;

pub async fn update_task(
    id: TaskId,
    update: TaskUpdate,
) -> Result<()>;

pub async fn complete_task(id: TaskId) -> Result<()>;

pub async fn block_task(
    blocked: TaskId,
    blocker: TaskId,
) -> Result<()>;

pub async fn query_tasks(
    filter: TaskFilter,
) -> Result<Vec<Task>>;
```

**Optional phase-gating (for long-running workflows):**

```rust
pub async fn create_phase(
    name: &str,
    gate_type: GateType,
) -> Result<PhaseId>;

pub async fn checkpoint(
    phase: PhaseId,
    state: serde_json::Value,
) -> Result<CheckpointId>;

pub async fn resume_from_checkpoint(
    checkpoint: CheckpointId,
) -> Result<AgentState>;
```

### 6.4 Recommended Crate Structure

```
pattern_tasks/
├── lib.rs              # Public API
├── models.rs           # Task, Phase, Checkpoint types
├── storage.rs          # SQLite backend
├── markdown_sync.rs    # Markdown ↔ database sync
└── phase_gate.rs       # Optional phase-gating logic
```

Keep phase-gating optional (feature-gated) if it's complex.

### 6.5 Integration Points

**With pattern_core:**
- Tasks should be queryable from agent context (similar to memory blocks)
- Agents should emit task updates as side effects of their execution

**With pattern_memory:**
- Task descriptions stored as markdown files
- Version history managed via jj

**With MCP / plugins:**
- Expose task operations as MCP tools
- External integrations can create/update tasks programmatically

---

## 7. Risk Mitigation and Maturity Path

### 7.1 MVP (Minimal Viable Product)

Start with explicit task creation + status tracking, no phase-gating:

- Create task
- List tasks
- Update status (open → blocked → done)
- Query by filter (status, dependencies)
- SQLite backend

**Estimated effort**: 1-2 weeks (Rust crate, basic schema, CLI interface)

### 7.2 Phase 2: Dependency Tracking

Add blocking relationships:

- Block task A on task B
- Query "what can I work on?" (tasks with no unmet dependencies)
- Auto-unblock tasks when dependencies complete

**Estimated effort**: 1 week (graph traversal, query optimization)

### 7.3 Phase 3: Markdown Sync + jj Integration

Align with Pattern's memory model:

- Task descriptions stored as markdown files
- jj backing for history
- Bidirectional sync between SQLite and markdown

**Estimated effort**: 2 weeks (file I/O, jj integration, conflict resolution)

### 7.4 Phase 4: Phase-Gating + Checkpoints (Optional)

If long-running workflows are a priority:

- Explicit phases
- Checkpoint/resume semantics
- Budget tracking

**Estimated effort**: 2-3 weeks (state serialization, resumption logic)

### 7.5 Non-Blocking Concerns

**Multi-agent write safety**: Assume single-writer (one agent per task) initially. If true concurrent writes are needed, add:
- Optimistic locking (version numbers)
- CRDT-based merging (similar to Loro for memory blocks)

**Distributed coordination**: If agents run on different machines, use:
- Git-backed storage (tasks synced via git push/pull)
- Event log / write-ahead log for ordering

**Observability**: Add logs for:
- Task creation/completion
- Dependency graph traversal
- Phase gate decisions

---

## 8. Comparison Table

| Aspect | Chainlink | Crosslink | ExoMonad |
|--------|-----------|-----------|----------|
| **Work-item model** | Issues + subissues | Issues + phases | Implicit (branches/PRs) |
| **Dependencies** | Explicit blocking relationships | Phase gates | Git branch dependencies |
| **Storage** | SQLite (local) | SQLite + git | Git (distributed) |
| **Multi-agent writes** | Single writer | Per-agent checkpoints | Per-agent worktrees |
| **Checkpoint/resume** | Implicit (sessions) | Explicit (phase boundaries) | Implicit (git branches) |
| **Coordination** | Within-session | Phased workflow | PR-based + messaging bus |
| **Maturity** | Production (agent-native) | Commercial (human-agent) | Research (limited docs) |
| **For Pattern** | Session persistence + VDD | Phase gates + checkpoints | Parallelism + isolation |

---

## References

- [github.com/dollspace-gay/chainlink](https://github.com/dollspace-gay/chainlink) — Local-first issue tracker for AI agents
- [forecast.bio/crosslink](https://forecast.bio/crosslink/) — Persistent memory and phase-gated workflow orchestration
- [recursion.wtf](https://recursion.wtf/) — Inanna Malick's blog (ExoMonad context)
- [Pattern Execution Models Reference](./execution-models.md#5-exomonad-haskell-based-agent-orchestration-limited-information) — Deeper dive on ExoMonad architecture
- [Pattern Agent Architecture](../architecture/pattern-agent-architecture.md) — How agents fit into Pattern's design
