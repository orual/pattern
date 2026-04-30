//! Regression test for the seed-before-subscriber bug.
//!
//! When a block is seeded via the persona-loader path (`create_block`,
//! `doc.import_from_json(...)`, `persist_block`) the import happens BEFORE
//! the subscriber's `subscribe_local_update` callback is installed. The
//! callback only fires for FUTURE updates, so without an initial render in
//! the worker, the seed content never reaches disk until the agent edits
//! the block.
//!
//! This caused `blocks/@<agent>/core/persona.md` to never be written for
//! agents whose persona content is declared in `persona.kdl` and never
//! mutated by the agent thereafter (the persona block is intentionally
//! read-only / append-only). The fix lives in
//! `pattern_memory::subscriber::worker::run_subscriber`, which now performs
//! one `render_cycle` before entering its event loop.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_memory::MemoryCache;

const AGENT: &str = "seed-render-agent";

fn seed_agent(db: &pattern_db::ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("seed-render-{agent_id}"),
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).unwrap();
}

/// Wait for `path` to exist (with the expected substring) up to `timeout`.
/// Returns the file content on success, or panics with the elapsed time.
async fn await_file(path: &std::path::Path, expected_substring: &str, timeout: Duration) -> String {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(path)
            && content.contains(expected_substring)
        {
            return content;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "file {path:?} did not appear with substring {expected_substring:?} within {timeout:?}; \
         exists={}, content={:?}",
        path.exists(),
        std::fs::read_to_string(path).ok()
    );
}

/// Seed a CORE block the same way `seed_persona_memory_blocks` does:
/// create_block → doc.import_from_json → persist_block. The on-disk file
/// must appear with the imported content even though no further mutation
/// happens. Pre-fix, the worker spawned with no events and the file was
/// never rendered — Core blocks like `persona`/`partner` ended up in DB
/// but never on disk.
#[tokio::test]
async fn seeded_core_block_renders_to_disk_without_further_mutation() {
    let db_dir = tempfile::tempdir().unwrap();
    let mount_dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        pattern_db::ConstellationDb::open(
            db_dir.path().join("memory.db"),
            db_dir.path().join("messages.db"),
        )
        .unwrap(),
    );
    seed_agent(&db, AGENT);

    let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (hb_tx, hb_rx) = crossbeam_channel::bounded(128);

    let cache = MemoryCache::new(Arc::clone(&db)).with_mount_path(
        mount_dir.path(),
        reembed_tx,
        hb_tx,
        hb_rx,
    );

    let create = BlockCreate::new("persona", MemoryBlockType::Core, BlockSchema::text());
    let doc = cache.create_block(AGENT, create).unwrap();

    let persona_text = "we/i are pattern. a constellation of processes.";
    doc.import_from_json(&serde_json::Value::String(persona_text.to_string()))
        .expect("import_from_json on Text schema");

    cache.persist_block(AGENT, "persona").unwrap();

    let expected = mount_dir
        .path()
        .join("blocks")
        .join(format!("@{AGENT}"))
        .join("core")
        .join("persona.md");

    let content = await_file(&expected, persona_text, Duration::from_secs(2)).await;
    assert!(
        content.contains(persona_text),
        "rendered file should contain the imported persona text; got:\n{content}"
    );
}

/// Same regression for working blocks. Working blocks happen to render in
/// practice today because agents typically mutate them mid-session, which
/// fires the subscribe_local_update callback. But the seed-time render
/// invariant should hold for any seeded block, not just core.
#[tokio::test]
async fn seeded_working_block_renders_to_disk_without_further_mutation() {
    let db_dir = tempfile::tempdir().unwrap();
    let mount_dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        pattern_db::ConstellationDb::open(
            db_dir.path().join("memory.db"),
            db_dir.path().join("messages.db"),
        )
        .unwrap(),
    );
    seed_agent(&db, AGENT);

    let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (hb_tx, hb_rx) = crossbeam_channel::bounded(128);

    let cache = MemoryCache::new(Arc::clone(&db)).with_mount_path(
        mount_dir.path(),
        reembed_tx,
        hb_tx,
        hb_rx,
    );

    let create = BlockCreate::new("scratchpad", MemoryBlockType::Working, BlockSchema::text());
    let doc = cache.create_block(AGENT, create).unwrap();

    let initial = "working notes for the current session.";
    doc.import_from_json(&serde_json::Value::String(initial.to_string()))
        .expect("import_from_json on Text schema");

    cache.persist_block(AGENT, "scratchpad").unwrap();

    let expected = mount_dir
        .path()
        .join("blocks")
        .join(format!("@{AGENT}"))
        .join("working")
        .join("scratchpad.md");

    let content = await_file(&expected, initial, Duration::from_secs(2)).await;
    assert!(content.contains(initial));
}
