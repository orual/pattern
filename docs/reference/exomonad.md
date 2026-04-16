# ExoMonad: Multi-Agent Orchestration Framework

**Repository**: https://github.com/tidepool-heavy-industries/tidepool.git (within `/` root, integrated with Tidepool)  
**Commit**: cc0ebf815967a215dfb662120ce24347f402ee71 (2026-04-15)

## Context and Scope

ExoMonad is **not a separate project**—it is mentioned throughout Tidepool's documentation (`CLAUDE.md`, `lazy_thunk_plan/07-swarm-architecture.md`, `.claude/plans/orchestration.md`) as the **development orchestration model** used by Tidepool itself. The Tidepool repository is **developed using ExoMonad**, not embedded with it.

This document synthesizes references to ExoMonad found in the Tidepool source to clarify what it is and how it differs from Tidepool.

## What ExoMonad Does

ExoMonad is a **multi-agent orchestration system** that manages parallel AI agents working on the same codebase. Key capabilities:

### Git Worktree Isolation

- Each agent (TL, leaf, worker) gets a **dedicated git worktree**.
- Worktrees map to branches in a **hierarchy**:
  ```
  main                              [human]
  ├── main.core-repr                [TL - Claude Opus]
  │   ├── main.core-repr.scaffold   [leaf - Gemini]
  │   ├── main.core-repr.serial     [leaf - Gemini]
  │   └── main.core-repr.pretty     [leaf - Gemini]
  ├── main.core-eval                [TL - Claude Opus]
  │   └── ...
  ```
- PRs target **parent branches**, not main directly. Merges cascade up the tree.
- Prevents merge conflicts and race conditions by design.

### Fire-and-Forget Execution

**The TL does not wait for leaves:**

1. TL writes a detailed spec and spawns a leaf (via `spawn_leaf_subtree`).
2. TL immediately moves on to the next task.
3. Leaf works independently: commits, files PR.
4. GitHub poller detects Copilot review comments.
5. Leaf iterates against Copilot feedback until clean.
6. Leaf calls `notify_parent` with `success`—TL gets `[CHILD COMPLETE]`.
7. TL reviews the merged diff and merges up.

**Convergence is leaf + Copilot**, not TL. This enables **massive parallelism** without the TL becoming a bottleneck.

### Agent Roles

| Role | Tool | Type | Responsibility |
|------|------|------|-----------------|
| **Human** | N/A | Root | Owns main branch, approves phase gates, makes architectural decisions |
| **TL** | `spawn_leaf_subtree` / `spawn_workers` | Coordinator | Owns a subtree branch, decomposes work, spawns agents, merges PRs |
| **Leaf** | Spawned | Implementation | Owns a leaf branch, implements one task spec, files PR, iterates against Copilot |
| **Worker** | Spawned | Assistant | Works in parent's directory, does NOT create branches/commit/PR (parent commits) |

### Spawn Tool Selection

| Tool | Use When | Litmus Test |
|------|----------|------------|
| `spawn_leaf_subtree` | Any well-specified implementation task | Will agent add mod declarations, deps, re-exports? Multiple agents in parallel? → leaf. |
| `spawn_workers` | Single agent scaffolding OR multiple agents with **zero file overlap** | Can you list every file each agent touches without intersection? → workers. Otherwise → leaf. |
| `spawn_subtree` | Task needs further decomposition or architectural judgment | Almost never needed; 10-30x more expensive than leaf. |

**Default is `spawn_leaf_subtree`**. The overhead (branch + PR) is worth the quality improvement from Copilot review and parallelism.

### Spec Quality as a Practice

Since the TL doesn't iterate on specs, the **v1 spec must be production-quality**. All `AgentSpec` fields map directly to prompt sections:

| Field | Purpose |
|-------|---------|
| `boundary` | DO NOT rules; known failure modes (rendered FIRST) |
| `read_first` | Exact files to read before coding |
| `steps` | Numbered concrete actions with code snippets |
| `verify` | Exact shell commands to run |
| `done_criteria` | Measurable completion checklist |
| `context` | Code snippets, type signatures, examples |

**Anti-patterns** (noted as Gemini failure modes):
- Adds unnecessary dependencies → "ZERO external deps"
- Invents escape hatches → "No `todo!()`, `Raw(String)`"
- Writes thinking-out-loud comments → "Doc comments only"
- Renames types → "Use EXACT type signatures"
- Makes architectural decisions → "Do not change module structure"
- Overengineers → "This is N lines in M files, not a new module"

### Messaging and Coordination

Agents coordinate via:
- **Git branches + PR comments** (primary mechanism).
- **Shared task lists** (implied in `plans/README.md` tracking active phase).
- **Copilot review feedback** (injected into agent panes via GitHub poller).
- **`notify_parent` calls** (explicit completion signals).

## ExoMonad in Tidepool: Practical Example

The Tidepool codebase uses ExoMonad to manage parallel work on known issues. From `.claude/plans/orchestration.md`:

```
Plan | Branch name | Key file(s) | Independence
-----|-------------|------------|------
01   | fix/orphaned-eval-threads | tidepool-mcp/src/lib.rs | Independent
02   | fix/cap-exec-output | tidepool/src/main.rs | Independent
03   | fix/signal-closure-leak | tidepool-codegen/src/signal_safety.rs | Independent
04   | fix/emit-panic-to-result | tidepool-codegen/src/emit/expr.rs | Independent
05   | fix/mutex-poisoning | tidepool-mcp/Cargo.toml, tidepool-eval/src/eval.rs | Independent
06   | fix/letrec-alloc-hints | tidepool-codegen/src/emit/expr.rs | Conflicts with 04
07   | fix/deep-force-iterative | tidepool-eval/src/eval.rs | Independent
```

**7 independent worktrees**, each owned by a leaf agent, can be merged in parallel. The TL specifies each plan, spawns a leaf, and moves on. Merges happen as leaves complete and notify parent. This is **force multiplier for engineering throughput**.

## How ExoMonad Differs from Tidepool

| Aspect | ExoMonad | Tidepool |
|--------|----------|----------|
| **Purpose** | Orchestrate multi-agent development | Execute Haskell code safely in Rust |
| **Scope** | Agent coordination, git workflow, code review | Code runtime, sandboxing, JIT compilation |
| **Users** | Development teams, LLM-driven projects | Rust applications embedding Haskell |
| **Dependency** | Standalone orchestration framework | Independent; can be embedded without ExoMonad |
| **Integration** | Tidepool is developed **using** ExoMonad | Tidepool can be **embedded in** any Rust project |

**Neither depends on the other** for functionality. ExoMonad organizes how Tidepool is built; Tidepool is a runtime consumers embed in their applications. Tidepool's `CLAUDE.md` says "All rules from the exomonad project apply here" because Tidepool is **developed under ExoMonad's orchestration model**, not because Tidepool implements ExoMonad.

## Relevance to Pattern

If Pattern adopts a multi-agent architecture (which it appears to be building), **ExoMonad's orchestration model is directly applicable**:

1. **Worktree isolation**: Each agent (decision-making, memory-management, task-tracking) gets its own worktree + branch. Prevents merge conflicts.

2. **Fire-and-forget spawning**: TL decomposes work into detailed specs, spawns leaves (agents), moves on. Leaves iterate against feedback (Copilot) until done. TL reviews final merges.

3. **Parallelism by design**: No TL bottleneck. Multiple agents work in parallel on non-overlapping branches. Merges converge up the tree.

4. **Spec-first discipline**: Because agents don't iterate with the TL, specs must be complete and unambiguous. This forces clarity in the coordination model.

## Caveats and Unknowns

1. **ExoMonad is not open-source** (as of this writing). References in Tidepool's code are internal; the actual orchestration framework is not publicly available.

2. **Specific implementation details are opaque**. How `notify_parent` works, how the GitHub poller integrates with Claude Agent Teams, how branch hierarchies are managed—these are described in Tidepool's documentation but the ExoMonad codebase itself is not examined here.

3. **Coupling to Claude Agent Teams**: ExoMonad appears to hook into Claude Agent Teams' messaging bus (per `CLAUDE.md`). Unclear what happens if agents are not LLMs or are from different vendors.

4. **No public documentation**: ExoMonad has no standalone public docs (as of 2026-04-15). Understanding comes from Tidepool's usage examples and CLAUDE.md guidelines.

## Recommendation for Pattern

ExoMonad's orchestration model is **conceptually sound** for multi-agent systems. Pattern should:

1. **Adopt the worktree hierarchy** to prevent merge conflicts and enable parallelism.
2. **Practice spec-first discipline**: Detailed, unambiguous task specs before spawning agents.
3. **Use fire-and-forget dispatch**: TL doesn't wait. Agents iterate independently. TL reviews merges.
4. **Leverage Copilot review loops**: Build feedback directly into agent iteration.

If ExoMonad becomes publicly available, adopt it. If not, Pattern can implement the **orchestration pattern** directly (worktrees, branch hierarchy, PR-based convergence, Copilot review integration).
