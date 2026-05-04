//! Loro document operations for structured memory blocks

use loro::{
    ContainerID, ContainerTrait, ExportMode, LoroDoc, LoroValue, VersionVector, cursor::PosType,
};
use serde_json::Value as JsonValue;

use crate::types::memory_types::{
    BlockMetadata, BlockSchema, CompositeSection, DocumentError, FieldType, LogEntrySchema,
    MemoryBlockType,
};

/// Wrapper around LoroDoc for schema-aware operations.
///
/// This struct combines the Loro CRDT document with block metadata,
/// providing a unified interface for document operations and metadata access.
#[derive(Clone, Debug)]
pub struct StructuredDocument {
    /// The underlying Loro CRDT document.
    doc: LoroDoc,

    /// Agent accessing this document (for attribution). May differ from
    /// the owning agent for shared blocks.
    accessor_agent_id: Option<String>,

    /// Block metadata including schema, permissions, and identity.
    metadata: BlockMetadata,
}

impl StructuredDocument {
    /// Create a new document with full metadata.
    ///
    /// This is the preferred constructor when loading from the database.
    pub fn new_with_metadata(metadata: BlockMetadata, accessor_agent_id: Option<String>) -> Self {
        Self {
            doc: LoroDoc::new(),
            accessor_agent_id,
            metadata,
        }
    }

    /// Create a new document from a Loro snapshot with full metadata.
    ///
    /// This is the preferred constructor when loading from the database.
    pub fn from_snapshot_with_metadata(
        snapshot: &[u8],
        metadata: BlockMetadata,
        accessor_agent_id: Option<String>,
    ) -> Result<Self, DocumentError> {
        let doc = LoroDoc::new();
        doc.import(snapshot)
            .map_err(|e| DocumentError::ImportFailed(e.to_string()))?;
        Ok(Self {
            doc,
            accessor_agent_id,
            metadata,
        })
    }

    /// Create a new document from an existing LoroDoc with full metadata.
    ///
    /// Used when reconstructing a document from checkpoint + updates for undo.
    pub fn from_doc_with_metadata(
        doc: LoroDoc,
        metadata: BlockMetadata,
        schema: BlockSchema,
    ) -> Result<Self, DocumentError> {
        let mut metadata = metadata;
        metadata.schema = schema;
        Ok(Self {
            doc,
            accessor_agent_id: None,
            metadata,
        })
    }

    /// Create a new document with minimal metadata (for testing/standalone use).
    pub fn new(schema: BlockSchema) -> Self {
        Self::new_with_metadata(BlockMetadata::standalone(schema), None)
    }

    /// Create with default Text schema.
    pub fn new_text() -> Self {
        Self::new(BlockSchema::text())
    }

    /// Create a new document with identity information (for testing).
    #[deprecated(note = "Use new_with_metadata instead")]
    pub fn new_with_identity(
        schema: BlockSchema,
        label: String,
        accessor_agent_id: Option<String>,
    ) -> Self {
        let mut metadata = BlockMetadata::standalone(schema);
        metadata.label = label;
        Self::new_with_metadata(metadata, accessor_agent_id)
    }

    /// Create a new document with the given schema and permission (for testing).
    #[deprecated(note = "Use new_with_metadata instead")]
    pub fn new_with_permission(
        schema: BlockSchema,
        permission: crate::types::memory_types::MemoryPermission,
    ) -> Self {
        let mut metadata = BlockMetadata::standalone(schema);
        metadata.permission = permission;
        Self::new_with_metadata(metadata, None)
    }

    /// Create from an existing Loro snapshot with permission (for testing).
    #[deprecated(note = "Use from_snapshot_with_metadata instead")]
    pub fn from_snapshot_with_permission(
        snapshot: &[u8],
        schema: BlockSchema,
        permission: crate::types::memory_types::MemoryPermission,
    ) -> Result<Self, DocumentError> {
        let mut metadata = BlockMetadata::standalone(schema);
        metadata.permission = permission;
        Self::from_snapshot_with_metadata(snapshot, metadata, None)
    }

    /// Create from an existing Loro snapshot (default ReadWrite permission).
    #[deprecated(note = "Use from_snapshot_with_metadata instead")]
    pub fn from_snapshot(snapshot: &[u8], schema: BlockSchema) -> Result<Self, DocumentError> {
        Self::from_snapshot_with_metadata(snapshot, BlockMetadata::standalone(schema), None)
    }

    /// Apply updates to the document.
    ///
    /// Accepts any byte slice produced by `LoroDoc::export_snapshot()` or
    /// `LoroDoc::export(ExportMode::updates(...))`. Used by the lightweight
    /// fork `merge_back` path.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::memory::StructuredDocument;
    ///
    /// let source = StructuredDocument::new_text();
    /// source.set_text("hello", true).unwrap();
    /// let snapshot = source.export_snapshot().unwrap();
    ///
    /// let target = StructuredDocument::new_text();
    /// target.apply_updates(&snapshot).unwrap();
    /// assert_eq!(target.text_content(), "hello");
    /// ```
    pub fn apply_updates(&self, updates: &[u8]) -> Result<(), DocumentError> {
        self.doc
            .import(updates)
            .map_err(|e| DocumentError::ImportFailed(e.to_string()))?;
        Ok(())
    }

    // ========== Fork / isolation helpers ==========

    /// Fork the underlying `LoroDoc`, returning a new `StructuredDocument` whose
    /// CRDT state diverges from the parent after this point.
    ///
    /// The forked document inherits all committed ops from the source at fork
    /// time. Subsequent writes on either side do not propagate until an explicit
    /// `apply_updates` call imports the snapshot. Metadata fields (label, schema,
    /// permissions) are cloned verbatim; use [`retag_owner`](Self::retag_owner)
    /// to rewrite ownership on the child.
    pub fn fork(&self) -> Self {
        let forked_doc = self.doc.fork();
        Self::from_forked_doc(forked_doc, self.metadata_snapshot())
    }

    /// Construct a `StructuredDocument` from a pre-forked `LoroDoc` and a
    /// metadata snapshot.
    ///
    /// The `accessor_agent_id` is left blank on the forked copy; the caller
    /// may set it after construction if attribution is required.
    pub fn from_forked_doc(doc: LoroDoc, metadata: BlockMetadata) -> Self {
        Self {
            doc,
            accessor_agent_id: None,
            metadata,
        }
    }

    /// Clone the current block metadata.
    ///
    /// Used by [`fork`](Self::fork) to carry metadata into the child without
    /// holding a borrow across the `LoroDoc::fork()` call.
    pub fn metadata_snapshot(&self) -> BlockMetadata {
        self.metadata.clone()
    }

    /// Rewrite the owning agent recorded in the embedded metadata.
    ///
    /// Called by `MemoryCache::fork_for_child` after forking each block to
    /// attribute the forked copy to the child agent rather than the parent.
    pub fn retag_owner(&mut self, new_owner: &str) {
        self.metadata.agent_id = new_owner.to_string();
    }

    /// Override the Loro peer ID used to author new operations on this document.
    ///
    /// Normally the Loro runtime assigns a random peer ID. This method allows
    /// callers to set a deterministic value — useful for reproducible test
    /// fixtures, migration tools, and scenarios where you need consistent
    /// vector-clock ordering across multiple documents.
    ///
    /// Returns an error if the document already has uncommitted ops with a
    /// different peer (see `loro::LoroDoc::set_peer_id`).
    pub fn set_peer_id(&self, peer_id: u64) -> Result<(), DocumentError> {
        self.doc
            .set_peer_id(peer_id)
            .map_err(|e| DocumentError::Other(e.to_string()))
    }

    // ========== Metadata Accessors ==========

    /// Get the full block metadata.
    pub fn metadata(&self) -> &BlockMetadata {
        &self.metadata
    }

    /// Get a mutable reference to the block metadata.
    pub fn metadata_mut(&mut self) -> &mut BlockMetadata {
        &mut self.metadata
    }

    /// Get the schema.
    pub fn schema(&self) -> &BlockSchema {
        &self.metadata.schema
    }

    /// Get the effective permission for this document.
    pub fn permission(&self) -> crate::types::memory_types::MemoryPermission {
        self.metadata.permission
    }

    /// Set the effective permission for this document (DB is source of truth).
    pub fn set_permission(&mut self, permission: crate::types::memory_types::MemoryPermission) {
        self.metadata.permission = permission;
    }

    /// Update the schema settings (DB is source of truth).
    ///
    /// This is used to update schema properties like viewport (Text) or display_limit (Log).
    /// The caller must ensure the schema variant is compatible.
    pub fn set_schema(&mut self, schema: BlockSchema) {
        self.metadata.schema = schema;
    }

    /// Get the block label for identification.
    pub fn label(&self) -> &str {
        &self.metadata.label
    }

    /// Get the agent that loaded this document (for attribution).
    pub fn accessor_agent_id(&self) -> Option<&str> {
        self.accessor_agent_id.as_deref()
    }

    /// Get the block ID.
    pub fn id(&self) -> &str {
        &self.metadata.id
    }

    /// Get the owning agent ID.
    pub fn agent_id(&self) -> &str {
        &self.metadata.agent_id
    }

    /// Get the block description.
    pub fn description(&self) -> &str {
        &self.metadata.description
    }

    /// Get the block type.
    pub fn block_type(&self) -> MemoryBlockType {
        self.metadata.block_type
    }

    /// Get the character limit.
    pub fn char_limit(&self) -> usize {
        self.metadata.char_limit
    }

    /// Check if the block is pinned.
    pub fn is_pinned(&self) -> bool {
        self.metadata.pinned
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
        op: crate::types::memory_types::MemoryOp,
        is_system: bool,
    ) -> Result<(), DocumentError> {
        if is_system {
            return Ok(());
        }

        let gate = crate::types::memory_types::MemoryGate::check(op, self.metadata.permission);
        if gate.is_allowed() {
            Ok(())
        } else {
            // Determine required permission based on operation
            let required = match op {
                crate::types::memory_types::MemoryOp::Read => {
                    crate::types::memory_types::MemoryPermission::ReadOnly
                }
                crate::types::memory_types::MemoryOp::Append => {
                    crate::types::memory_types::MemoryPermission::Append
                }
                crate::types::memory_types::MemoryOp::Overwrite => {
                    crate::types::memory_types::MemoryPermission::ReadWrite
                }
                crate::types::memory_types::MemoryOp::Delete => {
                    crate::types::memory_types::MemoryPermission::Admin
                }
            };
            Err(DocumentError::PermissionDenied {
                operation: format!("{:?}", op),
                required,
                actual: self.metadata.permission,
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
        self.check_permission(crate::types::memory_types::MemoryOp::Overwrite, is_system)?;

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
        self.check_permission(crate::types::memory_types::MemoryOp::Append, is_system)?;

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
        match &self.metadata.schema {
            BlockSchema::Text { .. } => self.append_text(content, is_system),
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
                schema: format!("{:?}", self.metadata.schema),
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
        self.check_permission(crate::types::memory_types::MemoryOp::Overwrite, is_system)?;

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
        if !is_system && let Some(true) = self.metadata.schema.is_field_read_only(field) {
            return Err(DocumentError::ReadOnlyField(field.to_string()));
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
        if !is_system && let Some(true) = self.metadata.schema.is_field_read_only(field) {
            return Err(DocumentError::ReadOnlyField(field.to_string()));
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
        if !is_system && let Some(true) = self.metadata.schema.is_field_read_only(field) {
            return Err(DocumentError::ReadOnlyField(field.to_string()));
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
        if !is_system && let Some(true) = self.metadata.schema.is_field_read_only(field) {
            return Err(DocumentError::ReadOnlyField(field.to_string()));
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
        if !is_system && let Some(true) = self.metadata.schema.is_section_read_only(section) {
            return Err(DocumentError::ReadOnlySection(section.to_string()));
        }

        // Get section schema and check field read-only permission
        let section_schema = self
            .metadata
            .schema
            .get_section_schema(section)
            .ok_or_else(|| DocumentError::FieldNotFound(section.to_string()))?;

        if !is_system && let Some(true) = section_schema.is_field_read_only(field) {
            return Err(DocumentError::ReadOnlyField(field.to_string()));
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
        if !is_system && let Some(true) = self.metadata.schema.is_section_read_only(section) {
            return Err(DocumentError::ReadOnlySection(section.to_string()));
        }

        // Verify section exists
        let _ = self
            .metadata
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
        self.check_permission(crate::types::memory_types::MemoryOp::Append, is_system)?;

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
        self.check_permission(crate::types::memory_types::MemoryOp::Append, is_system)?;

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
        self.check_permission(crate::types::memory_types::MemoryOp::Delete, is_system)?;

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
        let display_limit = limit.or({
            if let BlockSchema::Log { display_limit, .. } = &self.metadata.schema {
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
        self.check_permission(crate::types::memory_types::MemoryOp::Append, is_system)?;

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

    /// Export the document state as JSON.
    ///
    /// Returns the entire Loro document state as a JSON value, which includes
    /// all containers (text, map, list, etc.) and their contents.
    pub fn export_as_json(&self) -> Option<JsonValue> {
        let deep_value = self.doc.get_deep_value();
        loro_to_json(&deep_value)
    }

    /// Import content from a JSON value based on schema.
    ///
    /// For Text schema: expects a string value (or object with "content" key)
    /// For Map schema: expects an object with field values
    /// For List schema: expects an array (or object with "items" key)
    /// For Log schema: expects an array of entries (or object with "entries" key)
    /// For Composite: expects an object with section keys
    pub fn import_from_json(&self, value: &JsonValue) -> Result<(), DocumentError> {
        match &self.metadata.schema {
            BlockSchema::Text { .. } => {
                // Text: expect string or object with content field
                let text = if let Some(s) = value.as_str() {
                    s.to_string()
                } else if let Some(content) = value.get("content").and_then(|v| v.as_str()) {
                    content.to_string()
                } else {
                    return Err(DocumentError::Other(
                        "Text schema expects string or object with 'content' field".to_string(),
                    ));
                };
                self.set_text(&text, true)?;
            }
            BlockSchema::Map { fields } => {
                // Map: expect object with field values
                let obj = value.as_object().ok_or_else(|| {
                    DocumentError::Other("Map schema expects JSON object".to_string())
                })?;

                // Get the "fields" sub-object if present, otherwise use root
                let fields_obj = obj.get("fields").and_then(|v| v.as_object()).unwrap_or(obj);

                for field_def in fields {
                    if let Some(field_value) = fields_obj.get(&field_def.name) {
                        self.set_field(&field_def.name, field_value.clone(), true)?;
                    }
                }
            }
            BlockSchema::List { .. } => {
                // List: expect array or object with items
                let items = if let Some(arr) = value.as_array() {
                    arr.clone()
                } else if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
                    items.clone()
                } else {
                    return Err(DocumentError::Other(
                        "List schema expects array or object with 'items' field".to_string(),
                    ));
                };

                // Clear existing items and add new ones
                let list = self.doc.get_list("items");
                // Delete from end to start to avoid index shifting
                for i in (0..list.len()).rev() {
                    let _ = list.delete(i, 1);
                }
                // Insert new items
                for item in items {
                    let loro_value = json_to_loro(&item);
                    let _ = list.push(loro_value);
                }
            }
            BlockSchema::Log { .. } => {
                // Log: typically append-only, but for import we can set entries
                let entries = if let Some(arr) = value.as_array() {
                    arr.clone()
                } else if let Some(items) = value.get("entries").and_then(|v| v.as_array()) {
                    items.clone()
                } else if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
                    items.clone()
                } else {
                    return Err(DocumentError::Other(
                        "Log schema expects array or object with 'entries' field".to_string(),
                    ));
                };

                // Clear and re-add
                let list = self.doc.get_list("items");
                for i in (0..list.len()).rev() {
                    let _ = list.delete(i, 1);
                }
                for entry in entries {
                    self.append_log_entry(entry, true)?;
                }
            }
            BlockSchema::TaskList { .. } => {
                // TaskList: expect array or object with "items" field.
                let items = if let Some(arr) = value.as_array() {
                    arr.clone()
                } else if let Some(items) = value.get("items").and_then(|v| v.as_array()) {
                    items.clone()
                } else {
                    return Err(DocumentError::Other(
                        "TaskList schema expects array or object with 'items' field".to_string(),
                    ));
                };

                // Clear the existing movable list and re-insert each item as
                // a nested LoroMap CONTAINER (not a value-map snapshot). This
                // preserves field-level CRDT merge semantics on subsequent
                // in-place mutations. `comments` and `blocks` nested lists
                // are likewise stored as LoroList containers for correct
                // multi-writer append semantics.
                let list = self.doc.get_movable_list("items");
                for i in (0..list.len()).rev() {
                    let _ = list.delete(i, 1);
                }
                for item in items {
                    let Some(obj) = item.as_object() else {
                        return Err(DocumentError::Other(format!(
                            "TaskList item must be a JSON object, got: {item}"
                        )));
                    };
                    let item_map = list
                        .push_container(loro::LoroMap::new())
                        .map_err(|e| DocumentError::Other(e.to_string()))?;
                    for (key, value) in obj {
                        match (key.as_str(), value) {
                            ("comments" | "blocks", JsonValue::Array(arr)) => {
                                let nested = item_map
                                    .insert_container(key, loro::LoroList::new())
                                    .map_err(|e| DocumentError::Other(e.to_string()))?;
                                for elem in arr {
                                    nested
                                        .push(json_to_loro(elem))
                                        .map_err(|e| DocumentError::Other(e.to_string()))?;
                                }
                            }
                            _ => {
                                item_map
                                    .insert(key, json_to_loro(value))
                                    .map_err(|e| DocumentError::Other(e.to_string()))?;
                            }
                        }
                    }
                }
            }
            BlockSchema::Composite { sections } => {
                // Composite: expect object with section keys
                let obj = value.as_object().ok_or_else(|| {
                    DocumentError::Other("Composite schema expects JSON object".to_string())
                })?;

                for section in sections {
                    if let Some(section_obj) = obj.get(&section.name).and_then(|v| v.as_object()) {
                        // Handle section content (text)
                        if let Some(content) = section_obj.get("content").and_then(|v| v.as_str()) {
                            self.set_text_in_section(&section.name, content, true)?;
                        }
                        // Handle section fields
                        if let Some(fields) = section_obj.get("fields").and_then(|v| v.as_object())
                        {
                            for (field_name, field_value) in fields {
                                self.set_field_in_section(
                                    field_name,
                                    field_value.clone(),
                                    &section.name,
                                    true,
                                )?;
                            }
                        }
                    }
                }
            }
            BlockSchema::Skill { .. } => {
                // Skill: treat the body as text content, mirroring the Text arm.
                //
                // The YAML frontmatter lives in the "metadata" LoroMap and is
                // populated by `markdown_skill::write_skill_to_loro_doc` on the
                // external-edit inbound path. `import_from_json` handles only
                // the bare body string (for programmatic creation/seeding of a
                // body without metadata); it intentionally does NOT replicate
                // the loro_bridge logic.
                //
                // Accepted input shapes:
                //   - `Value::String(body)` — the body string directly.
                //   - `Value::Object` with exactly one `"body"` key — an
                //     object that contains ONLY the body.
                //
                // Rejected: any object with keys beyond `"body"`. Callers that
                // need to write metadata must use `write_skill_to_loro_doc`.
                let text = if let Some(s) = value.as_str() {
                    s.to_string()
                } else if let Some(obj) = value.as_object() {
                    // Check for stray keys — any key other than "body" means
                    // the caller is trying to write structured metadata through
                    // the wrong API path.
                    let extra_keys: Vec<&str> = obj
                        .keys()
                        .filter(|k| *k != "body")
                        .map(|k| k.as_str())
                        .collect();
                    if !extra_keys.is_empty() {
                        return Err(DocumentError::Other(format!(
                            "Skill blocks with structured metadata must use \
                             write_skill_to_loro_doc; import_from_json only accepts \
                             bare body strings. Got keys: {{{}}}",
                            extra_keys.join(", ")
                        )));
                    }
                    obj.get("body")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            DocumentError::Other(
                                "Skill schema object must contain a string 'body' field"
                                    .to_string(),
                            )
                        })?
                        .to_string()
                } else {
                    return Err(DocumentError::Other(
                        "Skill schema expects a string body or an object with a single 'body' field"
                            .to_string(),
                    ));
                };
                let body_text = self.doc.get_text("body");
                body_text
                    .update(&text, Default::default())
                    .map_err(|e| DocumentError::Other(e.to_string()))?;
            }
        }
        Ok(())
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
        let container_id = match &self.metadata.schema {
            BlockSchema::Text { .. } => self.doc.get_text("content").id(),
            BlockSchema::Map { .. } => self.doc.get_map("fields").id(),
            BlockSchema::List { .. } => self.doc.get_list("items").id(),
            BlockSchema::Log { .. } => self.doc.get_list("entries").id(),
            BlockSchema::Composite { .. } => self.doc.get_map("root").id(),
            BlockSchema::TaskList { .. } => self.doc.get_movable_list("items").id(),
            // Skill blocks store the markdown body in a LoroText container named
            // "body", mirroring the Text variant's "content" container convention.
            BlockSchema::Skill { .. } => self.doc.get_text("body").id(),
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
        self.render_schema(&self.metadata.schema)
    }

    /// Render a Composite schema's sections recursively
    fn render_composite(&self, sections: &[CompositeSection]) -> String {
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
            BlockSchema::Text { viewport } => {
                let content = self.text_content();
                match viewport {
                    Some(vp) => {
                        // Apply viewport: show only a window of lines
                        let lines: Vec<&str> = content.lines().collect();
                        let total_lines = lines.len();

                        // start_line is 1-indexed, convert to 0-indexed
                        let start_idx = vp.start_line.saturating_sub(1);
                        let end_idx = (start_idx + vp.display_lines).min(total_lines);

                        if start_idx >= total_lines {
                            // Viewport is past end of content
                            format!(
                                "[Viewport: lines {}-{} of {} (past end of content)]",
                                vp.start_line,
                                vp.start_line + vp.display_lines - 1,
                                total_lines
                            )
                        } else {
                            let visible: Vec<&str> = lines[start_idx..end_idx].to_vec();
                            let header = format!(
                                "[Showing lines {}-{} of {}]\n",
                                start_idx + 1,
                                end_idx,
                                total_lines
                            );
                            header + &visible.join("\n")
                        }
                    }
                    None => content,
                }
            }

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

            BlockSchema::TaskList {
                display_limit,
                default_status,
                default_owner,
            } => {
                let items_list = self.doc.get_movable_list("items");
                let total = items_list.len();
                let shown = display_limit.map(|lim| lim.min(total)).unwrap_or(total);
                let mut out = String::new();
                out.push_str(&format!("TaskList ({total} items"));
                if shown < total {
                    out.push_str(&format!(", showing {shown}"));
                }
                if let Some(s) = default_status {
                    out.push_str(&format!("; default_status={s:?}"));
                }
                if let Some(o) = default_owner {
                    out.push_str(&format!("; default_owner=@{o}"));
                }
                out.push_str(")\n");

                // Use get_deep_value() to get fully-resolved LoroValues
                // (LoroMovableList::get returns ValueOrContainer, not LoroValue).
                let deep = items_list.get_deep_value();
                let all_items = match &deep {
                    LoroValue::List(l) => l.as_ref(),
                    _ => &[] as &[LoroValue],
                };

                for value in all_items.iter().take(shown) {
                    if let LoroValue::Map(map) = value {
                        let id = map
                            .get("id")
                            .and_then(|v| match v {
                                LoroValue::String(s) => Some(s.as_ref()),
                                _ => None,
                            })
                            .unwrap_or("?");
                        let subject = map
                            .get("subject")
                            .and_then(|v| match v {
                                LoroValue::String(s) => Some(s.as_ref()),
                                _ => None,
                            })
                            .unwrap_or("");
                        let status = map
                            .get("status")
                            .and_then(|v| match v {
                                LoroValue::String(s) => Some(s.to_string()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let owner = map.get("owner").and_then(|v| match v {
                            LoroValue::String(s) => Some(s.to_string()),
                            _ => None,
                        });
                        let active_form = map.get("active_form").and_then(|v| match v {
                            LoroValue::String(s) => Some(s.to_string()),
                            _ => None,
                        });
                        let description = map
                            .get("description")
                            .and_then(|v| match v {
                                LoroValue::String(s) => Some(s.to_string()),
                                _ => None,
                            })
                            .unwrap_or_default();

                        // Build the item line.
                        // subject and active_form are rendered with Debug-format
                        // quoting ({:?}) so that values containing `"` are escaped
                        // rather than producing malformed output.
                        let mut line = format!("- id={id} subject={subject:?} status={status}");
                        if let Some(ref o) = owner {
                            // AgentId values are stored without a leading `@`
                            // (the convention in pattern_core types/ids.rs).
                            // We prepend it here only at render time. Strip any
                            // pre-existing `@` first so we never emit `@@agent`.
                            let bare = o.trim_start_matches('@');
                            line.push_str(&format!(" owner=@{bare}"));
                        }
                        if let Some(ref af) = active_form {
                            line.push_str(&format!(" active_form={af:?}"));
                        }
                        out.push_str(&line);
                        out.push('\n');

                        // Blocks.
                        if let Some(LoroValue::List(blocks)) = map.get("blocks")
                            && !blocks.is_empty()
                        {
                            let block_strs: Vec<String> = blocks
                                .iter()
                                .filter_map(|b| match b {
                                    LoroValue::Map(m) => {
                                        let handle = m.get("block").and_then(|v| match v {
                                            LoroValue::String(s) => Some(s.to_string()),
                                            _ => None,
                                        })?;
                                        let item_id = m.get("task_item").and_then(|v| match v {
                                            LoroValue::String(s) => Some(s.to_string()),
                                            _ => None,
                                        });
                                        Some(match item_id {
                                            Some(id) => {
                                                format!("(block)\"{handle}#{id}\"")
                                            }
                                            None => format!("(block)\"{handle}\""),
                                        })
                                    }
                                    _ => None,
                                })
                                .collect();
                            if !block_strs.is_empty() {
                                out.push_str(&format!("    blocks: {}\n", block_strs.join(", ")));
                            }
                        }

                        // Description excerpt (first line or ~80 chars).
                        if !description.is_empty() {
                            let excerpt = description
                                .lines()
                                .next()
                                .unwrap_or(&description)
                                .chars()
                                .take(80)
                                .collect::<String>();
                            out.push_str(&format!("    description: {excerpt}\n"));
                        }
                    } else {
                        // Non-map item — render as debug.
                        out.push_str(&format!("- {value:?}\n"));
                    }
                }

                if let Some(lim) = display_limit
                    && shown < total
                {
                    out.push_str(&format!(
                        "\n... {} more items not shown (display_limit={})\n",
                        total - shown,
                        lim,
                    ));
                }

                out
            }

            BlockSchema::Skill { .. } => {
                // Render name + description + keywords + body into a single
                // preview string so all fields are covered by the FTS5 index.
                // The FTS5 `content_preview` column is updated from the return
                // value of this function via `update_block_preview`.
                //
                // Metadata lives in a `"metadata"` LoroMap whose scalar fields
                // are stored as plain LoroValue strings. We project them here
                // with a minimal inline projection rather than importing
                // `pattern_memory::fs::markdown_skill::project_metadata_from_loro`
                // (which would create a circular dependency — pattern_core must
                // never depend on pattern_memory). The inline logic is strictly
                // read-only and tolerates missing or malformed fields by
                // substituting empty strings.
                let deep = self.doc.get_deep_value();
                let mut out = String::new();

                let metadata_map = match &deep {
                    LoroValue::Map(root) => match root.get("metadata") {
                        Some(LoroValue::Map(m)) => Some(m.clone()),
                        _ => None,
                    },
                    _ => None,
                };

                if let Some(meta) = &metadata_map {
                    // Name field.
                    if let Some(LoroValue::String(name)) = meta.get("name") {
                        out.push_str(name);
                        out.push('\n');
                    }

                    // Description field (optional — stored as String or Null).
                    if let Some(LoroValue::String(desc)) = meta.get("description") {
                        out.push_str(desc);
                        out.push('\n');
                    }

                    // Keywords — stored as a JSON-encoded array string.
                    // Missing `keywords_json` is valid (means no keywords);
                    // only a present-but-malformed or wrong-type value fires
                    // the metric.
                    if let Some(LoroValue::String(kw_json)) = meta.get("keywords_json") {
                        match serde_json::from_str::<serde_json::Value>(kw_json) {
                            Ok(serde_json::Value::Array(kws)) => {
                                let joined: Vec<&str> =
                                    kws.iter().filter_map(|v| v.as_str()).collect();
                                if !joined.is_empty() {
                                    out.push_str(&joined.join(" "));
                                    out.push('\n');
                                }
                            }
                            Ok(_) | Err(_) => {
                                // keywords_json contains unparseable JSON or a
                                // non-array JSON value. Emit a warning so the
                                // condition is observable in production.
                                // Fires with `kind=malformed` label so the
                                // metric time-series is symmetric with the
                                // wrong-type branch below.
                                metrics::counter!(
                                    "memory.skill.render_keywords_json_failed",
                                    "kind" => "malformed"
                                )
                                .increment(1);
                                tracing::warn!(
                                    block_id = %self.metadata().id,
                                    "Skill block 'keywords_json' could not be parsed as a JSON array; keywords omitted from render"
                                );
                            }
                        }
                    } else if let Some(other) = meta.get("keywords_json") {
                        // keywords_json is present but stored as a non-string
                        // LoroValue — this indicates a schema corruption or a
                        // bug in the writer path. Fire a metric with a
                        // distinct label so it's distinguishable from the
                        // JSON-parse-failure case above.
                        metrics::counter!(
                            "memory.skill.render_keywords_json_failed",
                            "kind" => "wrong_type"
                        )
                        .increment(1);
                        tracing::warn!(
                            block_id = %self.metadata().id,
                            loro_kind = ?std::mem::discriminant(other),
                            "Skill block 'keywords_json' has unexpected non-string LoroValue; \
                             keywords omitted from render"
                        );
                    }
                }

                // Body text.
                let body = self.doc.get_text("body").to_string();
                if !body.is_empty() {
                    out.push('\n');
                    out.push_str(&body);
                }

                out
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
        if schema.timestamp
            && let Some(timestamp) = obj.get("timestamp").and_then(|v| v.as_str())
        {
            parts.push(format!("[{}]", timestamp));
        }

        // Add agent_id if present and enabled in schema
        if schema.agent_id
            && let Some(agent_id) = obj.get("agent_id").and_then(|v| v.as_str())
        {
            parts.push(format!("({})", agent_id));
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
    use crate::types::memory_types::{FieldDef, LogEntrySchema};

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

        #[allow(deprecated)]
        let doc2 = StructuredDocument::from_snapshot(&snapshot, BlockSchema::text()).unwrap();
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
        use crate::types::memory_types::CompositeSection;

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
                    schema: Box::new(BlockSchema::text()),
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
        use crate::types::memory_types::CompositeSection;

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
        use crate::types::memory_types::CompositeSection;

        let schema = BlockSchema::Composite {
            sections: vec![CompositeSection {
                name: "existing".to_string(),
                schema: Box::new(BlockSchema::text()),
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
        let schema = BlockSchema::text();
        #[allow(deprecated)]
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
        use crate::types::memory_types::CompositeSection;

        let schema = BlockSchema::Composite {
            sections: vec![
                CompositeSection {
                    name: "diagnostics".to_string(),
                    schema: Box::new(BlockSchema::text()),
                    description: None,
                    read_only: true,
                },
                CompositeSection {
                    name: "notes".to_string(),
                    schema: Box::new(BlockSchema::text()),
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

    // ========== TaskList schema dispatch tests (Task 8) ==========

    fn make_task_list_schema() -> BlockSchema {
        BlockSchema::TaskList {
            default_owner: None,
            default_status: Some(crate::types::memory_types::TaskStatus::Pending),
            display_limit: Some(3),
        }
    }

    fn make_task_item_json(id: &str, subject: &str, status: &str) -> JsonValue {
        serde_json::json!({
            "id": id,
            "subject": subject,
            "description": "",
            "status": status,
            "blocks": [],
            "metadata": {},
            "comments": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        })
    }

    #[test]
    fn test_task_list_import_from_json_populates_movable_list() {
        let doc = StructuredDocument::new(make_task_list_schema());
        let items = serde_json::json!({
            "items": [
                make_task_item_json("a1", "write spec", "pending"),
                make_task_item_json("a2", "review spec", "in-progress"),
            ]
        });
        doc.import_from_json(&items).unwrap();

        let list = doc.doc.get_movable_list("items");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_task_list_import_from_json_accepts_bare_array() {
        let doc = StructuredDocument::new(make_task_list_schema());
        let items = serde_json::json!([make_task_item_json("b1", "task one", "pending"),]);
        doc.import_from_json(&items).unwrap();

        let list = doc.doc.get_movable_list("items");
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_task_list_import_from_json_rejects_malformed() {
        let doc = StructuredDocument::new(make_task_list_schema());
        let bad = serde_json::json!({ "wrong": "shape" });
        assert!(doc.import_from_json(&bad).is_err());
    }

    #[test]
    fn test_task_list_subscribe_content_returns_movable_list() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let doc = StructuredDocument::new(make_task_list_schema());

        // Call the production code path: subscribe_content should wire up the
        // LoroMovableList container. If the subscription fires on item insertion
        // we know (a) subscribe_content ran, (b) it chose the correct container.
        let fired = Arc::new(AtomicU32::new(0));
        let fired_clone = fired.clone();
        let _sub = doc.subscribe_content(Arc::new(move |_event| {
            fired_clone.fetch_add(1, Ordering::SeqCst);
        }));

        // Insert an item via the production API to trigger the subscription.
        let item = make_task_item_json("sub1", "subscription test", "pending");
        let payload = serde_json::json!({ "items": [item] });
        doc.import_from_json(&payload).unwrap();
        doc.commit();

        assert!(
            fired.load(Ordering::SeqCst) > 0,
            "subscribe_content subscription must fire when an item is inserted into the MovableList"
        );

        // Also verify the container type by inspecting what subscribe_content
        // subscribed to: get_movable_list returns a LoroMovableList, and its
        // ContainerID reports type MovableList.
        let container_id = doc.doc.get_movable_list("items").id();
        assert_eq!(
            format!("{:?}", container_id.container_type()),
            "MovableList",
            "TaskList subscribe_content must target the MovableList container"
        );
    }

    #[test]
    fn test_task_list_render_schema_empty() {
        let doc = StructuredDocument::new(BlockSchema::TaskList {
            default_owner: None,
            default_status: None,
            display_limit: None,
        });
        let rendered = doc.render();
        assert!(
            rendered.starts_with("TaskList (0 items)"),
            "Expected empty task list header, got: {rendered}"
        );
    }

    #[test]
    fn test_task_list_render_schema_respects_display_limit() {
        let schema = BlockSchema::TaskList {
            default_owner: None,
            default_status: None,
            display_limit: Some(2),
        };
        let doc = StructuredDocument::new(schema);
        // Insert 4 items.
        let items = serde_json::json!({
            "items": [
                make_task_item_json("c1", "one", "pending"),
                make_task_item_json("c2", "two", "pending"),
                make_task_item_json("c3", "three", "pending"),
                make_task_item_json("c4", "four", "pending"),
            ]
        });
        doc.import_from_json(&items).unwrap();
        doc.commit();

        let rendered = doc.render();
        assert!(
            rendered.contains("TaskList (4 items, showing 2)"),
            "Expected truncated header, got: {rendered}"
        );
        assert!(
            rendered.contains("... 2 more items not shown"),
            "Expected truncation indicator, got: {rendered}"
        );
        // Should show only 2 item lines.
        let item_lines: Vec<&str> = rendered
            .lines()
            .filter(|l| l.starts_with("- id="))
            .collect();
        assert_eq!(item_lines.len(), 2);
    }

    #[test]
    fn test_task_list_render_schema_shows_blocks_and_description() {
        let doc = StructuredDocument::new(BlockSchema::TaskList {
            default_owner: Some("agent-r".into()),
            default_status: Some(crate::types::memory_types::TaskStatus::InProgress),
            display_limit: None,
        });
        let items = serde_json::json!({
            "items": [{
                "id": "x1",
                "subject": "do thing",
                "description": "First line of desc\nSecond line",
                "status": "in-progress",
                "owner": "agent-r",
                "active_form": "doing the thing",
                "blocks": [
                    { "block": "alpha", "task_item": null },
                    { "block": "beta", "task_item": "y2" }
                ],
                "metadata": {},
                "comments": [],
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            }]
        });
        doc.import_from_json(&items).unwrap();
        doc.commit();

        let rendered = doc.render();
        assert!(rendered.contains("owner=@agent-r"), "missing owner");
        assert!(
            rendered.contains("active_form=\"doing the thing\""),
            "missing active_form"
        );
        assert!(
            rendered.contains("(block)\"alpha\""),
            "missing block-only edge"
        );
        assert!(
            rendered.contains("(block)\"beta#y2\""),
            "missing block#item edge"
        );
        assert!(
            rendered.contains("description: First line of desc"),
            "missing description excerpt"
        );
    }

    // region: Skill import_from_json

    /// Skill `import_from_json` accepts `{"body": "text content"}` and writes
    /// the body text into the LoroDoc's `"body"` LoroText container.
    #[test]
    fn test_skill_import_from_json_accepts_body_object() {
        let doc = StructuredDocument::new(BlockSchema::Skill {
            expected_keys: vec![],
        });
        doc.import_from_json(&serde_json::json!({"body": "text content"}))
            .expect("Skill import_from_json should accept {\"body\": \"...\"}");
        doc.commit();
        // The body text must match exactly what was written.
        // Access via inner() since the "body" container is Skill-specific and
        // not exposed through the StructuredDocument's text_content() helper
        // (which reads from the generic "content" container used by Text schema).
        assert_eq!(
            doc.inner().get_text("body").to_string(),
            "text content",
            "body LoroText should contain the written string"
        );
    }

    /// Skill `import_from_json` rejects objects with keys beyond `"body"` and
    /// returns a `DocumentError::Other` that names the offending key(s) and
    /// points callers toward `write_skill_to_loro_doc`.
    #[test]
    fn test_skill_import_from_json_rejects_extra_keys() {
        let doc = StructuredDocument::new(BlockSchema::Skill {
            expected_keys: vec![],
        });
        let err = doc
            .import_from_json(&serde_json::json!({"body": "x", "name": "bogus"}))
            .expect_err("Skill import_from_json must reject extra keys beyond 'body'");
        let msg = err.to_string();
        assert!(
            msg.contains("write_skill_to_loro_doc"),
            "error message should point callers to write_skill_to_loro_doc; got: {msg}"
        );
        assert!(
            msg.contains("name"),
            "error message should name the offending key 'name'; got: {msg}"
        );
    }

    // endregion: Skill import_from_json

    // region: fork

    /// `StructuredDocument::fork` snapshot-matches the source at fork time,
    /// and diverges independently after writes.
    #[test]
    fn fork_matches_source_at_fork_time_and_diverges_after_writes() {
        let parent = StructuredDocument::new_text();
        parent.set_text("initial", true).unwrap();

        let child = parent.fork();

        // At fork time: both see "initial".
        assert_eq!(
            parent.text_content(),
            "initial",
            "parent should still read 'initial' after fork"
        );
        assert_eq!(
            child.text_content(),
            "initial",
            "child should read 'initial' at fork time"
        );

        // After divergent writes: each side sees only its own content.
        parent.set_text("parent-change", true).unwrap();
        child.set_text("child-change", true).unwrap();

        assert_eq!(
            parent.text_content(),
            "parent-change",
            "parent should read its own write"
        );
        assert_eq!(
            child.text_content(),
            "child-change",
            "child should read its own write without seeing parent's write"
        );
    }

    /// `retag_owner` replaces the `agent_id` in the embedded metadata.
    #[test]
    fn retag_owner_changes_agent_id() {
        let mut doc = StructuredDocument::new_text();
        // Manually seed an agent_id via metadata_mut (the public path).
        doc.metadata_mut().agent_id = "original-agent".to_string();

        doc.retag_owner("new-agent");

        assert_eq!(
            doc.agent_id(),
            "new-agent",
            "retag_owner should update agent_id in metadata"
        );
    }

    // endregion: fork
}
