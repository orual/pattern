# Block Schema Field Permissions and Loro Integration

## Overview

This design adds two capabilities to StructuredDocument:

1. **Field-level read_only flags** - Schema fields can be marked read-only for agents, while system/source code can still write
2. **Loro subscription passthrough** - Expose Loro's subscription mechanism for edit watching

These support the data source v2 design where sources update read-only fields (like LSP diagnostics) while agents can only edit writable fields (like configuration).

---

## Field-Level Read-Only Flags

### Design Approach: Dual-Layer

Document methods take an `is_system: bool` parameter:
- **Agent tools pass `false`** - writes to read_only fields rejected
- **Source/system code passes `true`** - all writes allowed

This keeps both sides simple - no complex permission checking at the caller level.

### Schema Changes

Update `FieldDef` in `crates/pattern_core/src/memory/schema.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub description: String,
    pub field_type: FieldType,
    pub required: bool,
    pub default: Option<serde_json::Value>,

    /// If true, only system/source code can write to this field.
    /// Agent tools should reject writes to read-only fields.
    #[serde(default)]
    pub read_only: bool,
}
```

### BlockSchema Helper Method

Add method to check field permissions:

```rust
impl BlockSchema {
    /// Check if a field is read-only. Returns None if field not found or schema doesn't have fields.
    pub fn is_field_read_only(&self, field_name: &str) -> Option<bool> {
        match self {
            BlockSchema::Map { fields } => {
                fields.iter()
                    .find(|f| f.name == field_name)
                    .map(|f| f.read_only)
            }
            BlockSchema::Composite { sections } => {
                sections.iter()
                    .find(|(name, _)| name == field_name)
                    .and_then(|(_, schema)| {
                        // For composite, check if the sub-schema is entirely read-only
                        // This is a simplification - could make sections have their own read_only flag
                        None // Or implement section-level read_only
                    })
            }
            _ => None, // Text, List, Log, Tree don't have named fields
        }
    }

    /// Get all field names that are read-only
    pub fn read_only_fields(&self) -> Vec<&str> {
        match self {
            BlockSchema::Map { fields } => {
                fields.iter()
                    .filter(|f| f.read_only)
                    .map(|f| f.name.as_str())
                    .collect()
            }
            _ => vec![],
        }
    }
}
```

### StructuredDocument Changes

#### New Error Variant

```rust
#[derive(Debug, Error)]
pub enum DocumentError {
    // ... existing variants ...

    #[error("Field '{0}' is read-only and cannot be modified by agent")]
    ReadOnlyField(String),
}
```

#### Updated Method Signatures

Methods that write to fields gain `is_system: bool` parameter:

```rust
impl StructuredDocument {
    /// Set a field value. If is_system is false and field is read_only, returns error.
    pub fn set_field(
        &self,
        name: &str,
        value: impl Into<serde_json::Value>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check read-only if not system
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(name) {
                return Err(DocumentError::ReadOnlyField(name.to_string()));
            }
        }

        // ... existing implementation ...
    }

    /// Set a text field. If is_system is false and field is read_only, returns error.
    pub fn set_text_field(
        &self,
        name: &str,
        value: &str,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(name) {
                return Err(DocumentError::ReadOnlyField(name.to_string()));
            }
        }
        // ... existing implementation ...
    }

    /// Append to a list field. If is_system is false and field is read_only, returns error.
    pub fn append_to_list_field(
        &self,
        name: &str,
        value: impl Into<serde_json::Value>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(name) {
                return Err(DocumentError::ReadOnlyField(name.to_string()));
            }
        }
        // ... existing implementation ...
    }

    /// Remove from a list field. If is_system is false and field is read_only, returns error.
    pub fn remove_from_list_field(
        &self,
        name: &str,
        index: usize,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(name) {
                return Err(DocumentError::ReadOnlyField(name.to_string()));
            }
        }
        // ... existing implementation ...
    }

    /// Increment a counter field. If is_system is false and field is read_only, returns error.
    pub fn increment_counter(
        &self,
        name: &str,
        delta: i64,
        is_system: bool,
    ) -> Result<i64, DocumentError> {
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(name) {
                return Err(DocumentError::ReadOnlyField(name.to_string()));
            }
        }
        // ... existing implementation ...
    }
}
```

#### Non-Field Operations

For Text, List (non-Map), and Log schemas, there's no field-level granularity. The block-level permission applies. These methods don't need `is_system`:

- `set_text()`, `append_text()`, `replace_text()` - Text schema ops
- `push_item()`, `insert_item()`, `delete_item()` - List schema ops
- `append_log_entry()` - Log schema ops (typically system-only anyway)

### Rendering with Read-Only Indicators

Update `render()` to indicate read-only fields to the agent:

```rust
impl StructuredDocument {
    pub fn render(&self) -> String {
        match &self.schema {
            BlockSchema::Map { fields } => {
                let mut output = String::new();
                for field in fields {
                    let value = self.get_field(&field.name)
                        .map(|v| json_display(&v))
                        .unwrap_or_default();

                    // Mark read-only fields
                    let marker = if field.read_only { " [read-only]" } else { "" };
                    output.push_str(&format!("{}{}: {}\n", field.name, marker, value));
                }
                output
            }
            // ... other schemas unchanged ...
        }
    }
}
```

---

## Loro Subscription Integration

### Goals

1. Allow callers to subscribe to document changes
2. Support both container-level and root-level subscriptions
3. Enable attribution via commit messages
4. Ensure commits happen so subscriptions fire

### Loro Background

Loro provides two subscription methods:

```rust
// Subscribe to specific container
doc.subscribe(&container_id, callback) -> Subscription

// Subscribe to all changes
doc.subscribe_root(callback) -> Subscription
```

Subscriptions fire after transactions commit. Commits happen on:
- `doc.commit()` explicit call
- `doc.export(mode)`
- `doc.import(data)`
- `doc.checkout(version)`

### StructuredDocument Subscription API

```rust
use loro::{Subscriber, Subscription, ContainerID};

impl StructuredDocument {
    /// Subscribe to all changes on this document
    pub fn subscribe_root(&self, callback: Subscriber) -> Subscription {
        self.doc.subscribe_root(callback)
    }

    /// Subscribe to changes on a specific container
    pub fn subscribe(&self, container_id: &ContainerID, callback: Subscriber) -> Subscription {
        self.doc.subscribe(container_id, callback)
    }

    /// Subscribe to the main content container based on schema type
    pub fn subscribe_content(&self, callback: Subscriber) -> Subscription {
        let container_id = match &self.schema {
            BlockSchema::Text => self.doc.get_text("content").id(),
            BlockSchema::Map { .. } => self.doc.get_map("root").id(),
            BlockSchema::List { .. } => self.doc.get_list("items").id(),
            BlockSchema::Log { .. } => self.doc.get_list("entries").id(),
            BlockSchema::Tree { .. } => self.doc.get_tree("tree").id(),
            BlockSchema::Composite { .. } => {
                // For composite, subscribe to root map
                self.doc.get_map("root").id()
            }
        };
        self.doc.subscribe(&container_id, callback)
    }

    /// Set attribution message for the next commit
    pub fn set_attribution(&self, message: &str) {
        self.doc.set_next_commit_message(message);
    }

    /// Explicitly commit pending changes (triggers subscriptions)
    pub fn commit(&self) {
        self.doc.commit();
    }

    /// Commit with attribution message
    pub fn commit_with_attribution(&self, message: &str) {
        self.doc.set_next_commit_message(message);
        self.doc.commit();
    }
}
```

### Attribution Format

For audit/logging, use structured attribution messages:

```rust
// Agent edit
doc.set_attribution(&format!("agent:{}:field:{}", agent_id, field_name));

// Source update
doc.set_attribution(&format!("source:{}:update", source_id));

// System operation
doc.set_attribution("system:compression");
```

These can be parsed from the commit message in subscription callbacks.

### Ensuring Commits

Update write methods to commit after changes:

```rust
impl StructuredDocument {
    pub fn set_field(
        &self,
        name: &str,
        value: impl Into<serde_json::Value>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // ... permission check ...

        // ... set the value ...

        // Auto-commit so subscriptions fire
        self.doc.commit();

        Ok(())
    }
}
```

Or batch multiple operations before committing:

```rust
// Batch operations without auto-commit
doc.set_field_no_commit("field1", value1, true)?;
doc.set_field_no_commit("field2", value2, true)?;

// Commit once at the end
doc.commit_with_attribution("source:lsp:diagnostics_update");
```

### Subscription Callback Type

Loro's `Subscriber` type:

```rust
pub type Subscriber = Arc<dyn Fn(DiffEvent) + Send + Sync>;

pub struct DiffEvent {
    /// Whether the change was local or from import
    pub triggered_by: EventTriggerKind,
    /// The actual change events
    pub events: Vec<ContainerDiff>,
}
```

### Example: Data Source Edit Watching

```rust
impl LspSource {
    async fn setup_block_watching(&self, block: &StructuredDocument) {
        let source_id = self.source_id.clone();
        let handler = self.edit_handler.clone();

        // Subscribe to the diagnostics block
        let _sub = block.subscribe_content(Arc::new(move |event| {
            // Only handle local edits (agent changes), not our own imports
            if event.triggered_by.is_local() {
                for diff in event.events {
                    // Check which fields changed
                    if let Some(map_diff) = diff.diff.as_map() {
                        for (key, change) in map_diff.updated.iter() {
                            // Route to handler
                            handler.handle_field_edit(&source_id, key, change);
                        }
                    }
                }
            }
        }));

        // Store subscription to keep it alive
        self.subscriptions.lock().push(_sub);
    }
}
```

### Example: File Source Disk Sync

```rust
impl FileSource {
    fn setup_sync(&self, block: &StructuredDocument, path: &Path) {
        let path = path.to_owned();
        let debouncer = self.debouncer.clone();

        let _sub = block.subscribe_root(Arc::new(move |event| {
            if event.triggered_by.is_local() {
                // Agent made local edits - queue disk write
                debouncer.schedule_write(&path);
            }
        }));

        self.subscriptions.lock().push(_sub);
    }
}
```

---

## Integration with MemoryCache

The cache may need to coordinate subscriptions:

```rust
impl MemoryCache {
    /// Get block with subscription for edit watching
    pub async fn get_with_subscription(
        &self,
        agent_id: &str,
        label: &str,
        callback: Subscriber,
    ) -> Result<(StructuredDocument, Subscription), MemoryError> {
        let doc = self.get(agent_id, label).await?;
        let sub = doc.subscribe_root(callback);
        Ok((doc, sub))
    }
}
```

Or subscriptions can be managed externally by the caller (data sources, tools, etc.).

---

## Summary of Changes

### schema.rs
- Add `read_only: bool` to `FieldDef`
- Add `is_field_read_only()` and `read_only_fields()` to `BlockSchema`

### document.rs
- Add `DocumentError::ReadOnlyField`
- Add `is_system: bool` parameter to field write methods
- Add permission check before field writes
- Add `subscribe_root()`, `subscribe()`, `subscribe_content()`
- Add `set_attribution()`, `commit()`, `commit_with_attribution()`
- Update `render()` to show read-only indicators

### types.rs (if needed)
- Could add `EditEvent` type for structured edit tracking

---

## Design Decisions

### Text/List Schemas
No field-level granularity needed. Block-level permission (`MemoryPermission`) handles these. The whole block is either writable or not.

### Composite Schemas - Section-Level Gating
Composite schemas need **section-level** read_only, not just field-level. Common pattern:
- Read-only map section (source-updated diagnostics)
- Editable text section (agent notes)

Update `BlockSchema::Composite`:

```rust
pub enum BlockSchema {
    // ... other variants ...

    Composite {
        /// Named sections with their schemas and permissions
        sections: Vec<CompositeSection>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeSection {
    pub name: String,
    pub schema: Box<BlockSchema>,
    pub description: Option<String>,
    /// If true, only system/source code can write to this section
    #[serde(default)]
    pub read_only: bool,
}
```

Then `is_section_read_only()` checks section-level for Composite:

```rust
impl BlockSchema {
    pub fn is_section_read_only(&self, section_name: &str) -> Option<bool> {
        match self {
            BlockSchema::Composite { sections } => {
                sections.iter()
                    .find(|s| s.name == section_name)
                    .map(|s| s.read_only)
            }
            _ => None,
        }
    }

    /// Get the schema for a section (for type checking operations)
    pub fn get_section_schema(&self, section_name: &str) -> Option<&BlockSchema> {
        match self {
            BlockSchema::Composite { sections } => {
                sections.iter()
                    .find(|s| s.name == section_name)
                    .map(|s| s.schema.as_ref())
            }
            _ => None,
        }
    }
}
```

### Composite Operations - Section Key Parameter

Rather than adding new methods for Composite, update existing methods to take an optional `section` parameter. This determines which Loro container to operate on.

**Current hardcoded keys:**
- Text schema: `"content"` → `doc.get_text("content")`
- Map schema: `"root"` → `doc.get_map("root")`
- List schema: `"items"` → `doc.get_list("items")`
- Log schema: `"entries"` → `doc.get_list("entries")`

**With section parameter:**
```rust
impl StructuredDocument {
    /// Get the container key for an operation
    fn container_key(&self, section: Option<&str>) -> &str {
        section.unwrap_or_else(|| match &self.schema {
            BlockSchema::Text => "content",
            BlockSchema::Map { .. } => "root",
            BlockSchema::List { .. } => "items",
            BlockSchema::Log { .. } => "entries",
            BlockSchema::Composite { .. } => {
                // For composite without section, panic - caller must specify
                panic!("Composite schema requires section parameter")
            }
        })
    }

    /// Get the effective schema for an operation (section schema for Composite)
    fn effective_schema(&self, section: Option<&str>) -> &BlockSchema {
        match (&self.schema, section) {
            (BlockSchema::Composite { .. }, Some(name)) => {
                self.schema.get_section_schema(name)
                    .expect("Section not found in composite schema")
            }
            _ => &self.schema,
        }
    }
}
```

**Updated method signatures:**

```rust
impl StructuredDocument {
    // Text operations - add section parameter
    pub fn text_content(&self, section: Option<&str>) -> String {
        let key = self.container_key(section);
        self.doc.get_text(key).to_string()
    }

    pub fn set_text(
        &self,
        content: &str,
        section: Option<&str>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check section read_only for composite
        if !is_system {
            if let Some(name) = section {
                if let Some(true) = self.schema.is_section_read_only(name) {
                    return Err(DocumentError::ReadOnlySection(name.to_string()));
                }
            }
        }

        let key = self.container_key(section);
        let text = self.doc.get_text(key);
        // ... perform operation ...
        Ok(())
    }

    pub fn append_text(
        &self,
        content: &str,
        section: Option<&str>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Same pattern: check section read_only, get container by key
        // ...
    }

    // Map operations - add section parameter
    pub fn get_field(&self, name: &str, section: Option<&str>) -> Option<serde_json::Value> {
        let key = self.container_key(section);
        let map = self.doc.get_map(key);
        // ...
    }

    pub fn set_field(
        &self,
        name: &str,
        value: impl Into<serde_json::Value>,
        section: Option<&str>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check section read_only first (for composite)
        if !is_system {
            if let Some(sec_name) = section {
                if let Some(true) = self.schema.is_section_read_only(sec_name) {
                    return Err(DocumentError::ReadOnlySection(sec_name.to_string()));
                }
            }
        }

        // Then check field read_only (for Map sections)
        let effective = self.effective_schema(section);
        if !is_system {
            if let BlockSchema::Map { fields } = effective {
                if let Some(field) = fields.iter().find(|f| f.name == name) {
                    if field.read_only {
                        return Err(DocumentError::ReadOnlyField(name.to_string()));
                    }
                }
            }
        }

        let key = self.container_key(section);
        let map = self.doc.get_map(key);
        // ... perform operation ...
        Ok(())
    }

    // List operations - add section parameter
    pub fn list_items(&self, section: Option<&str>) -> Vec<serde_json::Value> {
        let key = self.container_key(section);
        let list = self.doc.get_list(key);
        // ...
    }

    pub fn push_item(
        &self,
        value: impl Into<serde_json::Value>,
        section: Option<&str>,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check section read_only, then operate
        // ...
    }
}
```

**New error variant:**
```rust
#[derive(Debug, Error)]
pub enum DocumentError {
    // ... existing ...

    #[error("Section '{0}' is read-only and cannot be modified by agent")]
    ReadOnlySection(String),
}
```

**Call site updates:**

Existing code using these methods needs to add `None` for the section parameter:

```rust
// Before
doc.text_content()
doc.set_field("key", value)

// After
doc.text_content(None)
doc.set_field("key", value, None, is_system)
```

For composite blocks:
```rust
// Access a specific section
let diagnostics = doc.get_field("errors", Some("diagnostics"))?;
doc.set_field("filter", "warning", Some("config"), false)?;
```

### StructuredDocument Self-Identification

StructuredDocument should carry its own identity for convenience (no need to pass context everywhere):

```rust
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,
    permission: MemoryPermission,

    /// Block label for identification
    label: String,
    /// Agent that loaded this document (for attribution)
    accessor_agent_id: Option<String>,
}

impl StructuredDocument {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn accessor_agent_id(&self) -> Option<&str> {
        self.accessor_agent_id.as_deref()
    }

    /// Set attribution automatically based on accessor
    pub fn auto_attribution(&self, operation: &str) {
        if let Some(agent_id) = &self.accessor_agent_id {
            self.set_attribution(&format!("agent:{}:{}", agent_id, operation));
        }
    }
}
```

This enables tools to:
```rust
// Tool code
doc.auto_attribution("set_field:severity_filter");
doc.set_field("severity_filter", "warning", false)?;
doc.commit();
// Then call memory.mark_dirty() and memory.persist_block() with doc.label()
```

### Persistence Responsibility

Can't enforce programmatically that callers call `mark_dirty()` / `persist_block()`, but:

1. Document carries `label` and `accessor_agent_id` so callers have the info they need
2. Tools should always have a reference to the MemoryStore that provided the document
3. Document convention: after modifying, call `memory.mark_dirty(label)` and eventually `memory.persist_block(label)`

Could add a helper that bundles both:

```rust
impl MemoryCache {
    /// Persist a modified document (marks dirty and persists)
    pub async fn save_document(&self, doc: &StructuredDocument) -> Result<(), MemoryError> {
        let label = doc.label();
        self.mark_dirty(label);
        self.persist_block(label).await
    }
}
```

---

## Final Design Decisions

### Auto-Commit Behavior
Write methods auto-commit. Granularity is good - each operation is atomic and triggers subscriptions immediately. No need for separate batch variants.

```rust
pub fn set_field(...) -> Result<(), DocumentError> {
    // ... permission checks ...
    // ... perform operation ...
    self.doc.commit();  // Always commit
    Ok(())
}
```

### Subscriptions
Entirely caller's responsibility. StructuredDocument exposes the Loro subscription methods, but doesn't manage subscription lifetime. Data sources, tools, or other callers create and hold `Subscription` handles as needed.

### UndoManager Integration
Loro provides `UndoManager` for granular undo/redo. Expose with StructuredDocument:

```rust
use loro::UndoManager;

impl StructuredDocument {
    /// Create an UndoManager for this document
    pub fn undo_manager(&self) -> UndoManager {
        UndoManager::new(&self.doc)
    }
}
```

Usage:
```rust
let mut undo = doc.undo_manager();
doc.set_field("key", "value1", None, false)?;
doc.set_field("key", "value2", None, false)?;
undo.undo();  // Back to "value1"
undo.redo();  // Forward to "value2"
```

This gives agents/tools fine-grained rollback without needing full version snapshots.

### Tree Schema - Dropped
Tree schema is dropped from the design. Composite covers the use cases:
- Map with complex fields ≈ shallow tree
- Composite with Map sections ≈ opinionated tree
- Deep arbitrary nesting is confusing and hard to implement well
- If path queries needed later, can port from Jacquard's value types

Update `BlockSchema`:
```rust
pub enum BlockSchema {
    Text,
    Map { fields: Vec<FieldDef> },
    List { item_schema: Option<Box<BlockSchema>>, max_items: Option<usize> },
    Log { display_limit: usize },
    Composite { sections: Vec<CompositeSection> },
    // Tree removed
}
```
