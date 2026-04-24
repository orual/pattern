//! Multi-agent concurrent stress test.
//!
//! N threads do parallel writes against a shared `MemoryCache` backed by a
//! single `memory.db`. Proves no deadlock and no data loss under contention.
//!
//! Verifies: v3-memory-rework.AC15.6.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockFilter, BlockSchema, MemoryBlockType, MemoryError};
use pattern_db::{ConstellationDb, Json, models};
use pattern_memory::MemoryCache;

/// Retry an operation up to 5 times on transient SQLite "database is locked"
/// errors, with exponential backoff (50ms → 100ms → 200ms → ...).
fn retry_on_locked<T>(mut f: impl FnMut() -> Result<T, MemoryError>) -> Result<T, MemoryError> {
    let mut delay = Duration::from_millis(50);
    for attempt in 0..5 {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) if is_locked_error(&e) && attempt < 4 => {
                std::thread::sleep(delay);
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Check if a MemoryError wraps a SQLite "database is locked" error.
fn is_locked_error(e: &MemoryError) -> bool {
    let msg = format!("{e}");
    msg.contains("database is locked")
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Open a ConstellationDb pair (on-disk so WAL contention is realistic).
fn open_test_db(dir: &std::path::Path) -> Arc<ConstellationDb> {
    let memory_path = dir.join("memory.db");
    let messages_path = dir.join("messages.db");
    Arc::new(ConstellationDb::open(memory_path, messages_path).unwrap())
}

/// Seed N agent rows for FK constraints.
fn seed_agents(db: &ConstellationDb, count: usize) {
    let conn = db.get().unwrap();
    for i in 0..count {
        let agent = models::Agent {
            id: format!("stress-agent-{i}"),
            name: format!("stress-agent-{i}"),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test".to_string(),
            system_prompt: "test".to_string(),
            config: Json(serde_json::json!({})),
            enabled_tools: Json(vec![]),
            tool_rules: None,
            status: models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(&conn, &agent).expect("seed agent");
    }
}

// ---------------------------------------------------------------------------
// AC15.6: concurrent stress test
// ---------------------------------------------------------------------------

/// Concurrent writes from N threads against a shared MemoryCache.
///
/// Each thread creates `writes_per_agent` blocks and writes text content.
/// After all threads join, we verify the exact block count landed in the DB.
///
/// Uses a 60-second timeout so a deadlock fails the test rather than hangs CI.
#[tokio::test]
async fn concurrent_memory_cache_stress() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_test_db(tmp.path());

    // Use enough agents and writes to exercise real contention, but stay
    // within SQLite's single-writer busy_timeout (5s). Production agents
    // would typically not all write simultaneously.
    let n_agents: usize = 5;
    let writes_per_agent: usize = 10;

    seed_agents(&db, n_agents);

    let cache = Arc::new(MemoryCache::new(Arc::clone(&db)));

    let mut handles = Vec::with_capacity(n_agents);
    for i in 0..n_agents {
        let cache_clone = Arc::clone(&cache);
        let handle = tokio::task::spawn_blocking(move || {
            let agent_id = format!("stress-agent-{i}");
            for turn in 0..writes_per_agent {
                let label = format!("block-{i}-{turn}");

                // Retry on transient SQLite "database is locked" errors.
                // WAL mode allows only one writer at a time; under heavy
                // concurrency the busy_timeout may be exhausted if many
                // writers queue up simultaneously.
                let doc = retry_on_locked(|| {
                    cache_clone.create_block(
                        &agent_id,
                        BlockCreate::new(&label, MemoryBlockType::Working, BlockSchema::text()),
                    )
                })
                .unwrap_or_else(|e| panic!("create block {label} failed after retries: {e}"));

                doc.set_text(&format!("content {i}:{turn}"), false)
                    .unwrap_or_else(|e| panic!("set_text {label} failed: {e}"));

                cache_clone.mark_dirty(&agent_id, &label);
                retry_on_locked(|| cache_clone.persist_block(&agent_id, &label))
                    .unwrap_or_else(|e| panic!("persist {label} failed after retries: {e}"));
            }
        });
        handles.push(handle);
    }

    // Join all with a timeout so deadlocks fail cleanly.
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        futures::future::try_join_all(handles),
    )
    .await;

    let joined = result
        .expect("stress test must complete within 60s (no deadlock)")
        .expect("no task panics");
    assert_eq!(joined.len(), n_agents);

    // Verify all writes landed by listing blocks.
    let all_blocks = cache
        .list_blocks(BlockFilter::by_prefix("block-"))
        .expect("list_blocks after stress");

    assert_eq!(
        all_blocks.len(),
        n_agents * writes_per_agent,
        "expected {} blocks, got {}",
        n_agents * writes_per_agent,
        all_blocks.len()
    );

    // Spot-check: each agent should have exactly writes_per_agent blocks.
    for i in 0..n_agents {
        let agent_id = format!("stress-agent-{i}");
        let agent_blocks = cache
            .list_blocks(BlockFilter::by_agent(&agent_id))
            .expect("list per agent");
        assert_eq!(
            agent_blocks.len(),
            writes_per_agent,
            "agent {agent_id} should have {writes_per_agent} blocks, got {}",
            agent_blocks.len()
        );
    }
}

/// Concurrent writes from N **separate** MemoryCache instances pointing at the
/// same ConstellationDb.
///
/// Each thread receives its own `MemoryCache::new(db.clone())` — NOT an
/// `Arc::clone` of the same cache. This exercises the r2d2 connection pool
/// contention path and SQLite WAL behaviour across distinct cache objects,
/// rather than the DashMap contention path tested by
/// `concurrent_memory_cache_stress`.
///
/// After all threads join, we verify that writes from every cache instance
/// landed in the shared DB.
///
/// Verifies: v3-memory-rework.AC15.7 (pool + WAL contention across instances).
#[tokio::test]
async fn concurrent_multi_cache_stress() {
    let tmp = tempfile::tempdir().unwrap();
    let db = open_test_db(tmp.path());

    let n_caches: usize = 4;
    let writes_per_cache: usize = 8;

    // All caches share a single agent namespace so we can count total blocks.
    seed_agents(&db, n_caches);

    let mut handles = Vec::with_capacity(n_caches);
    for i in 0..n_caches {
        // Each thread gets its own MemoryCache backed by the same Arc<Db>.
        let cache = MemoryCache::new(Arc::clone(&db));
        let handle = tokio::task::spawn_blocking(move || {
            let agent_id = format!("stress-agent-{i}");
            for turn in 0..writes_per_cache {
                let label = format!("mc-block-{i}-{turn}");

                let doc = retry_on_locked(|| {
                    cache.create_block(
                        &agent_id,
                        BlockCreate::new(&label, MemoryBlockType::Working, BlockSchema::text()),
                    )
                })
                .unwrap_or_else(|e| panic!("create_block {label} failed: {e}"));

                doc.set_text(&format!("multi-cache {i}:{turn}"), false)
                    .unwrap_or_else(|e| panic!("set_text {label} failed: {e}"));

                cache.mark_dirty(&agent_id, &label);
                retry_on_locked(|| cache.persist_block(&agent_id, &label))
                    .unwrap_or_else(|e| panic!("persist {label} failed: {e}"));
            }
        });
        handles.push(handle);
    }

    let result = tokio::time::timeout(
        Duration::from_secs(60),
        futures::future::try_join_all(handles),
    )
    .await;

    let joined = result
        .expect("multi-cache stress test must complete within 60s (no deadlock)")
        .expect("no task panics");
    assert_eq!(joined.len(), n_caches);

    // Verify all writes across all separate cache instances landed in the DB.
    // Use a fresh cache on the same DB to query — this ensures we're reading
    // from the DB, not any in-memory state.
    let verify_cache = MemoryCache::new(Arc::clone(&db));
    let all_blocks = verify_cache
        .list_blocks(BlockFilter::by_prefix("mc-block-"))
        .expect("list_blocks after multi-cache stress");

    assert_eq!(
        all_blocks.len(),
        n_caches * writes_per_cache,
        "expected {} blocks across all cache instances, got {}",
        n_caches * writes_per_cache,
        all_blocks.len()
    );

    // Spot-check per-agent block count.
    for i in 0..n_caches {
        let agent_id = format!("stress-agent-{i}");
        let agent_blocks = verify_cache
            .list_blocks(BlockFilter::by_agent(&agent_id))
            .expect("list per agent");
        assert_eq!(
            agent_blocks.len(),
            writes_per_cache,
            "cache-instance {i} (agent {agent_id}) should have {writes_per_cache} blocks, got {}",
            agent_blocks.len()
        );
    }
}
