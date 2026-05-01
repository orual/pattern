//! Regression test for seeded memory block content persistence.
//!
//! Reproduces a bug where `import_from_json` content on a document returned by
//! `create_block` was lost after `persist_block` + `get_rendered_content`.
//! The seed flow is: create_block → import_from_json → persist_block →
//! get_rendered_content, which must return the imported content.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope};
use pattern_memory::MemoryCache;
use serde_json::json;

/// Create a temporary in-memory ConstellationDb for testing.
fn test_db() -> (tempfile::TempDir, Arc<pattern_db::ConstellationDb>) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
    (dir, db)
}

/// Seed a minimal agent row in the DB so FK constraints are satisfied.
fn seed_agent(db: &pattern_db::ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("test-{agent_id}"),
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

/// Core bug reproduction: create_block → import_from_json → persist_block →
/// get_rendered_content should return the imported content, not empty string.
#[test]
fn seed_content_survives_persist_and_get() {
    let (_dir, db) = test_db();
    let cache = MemoryCache::new(db.clone());
    let agent = "seed-test-agent";
    seed_agent(&db, agent);

    let agent_scope = Scope::global(agent);

    // Step 1: create_block (like seed_persona_memory_blocks does).
    let create = BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text());
    let doc = cache.create_block(&agent_scope, create).unwrap();

    // Step 2: import content (like seed_persona_memory_blocks does).
    let content = json!("I am a helpful ADHD support agent.");
    doc.import_from_json(&content).unwrap();

    // Verify: content is present on the returned doc.
    assert_eq!(
        doc.text_content(),
        "I am a helpful ADHD support agent.",
        "import_from_json should have set the text content"
    );

    // Step 3: persist_block (like seed_persona_memory_blocks does).
    cache.persist_block(&agent_scope, "persona").unwrap();

    // Step 4: get_rendered_content (like the Memory.Get handler does).
    let rendered = cache
        .get_rendered_content(&agent_scope, "persona")
        .unwrap()
        .expect("block should exist");

    assert_eq!(
        rendered, "I am a helpful ADHD support agent.",
        "get_rendered_content must return the seeded content, not empty string"
    );
}

/// Variant: verify that the cache entry sees the imported content even before persist.
#[test]
fn cache_sees_imported_content_before_persist() {
    let (_dir, db) = test_db();
    let cache = MemoryCache::new(db.clone());
    let agent = "cache-see-agent";
    seed_agent(&db, agent);

    let agent_scope = Scope::global(agent);

    let create = BlockCreate::new("scratchpad", MemoryBlockType::Working, BlockSchema::text());
    let doc = cache.create_block(&agent_scope, create).unwrap();

    doc.import_from_json(&json!("scratch content")).unwrap();

    // Get from cache before persist — should see the imported content
    // because LoroDoc clone shares internal state.
    let cached_doc = cache
        .get_block(&agent_scope, "scratchpad")
        .unwrap()
        .expect("block should be in cache");

    assert_eq!(
        cached_doc.text_content(),
        "scratch content",
        "cache entry should see imported content via shared LoroDoc"
    );
}

/// Verify that VersionVector changes after writes and export_updates_since works.
#[test]
fn loro_version_vector_tracks_writes() {
    use loro::{ExportMode, LoroDoc};

    let doc = LoroDoc::new();
    let vv_empty = doc.oplog_vv();

    let text = doc.get_text("content");
    text.insert(0, "hello").unwrap();

    let vv_after = doc.oplog_vv();
    assert_ne!(vv_empty, vv_after, "VV must change after a write operation");

    // export_updates_since should produce non-empty blob.
    let updates = doc.export(ExportMode::updates(&vv_empty)).unwrap();
    assert!(
        !updates.is_empty(),
        "export_updates_since(empty_vv) must return non-empty blob after writes"
    );
}

/// Detailed test: check that the snapshot export of an empty doc, when loaded
/// back, produces a doc whose VV is the same. This matters because create_block
/// stores the snapshot and VV separately; if they mismatch, load_from_db may
/// produce incorrect results.
#[test]
fn empty_doc_snapshot_roundtrip_preserves_vv() {
    use loro::{ExportMode, LoroDoc};

    let doc = LoroDoc::new();
    let vv_original = doc.oplog_vv();
    let snapshot = doc.export(ExportMode::Snapshot).unwrap();

    // Load from snapshot.
    let doc2 = LoroDoc::new();
    doc2.import(&snapshot).unwrap();
    let vv_loaded = doc2.oplog_vv();

    // The loaded doc's VV should match the original.
    // If this fails, load_from_db might create a doc with wrong VV.
    assert_eq!(
        vv_original, vv_loaded,
        "VV mismatch after snapshot roundtrip"
    );
}

/// Check what happens when create_block's frontier is stored and then
/// content is imported — does persist see different versions?
#[test]
fn persist_does_not_skip_after_import() {
    use loro::LoroDoc;

    let doc = LoroDoc::new();
    let vv_at_create = doc.oplog_vv();

    // Now write content (simulating import_from_json after create_block).
    let text = doc.get_text("content");
    text.insert(0, "some content").unwrap();

    let vv_after_write = doc.oplog_vv();

    // The persist skip check is: doc.current_version() == last_persisted_frontier
    // last_persisted_frontier was set to vv_at_create.
    // doc.current_version() is now vv_after_write.
    // These MUST be different for persist to proceed.
    assert_ne!(
        vv_at_create, vv_after_write,
        "Version vectors must differ after write — persist must NOT skip. \
         If this fails, persist would skip and content would be lost!"
    );

    // Also check via clone (simulating that the cache has a clone).
    let clone = doc.clone();
    assert_eq!(
        clone.oplog_vv(),
        vv_after_write,
        "Clone's VV must match original (reference clone)"
    );
}

/// Verify LoroDoc clone is a reference clone (shared state).
/// If this test fails, our assumption about LoroDoc::clone() sharing state is wrong.
#[test]
fn loro_doc_clone_shares_state() {
    use loro::LoroDoc;

    let doc = LoroDoc::new();
    let clone = doc.clone();

    // Write to original.
    let text = doc.get_text("content");
    text.insert(0, "hello from original").unwrap();

    // Clone should see it.
    let clone_text = clone.get_text("content");
    let clone_content = clone_text.to_string();
    assert_eq!(
        clone_content, "hello from original",
        "LoroDoc::clone() should be a reference clone sharing state"
    );

    // Version vectors should match.
    assert_eq!(
        doc.oplog_vv(),
        clone.oplog_vv(),
        "version vectors should match for reference clones"
    );
}

/// Variant: verify content survives a full DB roundtrip (evict from cache, reload).
#[test]
fn seed_content_survives_db_roundtrip() {
    let (_dir, db) = test_db();
    let cache = MemoryCache::new(db.clone());
    let agent = "db-roundtrip-agent";
    seed_agent(&db, agent);

    let agent_scope = Scope::global(agent);

    let create = BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text());
    let doc = cache.create_block(&agent_scope, create).unwrap();
    doc.import_from_json(&json!("persona description text"))
        .unwrap();
    cache.persist_block(&agent_scope, "persona").unwrap();

    // Drop the cache entirely and create a fresh one — forces DB reload.
    drop(cache);
    let cache2 = MemoryCache::new(db.clone());

    let rendered = cache2
        .get_rendered_content(&agent_scope, "persona")
        .unwrap()
        .expect("block should exist in DB");

    assert_eq!(
        rendered, "persona description text",
        "content must survive full DB roundtrip (cache eviction + reload)"
    );
}

/// Verify that persist_block on a freshly created + imported block actually
/// writes to the DB (not skipped). This is the core regression: if persist
/// sets last_persisted_frontier at create time and the import doesn't change
/// the version (hypothetically), persist would skip and content would be lost
/// on restart.
#[test]
fn persist_after_import_writes_to_db() {
    let (_dir, db) = test_db();
    let cache = MemoryCache::new(db.clone());
    let agent = "persist-writes-agent";
    seed_agent(&db, agent);

    let agent_scope = Scope::global(agent);

    let create = BlockCreate::new("notes", MemoryBlockType::Working, BlockSchema::text());
    let doc = cache.create_block(&agent_scope, create).unwrap();
    let block_id = doc.id().to_string();

    doc.import_from_json(&json!("important notes")).unwrap();
    cache.persist_block(&agent_scope, "notes").unwrap();

    // Check the DB directly: there should be at least one update row.
    let conn = db.get().unwrap();
    let updates = pattern_db::queries::get_updates_since(&conn, &block_id, 0).unwrap();
    assert!(
        !updates.is_empty(),
        "persist must have stored at least one update in the DB"
    );

    // Verify the update, when applied to an empty doc, produces the content.
    let fresh_doc = pattern_core::memory::StructuredDocument::new(BlockSchema::text());
    for update in &updates {
        fresh_doc.apply_updates(&update.update_blob).unwrap();
    }
    assert_eq!(
        fresh_doc.text_content(),
        "important notes",
        "DB updates must reconstruct the imported content"
    );
}

/// Variant: persist_block on a freshly created block with NO content changes
/// should still succeed without error, even though there's nothing to persist.
#[test]
fn persist_empty_block_is_harmless() {
    let (_dir, db) = test_db();
    let cache = MemoryCache::new(db.clone());
    let agent = "persist-empty-agent";
    seed_agent(&db, agent);

    let agent_scope = Scope::global(agent);

    let create = BlockCreate::new("empty", MemoryBlockType::Working, BlockSchema::text());
    let _doc = cache.create_block(&agent_scope, create).unwrap();

    // Persist without any content changes. Should not error.
    cache.persist_block(&agent_scope, "empty").unwrap();

    // Content should be empty.
    let rendered = cache
        .get_rendered_content(&agent_scope, "empty")
        .unwrap()
        .expect("block should exist");
    assert_eq!(rendered, "", "empty block should render as empty string");
}
