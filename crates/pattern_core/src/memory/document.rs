//! Loro document operations for structured memory blocks

use loro::{ExportMode, LoroDoc, LoroValue, VersionVector};
use serde_json::Value as JsonValue;

use crate::memory::schema::{BlockSchema, FieldType, LogEntrySchema};

/// Wrapper around LoroDoc for schema-aware operations
#[derive(Clone, Debug)]
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,
    /// Effective permission for this access (block's inherent permission or shared permission)
    permission: pattern_db::models::MemoryPermission,
}

/// Errors that can occur during document operations
#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("Failed to import document: {0}")]
    ImportFailed(String),

    #[error("Failed to export document: {0}")]
    ExportFailed(String),

    #[error("Field not found: {0}")]
    FieldNotFound(String),

    #[error("Schema mismatch: expected {expected}, got {actual}")]
    SchemaMismatch { expected: String, actual: String },

    #[error("{0}")]
    Other(String),
}

impl StructuredDocument {
    /// Create a new document with the given schema and permission
    pub fn new_with_permission(
        schema: BlockSchema,
        permission: pattern_db::models::MemoryPermission,
    ) -> Self {
        let doc = LoroDoc::new();
        Self {
            doc,
            schema,
            permission,
        }
    }

    /// Create a new document with the given schema (default ReadWrite permission)
    pub fn new(schema: BlockSchema) -> Self {
        Self::new_with_permission(schema, pattern_db::models::MemoryPermission::ReadWrite)
    }

    /// Create with default Text schema
    pub fn new_text() -> Self {
        Self::new(BlockSchema::Text)
    }

    /// Create from an existing Loro snapshot with permission
    pub fn from_snapshot_with_permission(
        snapshot: &[u8],
        schema: BlockSchema,
        permission: pattern_db::models::MemoryPermission,
    ) -> Result<Self, DocumentError> {
        let doc = LoroDoc::new();
        doc.import(snapshot)
            .map_err(|e| DocumentError::ImportFailed(e.to_string()))?;
        Ok(Self {
            doc,
            schema,
            permission,
        })
    }

    /// Create from an existing Loro snapshot (default ReadWrite permission)
    pub fn from_snapshot(snapshot: &[u8], schema: BlockSchema) -> Result<Self, DocumentError> {
        Self::from_snapshot_with_permission(
            snapshot,
            schema,
            pattern_db::models::MemoryPermission::ReadWrite,
        )
    }

    /// Apply updates to the document
    pub fn apply_updates(&self, updates: &[u8]) -> Result<(), DocumentError> {
        self.doc
            .import(updates)
            .map_err(|e| DocumentError::ImportFailed(e.to_string()))?;
        Ok(())
    }

    /// Get the schema
    pub fn schema(&self) -> &BlockSchema {
        &self.schema
    }

    /// Get the effective permission for this document
    pub fn permission(&self) -> pattern_db::models::MemoryPermission {
        self.permission
    }

    /// Set the effective permission for this document (DB is source of truth)
    pub fn set_permission(&mut self, permission: pattern_db::models::MemoryPermission) {
        self.permission = permission;
    }

    /// Get the underlying LoroDoc (for advanced operations)
    pub fn inner(&self) -> &LoroDoc {
        &self.doc
    }

    // ========== Text Operations ==========

    /// Get text content
    pub fn text_content(&self) -> String {
        let text = self.doc.get_text("content");
        text.to_string()
    }

    /// Set text content (replaces all)
    pub fn set_text(&self, content: &str) -> Result<(), DocumentError> {
        let text = self.doc.get_text("content");
        let current_len = text.len_unicode();

        // Delete all current content, then insert new
        if current_len > 0 {
            text.delete(0, current_len)
                .map_err(|e| DocumentError::Other(e.to_string()))?;
        }
        text.insert(0, content)
            .map_err(|e| DocumentError::Other(e.to_string()))?;

        Ok(())
    }

    /// Append text to existing content
    pub fn append_text(&self, content: &str) -> Result<(), DocumentError> {
        let text = self.doc.get_text("content");
        let pos = text.len_unicode();
        text.insert(pos, content)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Replace all occurrences of find with replace
    /// Returns true if at least one replacement was made
    pub fn replace_text(&self, find: &str, replace: &str) -> Result<bool, DocumentError> {
        let current = self.text_content();
        if !current.contains(find) {
            return Ok(false);
        }

        let new_content = current.replace(find, replace);
        self.set_text(&new_content)?;
        Ok(true)
    }

    // ========== Map Operations ==========

    /// Get a field value from the map
    pub fn get_field(&self, field: &str) -> Option<JsonValue> {
        let map = self.doc.get_map("fields");
        map.get(field).and_then(|v| {
            if let Some(value) = v.as_value() {
                loro_to_json(value)
            } else {
                None
            }
        })
    }

    /// Set a field value in the map
    pub fn set_field(&self, field: &str, value: JsonValue) -> Result<(), DocumentError> {
        let map = self.doc.get_map("fields");
        let loro_value = json_to_loro(&value);
        map.insert(field, loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Get a text field (convenience method)
    pub fn get_text_field(&self, field: &str) -> Option<String> {
        self.get_field(field)
            .and_then(|v| v.as_str().map(String::from))
    }

    /// Set a text field (convenience method)
    pub fn set_text_field(&self, field: &str, value: &str) -> Result<(), DocumentError> {
        self.set_field(field, JsonValue::String(value.to_string()))
    }

    /// Get items from a list field
    pub fn get_list_field(&self, field: &str) -> Vec<JsonValue> {
        let list = self.doc.get_list(format!("list_{field}"));
        (0..list.len())
            .filter_map(|i| {
                list.get(i)
                    .and_then(|v| v.as_value().and_then(loro_to_json))
            })
            .collect()
    }

    /// Append an item to a list field
    pub fn append_to_list_field(&self, field: &str, item: JsonValue) -> Result<(), DocumentError> {
        let list = self.doc.get_list(format!("list_{field}"));
        let loro_value = json_to_loro(&item);
        list.push(loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Remove an item from a list field by index
    pub fn remove_from_list_field(&self, field: &str, index: usize) -> Result<(), DocumentError> {
        let list = self.doc.get_list(format!("list_{field}"));
        if index >= list.len() {
            return Err(DocumentError::Other(format!(
                "Index {} out of bounds (len={})",
                index,
                list.len()
            )));
        }
        list.delete(index, 1)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    // ========== Counter Operations ==========

    /// Get counter value
    pub fn get_counter(&self, field: &str) -> i64 {
        let counter = self.doc.get_counter(format!("counter_{field}"));
        counter.get_value() as i64
    }

    /// Increment counter by delta, returns new value
    pub fn increment_counter(&self, field: &str, delta: i64) -> Result<i64, DocumentError> {
        let counter = self.doc.get_counter(format!("counter_{field}"));
        counter
            .increment(delta as f64)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(counter.get_value() as i64)
    }

    // ========== List Operations (for List schema blocks) ==========

    /// Get all items from the list
    pub fn list_items(&self) -> Vec<JsonValue> {
        let list = self.doc.get_list("items");
        (0..list.len())
            .filter_map(|i| {
                list.get(i)
                    .and_then(|v| v.as_value().and_then(loro_to_json))
            })
            .collect()
    }

    /// Push an item to the end of the list
    pub fn push_item(&self, item: JsonValue) -> Result<(), DocumentError> {
        let list = self.doc.get_list("items");
        let loro_value = json_to_loro(&item);
        list.push(loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Insert an item at a specific index
    pub fn insert_item(&self, index: usize, item: JsonValue) -> Result<(), DocumentError> {
        let list = self.doc.get_list("items");
        if index > list.len() {
            return Err(DocumentError::Other(format!(
                "Index {} out of bounds (len={})",
                index,
                list.len()
            )));
        }
        let loro_value = json_to_loro(&item);
        list.insert(index, loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Delete an item at a specific index
    pub fn delete_item(&self, index: usize) -> Result<(), DocumentError> {
        let list = self.doc.get_list("items");
        if index >= list.len() {
            return Err(DocumentError::Other(format!(
                "Index {} out of bounds (len={})",
                index,
                list.len()
            )));
        }
        list.delete(index, 1)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Get the number of items in the list
    pub fn list_len(&self) -> usize {
        let list = self.doc.get_list("items");
        list.len()
    }

    // ========== Log Operations ==========

    /// Get log entries (most recent first), respecting display_limit from schema
    pub fn log_entries(&self, limit: Option<usize>) -> Vec<JsonValue> {
        let list = self.doc.get_list("entries");
        let len = list.len();

        // Determine how many to return
        let display_limit = limit.or_else(|| {
            if let BlockSchema::Log { display_limit, .. } = &self.schema {
                Some(*display_limit)
            } else {
                None
            }
        });

        let take = display_limit.unwrap_or(len).min(len);

        // Get most recent entries (from end of list)
        let start = len.saturating_sub(take);
        (start..len)
            .rev() // Reverse to get newest first
            .filter_map(|i| {
                list.get(i)
                    .and_then(|v| v.as_value().and_then(loro_to_json))
            })
            .collect()
    }

    /// Append a log entry
    pub fn append_log_entry(&self, entry: JsonValue) -> Result<(), DocumentError> {
        let list = self.doc.get_list("entries");
        let loro_value = json_to_loro(&entry);
        list.push(loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    // ========== Persistence ==========

    /// Export a complete snapshot
    pub fn export_snapshot(&self) -> Result<Vec<u8>, DocumentError> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|e| DocumentError::ExportFailed(e.to_string()))
    }

    /// Export updates since a specific version
    pub fn export_updates_since(&self, from: &VersionVector) -> Result<Vec<u8>, DocumentError> {
        self.doc
            .export(ExportMode::updates(from))
            .map_err(|e| DocumentError::ExportFailed(e.to_string()))
    }

    /// Get the current version vector
    pub fn current_version(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    // ========== Rendering ==========

    /// Render document content for LLM context
    pub fn render(&self) -> String {
        match &self.schema {
            BlockSchema::Text => self.text_content(),

            BlockSchema::Map { fields } => {
                let mut lines = Vec::new();
                for field_def in fields {
                    let field_name = &field_def.name;

                    if field_def.field_type == FieldType::List {
                        // Render list fields as bullets
                        let items = self.get_list_field(field_name);
                        if !items.is_empty() {
                            lines.push(format!("{}:", field_name));
                            for item in items {
                                lines.push(format!("- {}", json_display(&item)));
                            }
                        }
                    } else if field_def.field_type == FieldType::Counter {
                        // Render counter value
                        let value = self.get_counter(field_name);
                        lines.push(format!("{}: {}", field_name, value));
                    } else {
                        // Regular field
                        if let Some(value) = self.get_field(field_name) {
                            lines.push(format!("{}: {}", field_name, json_display(&value)));
                        }
                    }
                }
                lines.join("\n")
            }

            BlockSchema::List { .. } => {
                let items = self.list_items();
                let mut lines = Vec::new();

                for (i, item) in items.iter().enumerate() {
                    // Check if this looks like a task item with a "done" field
                    let prefix = if let Some(obj) = item.as_object() {
                        if let Some(done) = obj.get("done").and_then(|v| v.as_bool()) {
                            if done { "[x]" } else { "[ ]" }
                        } else {
                            &format!("{}.", i + 1)
                        }
                    } else {
                        &format!("{}.", i + 1)
                    };

                    lines.push(format!("{} {}", prefix, json_display(item)));
                }
                lines.join("\n")
            }

            BlockSchema::Log {
                display_limit,
                entry_schema,
            } => {
                let entries = self.log_entries(Some(*display_limit));
                let mut lines = Vec::new();

                for entry in entries {
                    lines.push(format_log_entry(&entry, entry_schema));
                }
                lines.join("\n")
            }

            BlockSchema::Tree { .. } => {
                // TODO: Tree rendering
                "[Tree rendering not yet implemented]".to_string()
            }

            BlockSchema::Composite { sections } => {
                let mut lines = Vec::new();
                for (name, _schema) in sections {
                    lines.push(format!("=== {} ===", name));
                    // TODO: Render each section according to its schema
                    lines.push("[Section rendering not yet implemented]".to_string());
                }
                lines.join("\n\n")
            }
        }
    }
}

// ========== Helper Functions ==========

/// Convert LoroValue to serde_json::Value
fn loro_to_json(value: &LoroValue) -> Option<JsonValue> {
    Some(match value {
        LoroValue::Null => JsonValue::Null,
        LoroValue::Bool(b) => JsonValue::Bool(*b),
        LoroValue::Double(d) => serde_json::Number::from_f64(*d).map(JsonValue::Number)?,
        LoroValue::I64(i) => JsonValue::Number((*i).into()),
        LoroValue::String(s) => JsonValue::String(s.to_string()),
        LoroValue::List(list) => {
            let items: Vec<JsonValue> = list.iter().filter_map(loro_to_json).collect();
            JsonValue::Array(items)
        }
        LoroValue::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map.iter() {
                if let Some(json_v) = loro_to_json(v) {
                    obj.insert(k.to_string(), json_v);
                }
            }
            JsonValue::Object(obj)
        }
        LoroValue::Binary(_) => return None, // Skip binary data
        LoroValue::Container(_) => return None, // Skip nested containers
    })
}

/// Convert serde_json::Value to LoroValue
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
            let items: Vec<LoroValue> = arr.iter().map(json_to_loro).collect();
            LoroValue::List(items.into())
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

/// Display a JSON value in human-readable format
fn json_display(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => n.to_string(),
        JsonValue::String(s) => s.clone(),
        JsonValue::Array(arr) => {
            let items: Vec<String> = arr.iter().map(json_display).collect();
            format!("[{}]", items.join(", "))
        }
        JsonValue::Object(obj) => {
            // For objects, show as "key: value" pairs
            let pairs: Vec<String> = obj
                .iter()
                .map(|(k, v)| format!("{}: {}", k, json_display(v)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
    }
}

/// Format a log entry for display
fn format_log_entry(entry: &JsonValue, schema: &LogEntrySchema) -> String {
    if let Some(obj) = entry.as_object() {
        let mut parts = Vec::new();

        // Add timestamp if present and enabled in schema
        if schema.timestamp {
            if let Some(timestamp) = obj.get("timestamp").and_then(|v| v.as_str()) {
                parts.push(format!("[{}]", timestamp));
            }
        }

        // Add agent_id if present and enabled in schema
        if schema.agent_id {
            if let Some(agent_id) = obj.get("agent_id").and_then(|v| v.as_str()) {
                parts.push(format!("({})", agent_id));
            }
        }

        // Add other fields
        for field_def in &schema.fields {
            if let Some(value) = obj.get(&field_def.name) {
                parts.push(json_display(value));
            }
        }

        parts.join(" ")
    } else {
        // Fallback for non-object entries
        json_display(entry)
    }
}

/// Create a snapshot with initial text content
pub fn create_text_snapshot(content: &str) -> Result<Vec<u8>, DocumentError> {
    let doc = LoroDoc::new();
    let text = doc.get_text("content");
    text.insert(0, content)
        .map_err(|e| DocumentError::Other(e.to_string()))?;
    doc.export(ExportMode::Snapshot)
        .map_err(|e| DocumentError::ExportFailed(e.to_string()))
}

/// Extract text from a snapshot
pub fn text_from_snapshot(snapshot: &[u8]) -> Result<String, DocumentError> {
    let doc = LoroDoc::new();
    doc.import(snapshot)
        .map_err(|e| DocumentError::ImportFailed(e.to_string()))?;
    let text = doc.get_text("content");
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::{FieldDef, LogEntrySchema};

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
        doc.set_text("Hello, world! Hello again!").unwrap();

        // Replace all occurrences
        let replaced = doc.replace_text("Hello", "Hi").unwrap();
        assert!(replaced);
        assert_eq!(doc.text_content(), "Hi, world! Hi again!");

        // No replacement needed
        let replaced = doc.replace_text("Goodbye", "Bye").unwrap();
        assert!(!replaced);
    }

    #[test]
    fn test_map_fields() {
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Name field".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                },
                FieldDef {
                    name: "age".to_string(),
                    description: "Age field".to_string(),
                    field_type: FieldType::Number,
                    required: false,
                    default: Some(JsonValue::Number(0.into())),
                },
            ],
        };

        let doc = StructuredDocument::new(schema);

        // Set text field
        doc.set_text_field("name", "Alice").unwrap();
        assert_eq!(doc.get_text_field("name"), Some("Alice".to_string()));

        // Set number field
        doc.set_field("age", JsonValue::Number(30.into())).unwrap();
        assert_eq!(doc.get_field("age"), Some(JsonValue::Number(30.into())));
    }

    #[test]
    fn test_counter_operations() {
        let schema = BlockSchema::Map {
            fields: vec![FieldDef {
                name: "score".to_string(),
                description: "Score counter".to_string(),
                field_type: FieldType::Counter,
                required: false,
                default: Some(JsonValue::Number(0.into())),
            }],
        };

        let doc = StructuredDocument::new(schema);

        // Initial value is 0
        assert_eq!(doc.get_counter("score"), 0);

        // Increment
        let new_val = doc.increment_counter("score", 5).unwrap();
        assert_eq!(new_val, 5);
        assert_eq!(doc.get_counter("score"), 5);

        // Decrement
        let new_val = doc.increment_counter("score", -2).unwrap();
        assert_eq!(new_val, 3);
        assert_eq!(doc.get_counter("score"), 3);
    }

    #[test]
    fn test_list_operations() {
        let schema = BlockSchema::List {
            item_schema: None,
            max_items: None,
        };

        let doc = StructuredDocument::new(schema);

        // Initially empty
        assert_eq!(doc.list_len(), 0);

        // Push items
        doc.push_item(JsonValue::String("first".to_string()))
            .unwrap();
        doc.push_item(JsonValue::String("second".to_string()))
            .unwrap();
        assert_eq!(doc.list_len(), 2);

        // Insert at index
        doc.insert_item(1, JsonValue::String("middle".to_string()))
            .unwrap();
        assert_eq!(doc.list_len(), 3);

        let items = doc.list_items();
        assert_eq!(items[0], JsonValue::String("first".to_string()));
        assert_eq!(items[1], JsonValue::String("middle".to_string()));
        assert_eq!(items[2], JsonValue::String("second".to_string()));

        // Delete item
        doc.delete_item(1).unwrap();
        assert_eq!(doc.list_len(), 2);
    }

    #[test]
    fn test_log_operations() {
        let schema = BlockSchema::Log {
            display_limit: 3,
            entry_schema: LogEntrySchema {
                timestamp: true,
                agent_id: false,
                fields: vec![FieldDef {
                    name: "message".to_string(),
                    description: "Log message".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                }],
            },
        };

        let doc = StructuredDocument::new(schema);

        // Add entries
        doc.append_log_entry(serde_json::json!({
            "timestamp": "2025-01-01T00:00:00Z",
            "message": "First entry"
        }))
        .unwrap();

        doc.append_log_entry(serde_json::json!({
            "timestamp": "2025-01-01T00:01:00Z",
            "message": "Second entry"
        }))
        .unwrap();

        doc.append_log_entry(serde_json::json!({
            "timestamp": "2025-01-01T00:02:00Z",
            "message": "Third entry"
        }))
        .unwrap();

        doc.append_log_entry(serde_json::json!({
            "timestamp": "2025-01-01T00:03:00Z",
            "message": "Fourth entry"
        }))
        .unwrap();

        // Should get only the 3 most recent (respecting display_limit)
        let entries = doc.log_entries(None);
        assert_eq!(entries.len(), 3);

        // Most recent should be first
        assert_eq!(
            entries[0]["message"],
            JsonValue::String("Fourth entry".to_string())
        );
        assert_eq!(
            entries[1]["message"],
            JsonValue::String("Third entry".to_string())
        );
        assert_eq!(
            entries[2]["message"],
            JsonValue::String("Second entry".to_string())
        );
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Test content").unwrap();

        // Export snapshot
        let snapshot = doc.export_snapshot().unwrap();

        // Import into new document
        let doc2 = StructuredDocument::from_snapshot(&snapshot, BlockSchema::Text).unwrap();
        assert_eq!(doc2.text_content(), "Test content");
    }

    #[test]
    fn test_render_map() {
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Name".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                },
                FieldDef {
                    name: "tags".to_string(),
                    description: "Tags".to_string(),
                    field_type: FieldType::List,
                    required: false,
                    default: None,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);
        doc.set_text_field("name", "Alice").unwrap();
        doc.append_to_list_field("tags", JsonValue::String("important".to_string()))
            .unwrap();
        doc.append_to_list_field("tags", JsonValue::String("urgent".to_string()))
            .unwrap();

        let rendered = doc.render();
        assert!(rendered.contains("name: Alice"));
        assert!(rendered.contains("tags:"));
        assert!(rendered.contains("- important"));
        assert!(rendered.contains("- urgent"));
    }

    #[test]
    fn test_render_list() {
        let schema = BlockSchema::List {
            item_schema: Some(Box::new(BlockSchema::Map {
                fields: vec![
                    FieldDef {
                        name: "title".to_string(),
                        description: "Title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        default: None,
                    },
                    FieldDef {
                        name: "done".to_string(),
                        description: "Done".to_string(),
                        field_type: FieldType::Boolean,
                        required: true,
                        default: Some(JsonValue::Bool(false)),
                    },
                ],
            })),
            max_items: None,
        };

        let doc = StructuredDocument::new(schema);

        doc.push_item(serde_json::json!({
            "title": "Task 1",
            "done": false
        }))
        .unwrap();

        doc.push_item(serde_json::json!({
            "title": "Task 2",
            "done": true
        }))
        .unwrap();

        let rendered = doc.render();
        assert!(rendered.contains("[ ]"));
        assert!(rendered.contains("[x]"));
        assert!(rendered.contains("Task 1"));
        assert!(rendered.contains("Task 2"));
    }

    #[test]
    fn test_list_field_operations() {
        let schema = BlockSchema::Map {
            fields: vec![FieldDef {
                name: "tags".to_string(),
                description: "Tags list".to_string(),
                field_type: FieldType::List,
                required: false,
                default: None,
            }],
        };

        let doc = StructuredDocument::new(schema);

        // Add items to list field
        doc.append_to_list_field("tags", JsonValue::String("tag1".to_string()))
            .unwrap();
        doc.append_to_list_field("tags", JsonValue::String("tag2".to_string()))
            .unwrap();
        doc.append_to_list_field("tags", JsonValue::String("tag3".to_string()))
            .unwrap();

        let tags = doc.get_list_field("tags");
        assert_eq!(tags.len(), 3);

        // Remove middle item
        doc.remove_from_list_field("tags", 1).unwrap();
        let tags = doc.get_list_field("tags");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], JsonValue::String("tag1".to_string()));
        assert_eq!(tags[1], JsonValue::String("tag3".to_string()));
    }

    #[test]
    fn test_create_text_snapshot() {
        let snapshot = create_text_snapshot("Hello, world!").unwrap();
        let text = text_from_snapshot(&snapshot).unwrap();
        assert_eq!(text, "Hello, world!");
    }
}
