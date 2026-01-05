//! User block schema and helpers for Bluesky users.

use crate::memory::{BlockSchema, CompositeSection, FieldDef, FieldType};

/// Default char limit for user blocks
pub const USER_BLOCK_CHAR_LIMIT: usize = 4096;

/// Create a composite schema for Bluesky user blocks.
///
/// Structure:
/// - `profile` section (read-only): Map with display_name, handle, did, avatar, description
/// - `notes` section (writable): Text for agent notes about this user
pub fn bluesky_user_schema() -> BlockSchema {
    BlockSchema::Composite {
        sections: vec![
            CompositeSection {
                name: "profile".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![
                        FieldDef {
                            name: "did".to_string(),
                            description: "User's DID".to_string(),
                            field_type: FieldType::Text,
                            required: true,
                            default: None,
                            read_only: true,
                        },
                        FieldDef {
                            name: "handle".to_string(),
                            description: "User's handle (e.g., alice.bsky.social)".to_string(),
                            field_type: FieldType::Text,
                            required: true,
                            default: None,
                            read_only: true,
                        },
                        FieldDef {
                            name: "display_name".to_string(),
                            description: "User's display name".to_string(),
                            field_type: FieldType::Text,
                            required: false,
                            default: None,
                            read_only: true,
                        },
                        FieldDef {
                            name: "avatar".to_string(),
                            description: "URL to user's avatar image".to_string(),
                            field_type: FieldType::Text,
                            required: false,
                            default: None,
                            read_only: true,
                        },
                        FieldDef {
                            name: "description".to_string(),
                            description: "User's bio/description".to_string(),
                            field_type: FieldType::Text,
                            required: false,
                            default: None,
                            read_only: true,
                        },
                        FieldDef {
                            name: "pronouns".to_string(),
                            description: "User's pronouns".to_string(),
                            field_type: FieldType::Text,
                            required: false,
                            default: None,
                            read_only: true,
                        },
                        FieldDef {
                            name: "last_seen".to_string(),
                            description: "When we last saw a post from this user".to_string(),
                            field_type: FieldType::Timestamp,
                            required: false,
                            default: None,
                            read_only: true,
                        },
                    ],
                }),
                description: Some("Bluesky profile information (auto-updated)".to_string()),
                read_only: true,
            },
            CompositeSection {
                name: "notes".to_string(),
                schema: Box::new(BlockSchema::text()),
                description: Some("Your notes about this user".to_string()),
                read_only: false,
            },
        ],
    }
}

/// Generate block ID from DID
pub fn user_block_id(did: &str) -> String {
    format!("atproto:{}", did)
}

/// Generate block label from handle
pub fn user_block_label(handle: &str) -> String {
    format!("bluesky_user:{}", handle)
}
