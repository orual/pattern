# V2 Integration Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace SurrealDB with SQLite/pattern_db, rewrite memory system with Loro CRDT and agent-scoping, refactor DatabaseAgent to use new infrastructure.

**Architecture:** Three sequential chunks - database replacement provides foundation, memory rework builds on new storage, agent rework integrates both into unified runtime.

**Tech Stack:** SQLite + sqlx, Loro CRDT, pattern_db crate, sqlite-vec, FTS5

---

## Overview

This is a major refactoring effort split into four chunks:

| Chunk | Description | Depends On |
|-------|-------------|------------|
| **1. Database Layer** | Add db_v2 module wrapping pattern_db | None |
| **2. Memory System** | Agent-scoped, structured Loro documents | Chunk 1 |
| **2.5. Context Rework** | Block type handling, summaries, activity logs | Chunk 2 |
| **3. Agent Rework** | New DatabaseAgent using new systems | Chunks 1, 2, 2.5 |

Each chunk has its own detailed plan document (linked below) for subagent dispatch.

### Key Design Documents

- `docs/refactoring/v2-structured-memory-sketch.md` - Structured Loro types, block schemas
- `docs/refactoring/v2-memory-system.md` - Memory architecture details
- `docs/refactoring/v2-database-design.md` - SQLite schema

---

## Chunk 1: Rip Out SurrealDB

**Goal:** Remove all SurrealDB dependencies from pattern_core, replace with pattern_db.

**Scope:**
- Remove `surrealdb` dependency from pattern_core
- Remove Entity macro usage (replace with direct sqlx)
- Replace db/client.rs global DB with ConstellationDb pattern
- Replace db/ops.rs operations with pattern_db queries
- Update all code that imports from db module
- Keep existing in-memory Memory system temporarily (chunk 2 replaces it)

**Key Files to Change:**
- `crates/pattern_core/Cargo.toml` - remove surrealdb, add pattern_db
- `crates/pattern_core/src/db/` - entire module rewrite
- `crates/pattern_core/src/agent/entity.rs` - remove Entity derive
- `crates/pattern_core/src/memory.rs` - remove Entity derive (keep struct)
- `crates/pattern_core/src/message.rs` - remove Entity derive

**Deliverable:** pattern_core compiles and tests pass with SQLite backend, no SurrealDB.

**Detailed Plan:** `docs/plans/2025-01-23-chunk1-surrealdb-removal.md`

---

## Chunk 2: Memory System

**Goal:** Replace DashMap-based user-scoped memory with agent-scoped, structured Loro-backed system.

**Scope:**
- New `MemoryStore` trait abstracting memory operations
- Agent-scoped ownership (agent_id instead of owner_id/user_id)
- **Structured Loro documents** - Text, List, Map, Tree, Counter types
- **Block schemas** - Define structure for templated blocks
- Shared block system via `shared_block_agents` table
- Typed operations (append_to_list, set_field, increment_counter)

**Key Changes:**
- `crates/pattern_core/src/memory_v2/` - new module (alongside old)
- `memory_v2/types.rs` - BlockType, Permission, schemas
- `memory_v2/document.rs` - Loro wrapper with typed operations
- `memory_v2/store.rs` - MemoryStore trait + SQLite impl
- `memory_v2/sharing.rs` - Shared block support

**New Dependencies:**
- `loro = "1.10"` with `features = ["counter"]`

**Deliverable:** Memory system with structured types, agent-scoped, SQLite-backed.

**Detailed Plan:** `docs/plans/2025-01-23-chunk2-memory-rework.md`

**Design Reference:** `docs/refactoring/v2-structured-memory-sketch.md`

---

## Chunk 2.5: Context & Prompt Rework

**Goal:** Update context building for new block types, structured summaries, activity logging.

**Scope:**
- Block type differentiation (Core/Working/Log/Archival)
- **Structured summaries** - SummaryBlock with chaining, topics, time ranges
- **Activity log integration** - Events to database, Log block as view
- Context builder aware of block schemas
- System prompt templates for structured blocks

**Key Changes:**
- `crates/pattern_core/src/context_v2/` - new module
- `context_v2/builder.rs` - New context builder
- `context_v2/summary.rs` - Structured SummaryBlock
- `context_v2/activity.rs` - Activity event logging
- `context_v2/prompt.rs` - System prompt templates

**Key Features:**
- Log blocks show recent N entries with timestamps
- Structured blocks render to readable format for LLM
- Summaries include metadata (time range, topics, message count)
- Activity events persist to `activity_events` table

**Deliverable:** Context building that handles structured memory and rich summaries.

**Detailed Plan:** `docs/plans/2025-01-23-chunk2.5-context-rework.md` (TODO)

---

## Chunk 3: Agent Rework

**Goal:** Refactor DatabaseAgent to use new database and memory systems.

**Scope:**
- Remove SurrealDB connection from DatabaseAgent
- Inject ConstellationDb instead
- Use MemoryStore trait for memory operations
- Update context building to use new memory format
- Preserve message batching with SQLite backend
- Update AgentRecord persistence

**Key Changes:**
- `crates/pattern_core/src/agent/impls/db_agent.rs` - major refactor
- `crates/pattern_core/src/agent/entity.rs` - simplify, no Entity macro
- `crates/pattern_core/src/context/state.rs` - use new memory
- `crates/pattern_core/src/context/compression.rs` - adapt for SQLite

**Deliverable:** Agents run with full SQLite backend, memory persists correctly.

**Detailed Plan:** `docs/plans/2025-01-23-chunk3-agent-rework.md`

---

## Pattern_DB Gaps to Fill First

Before starting chunks, pattern_db needs these additions:

### Critical (Blocking)
1. **`update_agent()` full update** - Currently only status update exists
2. **`update_block()` full update** - Currently only content update exists
3. **Loro helper functions** - Deserialize/serialize helpers for application layer

### Important (Can parallelize)
4. **Message embedding storage** - Add embeddings column or table
5. **Batch integrity queries** - Check tool call pairing

---

## Execution Strategy

### Option A: Sequential (Safer)
1. Fill pattern_db gaps
2. Chunk 1 (SurrealDB removal)
3. Chunk 2 (Memory rework)
4. Chunk 3 (Agent rework)
5. Integration testing

### Option B: Parallel Foundation + Sequential Core
1. **Parallel:** Fill pattern_db gaps + write Chunk 1 plan details
2. **Sequential:** Execute Chunks 1 → 2 → 3
3. Integration testing

### Option C: Maximum Parallelism (Riskier)
1. Fill pattern_db gaps
2. **Parallel:** Chunk 1 (new db layer) + Chunk 2 design (memory interfaces)
3. **Sequential:** Chunk 2 implementation → Chunk 3
4. Integration testing

---

## Success Criteria

- [ ] `cargo check -p pattern_core` passes with no surrealdb imports
- [ ] `cargo test -p pattern_core` passes
- [ ] Memory blocks persist with agent_id ownership
- [ ] Loro versioning works (can view history, rollback)
- [ ] Search uses FTS5 + sqlite-vec
- [ ] Existing CLI commands work (agent create, chat, memory operations)
- [ ] Discord/Bluesky integrations unaffected

---

## Risk Mitigation

1. **Data Migration:** CAR export from v1, import to v2 - separate migration tool
2. **Feature Parity:** Keep existing tool interfaces, change backends only
3. **Testing:** Each chunk has integration tests before proceeding
4. **Rollback:** Git branches per chunk, can revert if needed

---

## Incremental Development Strategy

**Key insight:** We don't need everything working immediately. Old code can stay for compiler feedback.

**Approach:**
- Create new modules alongside old (e.g., `memory_v2.rs`, `agent_v2.rs`)
- Old SurrealDB code stays compilable - gives us type checking
- New code imports from pattern_db, implements new patterns
- Gradual swap-over: update callsites one at a time
- Delete old modules only when fully migrated and tested

**Benefits:**
- Compiler errors guide what's missing in new system
- Can run old and new side-by-side for comparison
- No "big bang" switch - reduces risk
- Easier to pause/resume work

**Example Structure During Migration:**
```
pattern_core/src/
├── memory.rs          # OLD: DashMap, user-scoped (keep for now)
├── memory_v2.rs       # NEW: Loro, agent-scoped
├── agent/
│   └── impls/
│       ├── db_agent.rs      # OLD: SurrealDB
│       └── db_agent_v2.rs   # NEW: SQLite/pattern_db
├── db/
│   ├── mod.rs         # OLD: SurrealDB ops (keep)
│   └── sqlite.rs      # NEW: pattern_db integration
```

---

## Next Steps

1. User confirms chunk strategy and execution option
2. Write detailed plan for Chunk 1
3. Begin execution (or dispatch to subagents)
