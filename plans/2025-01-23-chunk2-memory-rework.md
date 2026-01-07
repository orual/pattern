# Chunk 2: Memory System Rework

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build new agent-scoped, Loro-backed memory system with **structured types** (Text, List, Map, Tree, Counter) alongside existing DashMap-based system.

**Architecture:** New `memory_v2` module with MemoryStore trait, Loro documents with typed containers, block schemas, SQLite persistence via db_v2.

**Tech Stack:** Loro CRDT 1.10+ (with counter feature), pattern_db, db_v2 module from Chunk 1

**Depends On:** Chunk 1 (db_v2 module exists)

**Design Reference:** `docs/refactoring/v2-structured-memory-sketch.md`

---

## Philosophy

**DO:**
- Add `memory_v2.rs` alongside `memory.rs`
- Add `loro` crate dependency
- Create new MemoryStore trait
- Build Loro document helpers
- Keep old memory.rs untouched

**DON'T:**
- Modify existing memory.rs
- Change tool implementations yet (Task 7+)
- Break compilation
- Try to share types between old and new

---

## Task 1: Add Loro Dependency

**Files:**
- Modify: `crates/pattern_core/Cargo.toml`

**Step 1: Add loro dependency with counter feature**

```toml
[dependencies]
# ... existing deps ...
loro = { version = "1.10", features = ["counter"] }
```

The `counter` feature enables `LoroCounter` for numeric tracking (energy levels, counts, etc.).

**Step 2: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/pattern_core/Cargo.toml
git commit -m "feat(pattern_core): add loro CRDT dependency with counter feature"
```

---

## Task 2: Create Memory V2 Module Structure with Schemas

**Files:**
- Create: `crates/pattern_core/src/memory_v2/mod.rs`
- Create: `crates/pattern_core/src/memory_v2/types.rs`
- Create: `crates/pattern_core/src/memory_v2/schema.rs`
- Modify: `crates/pattern_core/src/lib.rs`

**Step 1: Create module directory**

```bash
mkdir -p crates/pattern_core/src/memory_v2
```

**Step 2: Create types.rs with core types**

```rust
//! Core types for the v2 memory system
//!
//! Key differences from v1:
//! - Agent-scoped (agent_id) instead of user-scoped (owner_id)
//! - Loro CRDT documents for versioning
//! - Explicit block types (Core, Working, Archival, Log)
//! - Structured schemas (Text, Map, List, Tree, Log)
//! - Descriptions required (LLM-critical per Letta pattern)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Unique identifier for a memory block
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryBlockId(pub String);

impl MemoryBlockId {
    pub fn new() -> Self {
        Self(format!("mem_{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for MemoryBlockId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for MemoryBlockId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for MemoryBlockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Memory block types determining context inclusion behavior
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockType {
    /// Always in context, critical for agent identity
    /// Examples: persona, human, system guidelines
    Core,

    /// Working memory, can be swapped in/out based on relevance
    /// Examples: scratchpad, current_task, session_notes
    Working,

    /// Long-term storage, NOT in context by default
    /// Retrieved via recall/search tools using semantic search
    Archival,

    /// System-maintained logs (read-only to agent)
    /// Recent entries shown in context, older entries searchable
    Log,
}

impl BlockType {
    /// Whether this block type is always included in context
    pub fn always_in_context(&self) -> bool {
        matches!(self, BlockType::Core | BlockType::Log)
    }

    /// Whether this block type is searchable
    pub fn is_searchable(&self) -> bool {
        matches!(self, BlockType::Archival | BlockType::Log)
    }
}

/// Permission levels for memory operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Permission {
    /// Can only read
    ReadOnly,
    /// Can append but not overwrite
    AppendOnly,
    /// Full read/write access
    ReadWrite,
}

/// Access level for shared blocks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedAccess {
    /// Can read but not modify
    Read,
    /// Can append but not overwrite
    Append,
    /// Full read/write access
    Write,
}

/// Metadata for a memory block (without Loro document)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMetadata {
    /// Unique identifier
    pub id: MemoryBlockId,

    /// Owning agent (NOT user - this is the key change from v1)
    pub agent_id: String,

    /// Semantic label: "persona", "human", "scratchpad", etc.
    pub label: String,

    /// Description for the LLM (critical for proper usage)
    /// This tells the agent what the block is for and how to use it
    pub description: String,

    /// Block type determines context inclusion behavior
    pub block_type: BlockType,

    /// Schema defining the block's structure (None = free-form text)
    pub schema: Option<crate::memory_v2::schema::BlockSchema>,

    /// Character limit for the block (for rendered output)
    pub char_limit: usize,

    /// Whether the agent can modify this block
    pub read_only: bool,

    /// Whether this block is pinned (cannot be swapped from context)
    pub pinned: bool,

    /// Permission level
    pub permission: Permission,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last modified timestamp
    pub updated_at: DateTime<Utc>,
}

/// A memory version snapshot for history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryVersion {
    /// Version identifier (Loro frontiers as bytes)
    pub frontiers: Vec<u8>,

    /// When this version was created
    pub timestamp: DateTime<Utc>,

    /// Who made the change
    pub changed_by: ChangeSource,

    /// Optional description of what changed
    pub summary: Option<String>,
}

/// Source of a memory change
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeSource {
    Agent(String),
    User,
    System,
    Sync,
}

/// Result of a memory operation
#[derive(Debug)]
pub enum MemoryOpResult {
    /// Operation succeeded
    Success,
    /// Block not found
    NotFound,
    /// Permission denied
    PermissionDenied(String),
    /// Block limit exceeded
    LimitExceeded { current: usize, limit: usize },
    /// Schema violation
    SchemaViolation(String),
    /// Other error
    Error(String),
}

/// Archival memory entry (separate from blocks)
/// These are individual searchable entries the agent can store/retrieve
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivalEntry {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_type_context_inclusion() {
        assert!(BlockType::Core.always_in_context());
        assert!(BlockType::Log.always_in_context());
        assert!(!BlockType::Working.always_in_context());
        assert!(!BlockType::Archival.always_in_context());
    }

    #[test]
    fn test_memory_block_id_generation() {
        let id = MemoryBlockId::new();
        assert!(id.as_str().starts_with("mem_"));
    }
}
```

**Step 3: Create schema.rs with block schema definitions**

```rust
//! Block schema definitions for structured memory
//!
//! Schemas define the structure of a memory block's Loro document,
//! enabling typed operations like `set_field`, `append_to_list`, etc.

use serde::{Deserialize, Serialize};

/// Block schema defines the structure of a memory block's Loro document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockSchema {
    /// Free-form text (default, backward compatible)
    /// Uses: LoroText container
    Text,

    /// Key-value pairs with optional field definitions
    /// Uses: LoroMap with nested containers per field
    Map {
        fields: Vec<FieldDef>,
    },

    /// Ordered list of items
    /// Uses: LoroList (or LoroMovableList if reordering needed)
    List {
        item_schema: Option<Box<BlockSchema>>,
        max_items: Option<usize>,
    },

    /// Rolling log (full history kept in storage, limited display in context)
    /// Uses: LoroList - NO trimming on persist, display_limit applied at render time
    Log {
        /// How many entries to show when rendering for context (block-level setting)
        display_limit: usize,
        entry_schema: LogEntrySchema,
    },

    /// Hierarchical tree structure
    /// Uses: LoroTree
    Tree {
        node_schema: Option<Box<BlockSchema>>,
    },

    /// Custom composite with multiple named sections
    /// Uses: LoroMap with section containers
    Composite {
        sections: Vec<(String, BlockSchema)>,
    },
}

impl Default for BlockSchema {
    fn default() -> Self {
        BlockSchema::Text
    }
}

/// Field definition for Map schemas
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    /// Field name (used as Loro container key)
    pub name: String,

    /// Description for LLM understanding
    pub description: String,

    /// Field type determines Loro container
    pub field_type: FieldType,

    /// Whether the field must have a value
    pub required: bool,

    /// Default value (used when field is missing)
    pub default: Option<serde_json::Value>,
}

/// Field types mapping to Loro containers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    /// Single text value -> LoroText
    Text,

    /// Numeric value -> stored in LoroMap as JSON number
    /// Can use LoroCounter for increment/decrement operations
    Number,

    /// Boolean value -> stored in LoroMap as JSON boolean
    Boolean,

    /// List of values -> LoroList
    List,

    /// Timestamp (ISO 8601 string)
    Timestamp,

    /// Counter with increment/decrement -> LoroCounter
    Counter,
}

/// Schema for log entries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntrySchema {
    /// Include timestamp in each entry
    pub timestamp: bool,

    /// Include agent_id in each entry
    pub agent_id: bool,

    /// Additional fields per entry
    pub fields: Vec<FieldDef>,
}

impl Default for LogEntrySchema {
    fn default() -> Self {
        Self {
            timestamp: true,
            agent_id: false,
            fields: vec![
                FieldDef {
                    name: "content".into(),
                    description: "Log entry content".into(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                },
            ],
        }
    }
}

/// Pre-defined schemas for common use cases
pub mod templates {
    use super::*;

    /// Partner profile schema - tracks information about the human
    pub fn partner_profile() -> BlockSchema {
        BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".into(),
                    description: "Partner's preferred name".into(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                },
                FieldDef {
                    name: "preferences".into(),
                    description: "Known preferences and patterns".into(),
                    field_type: FieldType::List,
                    required: false,
                    default: Some(serde_json::json!([])),
                },
                FieldDef {
                    name: "energy_level".into(),
                    description: "Current energy level (1-10)".into(),
                    field_type: FieldType::Counter,
                    required: false,
                    default: Some(serde_json::json!(5)),
                },
                FieldDef {
                    name: "current_focus".into(),
                    description: "What they're currently working on".into(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                },
                FieldDef {
                    name: "last_interaction".into(),
                    description: "Timestamp of last interaction".into(),
                    field_type: FieldType::Timestamp,
                    required: false,
                    default: None,
                },
            ],
        }
    }

    /// Task list schema - ordered list of tasks
    pub fn task_list() -> BlockSchema {
        BlockSchema::List {
            item_schema: Some(Box::new(BlockSchema::Map {
                fields: vec![
                    FieldDef {
                        name: "title".into(),
                        description: "Task title".into(),
                        field_type: FieldType::Text,
                        required: true,
                        default: None,
                    },
                    FieldDef {
                        name: "done".into(),
                        description: "Whether task is completed".into(),
                        field_type: FieldType::Boolean,
                        required: true,
                        default: Some(serde_json::json!(false)),
                    },
                    FieldDef {
                        name: "priority".into(),
                        description: "Priority level (low/medium/high)".into(),
                        field_type: FieldType::Text,
                        required: false,
                        default: Some(serde_json::json!("medium")),
                    },
                    FieldDef {
                        name: "due".into(),
                        description: "Due date/time".into(),
                        field_type: FieldType::Timestamp,
                        required: false,
                        default: None,
                    },
                ],
            })),
            max_items: Some(100),
        }
    }

    /// Observation log schema - agent-managed log of observations/events
    /// NOTE: This is for agent memory, NOT constellation activity telemetry
    pub fn observation_log() -> BlockSchema {
        BlockSchema::Log {
            display_limit: 20,  // Show last 20 in context (full history kept for search)
            entry_schema: LogEntrySchema {
                timestamp: true,
                agent_id: false,  // Agent owns this, no need to track
                fields: vec![
                    FieldDef {
                        name: "observation".into(),
                        description: "What was observed".into(),
                        field_type: FieldType::Text,
                        required: true,
                        default: None,
                    },
                    FieldDef {
                        name: "context".into(),
                        description: "Context or source".into(),
                        field_type: FieldType::Text,
                        required: false,
                        default: None,
                    },
                ],
            },
        }
    }

    /// Simple scratchpad - free-form text
    pub fn scratchpad() -> BlockSchema {
        BlockSchema::Text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_schema_is_text() {
        assert!(matches!(BlockSchema::default(), BlockSchema::Text));
    }

    #[test]
    fn test_partner_profile_has_expected_fields() {
        let schema = templates::partner_profile();
        if let BlockSchema::Map { fields } = schema {
            let names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
            assert!(names.contains(&"name"));
            assert!(names.contains(&"preferences"));
            assert!(names.contains(&"energy_level"));
        } else {
            panic!("Expected Map schema");
        }
    }

    #[test]
    fn test_task_list_has_max_items() {
        let schema = templates::task_list();
        if let BlockSchema::List { max_items, .. } = schema {
            assert_eq!(max_items, Some(100));
        } else {
            panic!("Expected List schema");
        }
    }
}
```

**Step 4: Create mod.rs**

```rust
//! V2 Memory System
//!
//! Agent-scoped, Loro-backed memory with versioning and history.
//!
//! # Key Differences from V1
//!
//! - **Ownership**: Agent-scoped (`agent_id`) not user-scoped (`owner_id`)
//! - **Storage**: Loro CRDT documents with SQLite persistence
//! - **Versioning**: Full history via Loro, time-travel capable
//! - **No Cache**: SQLite is source of truth, no DashMap
//! - **Block Types**: Core, Working, Archival, Log
//! - **Schemas**: Structured types (Map, List, Tree, Counter)
//! - **Descriptions**: Required for LLM understanding (Letta pattern)

pub mod schema;
mod types;

pub use schema::BlockSchema;
pub use types::*;

// Placeholder for future modules
// mod document;  // Loro document operations
// mod store;     // MemoryStore trait and implementation
// mod context;   // Context building for LLM calls
```

**Step 5: Add to lib.rs**

```rust
pub mod memory_v2;
```

**Step 6: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 7: Run tests**

Run: `cargo test -p pattern_core memory_v2`
Expected: PASS

**Step 8: Commit**

```bash
git add crates/pattern_core/src/memory_v2/ crates/pattern_core/src/lib.rs
git commit -m "feat(pattern_core): add memory_v2 module with schemas and structured types"
```

---

## Task 3: Create Structured Loro Document Helpers

**Files:**
- Create: `crates/pattern_core/src/memory_v2/document.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

This task creates a `StructuredDocument` wrapper that handles all Loro container types based on block schema.

**Step 1: Create document.rs**

```rust
//! Loro document operations for structured memory blocks
//!
//! Each memory block's content is stored as a Loro document.
//! The document structure depends on the block's schema:
//! - Text schema: Single LoroText container
//! - Map schema: LoroMap with nested containers per field
//! - List schema: LoroList of items
//! - Log schema: LoroList with auto-trim
//!
//! All documents provide automatic versioning, history, and CRDT merging.

use loro::{LoroDoc, LoroText, LoroList, LoroMap, ExportMode, Frontiers, LoroValue};
use crate::memory_v2::schema::{BlockSchema, FieldType};
use chrono::Utc;
use serde_json::Value as JsonValue;

/// Wrapper around LoroDoc for schema-aware operations
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,
}

impl StructuredDocument {
    /// Create a new document with the given schema
    pub fn new(schema: BlockSchema) -> Self {
        Self {
            doc: LoroDoc::new(),
            schema,
        }
    }

    /// Create with default Text schema (backward compatible)
    pub fn new_text() -> Self {
        Self::new(BlockSchema::Text)
    }

    /// Create from an existing Loro snapshot
    pub fn from_snapshot(snapshot: &[u8], schema: BlockSchema) -> Result<Self, LoroError> {
        let doc = LoroDoc::new();
        doc.import(snapshot)?;
        Ok(Self { doc, schema })
    }

    /// Apply updates to the document
    pub fn apply_updates(&self, updates: &[u8]) -> Result<(), LoroError> {
        self.doc.import(updates)?;
        Ok(())
    }

    /// Get the schema
    pub fn schema(&self) -> &BlockSchema {
        &self.schema
    }

    // ========== Text Operations (for Text schema) ==========

    /// Get the text content (Text schema only)
    pub fn text_content(&self) -> String {
        self.doc.get_text("content").to_string()
    }

    /// Set text content (replaces existing)
    pub fn set_text(&self, content: &str) -> Result<(), LoroError> {
        let text = self.doc.get_text("content");
        let current = text.to_string();
        if !current.is_empty() {
            text.delete(0, current.len())?;
        }
        text.insert(0, content)?;
        Ok(())
    }

    /// Append to text content
    pub fn append_text(&self, content: &str) -> Result<(), LoroError> {
        let text = self.doc.get_text("content");
        let len = text.len_unicode();
        text.insert(len, content)?;
        Ok(())
    }

    /// Find and replace in text
    pub fn replace_text(&self, find: &str, replace: &str) -> Result<bool, LoroError> {
        let text = self.doc.get_text("content");
        let current = text.to_string();
        if let Some(pos) = current.find(find) {
            text.delete(pos, find.len())?;
            text.insert(pos, replace)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    // ========== Map Operations (for Map schema) ==========

    /// Get a field value from a Map schema block
    pub fn get_field(&self, field: &str) -> Option<JsonValue> {
        let map = self.doc.get_map("fields");
        map.get(field).map(|v| loro_to_json(&v))
    }

    /// Set a field value in a Map schema block
    pub fn set_field(&self, field: &str, value: JsonValue) -> Result<(), LoroError> {
        let map = self.doc.get_map("fields");
        let loro_val = json_to_loro(&value);
        map.insert(field, loro_val)?;
        Ok(())
    }

    /// Get a text field (returns as String)
    pub fn get_text_field(&self, field: &str) -> Option<String> {
        // Text fields are stored as nested LoroText containers
        let map = self.doc.get_map("fields");
        map.get(field).and_then(|v| {
            match v {
                LoroValue::String(s) => Some(s.to_string()),
                _ => None,
            }
        })
    }

    /// Set a text field
    pub fn set_text_field(&self, field: &str, value: &str) -> Result<(), LoroError> {
        let map = self.doc.get_map("fields");
        map.insert(field, value)?;
        Ok(())
    }

    /// Get a list field
    pub fn get_list_field(&self, field: &str) -> Vec<JsonValue> {
        let map = self.doc.get_map("fields");
        match map.get(field) {
            Some(LoroValue::List(list)) => {
                list.iter().map(|v| loro_to_json(v)).collect()
            }
            _ => vec![],
        }
    }

    /// Append to a list field
    pub fn append_to_list_field(&self, field: &str, item: JsonValue) -> Result<(), LoroError> {
        // For list fields, we use a nested LoroList
        let list = self.doc.get_list(&format!("list_{}", field));
        let loro_val = json_to_loro(&item);
        list.push(loro_val)?;
        Ok(())
    }

    /// Remove from a list field by index
    pub fn remove_from_list_field(&self, field: &str, index: usize) -> Result<(), LoroError> {
        let list = self.doc.get_list(&format!("list_{}", field));
        list.delete(index, 1)?;
        Ok(())
    }

    // ========== Counter Operations ==========

    /// Get counter value
    pub fn get_counter(&self, field: &str) -> i64 {
        // Counters are stored as i64 in the fields map
        let map = self.doc.get_map("fields");
        match map.get(field) {
            Some(LoroValue::I64(n)) => n,
            Some(LoroValue::Double(n)) => n as i64,
            _ => 0,
        }
    }

    /// Increment counter (can be negative for decrement)
    pub fn increment_counter(&self, field: &str, delta: i64) -> Result<i64, LoroError> {
        let map = self.doc.get_map("fields");
        let current = self.get_counter(field);
        let new_value = current + delta;
        map.insert(field, new_value)?;
        Ok(new_value)
    }

    // ========== List Operations (for List schema blocks) ==========

    /// Get all items in a List schema block
    pub fn list_items(&self) -> Vec<JsonValue> {
        let list = self.doc.get_list("items");
        list.iter()
            .map(|v| loro_to_json(&v))
            .collect()
    }

    /// Push item to List schema block
    pub fn push_item(&self, item: JsonValue) -> Result<(), LoroError> {
        let list = self.doc.get_list("items");
        let loro_val = json_to_loro(&item);
        list.push(loro_val)?;
        Ok(())
    }

    /// Insert item at index
    pub fn insert_item(&self, index: usize, item: JsonValue) -> Result<(), LoroError> {
        let list = self.doc.get_list("items");
        let loro_val = json_to_loro(&item);
        list.insert(index, loro_val)?;
        Ok(())
    }

    /// Delete item at index
    pub fn delete_item(&self, index: usize) -> Result<(), LoroError> {
        let list = self.doc.get_list("items");
        list.delete(index, 1)?;
        Ok(())
    }

    /// Get list length
    pub fn list_len(&self) -> usize {
        let list = self.doc.get_list("items");
        list.len()
    }

    // ========== Log Operations (for Log schema blocks) ==========

    /// Get log entries (most recent first)
    pub fn log_entries(&self, limit: Option<usize>) -> Vec<JsonValue> {
        let list = self.doc.get_list("entries");
        let entries: Vec<_> = list.iter()
            .map(|v| loro_to_json(&v))
            .collect();

        // Return most recent entries (end of list)
        match limit {
            Some(n) => entries.into_iter().rev().take(n).collect(),
            None => entries.into_iter().rev().collect(),
        }
    }

    /// Append log entry
    pub fn append_log_entry(&self, entry: JsonValue) -> Result<(), LoroError> {
        let list = self.doc.get_list("entries");
        let loro_val = json_to_loro(&entry);
        list.push(loro_val)?;
        Ok(())
    }

    // NOTE: No trim_log function - logs keep full history for search
    // Display limiting happens at render time via display_limit in BlockSchema::Log

    // ========== Persistence ==========

    /// Export a full snapshot
    pub fn export_snapshot(&self) -> Vec<u8> {
        self.doc.export(ExportMode::Snapshot).unwrap()
    }

    /// Export updates since a version
    pub fn export_updates_since(&self, from: &Frontiers) -> Vec<u8> {
        self.doc.export(ExportMode::Updates { from: from.clone() }).unwrap()
    }

    /// Get current version (frontiers)
    pub fn current_version(&self) -> Frontiers {
        self.doc.oplog_frontiers()
    }

    /// Export current version as bytes
    pub fn current_version_bytes(&self) -> Vec<u8> {
        let frontiers = self.current_version();
        serde_json::to_vec(&frontiers).unwrap_or_default()
    }

    /// Checkout to a specific version (read-only view)
    pub fn checkout(&self, version: &Frontiers) -> Result<(), LoroError> {
        self.doc.checkout(version)?;
        Ok(())
    }

    /// Return to editing mode after checkout
    pub fn attach(&self) {
        self.doc.attach();
    }

    /// Check if document is in detached (read-only) state
    pub fn is_detached(&self) -> bool {
        self.doc.is_detached()
    }

    /// Get the underlying LoroDoc (for advanced operations)
    pub fn inner(&self) -> &LoroDoc {
        &self.doc
    }

    // ========== Rendering ==========

    /// Render document content as string for LLM context
    pub fn render(&self) -> String {
        match &self.schema {
            BlockSchema::Text => self.text_content(),
            BlockSchema::Map { fields } => self.render_map(fields),
            BlockSchema::List { .. } => self.render_list(),
            BlockSchema::Log { display_limit, .. } => self.render_log(*display_limit),
            BlockSchema::Tree { .. } => self.render_tree(),
            BlockSchema::Composite { sections } => self.render_composite(sections),
        }
    }

    fn render_map(&self, fields: &[crate::memory_v2::schema::FieldDef]) -> String {
        let mut lines = Vec::new();
        for field in fields {
            if let Some(value) = self.get_field(&field.name) {
                match &field.field_type {
                    FieldType::List => {
                        let items = self.get_list_field(&field.name);
                        if !items.is_empty() {
                            lines.push(format!("{}:", field.name));
                            for item in items {
                                lines.push(format!("  - {}", json_display(&item)));
                            }
                        }
                    }
                    _ => {
                        lines.push(format!("{}: {}", field.name, json_display(&value)));
                    }
                }
            }
        }
        lines.join("\n")
    }

    fn render_list(&self) -> String {
        let items = self.list_items();
        items.iter().enumerate()
            .map(|(i, item)| {
                match item {
                    JsonValue::Object(obj) => {
                        let done = obj.get("done").and_then(|v| v.as_bool()).unwrap_or(false);
                        let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
                        let checkbox = if done { "[x]" } else { "[ ]" };
                        format!("{}. {} {}", i + 1, checkbox, title)
                    }
                    _ => format!("{}. {}", i + 1, json_display(item)),
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_log(&self, display_limit: usize) -> String {
        // Get only the most recent entries for context display
        let entries = self.log_entries(Some(display_limit));
        entries.iter()
            .map(|entry| {
                if let JsonValue::Object(obj) = entry {
                    let ts = obj.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
                    let desc = obj.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    format!("[{}] {}", ts, desc)
                } else {
                    json_display(entry)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_tree(&self) -> String {
        // TODO: Implement tree rendering
        "[Tree rendering not yet implemented]".to_string()
    }

    fn render_composite(&self, sections: &[(String, BlockSchema)]) -> String {
        let mut parts = Vec::new();
        for (name, _schema) in sections {
            // Get section content from doc
            parts.push(format!("## {}", name));
            // TODO: Render each section according to its schema
        }
        parts.join("\n\n")
    }
}

impl Default for StructuredDocument {
    fn default() -> Self {
        Self::new_text()
    }
}

/// Error type for Loro operations
#[derive(Debug, thiserror::Error)]
pub enum LoroError {
    #[error("Failed to import document: {0}")]
    ImportFailed(String),

    #[error("Failed to export document: {0}")]
    ExportFailed(String),

    #[error("Document is detached (read-only)")]
    Detached,

    #[error("Schema mismatch: expected {expected}, got {actual}")]
    SchemaMismatch { expected: String, actual: String },

    #[error("Field not found: {0}")]
    FieldNotFound(String),

    #[error("Loro error: {0}")]
    Other(String),
}

impl From<loro::LoroError> for LoroError {
    fn from(e: loro::LoroError) -> Self {
        LoroError::Other(e.to_string())
    }
}

// ========== Conversion Helpers ==========

fn loro_to_json(value: &LoroValue) -> JsonValue {
    match value {
        LoroValue::Null => JsonValue::Null,
        LoroValue::Bool(b) => JsonValue::Bool(*b),
        LoroValue::I64(n) => JsonValue::Number((*n).into()),
        LoroValue::Double(n) => {
            serde_json::Number::from_f64(*n)
                .map(JsonValue::Number)
                .unwrap_or(JsonValue::Null)
        }
        LoroValue::String(s) => JsonValue::String(s.to_string()),
        LoroValue::List(list) => {
            JsonValue::Array(list.iter().map(loro_to_json).collect())
        }
        LoroValue::Map(map) => {
            let obj: serde_json::Map<String, JsonValue> = map
                .iter()
                .map(|(k, v)| (k.to_string(), loro_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        LoroValue::Binary(b) => {
            JsonValue::String(base64::encode(b))
        }
        _ => JsonValue::Null,
    }
}

fn json_to_loro(value: &JsonValue) -> LoroValue {
    match value {
        JsonValue::Null => LoroValue::Null,
        JsonValue::Bool(b) => LoroValue::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                LoroValue::I64(i)
            } else if let Some(f) = n.as_f64() {
                LoroValue::Double(f)
            } else {
                LoroValue::Null
            }
        }
        JsonValue::String(s) => LoroValue::String(s.clone().into()),
        JsonValue::Array(arr) => {
            LoroValue::List(arr.iter().map(json_to_loro).collect())
        }
        JsonValue::Object(obj) => {
            let map: std::collections::HashMap<String, LoroValue> = obj
                .iter()
                .map(|(k, v)| (k.clone(), json_to_loro(v)))
                .collect();
            LoroValue::Map(map.into())
        }
    }
}

fn json_display(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Null => "null".to_string(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// Helper to create a snapshot with initial text content
pub fn create_text_snapshot(content: &str) -> Vec<u8> {
    let doc = StructuredDocument::new_text();
    doc.set_text(content).unwrap();
    doc.export_snapshot()
}

/// Helper to get text content from a snapshot
pub fn text_from_snapshot(snapshot: &[u8]) -> Result<String, LoroError> {
    let doc = StructuredDocument::from_snapshot(snapshot, BlockSchema::Text)?;
    Ok(doc.text_content())
}

/// Helper to generate content preview (first N characters)
pub fn content_preview(content: &str, max_len: usize) -> String {
    if content.len() <= max_len {
        content.to_string()
    } else {
        let mut preview: String = content.chars().take(max_len - 3).collect();
        preview.push_str("...");
        preview
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::schema::templates;

    #[test]
    fn test_text_document() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Hello, world!").unwrap();
        assert_eq!(doc.text_content(), "Hello, world!");
    }

    #[test]
    fn test_text_append() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Hello").unwrap();
        doc.append_text(", world!").unwrap();
        assert_eq!(doc.text_content(), "Hello, world!");
    }

    #[test]
    fn test_text_replace() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Hello, world!").unwrap();
        assert!(doc.replace_text("world", "Loro").unwrap());
        assert_eq!(doc.text_content(), "Hello, Loro!");
    }

    #[test]
    fn test_map_fields() {
        let doc = StructuredDocument::new(templates::partner_profile());
        doc.set_text_field("name", "Alice").unwrap();
        doc.set_field("energy_level", serde_json::json!(7)).unwrap();

        assert_eq!(doc.get_text_field("name"), Some("Alice".to_string()));
        assert_eq!(doc.get_counter("energy_level"), 7);
    }

    #[test]
    fn test_counter_operations() {
        let doc = StructuredDocument::new(templates::partner_profile());
        doc.set_field("energy_level", serde_json::json!(5)).unwrap();

        let new_val = doc.increment_counter("energy_level", 2).unwrap();
        assert_eq!(new_val, 7);

        let new_val = doc.increment_counter("energy_level", -3).unwrap();
        assert_eq!(new_val, 4);
    }

    #[test]
    fn test_list_operations() {
        let doc = StructuredDocument::new(templates::task_list());

        doc.push_item(serde_json::json!({"title": "Task 1", "done": false})).unwrap();
        doc.push_item(serde_json::json!({"title": "Task 2", "done": true})).unwrap();

        let items = doc.list_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["title"], "Task 1");
        assert_eq!(items[1]["done"], true);
    }

    #[test]
    fn test_log_operations() {
        let doc = StructuredDocument::new(templates::activity_log());

        doc.append_log_entry(serde_json::json!({
            "timestamp": "2025-01-23T10:00:00Z",
            "event_type": "message",
            "description": "User sent a message"
        })).unwrap();

        let entries = doc.log_entries(Some(10));
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_log_trim() {
        let doc = StructuredDocument::new(templates::activity_log());

        // Add more than max entries
        for i in 0..60 {
            doc.append_log_entry(serde_json::json!({
                "description": format!("Event {}", i)
            })).unwrap();
        }

        let trimmed = doc.trim_log(50).unwrap();
        assert_eq!(trimmed, 10);

        let entries = doc.log_entries(None);
        assert_eq!(entries.len(), 50);
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Test content").unwrap();

        let snapshot = doc.export_snapshot();
        let doc2 = StructuredDocument::from_snapshot(&snapshot, BlockSchema::Text).unwrap();

        assert_eq!(doc2.text_content(), "Test content");
    }

    #[test]
    fn test_render_map() {
        let doc = StructuredDocument::new(templates::partner_profile());
        doc.set_text_field("name", "Alice").unwrap();
        doc.set_field("energy_level", serde_json::json!(7)).unwrap();

        let rendered = doc.render();
        assert!(rendered.contains("name: Alice"));
        assert!(rendered.contains("energy_level: 7"));
    }

    #[test]
    fn test_render_list() {
        let doc = StructuredDocument::new(templates::task_list());
        doc.push_item(serde_json::json!({"title": "Write tests", "done": true})).unwrap();
        doc.push_item(serde_json::json!({"title": "Fix bugs", "done": false})).unwrap();

        let rendered = doc.render();
        assert!(rendered.contains("[x] Write tests"));
        assert!(rendered.contains("[ ] Fix bugs"));
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod document;
pub use document::*;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS (may need to adjust Loro API based on actual version)

**Step 4: Run tests**

Run: `cargo test -p pattern_core memory_v2::document`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add structured Loro document helpers for all schema types"
```

---

## Task 4: Create MemoryStore Trait

**Files:**
- Create: `crates/pattern_core/src/memory_v2/store.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create store.rs**

```rust
//! MemoryStore trait and SQLite implementation
//!
//! Provides the interface for memory operations that tools will use.
//! Backed by SQLite via db_v2, with structured Loro documents.

use async_trait::async_trait;
use crate::db_v2::{self, ConstellationDb};
use crate::memory_v2::{
    BlockMetadata, BlockType, Permission, MemoryBlockId,
    StructuredDocument, MemoryVersion, ChangeSource, ArchivalEntry,
    LoroError, BlockSchema, content_preview,
};
use chrono::Utc;
use std::sync::Arc;

/// Error type for memory store operations
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("Block not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Block limit exceeded: {current} > {limit}")]
    LimitExceeded { current: usize, limit: usize },

    #[error("Label already exists for agent: {0}")]
    DuplicateLabel(String),

    #[error("Database error: {0}")]
    Database(#[from] pattern_db::DbError),

    #[error("Loro error: {0}")]
    Loro(#[from] LoroError),

    #[error("Block is read-only")]
    ReadOnly,

    #[error("{0}")]
    Other(String),
}

pub type MemoryResult<T> = Result<T, MemoryError>;

/// Trait for memory storage operations
///
/// This is the interface that tools (context, recall, search) will use.
/// Abstracts over the actual storage implementation.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    // Block operations

    /// Create a new memory block
    async fn create_block(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
        block_type: BlockType,
        schema: Option<BlockSchema>,  // None = Text schema (default)
        initial_content: &str,        // For Text schema; ignored for structured schemas
        char_limit: usize,
    ) -> MemoryResult<MemoryBlockId>;

    /// Get block metadata by label
    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>>;

    /// Get block content
    async fn get_block_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>>;

    /// Get block with content (metadata + content together)
    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<(BlockMetadata, String)>>;

    /// List all blocks for an agent
    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>>;

    /// List blocks by type
    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>>;

    /// Update block content (replaces entire content)
    async fn set_block_content(
        &self,
        agent_id: &str,
        label: &str,
        content: &str,
        source: ChangeSource,
    ) -> MemoryResult<()>;

    /// Append to block content
    async fn append_block_content(
        &self,
        agent_id: &str,
        label: &str,
        content: &str,
        source: ChangeSource,
    ) -> MemoryResult<()>;

    /// Replace text in block content
    async fn replace_in_block(
        &self,
        agent_id: &str,
        label: &str,
        find: &str,
        replace: &str,
        source: ChangeSource,
    ) -> MemoryResult<bool>;

    // Structured operations (for Map/List/Log schema blocks)

    /// Set a field in a Map schema block
    async fn set_field(
        &self,
        agent_id: &str,
        label: &str,
        field: &str,
        value: serde_json::Value,
        source: ChangeSource,
    ) -> MemoryResult<()>;

    /// Append item to a list (either List schema block or list field in Map)
    async fn append_to_list(
        &self,
        agent_id: &str,
        label: &str,
        field: Option<&str>,  // None for List schema, Some(field) for list field in Map
        item: serde_json::Value,
        source: ChangeSource,
    ) -> MemoryResult<()>;

    /// Remove item from a list by index
    async fn remove_from_list(
        &self,
        agent_id: &str,
        label: &str,
        field: Option<&str>,
        index: usize,
        source: ChangeSource,
    ) -> MemoryResult<()>;

    /// Increment a counter field (can be negative for decrement)
    async fn increment_counter(
        &self,
        agent_id: &str,
        label: &str,
        field: &str,
        delta: i64,
        source: ChangeSource,
    ) -> MemoryResult<i64>;

    /// Append entry to a Log schema block (auto-trims on persist)
    async fn append_log_entry(
        &self,
        agent_id: &str,
        label: &str,
        entry: serde_json::Value,
        source: ChangeSource,
    ) -> MemoryResult<()>;

    /// Get rendered content for context (respects schema)
    async fn get_block_rendered(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>>;

    /// Delete (deactivate) a block
    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()>;

    // Archival operations

    /// Insert archival entry
    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> MemoryResult<String>;

    /// Get archival entry by ID
    async fn get_archival(&self, id: &str) -> MemoryResult<Option<ArchivalEntry>>;

    /// List archival entries for agent
    async fn list_archival(&self, agent_id: &str, limit: usize) -> MemoryResult<Vec<ArchivalEntry>>;

    /// Delete archival entry
    async fn delete_archival(&self, id: &str) -> MemoryResult<()>;

    // Search operations

    /// Search archival memory by text
    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>>;

    // Version operations

    /// Get block version history
    async fn get_block_history(
        &self,
        agent_id: &str,
        label: &str,
        limit: usize,
    ) -> MemoryResult<Vec<MemoryVersion>>;

    /// View block at specific version (returns content only)
    async fn get_block_at_version(
        &self,
        agent_id: &str,
        label: &str,
        version: &[u8],
    ) -> MemoryResult<String>;
}

/// SQLite-backed implementation of MemoryStore
pub struct SqliteMemoryStore {
    db: Arc<ConstellationDb>,
}

impl SqliteMemoryStore {
    pub fn new(db: Arc<ConstellationDb>) -> Self {
        Self { db }
    }

    /// Helper to load and reconstruct a structured Loro document from storage
    async fn load_document(&self, block_id: &str, schema: BlockSchema) -> MemoryResult<StructuredDocument> {
        let block = db_v2::get_block(&self.db, block_id).await?
            .ok_or_else(|| MemoryError::NotFound(block_id.to_string()))?;

        let doc = StructuredDocument::from_snapshot(&block.loro_snapshot, schema)?;
        Ok(doc)
    }

    /// Helper to load document with schema from metadata
    async fn load_document_with_schema(&self, agent_id: &str, label: &str) -> MemoryResult<(db_v2::MemoryBlock, StructuredDocument)> {
        let block = db_v2::get_block_by_label(&self.db, agent_id, label).await?
            .ok_or_else(|| MemoryError::NotFound(label.to_string()))?;

        // Get schema from block metadata (stored as JSON in schema_json column)
        let schema = block.schema_json.as_ref()
            .and_then(|json| serde_json::from_str::<BlockSchema>(json).ok())
            .unwrap_or(BlockSchema::Text);

        let doc = StructuredDocument::from_snapshot(&block.loro_snapshot, schema)?;
        Ok((block, doc))
    }

    /// Helper to store a document update
    async fn store_document_update(
        &self,
        block_id: &str,
        doc: &StructuredDocument,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let snapshot = doc.export_snapshot();
        let preview = content_preview(&doc.render(), 200);

        // Update the block with new snapshot
        db_v2::update_block_content(&self.db, block_id, &snapshot, Some(&preview)).await?;

        // Store update for delta tracking
        let update_source = match source {
            ChangeSource::Agent(_) => db_v2::UpdateSource::Agent,
            ChangeSource::User => db_v2::UpdateSource::Manual,
            ChangeSource::System => db_v2::UpdateSource::Migration,
            ChangeSource::Sync => db_v2::UpdateSource::Sync,
        };
        db_v2::store_update(&self.db, block_id, &snapshot, update_source).await?;

        Ok(())
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn create_block(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
        block_type: BlockType,
        schema: Option<BlockSchema>,
        initial_content: &str,
        char_limit: usize,
    ) -> MemoryResult<MemoryBlockId> {
        // Check for duplicate label
        if let Some(_) = db_v2::get_block_by_label(&self.db, agent_id, label).await? {
            return Err(MemoryError::DuplicateLabel(label.to_string()));
        }

        let id = MemoryBlockId::new();
        let schema = schema.unwrap_or(BlockSchema::Text);

        // Create document with schema
        let doc = StructuredDocument::new(schema.clone());
        if matches!(schema, BlockSchema::Text) && !initial_content.is_empty() {
            doc.set_text(initial_content)?;
        }

        let snapshot = doc.export_snapshot();
        let preview = content_preview(&doc.render(), 200);
        let schema_json = serde_json::to_string(&schema).ok();

        let db_block_type = match block_type {
            BlockType::Core => db_v2::MemoryBlockType::Core,
            BlockType::Working => db_v2::MemoryBlockType::Working,
            BlockType::Archival => db_v2::MemoryBlockType::Archival,
            BlockType::Log => db_v2::MemoryBlockType::Log,
        };

        let block = db_v2::MemoryBlock {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            block_type: db_block_type,
            schema_json,  // Store schema for reconstruction
            char_limit: char_limit as i64,
            read_only: false,
            pinned: block_type == BlockType::Core,
            permission: db_v2::MemoryPermission::ReadWrite,
            loro_snapshot: snapshot,
            frontier: None,
            last_seq: 0,
            content_preview: Some(preview),
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        db_v2::create_block(&self.db, &block).await?;
        Ok(id)
    }

    async fn get_block_metadata(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        let block = db_v2::get_block_by_label(&self.db, agent_id, label).await?;
        Ok(block.map(|b| BlockMetadata {
            id: MemoryBlockId(b.id),
            agent_id: b.agent_id,
            label: b.label,
            description: b.description,
            block_type: match b.block_type {
                db_v2::MemoryBlockType::Core => BlockType::Core,
                db_v2::MemoryBlockType::Working => BlockType::Working,
                db_v2::MemoryBlockType::Archival => BlockType::Archival,
                db_v2::MemoryBlockType::Log => BlockType::Log,
            },
            schema: b.schema_json.as_ref()
                .and_then(|json| serde_json::from_str(json).ok()),
            char_limit: b.char_limit as usize,
            read_only: b.read_only,
            pinned: b.pinned,
            permission: match b.permission {
                db_v2::MemoryPermission::ReadOnly => Permission::ReadOnly,
                db_v2::MemoryPermission::AppendOnly => Permission::AppendOnly,
                _ => Permission::ReadWrite,
            },
            created_at: b.created_at,
            updated_at: b.updated_at,
        }))
    }

    async fn get_block_content(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>> {
        match self.load_document_with_schema(agent_id, label).await {
            Ok((_, doc)) => Ok(Some(doc.render())),
            Err(MemoryError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn get_block_rendered(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<String>> {
        // Same as get_block_content for structured documents
        self.get_block_content(agent_id, label).await
    }

    async fn get_block(
        &self,
        agent_id: &str,
        label: &str,
    ) -> MemoryResult<Option<(BlockMetadata, String)>> {
        let meta = self.get_block_metadata(agent_id, label).await?;
        match meta {
            Some(m) => {
                let content = self.get_block_content(agent_id, label).await?.unwrap_or_default();
                Ok(Some((m, content)))
            }
            None => Ok(None),
        }
    }

    async fn list_blocks(&self, agent_id: &str) -> MemoryResult<Vec<BlockMetadata>> {
        let blocks = db_v2::list_blocks(&self.db, agent_id).await?;
        Ok(blocks
            .into_iter()
            .map(|b| BlockMetadata {
                id: MemoryBlockId(b.id),
                agent_id: b.agent_id,
                label: b.label,
                description: b.description,
                block_type: match b.block_type {
                    db_v2::MemoryBlockType::Core => BlockType::Core,
                    db_v2::MemoryBlockType::Working => BlockType::Working,
                    db_v2::MemoryBlockType::Archival => BlockType::Archival,
                    db_v2::MemoryBlockType::Log => BlockType::Log,
                },
                char_limit: b.char_limit as usize,
                read_only: b.read_only,
                pinned: b.pinned,
                permission: match b.permission {
                    db_v2::MemoryPermission::ReadOnly => Permission::ReadOnly,
                    db_v2::MemoryPermission::AppendOnly => Permission::AppendOnly,
                    _ => Permission::ReadWrite,
                },
                created_at: b.created_at,
                updated_at: b.updated_at,
            })
            .collect())
    }

    async fn list_blocks_by_type(
        &self,
        agent_id: &str,
        block_type: BlockType,
    ) -> MemoryResult<Vec<BlockMetadata>> {
        let db_type = match block_type {
            BlockType::Core => db_v2::MemoryBlockType::Core,
            BlockType::Working => db_v2::MemoryBlockType::Working,
            BlockType::Archival => db_v2::MemoryBlockType::Archival,
            BlockType::Log => db_v2::MemoryBlockType::Log,
        };
        let blocks = db_v2::list_blocks_by_type(&self.db, agent_id, db_type).await?;
        // Same mapping as list_blocks
        Ok(blocks
            .into_iter()
            .map(|b| BlockMetadata {
                id: MemoryBlockId(b.id),
                agent_id: b.agent_id,
                label: b.label,
                description: b.description,
                block_type,
                char_limit: b.char_limit as usize,
                read_only: b.read_only,
                pinned: b.pinned,
                permission: match b.permission {
                    db_v2::MemoryPermission::ReadOnly => Permission::ReadOnly,
                    db_v2::MemoryPermission::AppendOnly => Permission::AppendOnly,
                    _ => Permission::ReadWrite,
                },
                created_at: b.created_at,
                updated_at: b.updated_at,
            })
            .collect())
    }

    async fn set_block_content(
        &self,
        agent_id: &str,
        label: &str,
        content: &str,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        match doc.schema() {
            BlockSchema::Text => {
                // Direct text content
                doc.set_text(content)?;
            }
            BlockSchema::Map { .. } => {
                // Try to parse as JSON and apply fields
                let parsed: serde_json::Value = serde_json::from_str(content)
                    .map_err(|e| MemoryError::Other(format!("Invalid JSON for Map block: {}", e)))?;
                if let serde_json::Value::Object(map) = parsed {
                    for (key, value) in map {
                        doc.set_field(&key, value)?;
                    }
                } else {
                    return Err(MemoryError::Other("Map block requires JSON object".into()));
                }
            }
            BlockSchema::List { .. } => {
                // Try to parse as JSON array
                let parsed: serde_json::Value = serde_json::from_str(content)
                    .map_err(|e| MemoryError::Other(format!("Invalid JSON for List block: {}", e)))?;
                if let serde_json::Value::Array(arr) = parsed {
                    for item in arr {
                        doc.push_item(item)?;
                    }
                } else {
                    return Err(MemoryError::Other("List block requires JSON array".into()));
                }
            }
            _ => {
                return Err(MemoryError::Other(
                    "set_block_content not supported for this schema type".into()
                ));
            }
        }

        let rendered = doc.render();
        if rendered.len() > block.char_limit as usize {
            return Err(MemoryError::LimitExceeded {
                current: rendered.len(),
                limit: block.char_limit as usize,
            });
        }

        self.store_document_update(&block.id, &doc, source).await?;
        Ok(())
    }

    async fn append_block_content(
        &self,
        agent_id: &str,
        label: &str,
        content: &str,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        // Only works for Text schema blocks
        if !matches!(doc.schema(), BlockSchema::Text) {
            return Err(MemoryError::Other(
                "append_block_content only works on Text schema; use append_to_list for List schemas".into()
            ));
        }

        doc.append_text(content)?;
        let rendered = doc.render();
        if rendered.len() > block.char_limit as usize {
            return Err(MemoryError::LimitExceeded {
                current: rendered.len(),
                limit: block.char_limit as usize,
            });
        }

        self.store_document_update(&block.id, &doc, source).await?;
        Ok(())
    }

    async fn replace_in_block(
        &self,
        agent_id: &str,
        label: &str,
        find: &str,
        replace: &str,
        source: ChangeSource,
    ) -> MemoryResult<bool> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        // Only works for Text schema blocks
        if !matches!(doc.schema(), BlockSchema::Text) {
            return Err(MemoryError::Other(
                "replace_in_block only works on Text schema; use set_field for Map schemas".into()
            ));
        }

        let replaced = doc.replace_text(find, replace)?;
        if replaced {
            let rendered = doc.render();
            if rendered.len() > block.char_limit as usize {
                return Err(MemoryError::LimitExceeded {
                    current: rendered.len(),
                    limit: block.char_limit as usize,
                });
            }
            self.store_document_update(&block.id, &doc, source).await?;
        }

        Ok(replaced)
    }

    // Structured operations

    async fn set_field(
        &self,
        agent_id: &str,
        label: &str,
        field: &str,
        value: serde_json::Value,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        doc.set_field(field, value)?;
        self.store_document_update(&block.id, &doc, source).await?;
        Ok(())
    }

    async fn append_to_list(
        &self,
        agent_id: &str,
        label: &str,
        field: Option<&str>,
        item: serde_json::Value,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        match field {
            Some(f) => doc.append_to_list_field(f, item)?,  // Map schema with list field
            None => doc.push_item(item)?,                   // List schema
        }

        self.store_document_update(&block.id, &doc, source).await?;
        Ok(())
    }

    async fn remove_from_list(
        &self,
        agent_id: &str,
        label: &str,
        field: Option<&str>,
        index: usize,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        match field {
            Some(f) => doc.remove_from_list_field(f, index)?,
            None => doc.delete_item(index)?,
        }

        self.store_document_update(&block.id, &doc, source).await?;
        Ok(())
    }

    async fn increment_counter(
        &self,
        agent_id: &str,
        label: &str,
        field: &str,
        delta: i64,
        source: ChangeSource,
    ) -> MemoryResult<i64> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        let new_value = doc.increment_counter(field, delta)?;
        self.store_document_update(&block.id, &doc, source).await?;
        Ok(new_value)
    }

    async fn append_log_entry(
        &self,
        agent_id: &str,
        label: &str,
        entry: serde_json::Value,
        source: ChangeSource,
    ) -> MemoryResult<()> {
        let (block, doc) = self.load_document_with_schema(agent_id, label).await?;

        if block.read_only {
            return Err(MemoryError::ReadOnly);
        }

        doc.append_log_entry(entry)?;

        // NOTE: Logs keep FULL history for search purposes
        // Trimming happens at render time (context builder shows only recent)
        // This allows searching all historical entries

        self.store_document_update(&block.id, &doc, source).await?;
        Ok(())
    }

    async fn delete_block(&self, agent_id: &str, label: &str) -> MemoryResult<()> {
        let block = db_v2::get_block_by_label(&self.db, agent_id, label).await?
            .ok_or_else(|| MemoryError::NotFound(label.to_string()))?;

        db_v2::deactivate_block(&self.db, &block.id).await?;
        Ok(())
    }

    async fn insert_archival(
        &self,
        agent_id: &str,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> MemoryResult<String> {
        let id = format!("arch_{}", uuid::Uuid::new_v4());
        let entry = db_v2::models::ArchivalEntry {
            id: id.clone(),
            agent_id: agent_id.to_string(),
            content: content.to_string(),
            metadata: metadata.map(sqlx::types::Json),
            chunk_index: None,
            parent_entry_id: None,
            created_at: Utc::now(),
        };

        db_v2::queries::memory::create_archival_entry(self.db.pool(), &entry).await?;
        Ok(id)
    }

    async fn get_archival(&self, id: &str) -> MemoryResult<Option<ArchivalEntry>> {
        let entry = db_v2::queries::memory::get_archival_entry(self.db.pool(), id).await?;
        Ok(entry.map(|e| ArchivalEntry {
            id: e.id,
            agent_id: e.agent_id,
            content: e.content,
            metadata: e.metadata.map(|j| j.0),
            created_at: e.created_at,
        }))
    }

    async fn list_archival(&self, agent_id: &str, limit: usize) -> MemoryResult<Vec<ArchivalEntry>> {
        let entries = db_v2::queries::memory::list_archival_entries(self.db.pool(), agent_id, limit as i64).await?;
        Ok(entries
            .into_iter()
            .map(|e| ArchivalEntry {
                id: e.id,
                agent_id: e.agent_id,
                content: e.content,
                metadata: e.metadata.map(|j| j.0),
                created_at: e.created_at,
            })
            .collect())
    }

    async fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        db_v2::queries::memory::delete_archival_entry(self.db.pool(), id).await?;
        Ok(())
    }

    async fn search_archival(
        &self,
        agent_id: &str,
        query: &str,
        limit: usize,
    ) -> MemoryResult<Vec<ArchivalEntry>> {
        let results = db_v2::search_archival_fts(&self.db, query, Some(agent_id), limit as i64).await?;

        // FTS returns IDs, need to fetch full entries
        let mut entries = Vec::new();
        for result in results {
            if let Some(entry) = self.get_archival(&result.id).await? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    async fn get_block_history(
        &self,
        _agent_id: &str,
        _label: &str,
        _limit: usize,
    ) -> MemoryResult<Vec<MemoryVersion>> {
        // TODO: Implement via memory_block_history table
        // For now, return empty - versioning is tracked but history UI not implemented
        Ok(vec![])
    }

    async fn get_block_at_version(
        &self,
        agent_id: &str,
        label: &str,
        version: &[u8],
    ) -> MemoryResult<String> {
        let block = db_v2::get_block_by_label(&self.db, agent_id, label).await?
            .ok_or_else(|| MemoryError::NotFound(label.to_string()))?;

        let doc = self.load_document(&block.id).await?;

        // Parse frontiers from version bytes
        let frontiers: loro::Frontiers = serde_json::from_slice(version)
            .map_err(|e| MemoryError::Other(format!("Invalid version: {}", e)))?;

        doc.checkout(&frontiers)?;
        let content = doc.content();
        doc.attach(); // Return to editing mode

        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::schema::templates;

    async fn test_store() -> SqliteMemoryStore {
        let dir = tempfile::tempdir().unwrap();
        let db = ConstellationDb::open(dir.path().join("test.db")).await.unwrap();
        SqliteMemoryStore::new(Arc::new(db))
    }

    #[tokio::test]
    async fn test_create_and_get_text_block() {
        let store = test_store().await;

        store
            .create_block(
                "agent_1",
                "persona",
                "Agent personality",
                BlockType::Core,
                None,  // Text schema (default)
                "I am a helpful assistant.",
                5000,
            )
            .await
            .unwrap();

        let content = store.get_block_content("agent_1", "persona").await.unwrap();
        assert_eq!(content, Some("I am a helpful assistant.".to_string()));
    }

    #[tokio::test]
    async fn test_structured_map_block() {
        let store = test_store().await;

        // Create with partner profile schema
        store
            .create_block(
                "agent_1",
                "human",
                "Information about the human",
                BlockType::Core,
                Some(templates::partner_profile()),
                "",  // Ignored for structured schemas
                5000,
            )
            .await
            .unwrap();

        // Set fields
        store.set_field("agent_1", "human", "name", serde_json::json!("Alice"), ChangeSource::Agent("agent_1".into())).await.unwrap();
        store.set_field("agent_1", "human", "energy_level", serde_json::json!(7), ChangeSource::Agent("agent_1".into())).await.unwrap();

        // Increment counter
        let new_energy = store.increment_counter("agent_1", "human", "energy_level", -2, ChangeSource::Agent("agent_1".into())).await.unwrap();
        assert_eq!(new_energy, 5);

        // Append to list field
        store.append_to_list("agent_1", "human", Some("preferences"), serde_json::json!("Likes coffee"), ChangeSource::User).await.unwrap();

        // Check rendered output
        let content = store.get_block_content("agent_1", "human").await.unwrap().unwrap();
        assert!(content.contains("name: Alice"));
        assert!(content.contains("energy_level: 5"));
    }

    #[tokio::test]
    async fn test_structured_list_block() {
        let store = test_store().await;

        // Create task list
        store
            .create_block(
                "agent_1",
                "tasks",
                "Current tasks",
                BlockType::Working,
                Some(templates::task_list()),
                "",
                5000,
            )
            .await
            .unwrap();

        // Add tasks
        store.append_to_list("agent_1", "tasks", None, serde_json::json!({"title": "Write tests", "done": false}), ChangeSource::Agent("agent_1".into())).await.unwrap();
        store.append_to_list("agent_1", "tasks", None, serde_json::json!({"title": "Fix bugs", "done": true}), ChangeSource::Agent("agent_1".into())).await.unwrap();

        // Check rendered output
        let content = store.get_block_content("agent_1", "tasks").await.unwrap().unwrap();
        assert!(content.contains("[ ] Write tests"));
        assert!(content.contains("[x] Fix bugs"));
    }

    #[tokio::test]
    async fn test_log_block_keeps_full_history() {
        let store = test_store().await;

        // Create log block (agent-managed observations, NOT constellation activity)
        store
            .create_block(
                "agent_1",
                "observations",
                "Observation log",
                BlockType::Log,
                Some(BlockSchema::Log {
                    display_limit: 5,  // Only 5 shown in context (full history kept)
                    entry_schema: Default::default(),
                }),
                "",
                10000,
            )
            .await
            .unwrap();

        // Add entries
        for i in 0..10 {
            store.append_log_entry("agent_1", "observations", serde_json::json!({
                "observation": format!("Noticed event {}", i)
            }), ChangeSource::System).await.unwrap();
        }

        // Full history kept for search (all 10 entries)
        // But rendered content shows only display_limit (5) for context
        let content = store.get_block_content("agent_1", "observations").await.unwrap().unwrap();
        // Rendered content shows only recent entries per schema
        assert!(!content.is_empty());
    }

    #[tokio::test]
    async fn test_set_content_with_json_on_map() {
        let store = test_store().await;

        store
            .create_block(
                "agent_1",
                "profile",
                "Profile",
                BlockType::Core,
                Some(templates::partner_profile()),
                "",
                5000,
            )
            .await
            .unwrap();

        // Set content via JSON string (works for Map schemas)
        let json_content = r#"{"name": "Bob", "energy_level": 8}"#;
        store.set_block_content("agent_1", "profile", json_content, ChangeSource::User).await.unwrap();

        let content = store.get_block_content("agent_1", "profile").await.unwrap().unwrap();
        assert!(content.contains("name: Bob"));
        assert!(content.contains("energy_level: 8"));
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod store;
pub use store::*;
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern_core`
Expected: PASS

**Step 4: Run tests**

Run: `cargo test -p pattern_core memory_v2::store`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add MemoryStore trait with structured operations"
```

---

## Task 5: Create Context Builder for Memory

**Files:**
- Create: `crates/pattern_core/src/memory_v2/context.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create context.rs**

```rust
//! Context building for memory blocks
//!
//! Formats memory blocks for inclusion in LLM context.
//! Follows Letta pattern: label + description + content.

use crate::memory_v2::{MemoryStore, BlockType, BlockMetadata};

/// Builds the memory section of an LLM context
pub struct MemoryContextBuilder<'a, S: MemoryStore> {
    store: &'a S,
    agent_id: String,
}

impl<'a, S: MemoryStore> MemoryContextBuilder<'a, S> {
    pub fn new(store: &'a S, agent_id: impl Into<String>) -> Self {
        Self {
            store,
            agent_id: agent_id.into(),
        }
    }

    /// Build the full memory context section
    ///
    /// Includes:
    /// - Core blocks (always)
    /// - Working blocks (if active)
    /// - Log blocks (recent entries)
    /// - Excludes Archival (retrieved on demand)
    pub async fn build(&self) -> Result<String, crate::memory_v2::MemoryError> {
        let blocks = self.store.list_blocks(&self.agent_id).await?;

        let mut sections = Vec::new();

        // Group by type for consistent ordering
        let core_blocks: Vec<_> = blocks.iter()
            .filter(|b| b.block_type == BlockType::Core)
            .collect();
        let working_blocks: Vec<_> = blocks.iter()
            .filter(|b| b.block_type == BlockType::Working)
            .collect();
        let log_blocks: Vec<_> = blocks.iter()
            .filter(|b| b.block_type == BlockType::Log)
            .collect();

        // Core memory section
        if !core_blocks.is_empty() {
            sections.push("## Core Memory\n".to_string());
            for block in core_blocks {
                let content = self.store.get_block_content(&self.agent_id, &block.label).await?;
                if let Some(content) = content {
                    sections.push(self.format_block(block, &content));
                }
            }
        }

        // Working memory section
        if !working_blocks.is_empty() {
            sections.push("\n## Working Memory\n".to_string());
            for block in working_blocks {
                let content = self.store.get_block_content(&self.agent_id, &block.label).await?;
                if let Some(content) = content {
                    sections.push(self.format_block(block, &content));
                }
            }
        }

        // Recent logs section (show preview)
        if !log_blocks.is_empty() {
            sections.push("\n## Recent Activity\n".to_string());
            for block in log_blocks {
                let content = self.store.get_block_content(&self.agent_id, &block.label).await?;
                if let Some(content) = content {
                    // For logs, show last N lines
                    let recent: String = content
                        .lines()
                        .rev()
                        .take(10)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n");
                    sections.push(self.format_block(block, &recent));
                }
            }
        }

        Ok(sections.join("\n"))
    }

    /// Format a single block for context
    fn format_block(&self, block: &BlockMetadata, content: &str) -> String {
        format!(
            "<{label}>\n<!-- {description} -->\n{content}\n</{label}>\n",
            label = block.label,
            description = block.description,
            content = content
        )
    }

    /// Build list of archival labels for context hint
    pub async fn archival_labels(&self) -> Result<Vec<String>, crate::memory_v2::MemoryError> {
        let blocks = self.store.list_blocks_by_type(&self.agent_id, BlockType::Archival).await?;
        Ok(blocks.into_iter().map(|b| b.label).collect())
    }
}

/// Estimate token count for memory context (rough heuristic)
pub fn estimate_tokens(content: &str) -> usize {
    // Rough estimate: 4 chars per token
    content.len() / 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::{SqliteMemoryStore, BlockType, ChangeSource};
    use crate::db_v2::ConstellationDb;
    use std::sync::Arc;

    async fn test_store() -> SqliteMemoryStore {
        let dir = tempfile::tempdir().unwrap();
        let db = ConstellationDb::open(dir.path().join("test.db")).await.unwrap();
        SqliteMemoryStore::new(Arc::new(db))
    }

    #[tokio::test]
    async fn test_context_building() {
        let store = test_store().await;

        // Create some blocks
        store.create_block(
            "agent_1",
            "persona",
            "Your personality and behavior",
            BlockType::Core,
            "I am a helpful AI assistant.",
            5000,
        ).await.unwrap();

        store.create_block(
            "agent_1",
            "human",
            "Information about the human you're talking to",
            BlockType::Core,
            "Name: Alice\nPreferences: Concise responses",
            5000,
        ).await.unwrap();

        store.create_block(
            "agent_1",
            "scratchpad",
            "Working notes",
            BlockType::Working,
            "Current task: Help with refactoring",
            5000,
        ).await.unwrap();

        let builder = MemoryContextBuilder::new(&store, "agent_1");
        let context = builder.build().await.unwrap();

        assert!(context.contains("## Core Memory"));
        assert!(context.contains("<persona>"));
        assert!(context.contains("I am a helpful AI assistant."));
        assert!(context.contains("<human>"));
        assert!(context.contains("## Working Memory"));
        assert!(context.contains("<scratchpad>"));
    }

    #[test]
    fn test_token_estimation() {
        assert_eq!(estimate_tokens("Hello, world!"), 3); // 13 chars / 4
        assert_eq!(estimate_tokens(""), 0);
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod context;
pub use context::*;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core memory_v2::context`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add memory context builder for LLM integration"
```

---

## Task 6: Add Shared Block Support

**Files:**
- Create: `crates/pattern_core/src/memory_v2/sharing.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create sharing.rs**

```rust
//! Shared memory block support
//!
//! Enables explicit sharing of blocks between agents with
//! controlled access levels.

use crate::db_v2::{self, ConstellationDb};
use crate::memory_v2::{MemoryBlockId, SharedAccess, MemoryError, MemoryResult};
use std::sync::Arc;

/// Special agent ID for constellation-level blocks
pub const CONSTELLATION_OWNER: &str = "_constellation_";

/// Manager for shared memory blocks
pub struct SharedBlockManager {
    db: Arc<ConstellationDb>,
}

impl SharedBlockManager {
    pub fn new(db: Arc<ConstellationDb>) -> Self {
        Self { db }
    }

    /// Share a block with another agent
    pub async fn share_block(
        &self,
        block_id: &MemoryBlockId,
        agent_id: &str,
        access: SharedAccess,
    ) -> MemoryResult<()> {
        let write_access = matches!(access, SharedAccess::Write);

        // Check block exists
        let block = db_v2::get_block(&self.db, block_id.as_str()).await?
            .ok_or_else(|| MemoryError::NotFound(block_id.to_string()))?;

        // Can't share with the owner
        if block.agent_id == agent_id {
            return Err(MemoryError::Other("Cannot share block with its owner".into()));
        }

        // Create sharing record
        db_v2::queries::memory::create_shared_block_agent(
            self.db.pool(),
            block_id.as_str(),
            agent_id,
            write_access,
        ).await?;

        Ok(())
    }

    /// Remove sharing for a block
    pub async fn unshare_block(
        &self,
        block_id: &MemoryBlockId,
        agent_id: &str,
    ) -> MemoryResult<()> {
        db_v2::queries::memory::delete_shared_block_agent(
            self.db.pool(),
            block_id.as_str(),
            agent_id,
        ).await?;
        Ok(())
    }

    /// Get all agents a block is shared with
    pub async fn get_shared_agents(
        &self,
        block_id: &MemoryBlockId,
    ) -> MemoryResult<Vec<(String, SharedAccess)>> {
        let shares = db_v2::queries::memory::list_shared_block_agents(
            self.db.pool(),
            block_id.as_str(),
        ).await?;

        Ok(shares
            .into_iter()
            .map(|s| {
                let access = if s.write_access {
                    SharedAccess::Write
                } else {
                    SharedAccess::Read
                };
                (s.agent_id, access)
            })
            .collect())
    }

    /// Get all blocks shared with an agent
    pub async fn get_blocks_shared_with(
        &self,
        agent_id: &str,
    ) -> MemoryResult<Vec<(MemoryBlockId, SharedAccess)>> {
        let shares = db_v2::queries::memory::list_agent_shared_blocks(
            self.db.pool(),
            agent_id,
        ).await?;

        Ok(shares
            .into_iter()
            .map(|s| {
                let access = if s.write_access {
                    SharedAccess::Write
                } else {
                    SharedAccess::Read
                };
                (MemoryBlockId(s.block_id), access)
            })
            .collect())
    }

    /// Check if agent has access to block (either owner or shared)
    pub async fn check_access(
        &self,
        block_id: &MemoryBlockId,
        agent_id: &str,
    ) -> MemoryResult<Option<SharedAccess>> {
        let block = db_v2::get_block(&self.db, block_id.as_str()).await?;

        match block {
            None => Ok(None),
            Some(b) => {
                // Owner has full access
                if b.agent_id == agent_id {
                    return Ok(Some(SharedAccess::Write));
                }

                // Constellation blocks are readable by all
                if b.agent_id == CONSTELLATION_OWNER {
                    return Ok(Some(SharedAccess::Read));
                }

                // Check explicit shares
                let shares = db_v2::queries::memory::get_shared_block_agent(
                    self.db.pool(),
                    block_id.as_str(),
                    agent_id,
                ).await?;

                Ok(shares.map(|s| {
                    if s.write_access {
                        SharedAccess::Write
                    } else {
                        SharedAccess::Read
                    }
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::{SqliteMemoryStore, MemoryStore, BlockType};

    async fn test_db() -> Arc<ConstellationDb> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(ConstellationDb::open(dir.path().join("test.db")).await.unwrap())
    }

    #[tokio::test]
    async fn test_share_and_access() {
        let db = test_db().await;
        let store = SqliteMemoryStore::new(db.clone());
        let sharing = SharedBlockManager::new(db);

        // Agent A creates a block
        let block_id = store.create_block(
            "agent_a",
            "shared_notes",
            "Notes to share",
            BlockType::Working,
            "Some content",
            5000,
        ).await.unwrap();

        // Initially agent B has no access
        let access = sharing.check_access(&block_id, "agent_b").await.unwrap();
        assert!(access.is_none());

        // Share with agent B
        sharing.share_block(&block_id, "agent_b", SharedAccess::Read).await.unwrap();

        // Now agent B has read access
        let access = sharing.check_access(&block_id, "agent_b").await.unwrap();
        assert_eq!(access, Some(SharedAccess::Read));

        // Agent A still has write access (owner)
        let access = sharing.check_access(&block_id, "agent_a").await.unwrap();
        assert_eq!(access, Some(SharedAccess::Write));
    }
}
```

**Step 2: Add to mod.rs**

```rust
mod sharing;
pub use sharing::*;
```

**Step 3: Verify and test**

Run: `cargo check -p pattern_core && cargo test -p pattern_core memory_v2::sharing`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add shared block support for multi-agent memory"
```

---

## Task 7: Update pattern_db Queries (if missing)

Before this task, verify pattern_db has these queries. If not, add them:

**Required in pattern_db/src/queries/memory.rs:**
- `create_shared_block_agent(pool, block_id, agent_id, write_access)`
- `delete_shared_block_agent(pool, block_id, agent_id)`
- `list_shared_block_agents(pool, block_id)`
- `list_agent_shared_blocks(pool, agent_id)`
- `get_shared_block_agent(pool, block_id, agent_id)`

This may require a separate task to add missing queries to pattern_db.

---

## Chunk 2 Completion Checklist

- [ ] `loro` dependency added (with counter feature)
- [ ] `memory_v2/types.rs` with core types
- [ ] `memory_v2/schema.rs` with BlockSchema, FieldDef, templates
- [ ] `memory_v2/document.rs` with StructuredDocument (Text, Map, List, Log)
- [ ] `memory_v2/store.rs` with MemoryStore trait + SQLite impl
  - [ ] Structured operations: set_field, append_to_list, increment_counter, append_log_entry
  - [ ] JSON text support for set_block_content on structured blocks
  - [ ] Schema storage in database (schema_json column)
- [ ] `memory_v2/context.rs` with context builder
- [ ] `memory_v2/sharing.rs` with shared block support
- [ ] All tests pass (text + structured operations)
- [ ] Old `memory.rs` untouched and still compiles

---

## pattern_db Updates Needed

Before Chunk 2 starts, pattern_db needs:
- [ ] `schema_json` column on memory_blocks table (TEXT, nullable)
- [ ] Migration to add the column

---

## Notes for Chunk 3

With memory_v2 in place, Chunk 3 (Agent Rework) can:
- Create `db_agent_v2.rs` using MemoryStore trait
- Inject SqliteMemoryStore into agent
- Use MemoryContextBuilder for context building
- Keep old DatabaseAgent for reference
