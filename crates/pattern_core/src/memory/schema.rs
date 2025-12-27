//! Block schema definitions for structured memory
//!
//! Schemas define the structure of a memory block's Loro document,
//! enabling typed operations like `set_field`, `append_to_list`, etc.

use serde::{Deserialize, Serialize};

/// Block schema defines the structure of a memory block's Loro document
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BlockSchema {
    /// Free-form text (default, backward compatible)
    /// Uses: LoroText container
    Text,

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

    /// Hierarchical tree structure (stub for now)
    Tree {
        node_schema: Option<Box<BlockSchema>>,
    },

    /// Custom composite with multiple named sections
    Composite {
        sections: Vec<(String, BlockSchema)>,
    },
}

impl Default for BlockSchema {
    fn default() -> Self {
        BlockSchema::Text
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
    pub default: Option<serde_json::Value>,
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

/// Pre-defined schema templates for common use cases
pub mod templates {
    use super::*;

    /// Partner profile schema
    /// Tracks information about the human being supported
    pub fn partner_profile() -> BlockSchema {
        BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Partner's preferred name".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                },
                FieldDef {
                    name: "preferences".to_string(),
                    description: "List of preferences and notes".to_string(),
                    field_type: FieldType::List,
                    required: false,
                    default: None,
                },
                FieldDef {
                    name: "energy_level".to_string(),
                    description: "Current energy level (0-10)".to_string(),
                    field_type: FieldType::Counter,
                    required: false,
                    default: Some(serde_json::json!(5)),
                },
                FieldDef {
                    name: "current_focus".to_string(),
                    description: "What the partner is currently focused on".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                },
                FieldDef {
                    name: "last_interaction".to_string(),
                    description: "Timestamp of last interaction".to_string(),
                    field_type: FieldType::Timestamp,
                    required: false,
                    default: None,
                },
            ],
        }
    }

    /// Task list schema
    /// For ADHD task management
    pub fn task_list() -> BlockSchema {
        BlockSchema::List {
            item_schema: Some(Box::new(BlockSchema::Map {
                fields: vec![
                    FieldDef {
                        name: "title".to_string(),
                        description: "Task title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        default: None,
                    },
                    FieldDef {
                        name: "done".to_string(),
                        description: "Whether the task is completed".to_string(),
                        field_type: FieldType::Boolean,
                        required: true,
                        default: Some(serde_json::json!(false)),
                    },
                    FieldDef {
                        name: "priority".to_string(),
                        description: "Task priority (1-5, 1=highest)".to_string(),
                        field_type: FieldType::Number,
                        required: false,
                        default: Some(serde_json::json!(3)),
                    },
                    FieldDef {
                        name: "due".to_string(),
                        description: "Due date timestamp".to_string(),
                        field_type: FieldType::Timestamp,
                        required: false,
                        default: None,
                    },
                ],
            })),
            max_items: None,
        }
    }

    /// Observation log schema
    /// For agent memory of events
    pub fn observation_log() -> BlockSchema {
        BlockSchema::Log {
            display_limit: 20,
            entry_schema: LogEntrySchema {
                timestamp: true,
                agent_id: true,
                fields: vec![
                    FieldDef {
                        name: "observation".to_string(),
                        description: "What was observed".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        default: None,
                    },
                    FieldDef {
                        name: "context".to_string(),
                        description: "Additional context about the observation".to_string(),
                        field_type: FieldType::Text,
                        required: false,
                        default: None,
                    },
                ],
            },
        }
    }

    /// Scratchpad schema
    /// Simple free-form notes
    pub fn scratchpad() -> BlockSchema {
        BlockSchema::Text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_schema_is_text() {
        let schema = BlockSchema::default();
        assert_eq!(schema, BlockSchema::Text);
    }

    #[test]
    fn test_partner_profile_has_expected_fields() {
        let schema = templates::partner_profile();

        match schema {
            BlockSchema::Map { fields } => {
                assert_eq!(fields.len(), 5);

                // Check name field
                let name_field = fields.iter().find(|f| f.name == "name").unwrap();
                assert_eq!(name_field.field_type, FieldType::Text);
                assert!(name_field.required);

                // Check preferences field
                let prefs_field = fields.iter().find(|f| f.name == "preferences").unwrap();
                assert_eq!(prefs_field.field_type, FieldType::List);
                assert!(!prefs_field.required);

                // Check energy_level field
                let energy_field = fields.iter().find(|f| f.name == "energy_level").unwrap();
                assert_eq!(energy_field.field_type, FieldType::Counter);
                assert!(!energy_field.required);
                assert_eq!(energy_field.default, Some(serde_json::json!(5)));

                // Check current_focus field
                let focus_field = fields.iter().find(|f| f.name == "current_focus").unwrap();
                assert_eq!(focus_field.field_type, FieldType::Text);
                assert!(!focus_field.required);

                // Check last_interaction field
                let interaction_field = fields
                    .iter()
                    .find(|f| f.name == "last_interaction")
                    .unwrap();
                assert_eq!(interaction_field.field_type, FieldType::Timestamp);
                assert!(!interaction_field.required);
            }
            _ => panic!("Expected Map schema"),
        }
    }

    #[test]
    fn test_task_list_has_max_items() {
        let schema = templates::task_list();

        match schema {
            BlockSchema::List {
                item_schema,
                max_items,
            } => {
                // max_items should be None (unlimited)
                assert_eq!(max_items, None);

                // Check item schema
                assert!(item_schema.is_some());
                let item = item_schema.unwrap();

                match *item {
                    BlockSchema::Map { fields } => {
                        assert_eq!(fields.len(), 4);

                        // Check title
                        let title = fields.iter().find(|f| f.name == "title").unwrap();
                        assert_eq!(title.field_type, FieldType::Text);
                        assert!(title.required);

                        // Check done
                        let done = fields.iter().find(|f| f.name == "done").unwrap();
                        assert_eq!(done.field_type, FieldType::Boolean);
                        assert!(done.required);
                        assert_eq!(done.default, Some(serde_json::json!(false)));

                        // Check priority
                        let priority = fields.iter().find(|f| f.name == "priority").unwrap();
                        assert_eq!(priority.field_type, FieldType::Number);
                        assert!(!priority.required);
                        assert_eq!(priority.default, Some(serde_json::json!(3)));

                        // Check due
                        let due = fields.iter().find(|f| f.name == "due").unwrap();
                        assert_eq!(due.field_type, FieldType::Timestamp);
                        assert!(!due.required);
                    }
                    _ => panic!("Expected Map schema for task items"),
                }
            }
            _ => panic!("Expected List schema"),
        }
    }

    #[test]
    fn test_observation_log_structure() {
        let schema = templates::observation_log();

        match schema {
            BlockSchema::Log {
                display_limit,
                entry_schema,
            } => {
                assert_eq!(display_limit, 20);
                assert!(entry_schema.timestamp);
                assert!(entry_schema.agent_id);
                assert_eq!(entry_schema.fields.len(), 2);

                // Check observation field
                let obs = entry_schema
                    .fields
                    .iter()
                    .find(|f| f.name == "observation")
                    .unwrap();
                assert_eq!(obs.field_type, FieldType::Text);
                assert!(obs.required);

                // Check context field
                let ctx = entry_schema
                    .fields
                    .iter()
                    .find(|f| f.name == "context")
                    .unwrap();
                assert_eq!(ctx.field_type, FieldType::Text);
                assert!(!ctx.required);
            }
            _ => panic!("Expected Log schema"),
        }
    }

    #[test]
    fn test_scratchpad_is_text() {
        let schema = templates::scratchpad();
        assert_eq!(schema, BlockSchema::Text);
    }

    #[test]
    fn test_schema_serialization() {
        let schema = templates::partner_profile();
        let json = serde_json::to_string(&schema).unwrap();
        let deserialized: BlockSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(schema, deserialized);
    }
}
