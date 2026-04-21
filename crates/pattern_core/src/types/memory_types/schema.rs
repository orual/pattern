//! Block schema definitions for structured memory
//!
//! Schemas define the structure of a memory block's Loro document,
//! enabling typed operations like `set_field`, `append_to_list`, etc.

use serde::{Deserialize, Serialize};

/// A section within a Composite schema, containing its own schema and metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositeSection {
    /// Section name (used as key in the composite)
    pub name: String,

    /// Schema for this section's content
    pub schema: Box<BlockSchema>,

    /// Human-readable description of the section
    #[serde(default)]
    pub description: Option<String>,

    /// If true, only system/source code can write to this section.
    /// Agent tools should reject writes to read-only sections.
    #[serde(default)]
    pub read_only: bool,
}

/// Viewport for displaying a portion of text content
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextViewport {
    /// Starting line (1-indexed)
    pub start_line: usize,
    /// Number of lines to display
    pub display_lines: usize,
}

/// Block schema defines the structure of a memory block's Loro document
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BlockSchema {
    /// Free-form text with optional viewport for large content
    /// Uses: LoroText container
    Text {
        /// Optional viewport - if set, only displays a window of lines
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewport: Option<TextViewport>,
    },

    /// Key-value pairs with optional field definitions
    /// Uses: LoroMap with nested containers per field
    Map { fields: Vec<FieldDef> },

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

    /// Custom composite with multiple named sections
    Composite { sections: Vec<CompositeSection> },
}

impl Default for BlockSchema {
    fn default() -> Self {
        BlockSchema::text()
    }
}

impl BlockSchema {
    /// Create a simple text schema without viewport
    pub fn text() -> Self {
        BlockSchema::Text { viewport: None }
    }

    /// Create a text schema with a viewport
    pub fn text_with_viewport(start_line: usize, display_lines: usize) -> Self {
        BlockSchema::Text {
            viewport: Some(TextViewport {
                start_line,
                display_lines,
            }),
        }
    }

    /// Check if this is a Text schema (with or without viewport)
    pub fn is_text(&self) -> bool {
        matches!(self, BlockSchema::Text { .. })
    }
}

impl BlockSchema {
    /// Check if a field is read-only. Returns None if field not found or schema doesn't have fields.
    pub fn is_field_read_only(&self, field_name: &str) -> Option<bool> {
        match self {
            BlockSchema::Map { fields } => fields
                .iter()
                .find(|f| f.name == field_name)
                .map(|f| f.read_only),
            _ => None, // Text, List, Log, Composite don't have named fields at top level
        }
    }

    /// Get all field names that are read-only.
    pub fn read_only_fields(&self) -> Vec<&str> {
        match self {
            BlockSchema::Map { fields } => fields
                .iter()
                .filter(|f| f.read_only)
                .map(|f| f.name.as_str())
                .collect(),
            _ => vec![],
        }
    }

    /// Check if a section is read-only (for Composite schemas).
    /// Returns None if section not found or schema is not Composite.
    pub fn is_section_read_only(&self, section_name: &str) -> Option<bool> {
        match self {
            BlockSchema::Composite { sections } => sections
                .iter()
                .find(|s| s.name == section_name)
                .map(|s| s.read_only),
            _ => None,
        }
    }

    /// Get the schema for a section (for Composite schemas).
    /// Returns None if section not found or schema is not Composite.
    pub fn get_section_schema(&self, section_name: &str) -> Option<&BlockSchema> {
        match self {
            BlockSchema::Composite { sections } => sections
                .iter()
                .find(|s| s.name == section_name)
                .map(|s| s.schema.as_ref()),
            _ => None,
        }
    }
}

/// Definition of a field in a Map schema
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FieldDef {
    /// Field name
    pub name: String,

    /// Human-readable description of the field
    pub description: String,

    /// Field data type
    pub field_type: FieldType,

    /// Whether this field is required
    pub required: bool,

    /// Default value (if not required)
    #[serde(default)]
    pub default: Option<serde_json::Value>,

    /// If true, only system/source code can write to this field.
    /// Agent tools should reject writes to read-only fields.
    #[serde(default)]
    pub read_only: bool,
}

/// Field data types for structured schemas
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FieldType {
    /// Text content
    Text,

    /// Numeric value
    Number,

    /// Boolean flag
    Boolean,

    /// List of items
    List,

    /// Timestamp (ISO 8601 string)
    Timestamp,

    /// Counter (numeric value that can increment/decrement)
    Counter,
}

/// Schema for log entry structure
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogEntrySchema {
    /// Include timestamp field
    pub timestamp: bool,

    /// Include agent_id field
    pub agent_id: bool,

    /// Additional custom fields
    pub fields: Vec<FieldDef>,
}
