// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Block schema definitions for structured memory
//!
//! Schemas define the structure of a memory block's Loro document,
//! enabling typed operations like `set_field`, `append_to_list`, etc.

use serde::{Deserialize, Serialize};

use crate::types::ids::AgentId;

use super::TaskStatus;

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

/// Block schema defines the structure of a memory block's Loro document.
///
/// `#[non_exhaustive]` is applied so that adding new schema variants in
/// future phases (e.g. `Skill` in Phase 4) is a non-breaking change.
/// External match sites must include a `_ =>` catch-all arm; internal
/// match sites in `pattern_core` and `pattern_memory` carry explicit arms
/// for every variant.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BlockSchema {
    /// Free-form text with optional viewport for large content
    /// Uses: LoroText container
    Text {
        /// Optional viewport - if set, only displays a window of lines
        #[serde(default)]
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

    /// Ordered, movable list of task items stored in a `LoroMovableList`.
    ///
    /// Items carry per-item `TaskItem` records (status, owner, dependency
    /// edges, comments, metadata). The list-level fields here hold policy
    /// defaults applied when an item omits its own value.
    ///
    /// `display_limit` caps how many items are rendered into the LLM context
    /// window; excess items are summarised with a truncation indicator.
    /// `None` means no cap (all items shown).
    TaskList {
        /// Agent to assign new items to when no explicit owner is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_owner: Option<AgentId>,

        /// Status to apply to new items when none is specified at creation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_status: Option<TaskStatus>,

        /// Maximum number of items to render in the LLM context window.
        /// `None` means render all items.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_limit: Option<usize>,
    },

    /// A Skill block — YAML-frontmatter metadata + markdown body canonical
    /// file. See [`crate::types::memory_types::SkillMetadata`] for the
    /// typed frontmatter fields. `expected_keys` lists author-hint metadata
    /// keys this block template expects; treated as soft documentation, not
    /// enforced by the runtime.
    Skill {
        /// Author-declared hints about which metadata keys this block
        /// template expects. Soft documentation — not enforced by
        /// the runtime.
        #[serde(default)]
        expected_keys: Vec<String>,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `BlockSchema::Skill` with non-empty `expected_keys` round-trips through
    /// `serde_json` without loss.
    #[test]
    fn block_schema_skill_serde_round_trip_with_keys() {
        let schema = BlockSchema::Skill {
            expected_keys: vec!["checklist".to_string(), "workflow".to_string()],
        };
        let json = serde_json::to_string(&schema).expect("serialise BlockSchema::Skill");
        let recovered: BlockSchema =
            serde_json::from_str(&json).expect("deserialise BlockSchema::Skill");
        assert_eq!(schema, recovered);

        // Spot-check: outer key is "Skill", expected_keys present.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse as Value");
        assert!(
            v.get("Skill").is_some(),
            "outer key must be 'Skill', got: {v}"
        );
        let inner = &v["Skill"];
        let keys = inner["expected_keys"]
            .as_array()
            .expect("expected_keys must be array");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].as_str(), Some("checklist"));
        assert_eq!(keys[1].as_str(), Some("workflow"));
    }

    /// `BlockSchema::Skill` with empty `expected_keys` round-trips correctly.
    #[test]
    fn block_schema_skill_serde_round_trip_empty_keys() {
        let schema = BlockSchema::Skill {
            expected_keys: vec![],
        };
        let json = serde_json::to_string(&schema).expect("serialise BlockSchema::Skill empty");
        let recovered: BlockSchema =
            serde_json::from_str(&json).expect("deserialise BlockSchema::Skill empty");
        assert_eq!(schema, recovered);
    }

    /// AC1.1: `BlockSchema::TaskList` can be constructed and round-trips through
    /// `serde_json` without loss.
    ///
    /// This test is written *before* the `TaskList` variant is added to
    /// `BlockSchema`. It must fail (compile error / `no variant named TaskList`)
    /// until Task 7 implements the variant — TDD red phase.
    #[test]
    fn block_schema_task_list_serde_round_trip() {
        use crate::types::ids::AgentId;
        use crate::types::memory_types::TaskStatus;

        let schema = BlockSchema::TaskList {
            default_owner: None,
            default_status: Some(TaskStatus::Pending),
            display_limit: Some(20),
        };

        let json = serde_json::to_string(&schema).expect("serialise BlockSchema::TaskList");
        let recovered: BlockSchema =
            serde_json::from_str(&json).expect("deserialise BlockSchema::TaskList");

        assert_eq!(schema, recovered);

        // Spot-check: default_owner absent, default_status present, display_limit present.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse as Value");
        // Externally-tagged enum: the outer key is "TaskList".
        assert!(
            v.get("TaskList").is_some(),
            "outer key must be 'TaskList', got: {v}"
        );
        let inner = &v["TaskList"];
        assert!(
            inner["default_owner"].is_null(),
            "default_owner must be null when None"
        );
        assert_eq!(
            inner["default_status"].as_str(),
            Some("pending"),
            "default_status must serialize as kebab-case"
        );
        assert_eq!(
            inner["display_limit"].as_u64(),
            Some(20),
            "display_limit must serialize as integer"
        );

        // Ensure AgentId variant round-trips correctly too.
        let schema_with_owner = BlockSchema::TaskList {
            default_owner: Some(AgentId::from("agent-orual")),
            default_status: None,
            display_limit: None,
        };
        let json2 =
            serde_json::to_string(&schema_with_owner).expect("serialise TaskList with owner");
        let recovered2: BlockSchema =
            serde_json::from_str(&json2).expect("deserialise TaskList with owner");
        assert_eq!(schema_with_owner, recovered2);
    }
}
