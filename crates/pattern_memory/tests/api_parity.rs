//! API parity smoke test — confirms the extraction preserved the public
//! surface of `MemoryCache`, `StructuredDocument`, and `SharedBlockManager`.
//! Covers v3-memory-rework.AC1.2.

use std::sync::Arc;

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockFilter, BlockSchema, BlockType};
use pattern_memory::{MemoryCache, SharedBlockManager};

/// Create a temporary on-disk ConstellationDb for testing.
fn test_db() -> (tempfile::TempDir, Arc<pattern_db::ConstellationDb>) {
    let dir = tempfile::tempdir().unwrap();
    let _db_path = dir.path().join("constellation.db");
    let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
    (dir, db)
}

/// Seed a minimal agent row in the DB so FK constraints are satisfied.
fn seed_agent(db: &pattern_db::ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("smoke-test-{agent_id}"),
        description: None,
        model_provider: "test".to_string(),
        model_name: "test".to_string(),
        system_prompt: "test".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: pattern_db::models::AgentStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("failed to seed agent");
}

#[test]
fn memory_cache_create_get_list_round_trip() {
    let (_dir, db) = test_db();
    let cache = MemoryCache::new(db.clone());
    let agent = "api-parity-agent";
    seed_agent(&db, agent);

    // create_block — returns a StructuredDocument.
    let create = BlockCreate::new("notes", BlockType::Working, BlockSchema::text());
    let doc: StructuredDocument = cache.create_block(agent, create).unwrap();
    assert_eq!(doc.label(), "notes");
    assert_eq!(doc.block_type(), BlockType::Working);

    // get_block — round-trips.
    let fetched = cache.get_block(agent, "notes").unwrap();
    assert!(fetched.is_some());

    // list_blocks — includes the newly created block.
    let all = cache.list_blocks(BlockFilter::by_agent(agent)).unwrap();
    assert!(!all.is_empty());
    assert!(all.iter().any(|m| m.label == "notes"));

    // mark_dirty + persist_block — non-panicking.
    cache.mark_dirty(agent, "notes");
    cache.persist_block(agent, "notes").unwrap();

    // default_char_limit accessor.
    let limit = cache.default_char_limit();
    assert!(limit > 0);
}

#[test]
fn memory_cache_builder_methods() {
    let (_dir, db) = test_db();

    // with_default_char_limit — builder-style.
    let cache = MemoryCache::new(db).with_default_char_limit(4096);
    assert_eq!(cache.default_char_limit(), 4096);
}

#[test]
fn structured_document_text_round_trip() {
    // StructuredDocument is re-exported from pattern_memory.
    let doc = StructuredDocument::new_text();
    let rendered = doc.render();
    assert!(rendered.is_empty(), "new text doc should render empty");

    // set_text + render.
    let doc = StructuredDocument::new(BlockSchema::text());
    doc.set_text("hello world", false).unwrap();
    let rendered = doc.render();
    assert!(rendered.contains("hello world"));
}

#[test]
fn shared_block_manager_permission_helpers() {
    use pattern_core::types::memory_types::MemoryPermission;
    // Static permission helpers (no DB needed).
    assert!(SharedBlockManager::can_write(MemoryPermission::ReadWrite));
    assert!(!SharedBlockManager::can_write(MemoryPermission::ReadOnly));
    assert!(!SharedBlockManager::can_delete(MemoryPermission::ReadOnly));
}

#[tokio::test]
async fn shared_block_manager_constructs_with_db() {
    let (_dir, db) = test_db();
    let agent = "sbm-agent";
    seed_agent(&db, agent);

    let sbm = SharedBlockManager::new(db.clone());

    // get_blocks_shared_with on a fresh agent returns empty.
    let shared = sbm.get_blocks_shared_with(agent).await.unwrap();
    assert!(shared.is_empty());
}
