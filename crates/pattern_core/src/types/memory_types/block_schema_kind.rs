//! Discriminator enum for [`super::BlockSchema`] variants.
//!
//! [`BlockSchemaKind`] mirrors the shape of [`super::BlockSchema`] but carries
//! no payload — it is used for filtering blocks by schema type without
//! deserialising or passing the full schema value (which may include fields,
//! entry schemas, section lists, etc.).
//!
//! The `From<&BlockSchema>` impl converts a schema reference to its kind in
//! O(1) with no allocation. Add new variants here whenever a new
//! [`super::BlockSchema`] variant lands; Phase 4 adds `Skill`.

use serde::{Deserialize, Serialize};

use super::BlockSchema;

/// Discriminator variant of [`BlockSchema`], used for filtering without
/// carrying the variant's associated payload (e.g., `default_owner`,
/// `default_status`, `expected_keys`). Callers build filters against kind,
/// not the full schema value.
///
/// Serialises as kebab-case strings matching the `BlockSchema` serde
/// representation. `Skill` will be added in Phase 4 — this enum is
/// `#[non_exhaustive]` so that addition is a non-breaking change.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockSchemaKind {
    /// Corresponds to [`BlockSchema::Text`].
    Text,
    /// Corresponds to [`BlockSchema::Map`].
    Map,
    /// Corresponds to [`BlockSchema::List`].
    List,
    /// Corresponds to [`BlockSchema::Log`].
    Log,
    /// Corresponds to [`BlockSchema::Composite`].
    Composite,
    /// Corresponds to [`BlockSchema::TaskList`].
    TaskList,
}

impl From<&BlockSchema> for BlockSchemaKind {
    fn from(schema: &BlockSchema) -> Self {
        // This match is exhaustive over all currently-known variants.
        // When a new `BlockSchema` variant lands (e.g. `Skill` in Phase 4),
        // the compiler will produce a non-exhaustive-patterns error here,
        // prompting the implementor to add the corresponding `BlockSchemaKind`
        // variant and arm. That is the desired behaviour — the compile error
        // is the guardrail, not a catch-all arm.
        match schema {
            BlockSchema::Text { .. } => Self::Text,
            BlockSchema::Map { .. } => Self::Map,
            BlockSchema::List { .. } => Self::List,
            BlockSchema::Log { .. } => Self::Log,
            BlockSchema::Composite { .. } => Self::Composite,
            BlockSchema::TaskList { .. } => Self::TaskList,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::memory_types::{BlockSchema, TaskStatus};

    // --- BlockSchemaKind serde round-trips for all 6 variants ---

    #[test]
    fn block_schema_kind_text_round_trips_as_kebab() {
        let kind = BlockSchemaKind::Text;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""text""#);
        assert_eq!(
            serde_json::from_str::<BlockSchemaKind>(&json).unwrap(),
            kind
        );
    }

    #[test]
    fn block_schema_kind_map_round_trips_as_kebab() {
        let kind = BlockSchemaKind::Map;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""map""#);
        assert_eq!(
            serde_json::from_str::<BlockSchemaKind>(&json).unwrap(),
            kind
        );
    }

    #[test]
    fn block_schema_kind_list_round_trips_as_kebab() {
        let kind = BlockSchemaKind::List;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""list""#);
        assert_eq!(
            serde_json::from_str::<BlockSchemaKind>(&json).unwrap(),
            kind
        );
    }

    #[test]
    fn block_schema_kind_log_round_trips_as_kebab() {
        let kind = BlockSchemaKind::Log;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""log""#);
        assert_eq!(
            serde_json::from_str::<BlockSchemaKind>(&json).unwrap(),
            kind
        );
    }

    #[test]
    fn block_schema_kind_composite_round_trips_as_kebab() {
        let kind = BlockSchemaKind::Composite;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""composite""#);
        assert_eq!(
            serde_json::from_str::<BlockSchemaKind>(&json).unwrap(),
            kind
        );
    }

    #[test]
    fn block_schema_kind_task_list_round_trips_as_kebab() {
        let kind = BlockSchemaKind::TaskList;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, r#""task-list""#);
        assert_eq!(
            serde_json::from_str::<BlockSchemaKind>(&json).unwrap(),
            kind
        );
    }

    // --- From<&BlockSchema> correctness ---

    #[test]
    fn from_block_schema_text_yields_text_kind() {
        let schema = BlockSchema::Text { viewport: None };
        assert_eq!(BlockSchemaKind::from(&schema), BlockSchemaKind::Text);
    }

    #[test]
    fn from_block_schema_map_yields_map_kind() {
        let schema = BlockSchema::Map { fields: vec![] };
        assert_eq!(BlockSchemaKind::from(&schema), BlockSchemaKind::Map);
    }

    #[test]
    fn from_block_schema_list_yields_list_kind() {
        let schema = BlockSchema::List {
            item_schema: None,
            max_items: None,
        };
        assert_eq!(BlockSchemaKind::from(&schema), BlockSchemaKind::List);
    }

    #[test]
    fn from_block_schema_log_yields_log_kind() {
        use crate::types::memory_types::LogEntrySchema;
        let schema = BlockSchema::Log {
            display_limit: 10,
            entry_schema: LogEntrySchema {
                timestamp: true,
                agent_id: true,
                fields: vec![],
            },
        };
        assert_eq!(BlockSchemaKind::from(&schema), BlockSchemaKind::Log);
    }

    #[test]
    fn from_block_schema_composite_yields_composite_kind() {
        let schema = BlockSchema::Composite { sections: vec![] };
        assert_eq!(BlockSchemaKind::from(&schema), BlockSchemaKind::Composite);
    }

    #[test]
    fn from_block_schema_task_list_yields_task_list_kind() {
        let schema = BlockSchema::TaskList {
            default_owner: None,
            default_status: Some(TaskStatus::Pending),
            display_limit: None,
        };
        assert_eq!(BlockSchemaKind::from(&schema), BlockSchemaKind::TaskList);
    }
}
