# V2 Structured Memory Sketch

## Overview

Loro CRDT provides more than just text - it has Lists, Maps, Trees, and Counters. This enables **templated/structured memory blocks** where agents work with semantic structure, not just free-form strings.

**Loro Version:** 1.10.3 (production-ready, 5.2k GitHub stars)

## Loro Container Types

| Type | CRDT Algorithm | Use Case | Key Methods |
|------|----------------|----------|-------------|
| `LoroText` | Fugue | Rich text, cursor tracking | `insert()`, `delete()`, `mark()` |
| `LoroList` | RGA | Ordered items | `push()`, `insert()`, `delete()` |
| `LoroMap` | LWW | Key-value pairs | `insert()`, `get()`, `delete()` |
| `LoroTree` | Tree CRDT | Hierarchical data | `create()`, `mov()`, `mov_to()` |
| `LoroMovableList` | RGA+move | Drag-and-drop lists | `mov()`, `set()` |
| `LoroCounter` | Increment-only | Numeric tracking | `increment()` (feature-gated) |

## Loro API Basics

```rust
use loro::{LoroDoc, ExportMode};

// Create document
let doc = LoroDoc::new();

// Access containers (created on first access)
let text = doc.get_text("persona");
let list = doc.get_list("preferences");
let map = doc.get_map("profile");
let tree = doc.get_tree("project");

// Text operations
text.insert(0, "Hello")?;
text.delete(0, 5)?;
text.mark(0..5, "bold", true)?;  // Rich text

// List operations
list.push("item")?;
list.insert(0, "first")?;
list.delete(1)?;

// Map operations
map.insert("name", "Alice")?;
map.insert("energy", 7)?;
map.delete("energy")?;

// Nested containers
map.insert_container("tags", LoroList::new())?;

// Persistence
let snapshot = doc.export(ExportMode::Snapshot)?;  // Full state
let updates = doc.export(ExportMode::update(from_version))?;  // Incremental

// Import (merges automatically)
doc.import(&peer_bytes)?;

// Time travel
let checkpoint = doc.state_frontiers();
doc.checkout(&checkpoint)?;  // Read-only view
doc.checkout_to_latest()?;   // Return to head
```

**Important:** LoroDoc owns all containers. Keep doc alive as long as containers are used.

## Block Schema Concept

Instead of all blocks being opaque text, blocks can have **schemas** that define their structure:

```rust
/// Block schema defines the structure of a memory block's Loro document
pub enum BlockSchema {
    /// Free-form text (default, backward compatible)
    Text,

    /// Key-value pairs with optional field definitions
    Map {
        fields: Vec<FieldDef>,
    },

    /// Ordered list of items
    List {
        item_schema: Option<Box<BlockSchema>>,
        max_items: Option<usize>,
    },

    /// Rolling log (list with auto-trim)
    Log {
        max_entries: usize,
        entry_schema: LogEntrySchema,
    },

    /// Hierarchical tree
    Tree {
        node_schema: Option<Box<BlockSchema>>,
    },

    /// Custom composite
    Composite {
        sections: Vec<(String, BlockSchema)>,
    },
}

pub struct FieldDef {
    pub name: String,
    pub description: String,
    pub field_type: FieldType,
    pub required: bool,
    pub default: Option<serde_json::Value>,
}

pub enum FieldType {
    Text,
    Number,
    Boolean,
    List,
    Timestamp,
}
```

## Example Templates

### Partner Profile (Map)

```rust
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
            default: Some(json!([])),
        },
        FieldDef {
            name: "energy_level".into(),
            description: "Current energy (1-10)".into(),
            field_type: FieldType::Number,
            required: false,
            default: Some(json!(5)),
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
```

### Task List (List)

```rust
BlockSchema::List {
    item_schema: Some(Box::new(BlockSchema::Map {
        fields: vec![
            FieldDef { name: "title".into(), field_type: FieldType::Text, required: true, .. },
            FieldDef { name: "done".into(), field_type: FieldType::Boolean, required: true, default: Some(json!(false)), .. },
            FieldDef { name: "priority".into(), field_type: FieldType::Text, required: false, .. },
            FieldDef { name: "due".into(), field_type: FieldType::Timestamp, required: false, .. },
        ],
    })),
    max_items: Some(100),
}
```

### Activity Log (Log)

```rust
BlockSchema::Log {
    max_entries: 50,  // Keep recent 50 in context
    entry_schema: LogEntrySchema {
        timestamp: true,
        agent_id: true,
        fields: vec![
            FieldDef { name: "event_type".into(), field_type: FieldType::Text, required: true, .. },
            FieldDef { name: "description".into(), field_type: FieldType::Text, required: true, .. },
        ],
    },
}
```

## Agent Operations

With structured blocks, tools can offer semantic operations:

### Context Tool Extensions

```rust
pub enum ContextOperation {
    // Existing text operations
    Append { label: String, content: String },
    Replace { label: String, find: String, replace: String },

    // New structured operations
    SetField { label: String, field: String, value: serde_json::Value },
    AppendToList { label: String, field: Option<String>, item: serde_json::Value },
    RemoveFromList { label: String, field: Option<String>, index: usize },
    IncrementCounter { label: String, field: String, delta: i64 },

    // Existing swap operations
    Archive { label: String, archival_label: Option<String> },
    Load { archival_label: String, label: Option<String> },
    Swap { label: String, archival_label: String },
}
```

### Tool Descriptions for LLM

```
context.set_field: Update a specific field in a structured memory block.
Use for: Setting partner's name, updating energy level, changing current focus.
Example: set_field(label="human", field="energy_level", value=7)

context.append_to_list: Add an item to a list in a memory block.
Use for: Adding a new preference, recording a new task, logging an observation.
Example: append_to_list(label="human", field="preferences", item="Prefers morning check-ins")

context.increment_counter: Increment or decrement a numeric field.
Use for: Tracking counts, adjusting levels.
Example: increment_counter(label="human", field="energy_level", delta=-2)
```

## Context Rendering

Structured blocks render to readable text for LLM context:

### Map Rendering
```
<human>
<!-- Information about the person you're talking to -->
name: Alice
energy_level: 7
current_focus: Refactoring the memory system
preferences:
  - Prefers concise responses
  - Likes code examples
  - Morning person
</human>
```

### List Rendering
```
<tasks>
<!-- Current task list -->
1. [x] Design block schemas
2. [ ] Implement Loro wrappers (priority: high)
3. [ ] Write tests
4. [ ] Update context tool
</tasks>
```

### Log Rendering
```
<activity_log>
<!-- Recent activity (last 10 shown) -->
[2025-01-23 14:32] Agent processed message about memory design
[2025-01-23 14:28] Tool executed: context.append (scratchpad)
[2025-01-23 14:25] Memory updated: human.preferences (added item)
</activity_log>
```

## Implementation Considerations

### 1. Schema Storage
- Schema could be stored in block metadata (JSON)
- Or schema could be a separate table (reusable templates)
- Default schema is Text (backward compatible)

### 2. Validation
- On write: validate value against schema
- On read: handle schema evolution (missing fields get defaults)
- Migrations: schema changes should be additive

### 3. Loro Document Structure

For a Map block:
```rust
// Each field is a child container
doc.get_text("name")       // LoroText
doc.get_list("preferences") // LoroList
doc.get_map("metadata")     // For extensibility
```

For a List block:
```rust
doc.get_list("items")  // LoroList of LoroMaps (each item)
```

For a Log block:
```rust
doc.get_list("entries")  // LoroList, auto-trimmed on persist
```

### 4. Backward Compatibility
- Existing text blocks continue to work (schema = Text)
- New structured blocks opt-in via schema
- Migration tool can convert text → structured if needed

### 5. CRDT Benefits
- **Concurrent edits**: Multiple agents can modify same block
- **Merge semantics**: List appends merge cleanly, map fields LWW
- **History**: All changes tracked, rollback possible
- **Sync**: Future multi-device support

## Open Questions

1. **Schema evolution**: How do we handle schema changes over time?
2. **Nested structures**: How deep should we allow nesting?
3. **Custom renderers**: Should blocks define their own context rendering?
4. **Tool generation**: Can we auto-generate tool operations from schema?
5. **Validation errors**: How should agents handle schema violations?

## Next Steps

1. Research Loro API in detail (container types, persistence)
2. Design StructuredDocument wrapper with typed operations
3. Implement BlockSchema and validation
4. Extend MemoryStore trait with structured operations
5. Update context tool with new operations
6. Create default templates (partner profile, task list, log)
