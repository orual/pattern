//! Loro document operations for structured memory blocks

use loro::{
    ContainerID, ContainerTrait, ExportMode, LoroDoc, LoroValue, VersionVector, cursor::PosType,
};
use serde_json::Value as JsonValue;

use crate::memory::schema::{BlockSchema, FieldType, LogEntrySchema};

/// Wrapper around LoroDoc for schema-aware operations
#[derive(Clone, Debug)]
pub struct StructuredDocument {
    doc: LoroDoc,
    schema: BlockSchema,
    /// Effective permission for this access (block's inherent permission or shared permission)
    permission: pattern_db::models::MemoryPermission,

    /// Block label for identification.
    label: String,
    /// Agent that loaded this document (for attribution).
    accessor_agent_id: Option<String>,
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

    #[error("Field '{0}' is read-only and cannot be modified by agent")]
    ReadOnlyField(String),

    #[error("Section '{0}' is read-only and cannot be modified by agent")]
    ReadOnlySection(String),

    #[error("Operation '{operation}' not supported for schema {schema}")]
    InvalidSchemaForOperation { operation: String, schema: String },

    #[error(
        "Permission denied: {operation} requires {required} permission, but block has {actual}"
    )]
    PermissionDenied {
        operation: String,
        required: pattern_db::models::MemoryPermission,
        actual: pattern_db::models::MemoryPermission,
    },

    #[error("{0}")]
    Other(String),
}

impl StructuredDocument {
    /// Create a new document with identity information.
    pub fn new_with_identity(
        schema: BlockSchema,
        label: String,
        accessor_agent_id: Option<String>,
    ) -> Self {
        Self {
            doc: LoroDoc::new(),
            schema,
            permission: pattern_db::models::MemoryPermission::ReadWrite,
            label,
            accessor_agent_id,
        }
    }

    /// Create a new document with the given schema and permission
    pub fn new_with_permission(
        schema: BlockSchema,
        permission: pattern_db::models::MemoryPermission,
    ) -> Self {
        Self {
            doc: LoroDoc::new(),
            schema,
            permission,
            label: String::new(),
            accessor_agent_id: None,
        }
    }

    /// Create a new document with the given schema (default ReadWrite permission)
    pub fn new(schema: BlockSchema) -> Self {
        Self::new_with_identity(schema, String::new(), None)
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
            label: String::new(),
            accessor_agent_id: None,
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

    /// Get the block label for identification.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Get the agent that loaded this document (for attribution).
    pub fn accessor_agent_id(&self) -> Option<&str> {
        self.accessor_agent_id.as_deref()
    }

    /// Set attribution automatically based on accessor agent.
    pub fn auto_attribution(&self, operation: &str) {
        if let Some(agent_id) = &self.accessor_agent_id {
            self.set_attribution(&format!("agent:{}:{}", agent_id, operation));
        }
    }

    /// Get the underlying LoroDoc (for advanced operations)
    pub fn inner(&self) -> &LoroDoc {
        &self.doc
    }

    /// Check if an operation is allowed based on document permission.
    /// Returns Ok(()) if allowed, or PermissionDenied error if not.
    fn check_permission(
        &self,
        op: pattern_db::models::MemoryOp,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        if is_system {
            return Ok(());
        }

        let gate = pattern_db::models::MemoryGate::check(op, self.permission);
        if gate.is_allowed() {
            Ok(())
        } else {
            // Determine required permission based on operation
            let required = match op {
                pattern_db::models::MemoryOp::Read => {
                    pattern_db::models::MemoryPermission::ReadOnly
                }
                pattern_db::models::MemoryOp::Append => {
                    pattern_db::models::MemoryPermission::Append
                }
                pattern_db::models::MemoryOp::Overwrite => {
                    pattern_db::models::MemoryPermission::ReadWrite
                }
                pattern_db::models::MemoryOp::Delete => pattern_db::models::MemoryPermission::Admin,
            };
            Err(DocumentError::PermissionDenied {
                operation: format!("{:?}", op),
                required,
                actual: self.permission,
            })
        }
    }

    // ========== Text Operations ==========

    /// Get text content
    pub fn text_content(&self) -> String {
        let text = self.doc.get_text("content");
        text.to_string()
    }

    /// Set text content (replaces all).
    /// If is_system is false, checks that the document has Overwrite permission.
    pub fn set_text(&self, content: &str, is_system: bool) -> Result<(), DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Overwrite, is_system)?;

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

    /// Append text to existing content.
    /// If is_system is false, checks that the document has Append permission.
    pub fn append_text(&self, content: &str, is_system: bool) -> Result<(), DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Append, is_system)?;

        let text = self.doc.get_text("content");
        let pos = text.len_unicode();
        text.insert(pos, content)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Append content to the document based on schema type.
    /// - Text: appends as text
    /// - List: pushes item (parses content as JSON, or wraps as string)
    /// - Log: appends as log entry (parses content as JSON, or wraps as string)
    /// Returns error for Map/Composite schemas which don't support append.
    /// If is_system is false, checks that the document has Append permission.
    pub fn append(&self, content: &str, is_system: bool) -> Result<(), DocumentError> {
        match &self.schema {
            BlockSchema::Text => self.append_text(content, is_system),
            BlockSchema::List { .. } => {
                // Try to parse as JSON, fall back to string
                let item = serde_json::from_str(content)
                    .unwrap_or_else(|_| serde_json::Value::String(content.to_string()));
                self.push_item(item, is_system)
            }
            BlockSchema::Log { .. } => {
                // Try to parse as JSON, fall back to wrapping in a message object
                let entry = serde_json::from_str(content)
                    .unwrap_or_else(|_| serde_json::json!({ "message": content }));
                self.append_log_entry(entry, is_system)
            }
            _ => Err(DocumentError::InvalidSchemaForOperation {
                operation: "append".to_string(),
                schema: format!("{:?}", self.schema),
            }),
        }
    }

    /// Replace first occurrence of find with replace using Loro's native splice.
    /// Returns true if a replacement was made.
    /// If is_system is false, checks that the document has Overwrite permission.
    ///
    /// This uses surgical CRDT operations (splice) rather than rewriting the entire
    /// content, which provides better merge behavior and attribution tracking.
    pub fn replace_text(
        &self,
        find: &str,
        replace: &str,
        is_system: bool,
    ) -> Result<bool, DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Overwrite, is_system)?;

        let text = self.doc.get_text("content");
        let current = text.to_string();

        if let Some(byte_pos) = current.find(find) {
            // Convert byte positions to Unicode character positions using Loro's convert_pos
            // str::find() returns byte indices, but splice() needs Unicode scalar indices
            let unicode_pos = text
                .convert_pos(byte_pos, PosType::Bytes, PosType::Unicode)
                .ok_or_else(|| {
                    DocumentError::Other(format!("Invalid byte position: {}", byte_pos))
                })?;

            let find_byte_end = byte_pos + find.len();
            let unicode_end = text
                .convert_pos(find_byte_end, PosType::Bytes, PosType::Unicode)
                .ok_or_else(|| {
                    DocumentError::Other(format!("Invalid byte position: {}", find_byte_end))
                })?;
            let unicode_len = unicode_end - unicode_pos;

            // Surgical splice: delete unicode_len chars and insert replace
            text.splice(unicode_pos, unicode_len, replace)
                .map_err(|e| DocumentError::Other(format!("Splice failed: {}", e)))?;
            Ok(true)
        } else {
            Ok(false)
        }
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

    /// Set a field value in the map.
    /// If is_system is false and the field is read_only, returns ReadOnlyField error.
    pub fn set_field(
        &self,
        field: &str,
        value: JsonValue,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check read-only if not system
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(field) {
                return Err(DocumentError::ReadOnlyField(field.to_string()));
            }
        }

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

    /// Set a text field (convenience method).
    /// If is_system is false and the field is read_only, returns ReadOnlyField error.
    pub fn set_text_field(
        &self,
        field: &str,
        value: &str,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        self.set_field(field, JsonValue::String(value.to_string()), is_system)
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

    /// Append an item to a list field.
    /// If is_system is false and the field is read_only, returns ReadOnlyField error.
    pub fn append_to_list_field(
        &self,
        field: &str,
        item: JsonValue,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check read-only if not system
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(field) {
                return Err(DocumentError::ReadOnlyField(field.to_string()));
            }
        }

        let list = self.doc.get_list(format!("list_{field}"));
        let loro_value = json_to_loro(&item);
        list.push(loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Remove an item from a list field by index.
    /// If is_system is false and the field is read_only, returns ReadOnlyField error.
    pub fn remove_from_list_field(
        &self,
        field: &str,
        index: usize,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check read-only if not system
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(field) {
                return Err(DocumentError::ReadOnlyField(field.to_string()));
            }
        }

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

    /// Increment counter by delta, returns new value.
    /// If is_system is false and the field is read_only, returns ReadOnlyField error.
    pub fn increment_counter(
        &self,
        field: &str,
        delta: i64,
        is_system: bool,
    ) -> Result<i64, DocumentError> {
        // Check read-only if not system
        if !is_system {
            if let Some(true) = self.schema.is_field_read_only(field) {
                return Err(DocumentError::ReadOnlyField(field.to_string()));
            }
        }

        let counter = self.doc.get_counter(format!("counter_{field}"));
        counter
            .increment(delta as f64)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(counter.get_value() as i64)
    }

    // ========== Section Operations (for Composite schemas) ==========

    /// Set a field value in a specific section of a Composite schema.
    /// If is_system is false and the section is read-only, returns ReadOnlySection error.
    /// If is_system is false and the field is read-only, returns ReadOnlyField error.
    pub fn set_field_in_section(
        &self,
        field: &str,
        value: impl Into<JsonValue>,
        section: &str,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check section read-only permission
        if !is_system {
            if let Some(true) = self.schema.is_section_read_only(section) {
                return Err(DocumentError::ReadOnlySection(section.to_string()));
            }
        }

        // Get section schema and check field read-only permission
        let section_schema = self
            .schema
            .get_section_schema(section)
            .ok_or_else(|| DocumentError::FieldNotFound(section.to_string()))?;

        if !is_system {
            if let Some(true) = section_schema.is_field_read_only(field) {
                return Err(DocumentError::ReadOnlyField(field.to_string()));
            }
        }

        // Get the section's map container and set the field
        // Use namespaced container: section_{name}_fields
        let map = self.doc.get_map(format!("section_{section}_fields"));
        let loro_value = json_to_loro(&value.into());
        map.insert(field, loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;

        Ok(())
    }

    /// Set text content in a specific section of a Composite schema.
    /// If is_system is false and the section is read-only, returns ReadOnlySection error.
    pub fn set_text_in_section(
        &self,
        content: &str,
        section: &str,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        // Check section read-only permission
        if !is_system {
            if let Some(true) = self.schema.is_section_read_only(section) {
                return Err(DocumentError::ReadOnlySection(section.to_string()));
            }
        }

        // Verify section exists
        let _ = self
            .schema
            .get_section_schema(section)
            .ok_or_else(|| DocumentError::FieldNotFound(section.to_string()))?;

        // Get the section's text container and set content
        // Use namespaced container: section_{name}_content
        let text = self.doc.get_text(format!("section_{section}_content"));

        // Clear existing and insert new
        let len = text.len_unicode();
        if len > 0 {
            text.delete(0, len)
                .map_err(|e| DocumentError::Other(e.to_string()))?;
        }
        text.insert(0, content)
            .map_err(|e| DocumentError::Other(e.to_string()))?;

        Ok(())
    }

    /// Get a field value from a specific section of a Composite schema.
    pub fn get_field_in_section(&self, field: &str, section: &str) -> Option<JsonValue> {
        let map = self.doc.get_map(format!("section_{section}_fields"));
        map.get(field).and_then(|v| {
            if let Some(value) = v.as_value() {
                loro_to_json(value)
            } else {
                None
            }
        })
    }

    /// Get text content from a specific section of a Composite schema.
    pub fn get_text_in_section(&self, section: &str) -> String {
        let text = self.doc.get_text(format!("section_{section}_content"));
        text.to_string()
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

    /// Push an item to the end of the list.
    /// If is_system is false, checks that the document has Append permission.
    pub fn push_item(&self, item: JsonValue, is_system: bool) -> Result<(), DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Append, is_system)?;

        let list = self.doc.get_list("items");
        let loro_value = json_to_loro(&item);
        list.push(loro_value)
            .map_err(|e| DocumentError::Other(e.to_string()))?;
        Ok(())
    }

    /// Insert an item at a specific index.
    /// If is_system is false, checks that the document has Append permission.
    pub fn insert_item(
        &self,
        index: usize,
        item: JsonValue,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Append, is_system)?;

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

    /// Delete an item at a specific index.
    /// If is_system is false, checks that the document has Delete permission (Admin).
    pub fn delete_item(&self, index: usize, is_system: bool) -> Result<(), DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Delete, is_system)?;

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

    /// Append a log entry.
    /// If is_system is false, checks that the document has Append permission.
    pub fn append_log_entry(&self, entry: JsonValue, is_system: bool) -> Result<(), DocumentError> {
        self.check_permission(pattern_db::models::MemoryOp::Append, is_system)?;

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

    // ========== Subscriptions ==========

    /// Subscribe to all changes on this document.
    ///
    /// The callback will be invoked whenever changes are committed to the document.
    /// Returns a `Subscription` that will unsubscribe when dropped.
    ///
    /// # Example
    /// ```ignore
    /// use std::sync::Arc;
    /// let sub = doc.subscribe_root(Arc::new(|event| {
    ///     println!("Document changed: {:?}", event.triggered_by);
    /// }));
    /// ```
    pub fn subscribe_root(&self, callback: loro::event::Subscriber) -> loro::Subscription {
        self.doc.subscribe_root(callback)
    }

    /// Subscribe to changes on a specific container.
    ///
    /// # Arguments
    /// * `container_id` - The ID of the container to subscribe to
    /// * `callback` - The callback to invoke when changes occur
    ///
    /// Returns a `Subscription` that will unsubscribe when dropped.
    pub fn subscribe(
        &self,
        container_id: &ContainerID,
        callback: loro::event::Subscriber,
    ) -> loro::Subscription {
        self.doc.subscribe(container_id, callback)
    }

    /// Subscribe to the main content container based on schema type.
    ///
    /// This is a convenience method that selects the appropriate container
    /// based on the document's schema:
    /// - Text: subscribes to the "content" text container
    /// - Map: subscribes to the "fields" map container
    /// - List: subscribes to the "items" list container
    /// - Log: subscribes to the "entries" list container
    /// - Composite: subscribes to the "root" map container
    ///
    /// Returns a `Subscription` that will unsubscribe when dropped.
    pub fn subscribe_content(&self, callback: loro::event::Subscriber) -> loro::Subscription {
        let container_id = match &self.schema {
            BlockSchema::Text => self.doc.get_text("content").id(),
            BlockSchema::Map { .. } => self.doc.get_map("fields").id(),
            BlockSchema::List { .. } => self.doc.get_list("items").id(),
            BlockSchema::Log { .. } => self.doc.get_list("entries").id(),
            BlockSchema::Composite { .. } => self.doc.get_map("root").id(),
        };
        self.doc.subscribe(&container_id, callback)
    }

    /// Explicitly commit pending changes (triggers subscriptions).
    ///
    /// Changes made to containers (text, map, list, counter) are batched until
    /// commit is called. This triggers all subscriptions with the accumulated changes.
    pub fn commit(&self) {
        self.doc.commit();
    }

    /// Set attribution for the next commit.
    ///
    /// The attribution message will be included in the change metadata,
    /// allowing tracking of who or what made the change.
    pub fn set_attribution(&self, attribution: &str) {
        self.doc.set_next_commit_message(attribution);
    }

    /// Commit with an attribution message.
    ///
    /// Convenience method that sets the attribution and commits in one call.
    /// The attribution is stored in the change metadata for change tracking.
    pub fn commit_with_attribution(&self, attribution: &str) {
        self.doc.set_next_commit_message(attribution);
        self.doc.commit();
    }

    // ========== Rendering ==========

    /// Render document content for LLM context
    pub fn render(&self) -> String {
        self.render_schema(&self.schema)
    }

    /// Render a Composite schema's sections recursively
    fn render_composite(&self, sections: &[crate::memory::schema::CompositeSection]) -> String {
        let mut output = Vec::new();

        for section in sections {
            // Add read-only indicator to section header if applicable
            let read_only_marker = if section.read_only {
                " [read-only]"
            } else {
                ""
            };
            output.push(format!("=== {}{} ===", section.name, read_only_marker));
            let section_content = self.render_schema(&section.schema);
            if !section_content.is_empty() {
                output.push(section_content);
            }
        }

        output.join("\n\n")
    }

    /// Render content according to a specific schema (for recursive rendering)
    fn render_schema(&self, schema: &BlockSchema) -> String {
        match schema {
            BlockSchema::Text => self.text_content(),

            BlockSchema::Map { fields } => {
                let mut lines = Vec::new();
                for field_def in fields {
                    let field_name = &field_def.name;

                    // Mark read-only fields with indicator
                    let read_only_marker = if field_def.read_only {
                        " [read-only]"
                    } else {
                        ""
                    };

                    if field_def.field_type == FieldType::List {
                        // Render list fields as bullets
                        let items = self.get_list_field(field_name);
                        if !items.is_empty() {
                            lines.push(format!("{}{}:", field_name, read_only_marker));
                            for item in items {
                                lines.push(format!("- {}", json_display(&item)));
                            }
                        }
                    } else if field_def.field_type == FieldType::Counter {
                        // Render counter value
                        let value = self.get_counter(field_name);
                        lines.push(format!("{}{}: {}", field_name, read_only_marker, value));
                    } else {
                        // Regular field
                        if let Some(value) = self.get_field(field_name) {
                            lines.push(format!(
                                "{}{}: {}",
                                field_name,
                                read_only_marker,
                                json_display(&value)
                            ));
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
                            if done {
                                "[x]".to_string()
                            } else {
                                "[ ]".to_string()
                            }
                        } else {
                            format!("{}.", i + 1)
                        }
                    } else {
                        format!("{}.", i + 1)
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

            BlockSchema::Composite { sections } => self.render_composite(sections),
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
        doc.set_text("Hello, world!", true).unwrap();
        assert_eq!(doc.text_content(), "Hello, world!");
    }

    #[test]
    fn test_text_append() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Hello", true).unwrap();
        doc.append_text(", world!", true).unwrap();
        assert_eq!(doc.text_content(), "Hello, world!");
    }

    #[test]
    fn test_text_replace() {
        let doc = StructuredDocument::new_text();
        doc.set_text("Hello, world! Hello again!", true).unwrap();

        // Replace first occurrence
        let replaced = doc.replace_text("Hello", "Hi", true).unwrap();
        assert!(replaced);
        assert_eq!(doc.text_content(), "Hi, world! Hello again!");

        // No replacement needed
        let replaced = doc.replace_text("Goodbye", "Bye", true).unwrap();
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
                    read_only: false,
                },
                FieldDef {
                    name: "age".to_string(),
                    description: "Age field".to_string(),
                    field_type: FieldType::Number,
                    required: false,
                    default: Some(JsonValue::Number(0.into())),
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);

        // Set text field (is_system=true for test setup)
        doc.set_text_field("name", "Alice", true).unwrap();
        assert_eq!(doc.get_text_field("name"), Some("Alice".to_string()));

        // Set number field (is_system=true for test setup)
        doc.set_field("age", JsonValue::Number(30.into()), true)
            .unwrap();
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
                read_only: false,
            }],
        };

        let doc = StructuredDocument::new(schema);

        // Initial value is 0
        assert_eq!(doc.get_counter("score"), 0);

        // Increment (is_system=true for test setup)
        let new_val = doc.increment_counter("score", 5, true).unwrap();
        assert_eq!(new_val, 5);
        assert_eq!(doc.get_counter("score"), 5);

        // Decrement (is_system=true for test setup)
        let new_val = doc.increment_counter("score", -2, true).unwrap();
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
        doc.push_item(JsonValue::String("first".to_string()), true)
            .unwrap();
        doc.push_item(JsonValue::String("second".to_string()), true)
            .unwrap();
        assert_eq!(doc.list_len(), 2);

        // Insert at index
        doc.insert_item(1, JsonValue::String("middle".to_string()), true)
            .unwrap();
        assert_eq!(doc.list_len(), 3);

        let items = doc.list_items();
        assert_eq!(items[0], JsonValue::String("first".to_string()));
        assert_eq!(items[1], JsonValue::String("middle".to_string()));
        assert_eq!(items[2], JsonValue::String("second".to_string()));

        // Delete item
        doc.delete_item(1, true).unwrap();
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
                    read_only: false,
                }],
            },
        };

        let doc = StructuredDocument::new(schema);

        // Add entries
        doc.append_log_entry(
            serde_json::json!({
                "timestamp": "2025-01-01T00:00:00Z",
                "message": "First entry"
            }),
            true,
        )
        .unwrap();

        doc.append_log_entry(
            serde_json::json!({
                "timestamp": "2025-01-01T00:01:00Z",
                "message": "Second entry"
            }),
            true,
        )
        .unwrap();

        doc.append_log_entry(
            serde_json::json!({
                "timestamp": "2025-01-01T00:02:00Z",
                "message": "Third entry"
            }),
            true,
        )
        .unwrap();

        doc.append_log_entry(
            serde_json::json!({
                "timestamp": "2025-01-01T00:03:00Z",
                "message": "Fourth entry"
            }),
            true,
        )
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
        doc.set_text("Test content", true).unwrap();

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
                    read_only: false,
                },
                FieldDef {
                    name: "tags".to_string(),
                    description: "Tags".to_string(),
                    field_type: FieldType::List,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);
        doc.set_text_field("name", "Alice", true).unwrap();
        doc.append_to_list_field("tags", JsonValue::String("important".to_string()), true)
            .unwrap();
        doc.append_to_list_field("tags", JsonValue::String("urgent".to_string()), true)
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
                        read_only: false,
                    },
                    FieldDef {
                        name: "done".to_string(),
                        description: "Done".to_string(),
                        field_type: FieldType::Boolean,
                        required: true,
                        default: Some(JsonValue::Bool(false)),
                        read_only: false,
                    },
                ],
            })),
            max_items: None,
        };

        let doc = StructuredDocument::new(schema);

        doc.push_item(
            serde_json::json!({
                "title": "Task 1",
                "done": false
            }),
            true,
        )
        .unwrap();

        doc.push_item(
            serde_json::json!({
                "title": "Task 2",
                "done": true
            }),
            true,
        )
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
                read_only: false,
            }],
        };

        let doc = StructuredDocument::new(schema);

        // Add items to list field (is_system=true for test setup)
        doc.append_to_list_field("tags", JsonValue::String("tag1".to_string()), true)
            .unwrap();
        doc.append_to_list_field("tags", JsonValue::String("tag2".to_string()), true)
            .unwrap();
        doc.append_to_list_field("tags", JsonValue::String("tag3".to_string()), true)
            .unwrap();

        let tags = doc.get_list_field("tags");
        assert_eq!(tags.len(), 3);

        // Remove middle item (is_system=true for test setup)
        doc.remove_from_list_field("tags", 1, true).unwrap();
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

    #[test]
    fn test_document_error_read_only_variants() {
        let field_err = DocumentError::ReadOnlyField("status".to_string());
        let section_err = DocumentError::ReadOnlySection("diagnostics".to_string());

        let field_msg = format!("{}", field_err);
        let section_msg = format!("{}", section_err);

        assert!(field_msg.contains("status"));
        assert!(field_msg.contains("read-only"));
        assert!(section_msg.contains("diagnostics"));
        assert!(section_msg.contains("read-only"));
    }

    #[test]
    fn test_structured_document_section_operations() {
        use crate::memory::schema::CompositeSection;

        let schema = BlockSchema::Composite {
            sections: vec![
                CompositeSection {
                    name: "diagnostics".to_string(),
                    schema: Box::new(BlockSchema::Map {
                        fields: vec![FieldDef {
                            name: "error_count".to_string(),
                            description: "Error count".to_string(),
                            field_type: FieldType::Counter,
                            required: true,
                            default: Some(serde_json::json!(0)),
                            read_only: false,
                        }],
                    }),
                    description: None,
                    read_only: true, // Section is read-only
                },
                CompositeSection {
                    name: "notes".to_string(),
                    schema: Box::new(BlockSchema::Text),
                    description: None,
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);

        // System can write to read-only section
        assert!(
            doc.set_field_in_section("error_count", 5, "diagnostics", true)
                .is_ok()
        );

        // Agent cannot write to read-only section
        let result = doc.set_field_in_section("error_count", 10, "diagnostics", false);
        assert!(matches!(result, Err(DocumentError::ReadOnlySection(_))));

        // Agent can write to writable section
        assert!(doc.set_text_in_section("my notes", "notes", false).is_ok());

        // Verify text was stored correctly
        assert_eq!(doc.get_text_in_section("notes"), "my notes");
    }

    #[test]
    fn test_section_field_level_read_only() {
        use crate::memory::schema::CompositeSection;

        let schema = BlockSchema::Composite {
            sections: vec![CompositeSection {
                name: "config".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![
                        FieldDef {
                            name: "version".to_string(),
                            description: "Config version".to_string(),
                            field_type: FieldType::Text,
                            required: true,
                            default: None,
                            read_only: true, // Field is read-only
                        },
                        FieldDef {
                            name: "setting".to_string(),
                            description: "User setting".to_string(),
                            field_type: FieldType::Text,
                            required: false,
                            default: None,
                            read_only: false,
                        },
                    ],
                }),
                description: None,
                read_only: false, // Section is NOT read-only
            }],
        };

        let doc = StructuredDocument::new(schema);

        // Agent can write to writable field in writable section
        assert!(
            doc.set_field_in_section("setting", "value", "config", false)
                .is_ok()
        );
        assert_eq!(
            doc.get_field_in_section("setting", "config"),
            Some(JsonValue::String("value".to_string()))
        );

        // Agent cannot write to read-only field (even in writable section)
        let result = doc.set_field_in_section("version", "1.0", "config", false);
        assert!(matches!(result, Err(DocumentError::ReadOnlyField(_))));

        // System can write to read-only field
        assert!(
            doc.set_field_in_section("version", "2.0", "config", true)
                .is_ok()
        );
        assert_eq!(
            doc.get_field_in_section("version", "config"),
            Some(JsonValue::String("2.0".to_string()))
        );
    }

    #[test]
    fn test_section_not_found() {
        use crate::memory::schema::CompositeSection;

        let schema = BlockSchema::Composite {
            sections: vec![CompositeSection {
                name: "existing".to_string(),
                schema: Box::new(BlockSchema::Text),
                description: None,
                read_only: false,
            }],
        };

        let doc = StructuredDocument::new(schema);

        // Trying to write to non-existent section returns FieldNotFound
        let result = doc.set_text_in_section("content", "nonexistent", false);
        assert!(matches!(result, Err(DocumentError::FieldNotFound(_))));

        let result = doc.set_field_in_section("field", "value", "nonexistent", false);
        assert!(matches!(result, Err(DocumentError::FieldNotFound(_))));
    }

    #[test]
    fn test_structured_document_field_permission_check() {
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "readonly_field".to_string(),
                    description: "Read-only".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: true,
                },
                FieldDef {
                    name: "writable_field".to_string(),
                    description: "Writable".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);

        // Agent (is_system=false) can write to writable field
        assert!(
            doc.set_field(
                "writable_field",
                JsonValue::String("value".to_string()),
                false
            )
            .is_ok()
        );

        // Agent cannot write to read-only field
        let result = doc.set_field(
            "readonly_field",
            JsonValue::String("value".to_string()),
            false,
        );
        assert!(matches!(result, Err(DocumentError::ReadOnlyField(_))));

        // System (is_system=true) can write to read-only field
        assert!(
            doc.set_field(
                "readonly_field",
                JsonValue::String("system_value".to_string()),
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn test_structured_document_identity() {
        let schema = BlockSchema::Text;
        let doc = StructuredDocument::new_with_identity(
            schema,
            "my_block".to_string(),
            Some("agent_123".to_string()),
        );

        assert_eq!(doc.label(), "my_block");
        assert_eq!(doc.accessor_agent_id(), Some("agent_123"));
    }

    #[test]
    fn test_render_read_only_indicators() {
        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "status".to_string(),
                    description: "Status".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: true,
                },
                FieldDef {
                    name: "notes".to_string(),
                    description: "Notes".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);
        doc.set_field("status", JsonValue::String("active".to_string()), true)
            .unwrap();
        doc.set_field("notes", JsonValue::String("some notes".to_string()), true)
            .unwrap();

        let rendered = doc.render();

        // Read-only field should have indicator
        assert!(
            rendered.contains("status [read-only]: active"),
            "Expected 'status [read-only]: active' in rendered output:\n{}",
            rendered
        );
        // Writable field should not have indicator
        assert!(
            rendered.contains("notes: some notes"),
            "Expected 'notes: some notes' in rendered output:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("notes [read-only]"),
            "Should not contain 'notes [read-only]' in rendered output:\n{}",
            rendered
        );
    }

    #[test]
    fn test_render_composite_read_only_section_indicator() {
        use crate::memory::schema::CompositeSection;

        let schema = BlockSchema::Composite {
            sections: vec![
                CompositeSection {
                    name: "diagnostics".to_string(),
                    schema: Box::new(BlockSchema::Text),
                    description: None,
                    read_only: true,
                },
                CompositeSection {
                    name: "notes".to_string(),
                    schema: Box::new(BlockSchema::Text),
                    description: None,
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema);
        doc.set_text_in_section("errors here", "diagnostics", true)
            .unwrap();
        doc.set_text_in_section("user notes", "notes", true)
            .unwrap();

        let rendered = doc.render();

        // Read-only section should have indicator in header
        assert!(
            rendered.contains("=== diagnostics [read-only] ==="),
            "Expected '=== diagnostics [read-only] ===' in rendered output:\n{}",
            rendered
        );
        // Writable section should not have indicator
        assert!(
            rendered.contains("=== notes ==="),
            "Expected '=== notes ===' in rendered output:\n{}",
            rendered
        );
        assert!(
            !rendered.contains("notes [read-only]"),
            "Should not contain 'notes [read-only]' in rendered output:\n{}",
            rendered
        );
    }

    #[test]
    fn test_structured_document_subscription() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let schema = BlockSchema::Map {
            fields: vec![FieldDef {
                name: "counter".to_string(),
                description: "A counter".to_string(),
                field_type: FieldType::Counter,
                required: true,
                default: Some(serde_json::json!(0)),
                read_only: false,
            }],
        };

        let doc = StructuredDocument::new(schema);

        let changed = Arc::new(AtomicBool::new(false));
        let changed_clone = changed.clone();

        let _sub = doc.subscribe_root(Arc::new(move |_event| {
            changed_clone.store(true, Ordering::SeqCst);
        }));

        // Make a change and commit
        doc.increment_counter("counter", 1, true).unwrap();
        doc.commit();

        // Subscription should have fired
        assert!(changed.load(Ordering::SeqCst));
    }
}
