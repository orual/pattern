# Chunk 2: Memory System Rework (Updated)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add structured types (Text, List, Map, Log) to the existing memory_v2 cache layer.

**Architecture:** StructuredDocument wraps LoroDoc + BlockSchema, CachedBlock updated to use it.

**Tech Stack:** Loro CRDT 1.10+ (already added), pattern_db, existing memory_v2/cache.rs

**Builds On:** Existing memory_v2 module with MemoryCache, CachedBlock, lazy loading, persistence

---

## Current State (Already Implemented)

```
memory_v2/
├── mod.rs      # Exports MemoryCache, types
├── types.rs    # CachedBlock (holds LoroDoc), BlockType, MemoryError
└── cache.rs    # MemoryCache with DashMap, lazy load, sync-on-hit, persist
```

**CachedBlock currently holds:**
- Raw `LoroDoc`
- Metadata (id, agent_id, label, description, block_type, permission, etc.)
- Cache state (last_seq, dirty, last_accessed)

**This plan adds:**
- BlockSchema enum defining structure (Text, Map, List, Log)
- StructuredDocument wrapping LoroDoc with typed operations
- Update CachedBlock to use StructuredDocument
- Schema-aware operations on MemoryCache

---

## Task 1: Add BlockSchema Types

**Files:**
- Create: `crates/pattern_core/src/memory_v2/schema.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create schema.rs**

```rust
//! Block schemas defining structured memory types
//!
//! Schemas define how a block's LoroDoc is organized and what
//! operations are valid on it.

use serde::{Deserialize, Serialize};

/// Block schema defines the structure of a memory block's Loro document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockSchema {
    /// Free-form text (default, backward compatible)
    /// Uses: LoroText container named "content"
    Text,

    /// Key-value pairs with optional field definitions
    /// Uses: LoroMap with field containers
    Map {
        fields: Vec<FieldDef>,
    },

    /// Ordered list of items
    /// Uses: LoroList
    List {
        item_schema: Option<Box<BlockSchema>>,
        max_items: Option<usize>,
    },

    /// Log with display limit (full history kept, limited display in context)
    /// Uses: LoroList - NO trimming, display_limit applied at render time
    Log {
        /// How many entries to show when rendering for context (block-level)
        display_limit: usize,
        entry_schema: LogEntrySchema,
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
    pub name: String,
    pub description: String,
    pub field_type: FieldType,
    pub required: bool,
    pub default: Option<serde_json::Value>,
}

/// Field types for structured blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    Text,
    Number,
    Boolean,
    List,
    Timestamp,
}

/// Schema for log entries
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogEntrySchema {
    pub timestamp: bool,
    pub agent_id: bool,
    pub fields: Vec<FieldDef>,
}

/// Common schema templates
pub mod templates {
    use super::*;

    /// Partner profile - structured info about the human
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
                    description: "Current energy (1-10)".into(),
                    field_type: FieldType::Number,
                    required: false,
                    default: Some(serde_json::json!(5)),
                },
                FieldDef {
                    name: "current_focus".into(),
                    description: "What they're working on now".into(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                },
            ],
        }
    }

    /// Task list
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
                        description: "Whether completed".into(),
                        field_type: FieldType::Boolean,
                        required: true,
                        default: Some(serde_json::json!(false)),
                    },
                    FieldDef {
                        name: "priority".into(),
                        description: "Priority level".into(),
                        field_type: FieldType::Text,
                        required: false,
                        default: None,
                    },
                ],
            })),
            max_items: Some(100),
        }
    }

    /// Observation log - agent-managed log of observations
    /// NOTE: NOT constellation activity (system telemetry)
    pub fn observation_log() -> BlockSchema {
        BlockSchema::Log {
            display_limit: 20,
            entry_schema: LogEntrySchema {
                timestamp: true,
                agent_id: false,
                fields: vec![
                    FieldDef {
                        name: "observation".into(),
                        description: "What was observed".into(),
                        field_type: FieldType::Text,
                        required: true,
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
    fn test_default_schema() {
        let schema = BlockSchema::default();
        assert!(matches!(schema, BlockSchema::Text));
    }

    #[test]
    fn test_templates() {
        let profile = templates::partner_profile();
        assert!(matches!(profile, BlockSchema::Map { .. }));

        let tasks = templates::task_list();
        assert!(matches!(tasks, BlockSchema::List { .. }));

        let log = templates::observation_log();
        assert!(matches!(log, BlockSchema::Log { display_limit: 20, .. }));
    }
}
```

**Step 2: Update mod.rs**

```rust
//! V2 Memory System
//!
//! In-memory LoroDoc cache with lazy loading and write-through persistence.

mod cache;
mod schema;
mod types;

pub use cache::MemoryCache;
pub use schema::*;
pub use types::*;
```

**Step 3: Verify and test**

```bash
cargo check -p pattern_core
cargo test -p pattern_core memory_v2::schema
```

**Step 4: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add BlockSchema types for structured memory"
```

---

## Task 2: Create StructuredDocument Wrapper

**Files:**
- Create: `crates/pattern_core/src/memory_v2/document.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

**Step 1: Create document.rs**

```rust
//! StructuredDocument - schema-aware wrapper around LoroDoc
//!
//! Provides typed operations based on the block's schema.

use crate::memory_v2::{BlockSchema, FieldDef, MemoryError, MemoryResult};
use loro::{LoroDoc, LoroList, LoroMap, LoroText, LoroValue};
use serde_json::Value as JsonValue;

/// Schema-aware wrapper around LoroDoc
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

    /// Create from existing LoroDoc with schema
    pub fn from_doc(doc: LoroDoc, schema: BlockSchema) -> Self {
        Self { doc, schema }
    }

    /// Get the underlying LoroDoc (for advanced operations)
    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    /// Get the schema
    pub fn schema(&self) -> &BlockSchema {
        &self.schema
    }

    // ========== Text Schema Operations ==========

    /// Get text content (for Text schema)
    pub fn get_text(&self) -> String {
        let text = self.doc.get_text("content");
        text.to_string()
    }

    /// Set text content (for Text schema, replaces all)
    pub fn set_text(&self, content: &str) -> MemoryResult<()> {
        let text = self.doc.get_text("content");
        let len = text.len_unicode();
        if len > 0 {
            text.delete(0, len).map_err(|e| MemoryError::Loro(e.to_string()))?;
        }
        text.insert(0, content).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    /// Append to text content
    pub fn append_text(&self, content: &str) -> MemoryResult<()> {
        let text = self.doc.get_text("content");
        let len = text.len_unicode();
        text.insert(len, content).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    // ========== Map Schema Operations ==========

    /// Get a field value (for Map schema)
    pub fn get_field(&self, field: &str) -> Option<JsonValue> {
        let map = self.doc.get_map("fields");
        map.get(field).and_then(|v| loro_to_json(&v))
    }

    /// Set a field value (for Map schema)
    pub fn set_field(&self, field: &str, value: JsonValue) -> MemoryResult<()> {
        let map = self.doc.get_map("fields");
        let loro_val = json_to_loro(&value);
        map.insert(field, loro_val).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    /// Delete a field (for Map schema)
    pub fn delete_field(&self, field: &str) -> MemoryResult<()> {
        let map = self.doc.get_map("fields");
        map.delete(field).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    // ========== List Schema Operations ==========

    /// Get all list items
    pub fn get_items(&self) -> Vec<JsonValue> {
        let list = self.doc.get_list("items");
        (0..list.len())
            .filter_map(|i| list.get(i).and_then(|v| loro_to_json(&v)))
            .collect()
    }

    /// Push an item to the list
    pub fn push_item(&self, item: JsonValue) -> MemoryResult<()> {
        let list = self.doc.get_list("items");
        let loro_val = json_to_loro(&item);
        list.push(loro_val).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    /// Insert item at index
    pub fn insert_item(&self, index: usize, item: JsonValue) -> MemoryResult<()> {
        let list = self.doc.get_list("items");
        let loro_val = json_to_loro(&item);
        list.insert(index, loro_val).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    /// Remove item at index
    pub fn remove_item(&self, index: usize) -> MemoryResult<()> {
        let list = self.doc.get_list("items");
        list.delete(index, 1).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    // ========== Log Schema Operations ==========

    /// Get log entries (optionally limited)
    pub fn get_log_entries(&self, limit: Option<usize>) -> Vec<JsonValue> {
        let list = self.doc.get_list("entries");
        let len = list.len();
        let start = match limit {
            Some(n) if len > n => len - n,
            _ => 0,
        };
        (start..len)
            .filter_map(|i| list.get(i).and_then(|v| loro_to_json(&v)))
            .collect()
    }

    /// Append a log entry (full history kept)
    pub fn append_log_entry(&self, entry: JsonValue) -> MemoryResult<()> {
        let list = self.doc.get_list("entries");
        let loro_val = json_to_loro(&entry);
        list.push(loro_val).map_err(|e| MemoryError::Loro(e.to_string()))?;
        Ok(())
    }

    // ========== Rendering ==========

    /// Render document content based on schema
    pub fn render(&self) -> String {
        match &self.schema {
            BlockSchema::Text => self.get_text(),
            BlockSchema::Map { fields } => self.render_map(fields),
            BlockSchema::List { .. } => self.render_list(),
            BlockSchema::Log { display_limit, .. } => self.render_log(*display_limit),
        }
    }

    fn render_map(&self, fields: &[FieldDef]) -> String {
        let mut lines = Vec::new();
        for field in fields {
            if let Some(value) = self.get_field(&field.name) {
                lines.push(format!("{}: {}", field.name, json_display(&value)));
            }
        }
        lines.join("\n")
    }

    fn render_list(&self) -> String {
        let items = self.get_items();
        items.iter()
            .enumerate()
            .map(|(i, item)| format!("{}. {}", i + 1, json_display(item)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_log(&self, display_limit: usize) -> String {
        let entries = self.get_log_entries(Some(display_limit));
        entries.iter()
            .map(|entry| {
                if let JsonValue::Object(obj) = entry {
                    let ts = obj.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
                    let content = obj.get("observation")
                        .or_else(|| obj.get("content"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if ts.is_empty() {
                        content.to_string()
                    } else {
                        format!("[{}] {}", ts, content)
                    }
                } else {
                    json_display(entry)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ========== Persistence Helpers ==========

    /// Import data into the document
    pub fn import(&self, data: &[u8]) -> MemoryResult<()> {
        self.doc.import(data).map_err(|e| MemoryError::Loro(e.to_string()))
    }

    /// Export snapshot
    pub fn export_snapshot(&self) -> Vec<u8> {
        self.doc.export(loro::ExportMode::Snapshot).unwrap_or_default()
    }

    /// Export updates since version
    pub fn export_updates(&self, from: &loro::VersionVector) -> Vec<u8> {
        self.doc.export(loro::ExportMode::Updates {
            from: std::borrow::Cow::Borrowed(from),
        }).unwrap_or_default()
    }

    /// Get current version
    pub fn version(&self) -> loro::VersionVector {
        self.doc.oplog_vv().to_owned()
    }
}

// ========== JSON <-> Loro Conversion ==========

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
            LoroValue::List(arr.iter().map(json_to_loro).collect::<Vec<_>>().into())
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

fn loro_to_json(value: &LoroValue) -> Option<JsonValue> {
    Some(match value {
        LoroValue::Null => JsonValue::Null,
        LoroValue::Bool(b) => JsonValue::Bool(*b),
        LoroValue::I64(i) => JsonValue::Number((*i).into()),
        LoroValue::Double(f) => serde_json::Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        LoroValue::String(s) => JsonValue::String(s.to_string()),
        LoroValue::List(arr) => {
            JsonValue::Array(arr.iter().filter_map(loro_to_json).collect())
        }
        LoroValue::Map(map) => {
            let obj: serde_json::Map<String, JsonValue> = map
                .iter()
                .filter_map(|(k, v)| loro_to_json(v).map(|v| (k.clone(), v)))
                .collect();
            JsonValue::Object(obj)
        }
        _ => return None, // Container types handled differently
    })
}

fn json_display(value: &JsonValue) -> String {
    match value {
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_display).collect();
            items.join(", ")
        }
        JsonValue::Object(_) => serde_json::to_string(value).unwrap_or_default(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_v2::schema::templates;

    #[test]
    fn test_text_document() {
        let doc = StructuredDocument::new(BlockSchema::Text);
        doc.set_text("Hello, world!").unwrap();
        assert_eq!(doc.get_text(), "Hello, world!");

        doc.append_text(" More text.").unwrap();
        assert_eq!(doc.get_text(), "Hello, world! More text.");
    }

    #[test]
    fn test_map_document() {
        let doc = StructuredDocument::new(templates::partner_profile());
        doc.set_field("name", serde_json::json!("Alice")).unwrap();
        doc.set_field("energy_level", serde_json::json!(7)).unwrap();

        assert_eq!(doc.get_field("name"), Some(serde_json::json!("Alice")));
        assert_eq!(doc.get_field("energy_level"), Some(serde_json::json!(7)));
    }

    #[test]
    fn test_list_document() {
        let doc = StructuredDocument::new(templates::task_list());
        doc.push_item(serde_json::json!({"title": "Task 1", "done": false})).unwrap();
        doc.push_item(serde_json::json!({"title": "Task 2", "done": true})).unwrap();

        let items = doc.get_items();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn test_log_document() {
        let doc = StructuredDocument::new(templates::observation_log());

        for i in 0..10 {
            doc.append_log_entry(serde_json::json!({
                "timestamp": format!("2025-01-{:02}", i + 1),
                "observation": format!("Observation {}", i)
            })).unwrap();
        }

        // Full history kept
        let all = doc.get_log_entries(None);
        assert_eq!(all.len(), 10);

        // Display limited
        let recent = doc.get_log_entries(Some(5));
        assert_eq!(recent.len(), 5);
    }

    #[test]
    fn test_render() {
        let doc = StructuredDocument::new(BlockSchema::Text);
        doc.set_text("Test content").unwrap();
        assert_eq!(doc.render(), "Test content");
    }
}
```

**Step 2: Update mod.rs**

```rust
mod document;
pub use document::StructuredDocument;
```

**Step 3: Verify and test**

```bash
cargo check -p pattern_core
cargo test -p pattern_core memory_v2::document
```

**Step 4: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): add StructuredDocument with typed operations"
```

---

## Task 3: Update CachedBlock to Use StructuredDocument

**Files:**
- Modify: `crates/pattern_core/src/memory_v2/types.rs`
- Modify: `crates/pattern_core/src/memory_v2/cache.rs`

**Step 1: Update types.rs**

Change CachedBlock to hold StructuredDocument instead of LoroDoc:

```rust
//! Types for the v2 memory system

use chrono::{DateTime, Utc};
use loro::VersionVector;
use crate::memory_v2::StructuredDocument;

/// A cached memory block with its StructuredDocument
pub struct CachedBlock {
    /// Block metadata from DB
    pub id: String,
    pub agent_id: String,
    pub label: String,
    pub description: String,
    pub block_type: BlockType,
    pub char_limit: i64,
    pub permission: pattern_db::models::MemoryPermission,

    /// The structured document (LoroDoc + schema)
    pub doc: StructuredDocument,

    /// Last sequence number we've seen from DB
    pub last_seq: i64,

    /// Version at last persist (for delta export)
    pub last_persisted_version: Option<VersionVector>,

    /// Whether we have unpersisted changes
    pub dirty: bool,

    /// When this was last accessed (for eviction)
    pub last_accessed: DateTime<Utc>,
}

// ... rest of types.rs unchanged ...
```

**Step 2: Update cache.rs**

Update MemoryCache to:
1. Load schema from DB (stored in metadata JSON or separate column)
2. Create StructuredDocument instead of raw LoroDoc
3. Update persistence to work with StructuredDocument

Key changes in `load_from_db`:

```rust
async fn load_from_db(&self, agent_id: &str, label: &str) -> MemoryResult<Option<CachedBlock>> {
    let block = pattern_db::queries::get_block_by_label(self.db.pool(), agent_id, label).await?;
    let block = match block {
        Some(b) => b,
        None => return Ok(None),
    };

    // Get schema from metadata (default to Text if not set)
    let schema = block.metadata
        .as_ref()
        .and_then(|m| m.get("schema"))
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();

    // Create StructuredDocument
    let doc = StructuredDocument::new(schema);

    // Import snapshot
    if !block.loro_snapshot.is_empty() {
        doc.import(&block.loro_snapshot)?;
    }

    // Apply updates
    let (_, updates) = pattern_db::queries::get_checkpoint_and_updates(
        self.db.pool(), &block.id
    ).await?;
    for update in &updates {
        doc.import(&update.update_blob)?;
    }

    let last_seq = updates.last().map(|u| u.seq).unwrap_or(block.last_seq);
    let version = doc.version();

    Ok(Some(CachedBlock {
        id: block.id,
        agent_id: block.agent_id,
        label: block.label,
        description: block.description,
        block_type: block.block_type.into(),
        char_limit: block.char_limit,
        permission: block.permission,
        doc,
        last_seq,
        last_persisted_version: Some(version),
        dirty: false,
        last_accessed: Utc::now(),
    }))
}
```

Update return type of `get()`:
```rust
pub async fn get(&self, agent_id: &str, label: &str) -> MemoryResult<Option<&StructuredDocument>>
```

Or since we're using DashMap, return a guard or clone the doc.

**Step 3: Update tests**

Update tests to work with StructuredDocument API.

**Step 4: Verify and test**

```bash
cargo check -p pattern_core
cargo test -p pattern_core memory_v2
```

**Step 5: Commit**

```bash
git add crates/pattern_core/src/memory_v2/
git commit -m "feat(pattern_core): update CachedBlock to use StructuredDocument"
```

---

## Task 4: Add High-Level Cache Operations

**Files:**
- Create: `crates/pattern_core/src/memory_v2/ops.rs`
- Modify: `crates/pattern_core/src/memory_v2/mod.rs`

Add convenience methods to MemoryCache that work through StructuredDocument:

```rust
//! High-level operations on cached memory blocks

use crate::memory_v2::{MemoryCache, MemoryError, MemoryResult, BlockSchema};
use serde_json::Value as JsonValue;

impl MemoryCache {
    // ========== Text Operations ==========

    /// Get text content of a block
    pub async fn get_text(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        // Implementation using get() and doc.get_text()
    }

    /// Set text content (replaces)
    pub async fn set_text(&self, agent_id: &str, label: &str, content: &str) -> MemoryResult<()> {
        // Implementation
    }

    /// Append text
    pub async fn append_text(&self, agent_id: &str, label: &str, content: &str) -> MemoryResult<()> {
        // Implementation
    }

    // ========== Map Operations ==========

    /// Get field value
    pub async fn get_field(&self, agent_id: &str, label: &str, field: &str) -> MemoryResult<Option<JsonValue>> {
        // Implementation
    }

    /// Set field value
    pub async fn set_field(&self, agent_id: &str, label: &str, field: &str, value: JsonValue) -> MemoryResult<()> {
        // Implementation
    }

    // ========== List Operations ==========

    /// Append to list
    pub async fn append_to_list(&self, agent_id: &str, label: &str, item: JsonValue) -> MemoryResult<()> {
        // Implementation
    }

    // ========== Log Operations ==========

    /// Append log entry
    pub async fn append_log_entry(&self, agent_id: &str, label: &str, entry: JsonValue) -> MemoryResult<()> {
        // Implementation
    }

    // ========== Render ==========

    /// Get rendered content for context
    pub async fn get_rendered(&self, agent_id: &str, label: &str) -> MemoryResult<Option<String>> {
        // Implementation using doc.render()
    }
}
```

---

## Task 5: Add Block Creation

**Files:**
- Modify: `crates/pattern_core/src/memory_v2/cache.rs`

Add method to create new blocks through the cache:

```rust
impl MemoryCache {
    /// Create a new memory block
    pub async fn create_block(
        &self,
        agent_id: &str,
        label: &str,
        description: &str,
        block_type: BlockType,
        schema: BlockSchema,
        permission: MemoryPermission,
        char_limit: i64,
    ) -> MemoryResult<()> {
        // 1. Create StructuredDocument with schema
        // 2. Export initial snapshot
        // 3. Insert into DB via pattern_db::queries::create_block
        // 4. Add to cache
    }
}
```

---

## Completion Checklist

- [ ] `memory_v2/schema.rs` with BlockSchema, FieldDef, templates
- [ ] `memory_v2/document.rs` with StructuredDocument
- [ ] CachedBlock updated to use StructuredDocument
- [ ] MemoryCache loads schema from DB
- [ ] High-level operations (get_text, set_field, append_to_list, etc.)
- [ ] Block creation through cache
- [ ] All tests pass
- [ ] Old memory.rs untouched

---

## Notes for Chunk 2.5

With StructuredDocument in place, Chunk 2.5 (Context Rework) can:
- Use `doc.render()` for context-ready output
- Leverage schema info for system prompt hints
- Log blocks automatically limit display via schema

---

## Migration Notes

- Existing blocks without schema metadata default to `BlockSchema::Text`
- Schema stored in `metadata` JSON column: `{"schema": {...}}`
- Can add schema to existing blocks by updating metadata
