# SurrealDB Compatibility Crate Extraction Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extract db_v1, export, and old memory types into `pattern_surreal_compat` crate to unblock Phase E integration work.

**Architecture:** Create a new crate containing all SurrealDB-dependent code that Phase E is deprecating. This allows pattern_core to compile with the new V2 architecture while preserving the old code for migration tools and reference.

**Tech Stack:** Rust, SurrealDB, pattern_macros Entity derive, DAG-CBOR/CAR exports

---

## Background

Phase E is replacing SurrealDB with SQLite (via pattern_db). However, several modules still depend on:
1. `db_v1` - SurrealDB database layer with Entity macro
2. `export` - CAR file export/import using SurrealDB entities
3. Old `memory.rs` - `MemoryBlock` Entity and `Memory` cache (replaced by memory/ CRDT system)

Current state:
- **E2: Tool module consolidation** - DONE (ToolContext-only)
- **E2.5: Trait swap** - DONE (AgentV2 → Agent, DatabaseAgentV2 → DatabaseAgent)
- 100+ compilation errors in pattern_core due to missing/moved types
- Blocked modules: coordination/groups, data_source/*, constellation_memory, etc.

Remaining Phase E tasks after this extraction:
- E3: Heartbeat processor
- E4: Coordination patterns
- E5: Queue/Wakeup to pattern_db
- E6: RuntimeContext + CLI/Discord

## Files to Extract

### From pattern_core/src/ (move to pattern_surreal_compat/)

**db_v1/ (entire module)**
- `mod.rs` - DatabaseBackend trait, DatabaseError, configs
- `client.rs` - SurrealDB connection handling
- `entity/mod.rs` - Entity trait, DbEntity
- `entity/base.rs` - BaseTask, BaseEvent, AgentMemoryRelation
- `migration.rs` - SurrealDB migrations
- `ops.rs` - Query operations
- `ops/atproto.rs` - ATProto-specific ops
- `schema.rs` - ToolCall, EnergyLevel schemas

**export/ (entire module)**
- `mod.rs` - Export constants
- `types.rs` - ExportManifest, AgentExport, MessageChunk, etc.
- `exporter.rs` - AgentExporter
- `importer.rs` - AgentImporter

**From git history (main branch)**
- `memory.rs` → `memory_v1.rs` - MemoryBlock Entity, Memory cache
- Note: `MemoryPermission` and `MemoryType` stay in pattern_core (already in memory/mod.rs)


## Modules to Temporarily Remove from pattern_core

After extraction, these modules still won't compile due to deep V1 dependencies.
Move them out of source tree (keep as .bak for reference):

1. `constellation_memory.rs` - Uses old MemoryBlock
2. `data_source/bluesky.rs` - Uses AgentHandle, old MemoryBlock
3. `data_source/coordinator.rs` - Uses old MemoryBlock
4. `data_source/file.rs` - Uses old MemoryBlock
5. `data_source/helpers.rs` - Uses AgentHandle
6. `data_source/homeassistant.rs` - Uses old MemoryBlock
7. `coordination/groups.rs` - Uses AgentRecord (old entity), DbEntity trait
8. Parts of `agent/entity.rs` - Uses db_v1 helpers

---

## Task Breakdown

### Task 1: Create pattern_surreal_compat crate structure

**Files:**
- Create: `crates/pattern_surreal_compat/Cargo.toml`
- Create: `crates/pattern_surreal_compat/src/lib.rs`

**Step 1: Create Cargo.toml**

```toml
[package]
name = "pattern-surreal-compat"
version = "0.4.0"
edition = "2021"
description = "SurrealDB compatibility layer for Pattern (deprecated, for migration only)"

[dependencies]
# Core dependencies from pattern_core's db_v1
surrealdb = { version = "2.0.4", features = ["kv-mem"] }
async-trait = "0.1"
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "1.0"
miette = { version = "7.0", features = ["fancy"] }
tracing = "0.1"
compact_str = { version = "0.8", features = ["serde"] }
dashmap = "6.1"
cid = "0.11"

# Pattern dependencies
pattern-macros = { path = "../pattern_macros" }

[features]
default = []
surreal-remote = ["surrealdb/protocol-ws"]
```

**Step 2: Create initial lib.rs**

```rust
//! SurrealDB Compatibility Layer for Pattern
//!
//! This crate contains deprecated SurrealDB-based code preserved for:
//! - Migration from SurrealDB to SQLite
//! - CAR file export/import functionality
//! - Reference during Phase E integration
//!
//! **Do not add new code here. This crate is in maintenance-only mode.**

pub mod db;
pub mod entity;
pub mod export;
pub mod memory;

// Re-export key types at crate root
pub use db::{DatabaseBackend, DatabaseConfig, DatabaseError, Query, Result as DbResult};
pub use entity::{AgentMemoryRelation, BaseEvent, BaseTask, DbEntity};
pub use memory::{Memory, MemoryBlock};
```

**Step 3: Run cargo check on new crate skeleton**

```bash
cargo check -p pattern-surreal-compat
```

Expected: Errors about missing modules (we'll add them next)

**Step 4: Commit**

```bash
git add crates/pattern_surreal_compat/
git commit -m "chore: create pattern_surreal_compat crate skeleton"
```

---

### Task 2: Move db_v1 module

**Step 1: Copy db_v1 to new crate**

```bash
cp -r crates/pattern_core/src/db_v1/* crates/pattern_surreal_compat/src/db/
```

**Step 2: Update imports in db/mod.rs**

Replace `crate::` imports with either:
- Direct imports from dependencies
- `crate::` for items within pattern_surreal_compat

Key changes:
- `crate::embeddings::EmbeddingError` → define locally or use miette
- `crate::id::*` → use pattern_id crate or define locally
- `crate::memory::MemoryPermission` → will be in crate::memory

**Step 3: Fix compilation errors iteratively**

```bash
cargo check -p pattern-surreal-compat 2>&1 | head -30
```

Fix import paths until module compiles.

**Step 4: Commit**

```bash
git add crates/pattern_surreal_compat/src/db/
git commit -m "feat(surreal-compat): add db module from pattern_core/db_v1"
```

---

### Task 3: Move entity module

**Step 1: Copy entity submodule**

```bash
cp -r crates/pattern_core/src/db_v1/entity/* crates/pattern_surreal_compat/src/entity/
```

**Step 2: Update imports**

The entity module depends on:
- `crate::id::*` - ID types
- `crate::memory::MemoryPermission` - permission enum
- `crate::users::User` - user entity
- `crate::agent::AgentRecord` - agent entity

These need to either:
- Be defined locally in pattern_surreal_compat
- Be imported from pattern_core (creates circular dep - avoid)
- Be stubbed out

For now, stub or copy necessary types.

**Step 3: Compile and fix**

```bash
cargo check -p pattern-surreal-compat
```

**Step 4: Commit**

```bash
git add crates/pattern_surreal_compat/src/entity/
git commit -m "feat(surreal-compat): add entity module"
```

---

### Task 4: Restore memory.rs from git history

**Step 1: Extract old memory.rs**

```bash
git show main:crates/pattern_core/src/memory.rs > crates/pattern_surreal_compat/src/memory.rs
```

**Step 2: Rename MemoryPermission/MemoryType if needed**

The new pattern_core/memory/ has its own `MemoryPermission` and `MemoryType`.
Check if they're compatible - if so, we can re-export from pattern_core.
If not, keep local copies with a suffix like `MemoryPermissionV1`.

**Step 3: Update imports**

- `crate::MemoryId` → define locally or import from pattern_id
- `crate::UserId` → same
- `pattern_macros::Entity` → import from pattern_macros

**Step 4: Compile and fix**

**Step 5: Commit**

```bash
git add crates/pattern_surreal_compat/src/memory.rs
git commit -m "feat(surreal-compat): restore MemoryBlock entity from main branch"
```

---

### Task 5: Move export module

**Step 1: Copy export module**

```bash
cp -r crates/pattern_core/src/export/* crates/pattern_surreal_compat/src/export/
```

**Step 2: Update types.rs imports**

The export module references:
- `crate::AgentId` - ID type
- `crate::agent::AgentRecord` - agent entity (needs stub or copy)
- `crate::message::Message` - message entity
- `crate::context::CompressionStrategy` - config type
- `crate::config::ToolRuleConfig` - tool rules
- `crate::coordination::groups::*` - group types

This is the most complex module. Options:
1. Stub out complex types we don't need for export
2. Copy necessary types
3. Make export a separate crate with optional deps

For now, stub complex types and focus on compiling.

**Step 3: Compile and fix iteratively**

**Step 4: Commit**

```bash
git add crates/pattern_surreal_compat/src/export/
git commit -m "feat(surreal-compat): add export module"
```

---

### Task 6: Add ID types

The db_v1 and entity modules need ID types. Options:

1. **Create pattern_id crate** (cleanest, but more work)
2. **Copy ID definitions** into pattern_surreal_compat
3. **Import from pattern_core** (may work if id module doesn't depend on broken code)

**Check pattern_core id module:**

```bash
cargo check -p pattern-core --lib 2>&1 | grep "id::" | head -10
```

If id module compiles independently, we can import from pattern_core.

**Step 1: Decide approach based on check results**

**Step 2: Implement chosen approach**

**Step 3: Commit**

---

### Task 7: Remove broken modules from pattern_core

After extraction, temporarily move non-compiling modules out of source tree:

**Step 1: Backup broken modules**

```bash
cd crates/pattern_core/src

# Data sources that need AgentHandle/old MemoryBlock
mkdir -p data_source.bak
mv data_source/bluesky.rs data_source.bak/
mv data_source/coordinator.rs data_source.bak/
mv data_source/file.rs data_source.bak/
mv data_source/helpers.rs data_source.bak/
mv data_source/homeassistant.rs data_source.bak/

# Constellation memory
mv constellation_memory.rs constellation_memory.rs.bak

# Keep data_source/mod.rs but comment out broken modules
```

**Step 2: Update data_source/mod.rs**

Comment out broken module declarations:

```rust
// Temporarily disabled - pending Phase E completion
// mod bluesky;
// mod coordinator;
// mod file;
// mod helpers;
// mod homeassistant;

mod cursor_store;
mod trait_def;
// ... keep working modules
```

**Step 3: Update lib.rs if needed**

Comment out broken module exports.

**Step 4: Run cargo check**

```bash
cargo check -p pattern-core
```

Goal: Reduce errors significantly. Some may remain in coordination/groups.rs etc.

**Step 5: Commit**

```bash
git add -A
git commit -m "chore: temporarily disable modules pending Phase E completion

Moved to .bak:
- data_source/{bluesky,coordinator,file,helpers,homeassistant}.rs
- constellation_memory.rs

These will be restored after Phase E integration completes."
```

---

### Task 8: Handle coordination/groups.rs

This module heavily uses SurrealDB entities. Options:

1. **Move to pattern_surreal_compat** - if it's primarily SurrealDB code
2. **Stub it** - if we're replacing with new implementation
3. **Fix incrementally** - update to use pattern_db

Based on Phase E plan, groups should move to new architecture. For now:

**Step 1: Check what groups.rs actually does**

```bash
wc -l crates/pattern_core/src/coordination/groups.rs
head -100 crates/pattern_core/src/coordination/groups.rs
```

**Step 2: Decide approach**

If it's mostly entity definitions + CRUD → move to compat
If it's coordination logic → keep and fix

**Step 3: Implement chosen approach**

**Step 4: Commit**

---

### Task 9: Verify pattern_core compiles

**Step 1: Full check**

```bash
cargo check -p pattern-core 2>&1 | tee /tmp/check-output.txt
```

**Step 2: Count remaining errors**

```bash
grep "^error\[" /tmp/check-output.txt | wc -l
```

**Goal:** < 20 errors, all in known locations that Phase E will fix.

**Step 3: Document remaining issues**

Create `/tmp/remaining-issues.md` listing what still needs work.

**Step 4: Commit any final fixes**

---

### Task 10: Add workspace member

**Step 1: Update root Cargo.toml**

Add pattern-surreal-compat to workspace members:

```toml
members = [
    # ... existing
    "crates/pattern_surreal_compat",
]
```

**Step 2: Full workspace check**

```bash
cargo check --workspace 2>&1 | tail -20
```

**Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add pattern_surreal_compat to workspace"
```

---

## Success Criteria

- [ ] `pattern_surreal_compat` crate compiles
- [ ] `pattern_core` compiles with reduced module set
- [ ] All db_v1 code preserved in new crate
- [ ] Export functionality preserved
- [ ] Old MemoryBlock entity preserved
- [ ] Broken modules backed up (not deleted)
- [ ] < 20 remaining errors in pattern_core (all from known Phase E targets)

---

## Post-Extraction: Resume Phase E

After this extraction, continue with remaining Phase E tasks:

1. ~~**E2.5 Trait Swap**~~ - DONE
2. **E3 Heartbeat** - fix for new Agent trait
3. **E4 Coordination** - fix patterns for new trait (groups.rs may need special handling)
4. **E5 Queue/Wakeup** - port to pattern_db
5. **E6 RuntimeContext** - create and wire up

**Post-E6: Additional module restoration**

After core Phase E completes:
- **E7 Data Sources** - restore and update data_source modules (bluesky, coordinator, file, helpers, homeassistant)
- **E8 Auth/Identity** - fix oauth, atproto_identity, discord_identity modules

The compat crate remains available for:
- Migration scripts (SurrealDB → SQLite)
- CAR export/import (if needed)
- Reference during debugging

---

## Notes

- Do NOT delete .bak files until Phase E is complete and tested
- The compat crate is deprecated - no new features
- ID types may need their own crate eventually (pattern_id)
- Export module is complex - may need further refactoring later
