//! Pre-defined schema templates for common memory block shapes.
//!
//! These constructors return canned [`BlockSchema`] values that match common
//! use patterns (partner profile, task list, observation log, scratchpad).
//! Schema type definitions live in [`pattern_core::types::memory_types`].

use pattern_core::types::memory_types::{BlockSchema, FieldDef, FieldType, LogEntrySchema};

/// Pre-defined schema templates for common use cases.
pub mod templates {
    use super::*;

    /// Partner profile schema.
    /// Tracks information about the human being supported.
    pub fn partner_profile() -> BlockSchema {
        BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Partner's preferred name".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: false,
                },
                FieldDef {
                    name: "preferences".to_string(),
                    description: "List of preferences and notes".to_string(),
                    field_type: FieldType::List,
                    required: false,
                    default: None,
                    read_only: false,
                },
                FieldDef {
                    name: "energy_level".to_string(),
                    description: "Current energy level (0-10)".to_string(),
                    field_type: FieldType::Counter,
                    required: false,
                    default: Some(serde_json::json!(5)),
                    read_only: false,
                },
                FieldDef {
                    name: "current_focus".to_string(),
                    description: "What the partner is currently focused on".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: false,
                },
                FieldDef {
                    name: "last_interaction".to_string(),
                    description: "Timestamp of last interaction".to_string(),
                    field_type: FieldType::Timestamp,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        }
    }

    /// Task list schema (legacy, pre-v3-task-skill-blocks).
    ///
    /// Returns a `BlockSchema::List` with a Map item schema. This was the
    /// original approach before the dedicated `BlockSchema::TaskList` variant
    /// was added in v3-task-skill-blocks Phase 1.
    ///
    /// **Prefer `BlockSchema::TaskList { .. }` for new code.** `BlockSchema::TaskList`
    /// uses a `LoroMovableList` for proper item reordering semantics, carries
    /// richer item metadata (status, owner, blocks, comments), and round-trips
    /// through a dedicated KDL serializer. This function is retained for
    /// backward-compatibility with existing persona TOML files and tests that
    /// reference the old shape, but new task lists should use `TaskList`.
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
                        read_only: false,
                    },
                    FieldDef {
                        name: "done".to_string(),
                        description: "Whether the task is completed".to_string(),
                        field_type: FieldType::Boolean,
                        required: true,
                        default: Some(serde_json::json!(false)),
                        read_only: false,
                    },
                    FieldDef {
                        name: "priority".to_string(),
                        description: "Task priority (1-5, 1=highest)".to_string(),
                        field_type: FieldType::Number,
                        required: false,
                        default: Some(serde_json::json!(3)),
                        read_only: false,
                    },
                    FieldDef {
                        name: "due".to_string(),
                        description: "Due date timestamp".to_string(),
                        field_type: FieldType::Timestamp,
                        required: false,
                        default: None,
                        read_only: false,
                    },
                ],
            })),
            max_items: None,
        }
    }

    /// Observation log schema.
    /// For agent memory of events.
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
                        read_only: false,
                    },
                    FieldDef {
                        name: "context".to_string(),
                        description: "Additional context about the observation".to_string(),
                        field_type: FieldType::Text,
                        required: false,
                        default: None,
                        read_only: false,
                    },
                ],
            },
        }
    }

    /// Scratchpad schema.
    /// Simple free-form notes.
    pub fn scratchpad() -> BlockSchema {
        BlockSchema::text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::memory_types::CompositeSection;

    #[test]
    fn test_default_schema_is_text() {
        let schema = BlockSchema::default();
        assert_eq!(schema, BlockSchema::text());
    }

    #[test]
    fn test_partner_profile_has_expected_fields() {
        let schema = templates::partner_profile();

        match schema {
            BlockSchema::Map { fields } => {
                assert_eq!(fields.len(), 5);

                // Check name field.
                let name_field = fields.iter().find(|f| f.name == "name").unwrap();
                assert_eq!(name_field.field_type, FieldType::Text);
                assert!(name_field.required);

                // Check preferences field.
                let prefs_field = fields.iter().find(|f| f.name == "preferences").unwrap();
                assert_eq!(prefs_field.field_type, FieldType::List);
                assert!(!prefs_field.required);

                // Check energy_level field.
                let energy_field = fields.iter().find(|f| f.name == "energy_level").unwrap();
                assert_eq!(energy_field.field_type, FieldType::Counter);
                assert!(!energy_field.required);
                assert_eq!(energy_field.default, Some(serde_json::json!(5)));

                // Check current_focus field.
                let focus_field = fields.iter().find(|f| f.name == "current_focus").unwrap();
                assert_eq!(focus_field.field_type, FieldType::Text);
                assert!(!focus_field.required);

                // Check last_interaction field.
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
                // max_items should be None (unlimited).
                assert_eq!(max_items, None);

                // Check item schema.
                assert!(item_schema.is_some());
                let item = item_schema.unwrap();

                match *item {
                    BlockSchema::Map { fields } => {
                        assert_eq!(fields.len(), 4);

                        // Check title.
                        let title = fields.iter().find(|f| f.name == "title").unwrap();
                        assert_eq!(title.field_type, FieldType::Text);
                        assert!(title.required);

                        // Check done.
                        let done = fields.iter().find(|f| f.name == "done").unwrap();
                        assert_eq!(done.field_type, FieldType::Boolean);
                        assert!(done.required);
                        assert_eq!(done.default, Some(serde_json::json!(false)));

                        // Check priority.
                        let priority = fields.iter().find(|f| f.name == "priority").unwrap();
                        assert_eq!(priority.field_type, FieldType::Number);
                        assert!(!priority.required);
                        assert_eq!(priority.default, Some(serde_json::json!(3)));

                        // Check due.
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

                // Check observation field.
                let obs = entry_schema
                    .fields
                    .iter()
                    .find(|f| f.name == "observation")
                    .unwrap();
                assert_eq!(obs.field_type, FieldType::Text);
                assert!(obs.required);

                // Check context field.
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
        assert_eq!(schema, BlockSchema::text());
    }

    #[test]
    fn test_schema_serialization() {
        let schema = templates::partner_profile();
        let json = serde_json::to_string(&schema).unwrap();
        let deserialized: BlockSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(schema, deserialized);
    }

    #[test]
    fn test_field_def_read_only() {
        let field = FieldDef {
            name: "status".to_string(),
            description: "Current status".to_string(),
            field_type: FieldType::Text,
            required: true,
            default: None,
            read_only: true,
        };

        assert!(field.read_only);

        // Default should be false.
        let field2 = FieldDef {
            name: "notes".to_string(),
            description: "User notes".to_string(),
            field_type: FieldType::Text,
            required: false,
            default: None,
            read_only: false,
        };

        assert!(!field2.read_only);
    }

    #[test]
    fn test_block_schema_read_only_helpers() {
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

        assert_eq!(schema.is_field_read_only("status"), Some(true));
        assert_eq!(schema.is_field_read_only("notes"), Some(false));
        assert_eq!(schema.is_field_read_only("nonexistent"), None);

        let read_only = schema.read_only_fields();
        assert_eq!(read_only, vec!["status"]);
    }

    #[test]
    fn test_composite_section_read_only() {
        let schema = BlockSchema::Composite {
            sections: vec![
                CompositeSection {
                    name: "diagnostics".to_string(),
                    schema: Box::new(BlockSchema::Map {
                        fields: vec![FieldDef {
                            name: "errors".to_string(),
                            description: "Error list".to_string(),
                            field_type: FieldType::List,
                            required: true,
                            default: None,
                            read_only: false, // Field-level, section overrides.
                        }],
                    }),
                    description: Some("LSP diagnostics".to_string()),
                    read_only: true, // Whole section is read-only.
                },
                CompositeSection {
                    name: "config".to_string(),
                    schema: Box::new(BlockSchema::Map {
                        fields: vec![FieldDef {
                            name: "filter".to_string(),
                            description: "Filter setting".to_string(),
                            field_type: FieldType::Text,
                            required: false,
                            default: None,
                            read_only: false,
                        }],
                    }),
                    description: Some("User configuration".to_string()),
                    read_only: false,
                },
            ],
        };

        assert_eq!(schema.is_section_read_only("diagnostics"), Some(true));
        assert_eq!(schema.is_section_read_only("config"), Some(false));
        assert_eq!(schema.is_section_read_only("nonexistent"), None);

        // Test get_section_schema.
        let diagnostics_schema = schema.get_section_schema("diagnostics");
        assert!(diagnostics_schema.is_some());
        match diagnostics_schema.unwrap() {
            BlockSchema::Map { fields } => {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name, "errors");
            }
            _ => panic!("Expected Map schema for diagnostics section"),
        }

        assert!(schema.get_section_schema("config").is_some());
        assert!(schema.get_section_schema("nonexistent").is_none());

        // Test that non-Composite schemas return None.
        let text_schema = BlockSchema::text();
        assert_eq!(text_schema.is_section_read_only("any"), None);
        assert!(text_schema.get_section_schema("any").is_none());
    }
}
