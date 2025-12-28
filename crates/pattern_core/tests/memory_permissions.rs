//! Integration test for memory block field permissions.

use pattern_core::memory::{
    BlockSchema, CompositeSection, DocumentError, FieldDef, FieldType, StructuredDocument,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[test]
fn test_field_level_permissions() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "diagnostics".to_string(),
                description: "LSP diagnostics (source-updated)".to_string(),
                field_type: FieldType::List,
                required: true,
                default: Some(serde_json::json!([])),
                read_only: true,
            },
            FieldDef {
                name: "severity_filter".to_string(),
                description: "Filter level (agent-configurable)".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: Some(serde_json::json!("warning")),
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);

    // Agent can modify writable field
    assert!(
        doc.set_field(
            "severity_filter",
            serde_json::json!("error"),
            false // is_system = false (agent)
        )
        .is_ok()
    );

    // Agent cannot modify read-only field
    let result = doc.set_field(
        "diagnostics",
        serde_json::json!(["error1"]),
        false, // is_system = false (agent)
    );
    assert!(matches!(result, Err(DocumentError::ReadOnlyField(_))));

    // System can modify read-only field
    assert!(
        doc.set_field(
            "diagnostics",
            serde_json::json!(["error1", "error2"]),
            true // is_system = true
        )
        .is_ok()
    );

    // Verify the values
    assert_eq!(
        doc.get_field("severity_filter").unwrap(),
        serde_json::json!("error")
    );
}

#[test]
fn test_composite_section_permissions() {
    let schema = BlockSchema::Composite {
        sections: vec![
            CompositeSection {
                name: "status".to_string(),
                schema: Box::new(BlockSchema::Map {
                    fields: vec![FieldDef {
                        name: "health".to_string(),
                        description: "System health".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        default: Some(serde_json::json!("unknown")),
                        read_only: false,
                    }],
                }),
                description: Some("System status (source-managed)".to_string()),
                read_only: true, // Entire section is read-only
            },
            CompositeSection {
                name: "notes".to_string(),
                schema: Box::new(BlockSchema::Text),
                description: Some("Agent notes".to_string()),
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);

    // Agent cannot write to read-only section
    let result = doc.set_field_in_section(
        "health", "good", "status", false, // is_system = false (agent)
    );
    assert!(matches!(result, Err(DocumentError::ReadOnlySection(_))));

    // System can write to read-only section
    assert!(
        doc.set_field_in_section(
            "health", "good", "status", true // is_system = true
        )
        .is_ok()
    );

    // Agent can write to writable section
    assert!(
        doc.set_text_in_section(
            "My notes here",
            "notes",
            false // is_system = false (agent)
        )
        .is_ok()
    );
}

#[test]
fn test_subscription_fires_on_commit() {
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
    let change_count = Arc::new(AtomicU32::new(0));
    let change_count_clone = change_count.clone();

    let _sub = doc.subscribe_root(Arc::new(move |_event| {
        change_count_clone.fetch_add(1, Ordering::SeqCst);
    }));

    // Make changes and commit
    doc.increment_counter("counter", 1, true).unwrap();
    doc.commit();

    doc.increment_counter("counter", 5, true).unwrap();
    doc.commit();

    // Should have fired twice
    assert_eq!(change_count.load(Ordering::SeqCst), 2);
}

#[test]
fn test_render_shows_permissions() {
    let schema = BlockSchema::Map {
        fields: vec![
            FieldDef {
                name: "readonly".to_string(),
                description: "Read-only field".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: true,
            },
            FieldDef {
                name: "writable".to_string(),
                description: "Writable field".to_string(),
                field_type: FieldType::Text,
                required: false,
                default: None,
                read_only: false,
            },
        ],
    };

    let doc = StructuredDocument::new(schema);
    doc.set_field("readonly", serde_json::json!("value1"), true)
        .unwrap();
    doc.set_field("writable", serde_json::json!("value2"), true)
        .unwrap();

    let rendered = doc.render();

    assert!(
        rendered.contains("readonly [read-only]: value1"),
        "Expected 'readonly [read-only]: value1' in rendered output:\n{}",
        rendered
    );
    assert!(
        rendered.contains("writable: value2"),
        "Expected 'writable: value2' in rendered output:\n{}",
        rendered
    );
    assert!(
        !rendered.contains("writable [read-only]"),
        "Should not contain 'writable [read-only]' in rendered output:\n{}",
        rendered
    );
}

#[test]
fn test_identity_fields() {
    // Test with identity set
    let doc_with_identity = StructuredDocument::new_with_identity(
        BlockSchema::Text,
        "test_block".to_string(),
        Some("agent_42".to_string()),
    );
    assert_eq!(doc_with_identity.label(), "test_block");
    assert_eq!(doc_with_identity.accessor_agent_id(), Some("agent_42"));

    // Test without identity (default constructor)
    let doc_without_identity = StructuredDocument::new(BlockSchema::Text);
    assert_eq!(doc_without_identity.label(), "");
    assert_eq!(doc_without_identity.accessor_agent_id(), None);
}

#[test]
fn test_auto_attribution_sets_commit_message() {
    // Create doc with identity
    let doc = StructuredDocument::new_with_identity(
        BlockSchema::Text,
        "test_block".to_string(),
        Some("agent_42".to_string()),
    );

    // Make a change
    doc.set_text("hello world", true).unwrap();

    // Set attribution and commit
    doc.auto_attribution("append");
    doc.commit();

    // Verify the commit message was set correctly by checking change history
    let loro_doc = doc.inner();
    let frontiers = loro_doc.oplog_frontiers();

    // Get the change and verify message
    let mut found_message = false;
    for id in frontiers.iter() {
        if let Some(change) = loro_doc.get_change(id) {
            assert_eq!(
                change.message(),
                "agent:agent_42:append",
                "Attribution message should be 'agent:agent_42:append'"
            );
            found_message = true;
        }
    }
    assert!(
        found_message,
        "Should have found a change with the attribution message"
    );
}
