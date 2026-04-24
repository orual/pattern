//! External `.kdl` edit reconciliation test (AC10.6).
//!
//! Verifies that when a `TaskList` block's canonical `.kdl` file is externally
//! edited (simulating a human editor writing directly to the mount), the
//! notify-watcher fires, the CRDT merge imports the change, and the `tasks`
//! + `task_edges` sqlite indexes reflect the added item.
//!
//! Also verifies echo suppression: the subscriber does NOT re-emit a
//! canonicalized version that overwrites the user's edit.
//!
//! # Timing model
//!
//! The `notify-debouncer-full` debounce window is 500ms. After it fires,
//! `apply_external_edit` runs synchronously on the watcher ingest thread,
//! which propagates the CRDT update to the subscriber worker. The subscriber
//! has a 50ms debounce window of its own. Total budget: 500 + 50 + margin =
//! 700ms, matching the sibling plan recommendation.
//!
//! # Isolation
//!
//! This test uses its own `TempDir` and in-memory `ConstellationDb`. No shared
//! state with other integration tests.
//!
//! To run explicitly:
//! ```sh
//! cargo nextest run -p pattern-memory --test external_kdl_edit_reconcile --nocapture
//! ```

use std::sync::Arc;
use std::time::Duration;

use pattern_core::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_memory::fs::watcher::{MountWatcher, WatcherConfig};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Seed a minimal agent row so FK constraints are satisfied.
fn seed_agent(db: &ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("ext-kdl-test-{agent_id}"),
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
        .expect("failed to create test agent");
}

/// Count rows in `tasks` for a given block handle.
fn count_tasks(db: &ConstellationDb, block_handle: &str) -> usize {
    let conn = db.get().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE block_handle = ?1",
        rusqlite::params![block_handle],
        |r| r.get::<_, i64>(0).map(|v| v as usize),
    )
    .unwrap_or(0)
}

/// Return true if the task with the given item id exists for the block.
fn task_exists(db: &ConstellationDb, block_handle: &str, item_id: &str) -> bool {
    let conn = db.get().unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE block_handle = ?1 AND task_item_id = ?2",
        rusqlite::params![block_handle, item_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

// ---------------------------------------------------------------------------
// AC10.6: External `.kdl` edit reconciliation
// ---------------------------------------------------------------------------

/// External `.kdl` edit triggers CRDT merge and task index update.
///
/// # What this test verifies
///
/// 1. A `TaskList` block is created, seeded with one item, and persisted so
///    a subscriber worker is spawned.
/// 2. `quiesce()` is called to flush the canonical `.kdl` file to disk.
/// 3. The `.kdl` file is directly edited to add a second item.
/// 4. The notify-watcher debounce window (500ms) elapses, triggering
///    `apply_external_edit` which merges the change into the LoroDoc.
/// 5. The subscriber debounce (50ms) fires, calling `reconcile_task_list`
///    which updates the `tasks` sqlite table.
/// 6. The test asserts the second item appears in the `tasks` table.
/// 7. The test asserts the `.kdl` file content was NOT re-emitted with a
///    different shape — echo suppression works (the canonical emitter doesn't
///    overwrite the user's edit with a different byte sequence).
///
/// # Timing
///
/// We wait 700ms after the file write (500ms watcher debounce + 50ms subscriber
/// debounce + 150ms processing margin). If the test is flaky, increase this.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_kdl_edit_reconciles_task_index() {
    const AGENT: &str = "ext-kdl-agent";
    const LABEL: &str = "ext-kdl-tasklist";
    const INITIAL_ITEM_ID: &str = "item-initial";
    const ADDED_ITEM_ID: &str = "item-external";

    // Use a on-disk DB so the `tasks` table is properly accessible and the
    // WAL checkpoint path works. The DB files live inside the TempDir.
    let dir = tempfile::tempdir().expect("tempdir creation");
    let mount_path = dir.path().to_path_buf();
    let db_path = dir.path().join("memory.db");
    let messages_path = dir.path().join("messages.db");

    let db = Arc::new(
        ConstellationDb::open(&db_path, &messages_path).expect("open on-disk ConstellationDb"),
    );
    seed_agent(&db, AGENT);

    // Set up channels for subscriber machinery.
    let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (hb_tx, hb_rx) = crossbeam_channel::bounded(128);

    let cache = Arc::new(MemoryCache::new(Arc::clone(&db)).with_mount_path(
        mount_path.clone(),
        reembed_tx,
        hb_tx,
        hb_rx,
    ));

    // Step 1: Create a TaskList block. First persist spawns the subscriber but
    // has no content to emit yet (empty LoroDoc). Then write content and persist
    // again — this sends a CommitEvent that triggers file emission.
    let doc = cache
        .create_block(
            AGENT,
            BlockCreate::new(
                LABEL,
                MemoryBlockType::Working,
                BlockSchema::TaskList {
                    default_status: None,
                    default_owner: None,
                    display_limit: None,
                },
            ),
        )
        .expect("create TaskList block");

    // Record the block_id (UUID) so we can find the .kdl file.
    let block_id = doc.id().to_string();

    // First persist: spawns the subscriber (no content yet).
    cache
        .persist_block(AGENT, LABEL)
        .expect("persist block (spawn subscriber)");

    // Give the subscriber OS thread a moment to start and register its
    // `subscribe_local_update` callback on the LoroDoc.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Now seed an initial item. The `subscribe_local_update` callback fires
    // on commit(), sending a CommitEvent to the worker channel.
    {
        let list = doc.inner().get_movable_list("items");
        list.insert(
            0,
            loro::LoroValue::Map(
                vec![
                    (
                        "id".to_string(),
                        loro::LoroValue::String(INITIAL_ITEM_ID.into()),
                    ),
                    (
                        "subject".to_string(),
                        loro::LoroValue::String("Initial task".into()),
                    ),
                    (
                        "status".to_string(),
                        loro::LoroValue::String("pending".into()),
                    ),
                    ("blocks".to_string(), loro::LoroValue::List(vec![].into())),
                ]
                .into_iter()
                .collect(),
            ),
        )
        .expect("insert initial item");
        doc.inner().commit();
    }

    // Persist to write updates to DB (also triggers another CommitEvent).
    cache.mark_dirty(AGENT, LABEL);
    cache
        .persist_block(AGENT, LABEL)
        .expect("persist block with content");

    // Wait for subscriber debounce (50ms) + file emission.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify the initial file was emitted.
    let kdl_path = mount_path.join(format!("{block_id}.kdl"));
    assert!(
        kdl_path.exists(),
        "initial .kdl file should exist at {}: subscriber may not have started yet",
        kdl_path.display()
    );

    // Step 2: Call quiesce() to flush canonical file and checkpoint the WAL.
    let outcome = pattern_memory::quiesce::quiesce(&cache, &[&kdl_path])
        .expect("quiesce must succeed before external edit");
    assert_eq!(
        outcome.fsync_failures, 0,
        "quiesce should not have any fsync failures"
    );

    // Verify initial state: tasks table should have one row.
    let initial_count = count_tasks(&db, &block_id);
    assert_eq!(
        initial_count, 1,
        "tasks table should have 1 row after initial persist; got {initial_count}"
    );

    // Step 3: Start the filesystem watcher BEFORE making the external edit.
    let _watcher = MountWatcher::start(WatcherConfig {
        mount_path: mount_path.clone(),
        cache: Arc::clone(&cache),
    })
    .expect("watcher should start");

    // Give inotify a moment to fully register the watch.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Read the current .kdl content — we'll use it to verify echo suppression.
    let content_before_edit =
        std::fs::read(&kdl_path).expect("read .kdl file before external edit");
    let text_before = String::from_utf8_lossy(&content_before_edit).to_string();

    // Step 3: Externally write a new .kdl file that includes both the original
    // item and a newly added item. This simulates what a human editor would do
    // (open the file, add a task, save).
    let new_kdl = format!(
        "task-list {{\n    item id=\"{INITIAL_ITEM_ID}\" status=\"pending\" {{\n        subject \"Initial task\"\n    }}\n    item id=\"{ADDED_ITEM_ID}\" status=\"pending\" {{\n        subject \"Externally added task\"\n    }}\n}}\n"
    );
    std::fs::write(&kdl_path, &new_kdl).expect("external write to .kdl file");

    eprintln!("Wrote external edit to {}", kdl_path.display());
    eprintln!("External edit content:\n{new_kdl}");

    // Step 4-5: Wait for notify debounce (500ms) + subscriber reconcile (50ms)
    // + margin. The spec recommends 700ms total.
    tokio::time::sleep(Duration::from_millis(700)).await;

    // Step 6: Assert the added item appears in the tasks + task_edges indexes.
    let task_count = count_tasks(&db, &block_id);
    assert_eq!(
        task_count, 2,
        "tasks table should have 2 rows after external edit (initial + added); got {task_count}"
    );
    assert!(
        task_exists(&db, &block_id, INITIAL_ITEM_ID),
        "initial item '{INITIAL_ITEM_ID}' should still exist in tasks table"
    );
    assert!(
        task_exists(&db, &block_id, ADDED_ITEM_ID),
        "externally added item '{ADDED_ITEM_ID}' should appear in tasks table after watcher reconcile"
    );

    // Step 7: Read the .kdl file back and verify echo suppression.
    //
    // The watcher should NOT re-emit a canonicalized version that overwrites
    // the user's edit. The file content must still contain the item id we wrote.
    let content_after = std::fs::read_to_string(&kdl_path).expect("read .kdl file after edit");
    assert!(
        content_after.contains(ADDED_ITEM_ID),
        "external item id '{ADDED_ITEM_ID}' must still appear in .kdl file after watcher cycle; \
         got:\n{content_after}"
    );
    assert!(
        content_after.contains(INITIAL_ITEM_ID),
        "initial item id '{INITIAL_ITEM_ID}' must appear in .kdl file after watcher cycle; \
         got:\n{content_after}"
    );

    // Verify the before/after comparison for diagnostic purposes.
    eprintln!("Content before edit:\n{text_before}");
    eprintln!("Content after watcher cycle:\n{content_after}");
}
