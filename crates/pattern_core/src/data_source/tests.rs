//! Integration tests for the data_source module.
//!
//! Tests cover:
//! - Core type serialization/deserialization
//! - Helper utilities (NotificationBuilder, EphemeralBlockCache)
//! - Trait object safety for DataStream and DataBlock

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use serde_json::json;
use tokio::sync::broadcast;

use super::*;
use crate::error::Result;
use crate::id::AgentId;
use crate::memory::{BlockSchema, MemoryPermission};
use crate::runtime::ToolContext;

// ==================== Core Type Tests ====================

#[test]
fn test_block_ref_creation() {
    let block_ref = BlockRef::new("test_label", "block_123");
    assert_eq!(block_ref.label, "test_label");
    assert_eq!(block_ref.block_id, "block_123");
    assert_eq!(block_ref.agent_id, "_constellation_"); // Default owner
}

#[test]
fn test_block_ref_owned_by() {
    let block_ref = BlockRef::new("test_label", "block_123").owned_by("agent_456");
    assert_eq!(block_ref.label, "test_label");
    assert_eq!(block_ref.block_id, "block_123");
    assert_eq!(block_ref.agent_id, "agent_456");
}

#[test]
fn test_block_ref_equality() {
    let ref1 = BlockRef::new("label", "id").owned_by("owner");
    let ref2 = BlockRef::new("label", "id").owned_by("owner");
    let ref3 = BlockRef::new("label", "different_id").owned_by("owner");

    assert_eq!(ref1, ref2);
    assert_ne!(ref1, ref3);
}

#[test]
fn test_block_ref_serialization() {
    let block_ref = BlockRef::new("test_label", "block_123").owned_by("agent_456");

    let json = serde_json::to_string(&block_ref).unwrap();
    let parsed: BlockRef = serde_json::from_str(&json).unwrap();

    assert_eq!(block_ref, parsed);
}

#[test]
fn test_stream_cursor_creation() {
    let cursor = StreamCursor::new("cursor_abc");
    assert_eq!(cursor.as_str(), "cursor_abc");
}

#[test]
fn test_stream_cursor_default() {
    let cursor = StreamCursor::default();
    assert_eq!(cursor.as_str(), "");
}

#[test]
fn test_stream_cursor_serialization() {
    let cursor = StreamCursor::new("cursor_abc");

    let json = serde_json::to_string(&cursor).unwrap();
    let parsed: StreamCursor = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.as_str(), "cursor_abc");
}

#[test]
fn test_block_schema_spec_pinned() {
    let spec = BlockSchemaSpec::pinned("config", BlockSchema::Text, "Configuration block");

    assert!(spec.pinned);
    assert_eq!(spec.label_pattern, "config");
    assert_eq!(spec.description, "Configuration block");
    assert_eq!(spec.schema, BlockSchema::Text);
}

#[test]
fn test_block_schema_spec_ephemeral() {
    let spec = BlockSchemaSpec::ephemeral("user_{id}", BlockSchema::Text, "User profile");

    assert!(!spec.pinned);
    assert_eq!(spec.label_pattern, "user_{id}");
    assert_eq!(spec.description, "User profile");
}

#[test]
fn test_block_schema_spec_serialization() {
    let spec = BlockSchemaSpec::pinned("config", BlockSchema::Text, "Configuration block");

    let json = serde_json::to_string(&spec).unwrap();
    let parsed: BlockSchemaSpec = serde_json::from_str(&json).unwrap();

    assert_eq!(spec.label_pattern, parsed.label_pattern);
    assert_eq!(spec.pinned, parsed.pinned);
    assert_eq!(spec.description, parsed.description);
}

#[test]
fn test_stream_event_creation() {
    let event = StreamEvent::new("source_1", "message", json!({"text": "hello"}));

    assert_eq!(event.source_id, "source_1");
    assert_eq!(event.event_type, "message");
    assert_eq!(event.payload, json!({"text": "hello"}));
    assert!(event.cursor.is_none());
}

#[test]
fn test_notification_creation() {
    let msg = crate::messages::Message::user("test message");
    let batch_id = crate::utils::get_next_message_position_sync();
    let notification = Notification::new(msg, batch_id);

    assert!(notification.block_refs.is_empty());
    assert_eq!(notification.batch_id, batch_id);
}

#[test]
fn test_notification_with_blocks() {
    let msg = crate::messages::Message::user("test message");
    let batch_id = crate::utils::get_next_message_position_sync();
    let blocks = vec![
        BlockRef::new("label1", "id1"),
        BlockRef::new("label2", "id2"),
    ];

    let notification = Notification::new(msg, batch_id).with_blocks(blocks);

    assert_eq!(notification.block_refs.len(), 2);
    assert_eq!(notification.block_refs[0].label, "label1");
    assert_eq!(notification.block_refs[1].label, "label2");
}

// ==================== Permission Rule Tests ====================

#[test]
fn test_permission_rule_creation() {
    let rule = PermissionRule::new("*.rs", MemoryPermission::ReadWrite);

    assert_eq!(rule.pattern, "*.rs");
    assert_eq!(rule.permission, MemoryPermission::ReadWrite);
    assert!(rule.operations_requiring_escalation.is_empty());
}

#[test]
fn test_permission_rule_with_escalation() {
    let rule = PermissionRule::new("*.config.toml", MemoryPermission::ReadWrite)
        .with_escalation(["delete", "rename"]);

    assert_eq!(rule.operations_requiring_escalation.len(), 2);
    assert!(
        rule.operations_requiring_escalation
            .contains(&"delete".to_string())
    );
    assert!(
        rule.operations_requiring_escalation
            .contains(&"rename".to_string())
    );
}

#[test]
fn test_permission_rule_serialization() {
    let rule =
        PermissionRule::new("src/**/*.rs", MemoryPermission::ReadOnly).with_escalation(["delete"]);

    let json = serde_json::to_string(&rule).unwrap();
    let parsed: PermissionRule = serde_json::from_str(&json).unwrap();

    assert_eq!(rule.pattern, parsed.pattern);
    assert_eq!(rule.permission, parsed.permission);
    assert_eq!(
        rule.operations_requiring_escalation,
        parsed.operations_requiring_escalation
    );
}

// ==================== File Change Tests ====================

#[test]
fn test_file_change_types() {
    // Verify all variants exist and are distinguishable
    assert_ne!(FileChangeType::Created, FileChangeType::Modified);
    assert_ne!(FileChangeType::Modified, FileChangeType::Deleted);
    assert_ne!(FileChangeType::Created, FileChangeType::Deleted);
}

#[test]
fn test_file_change_serialization() {
    let change = FileChange {
        path: PathBuf::from("/src/main.rs"),
        change_type: FileChangeType::Modified,
        block_id: Some("block_123".to_string()),
        timestamp: Some(Utc::now()),
    };

    let json = serde_json::to_string(&change).unwrap();
    let parsed: FileChange = serde_json::from_str(&json).unwrap();

    assert_eq!(change.path, parsed.path);
    assert_eq!(change.change_type, parsed.change_type);
    assert_eq!(change.block_id, parsed.block_id);
}

// ==================== Version Info Tests ====================

#[test]
fn test_version_info_serialization() {
    let version = VersionInfo {
        version_id: "v1".to_string(),
        timestamp: Utc::now(),
        description: Some("Initial version".to_string()),
    };

    let json = serde_json::to_string(&version).unwrap();
    let parsed: VersionInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(version.version_id, parsed.version_id);
    assert_eq!(version.description, parsed.description);
}

// ==================== Conflict Resolution Tests ====================

#[test]
fn test_conflict_resolution_variants() {
    let disk_wins = ConflictResolution::DiskWins;
    let agent_wins = ConflictResolution::AgentWins;
    let merge = ConflictResolution::Merge;
    let conflict = ConflictResolution::Conflict {
        disk_summary: "disk changes".to_string(),
        agent_summary: "agent changes".to_string(),
    };

    // Verify serialization works for all variants
    let _ = serde_json::to_string(&disk_wins).unwrap();
    let _ = serde_json::to_string(&agent_wins).unwrap();
    let _ = serde_json::to_string(&merge).unwrap();
    let _ = serde_json::to_string(&conflict).unwrap();
}

#[test]
fn test_reconcile_result_variants() {
    let resolved = ReconcileResult::Resolved {
        path: "/src/main.rs".to_string(),
        resolution: ConflictResolution::DiskWins,
    };
    let needs_resolution = ReconcileResult::NeedsResolution {
        path: "/src/main.rs".to_string(),
        disk_changes: "added line".to_string(),
        agent_changes: "deleted line".to_string(),
    };
    let no_change = ReconcileResult::NoChange {
        path: "/src/main.rs".to_string(),
    };

    // Verify serialization works for all variants
    let _ = serde_json::to_string(&resolved).unwrap();
    let _ = serde_json::to_string(&needs_resolution).unwrap();
    let _ = serde_json::to_string(&no_change).unwrap();
}

// ==================== Manager Types Tests ====================

#[test]
fn test_stream_source_info() {
    let info = StreamSourceInfo {
        source_id: "bluesky".to_string(),
        name: "Bluesky Firehose".to_string(),
        block_schemas: vec![BlockSchemaSpec::pinned(
            "config",
            BlockSchema::Text,
            "Config",
        )],
        status: StreamStatus::Running,
        supports_pull: true,
    };

    assert_eq!(info.source_id, "bluesky");
    assert!(info.supports_pull);
    assert_eq!(info.status, StreamStatus::Running);
}

#[test]
fn test_block_source_info() {
    let info = BlockSourceInfo {
        source_id: "files".to_string(),
        name: "File System".to_string(),
        block_schema: BlockSchemaSpec::ephemeral("file_{path}", BlockSchema::Text, "File content"),
        permission_rules: vec![PermissionRule::new("**/*.rs", MemoryPermission::ReadWrite)],
        status: BlockSourceStatus::Watching,
    };

    assert_eq!(info.source_id, "files");
    assert_eq!(info.status, BlockSourceStatus::Watching);
    assert_eq!(info.permission_rules.len(), 1);
}

#[test]
fn test_edit_feedback_variants() {
    let applied = EditFeedback::Applied {
        message: Some("Success".to_string()),
    };
    let pending = EditFeedback::Pending {
        message: Some("Awaiting confirmation".to_string()),
    };
    let rejected = EditFeedback::Rejected {
        reason: "Permission denied".to_string(),
    };

    // Pattern matching should work
    match applied {
        EditFeedback::Applied { message } => assert!(message.is_some()),
        _ => panic!("Expected Applied"),
    }
    match pending {
        EditFeedback::Pending { message } => assert!(message.is_some()),
        _ => panic!("Expected Pending"),
    }
    match rejected {
        EditFeedback::Rejected { reason } => assert_eq!(reason, "Permission denied"),
        _ => panic!("Expected Rejected"),
    }
}

#[test]
fn test_block_edit_creation() {
    let edit = BlockEdit {
        agent_id: AgentId::new("agent_1"),
        block_id: "block_123".to_string(),
        block_label: "user_profile".to_string(),
        field: Some("name".to_string()),
        old_value: Some(json!("Alice")),
        new_value: json!("Bob"),
    };

    assert_eq!(edit.block_label, "user_profile");
    assert_eq!(edit.field, Some("name".to_string()));
}

// ==================== Stream Status Tests ====================

#[test]
fn test_stream_status_variants() {
    assert_ne!(StreamStatus::Stopped, StreamStatus::Running);
    assert_ne!(StreamStatus::Running, StreamStatus::Paused);
    assert_ne!(StreamStatus::Stopped, StreamStatus::Paused);
}

// ==================== Block Source Status Tests ====================

#[test]
fn test_block_source_status_variants() {
    assert_ne!(BlockSourceStatus::Idle, BlockSourceStatus::Watching);
}

// ==================== Object Safety Tests ====================
//
// These tests verify that DataStream and DataBlock can be used as trait objects.
// This is critical for the SourceManager implementation which stores them as
// Box<dyn DataStream> and Box<dyn DataBlock>.

/// A minimal mock implementation of DataStream for object safety testing.
#[derive(Debug)]
struct MockDataStream {
    id: String,
}

#[async_trait::async_trait]
impl DataStream for MockDataStream {
    fn source_id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Mock Stream"
    }

    fn block_schemas(&self) -> Vec<BlockSchemaSpec> {
        vec![]
    }

    async fn start(
        &self,
        _ctx: Arc<dyn ToolContext>,
        _owner: AgentId,
    ) -> Result<broadcast::Receiver<Notification>> {
        let (tx, rx) = broadcast::channel(16);
        drop(tx); // Close immediately for testing
        Ok(rx)
    }

    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    fn pause(&self) {}

    fn resume(&self) {}

    fn status(&self) -> StreamStatus {
        StreamStatus::Stopped
    }
}

/// A minimal mock implementation of DataBlock for object safety testing.
#[derive(Debug)]
struct MockDataBlock {
    id: String,
    rules: Vec<PermissionRule>,
}

#[async_trait::async_trait]
impl DataBlock for MockDataBlock {
    fn source_id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Mock Block Source"
    }

    fn block_schema(&self) -> BlockSchemaSpec {
        BlockSchemaSpec::ephemeral("mock_{id}", BlockSchema::Text, "Mock block")
    }

    fn permission_rules(&self) -> &[PermissionRule] {
        &self.rules
    }

    fn permission_for(&self, _path: &std::path::Path) -> MemoryPermission {
        MemoryPermission::ReadOnly
    }

    async fn load(
        &self,
        _path: &std::path::Path,
        _ctx: Arc<dyn ToolContext>,
        _owner: AgentId,
    ) -> Result<BlockRef> {
        Ok(BlockRef::new("mock_label", "mock_id"))
    }

    async fn create(
        &self,
        _path: &std::path::Path,
        _initial_content: Option<&str>,
        _ctx: Arc<dyn ToolContext>,
        _owner: AgentId,
    ) -> Result<BlockRef> {
        Ok(BlockRef::new("mock_label", "mock_id"))
    }

    async fn save(&self, _block_ref: &BlockRef, _ctx: Arc<dyn ToolContext>) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _path: &std::path::Path, _ctx: Arc<dyn ToolContext>) -> Result<()> {
        Ok(())
    }

    async fn start_watch(&self) -> Option<broadcast::Receiver<FileChange>> {
        None
    }

    async fn stop_watch(&self) -> Result<()> {
        Ok(())
    }

    fn status(&self) -> BlockSourceStatus {
        BlockSourceStatus::Idle
    }

    async fn reconcile(
        &self,
        _paths: &[PathBuf],
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<ReconcileResult>> {
        Ok(vec![])
    }

    async fn history(
        &self,
        _block_ref: &BlockRef,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<Vec<VersionInfo>> {
        Ok(vec![])
    }

    async fn rollback(
        &self,
        _block_ref: &BlockRef,
        _version: &str,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<()> {
        Ok(())
    }

    async fn diff(
        &self,
        _block_ref: &BlockRef,
        _from: Option<&str>,
        _to: Option<&str>,
        _ctx: Arc<dyn ToolContext>,
    ) -> Result<String> {
        Ok(String::new())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[test]
fn test_data_stream_object_safety() {
    // This test verifies that DataStream can be used as a trait object.
    // If this compiles, the trait is object-safe.
    let stream = MockDataStream {
        id: "test_stream".to_string(),
    };

    // Can create Box<dyn DataStream>
    let boxed: Box<dyn DataStream> = Box::new(stream);

    // Can call methods through the trait object
    assert_eq!(boxed.source_id(), "test_stream");
    assert_eq!(boxed.name(), "Mock Stream");
    assert!(boxed.block_schemas().is_empty());
    assert!(!boxed.supports_pull());
    assert_eq!(boxed.status(), StreamStatus::Stopped);
}

#[test]
fn test_data_block_object_safety() {
    // This test verifies that DataBlock can be used as a trait object.
    // If this compiles, the trait is object-safe.
    let block = MockDataBlock {
        id: "test_block".to_string(),
        rules: vec![PermissionRule::new("**/*.rs", MemoryPermission::ReadWrite)],
    };

    // Can create Box<dyn DataBlock>
    let boxed: Box<dyn DataBlock> = Box::new(block);

    // Can call methods through the trait object
    assert_eq!(boxed.source_id(), "test_block");
    assert_eq!(boxed.name(), "Mock Block Source");
    assert_eq!(boxed.permission_rules().len(), 1);
    assert_eq!(boxed.status(), BlockSourceStatus::Idle);

    // Test the default matches() implementation via trait object
    assert!(boxed.matches(std::path::Path::new("src/main.rs")));
}

#[test]
fn test_data_stream_in_vec() {
    // Verify multiple DataStream trait objects can be stored in a Vec
    let streams: Vec<Box<dyn DataStream>> = vec![
        Box::new(MockDataStream {
            id: "stream1".to_string(),
        }),
        Box::new(MockDataStream {
            id: "stream2".to_string(),
        }),
    ];

    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].source_id(), "stream1");
    assert_eq!(streams[1].source_id(), "stream2");
}

#[test]
fn test_data_block_in_vec() {
    // Verify multiple DataBlock trait objects can be stored in a Vec
    let blocks: Vec<Box<dyn DataBlock>> = vec![
        Box::new(MockDataBlock {
            id: "block1".to_string(),
            rules: vec![],
        }),
        Box::new(MockDataBlock {
            id: "block2".to_string(),
            rules: vec![],
        }),
    ];

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].source_id(), "block1");
    assert_eq!(blocks[1].source_id(), "block2");
}

#[tokio::test]
async fn test_data_stream_lifecycle() {
    use crate::tool::builtin::create_test_context_with_agent;

    // Create a DataStream trait object
    let stream = MockDataStream {
        id: "lifecycle_test_stream".to_string(),
    };
    let boxed: Box<dyn DataStream> = Box::new(stream);

    // Create a test context for the async operations
    let agent_id = "lifecycle_test_agent";
    let (_dbs, _memory, ctx) = create_test_context_with_agent(agent_id).await;
    let owner = AgentId::new(agent_id);

    // Test start() through trait object
    let result = boxed
        .start(ctx.clone() as Arc<dyn ToolContext>, owner)
        .await;
    assert!(
        result.is_ok(),
        "start() should succeed on Box<dyn DataStream>"
    );

    // Test stop() through trait object
    let stop_result = boxed.stop().await;
    assert!(
        stop_result.is_ok(),
        "stop() should succeed on Box<dyn DataStream>"
    );
}

// ==================== Helper Integration Tests ====================
// Note: Unit tests for helpers are in helpers.rs, these test integration scenarios

#[test]
fn test_notification_builder_integration() {
    // Test building a complex notification with multiple blocks
    let block1 = BlockRef::new("user_alice", "user_block_1").owned_by("agent_1");
    let block2 = BlockRef::new("context_current", "ctx_block_1");

    let notification = NotificationBuilder::new()
        .text("User Alice mentioned you in a thread")
        .block(block1.clone())
        .block(block2.clone())
        .build();

    assert_eq!(notification.block_refs.len(), 2);
    assert_eq!(notification.block_refs[0], block1);
    assert_eq!(notification.block_refs[1], block2);
}

#[test]
fn test_ephemeral_block_cache_integration() {
    // Test the cache with synchronous operations only
    let cache = EphemeralBlockCache::new();

    // Verify initial state
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);

    // Test invalidation and clear
    cache.invalidate("nonexistent"); // Should not panic
    cache.clear(); // Should not panic on empty cache
}
