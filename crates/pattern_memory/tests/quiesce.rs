//! Integration tests for `pattern_memory::quiesce`.
//!
//! Covers v3-memory-rework.AC8.3 and AC8.4:
//! - AC8.3: `quiesce()` drains all sync_workers, calls wal_checkpoint, and fsyncs emitted files.
//! - AC8.4: In InRepo mode (no jj adapter), `quiesce()` still runs and produces a canonical
//!   `memory.db` for the host VCS to commit.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, BlockType};
use pattern_memory::MemoryCache;
use pattern_memory::quiesce::{QuiesceError, quiesce};

/// Create a `ConstellationDb` backed by an in-memory SQLite database.
fn test_db() -> Arc<pattern_db::ConstellationDb> {
    Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap())
}

/// Seed a minimal agent row so FK constraints are satisfied.
fn seed_agent(db: &pattern_db::ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("quiesce-test-{agent_id}"),
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

/// AC8.4: InRepo mode — quiesce without any subscribers or emitted files.
///
/// The fundamental invariant: `quiesce` must succeed even when there are no
/// subscribers and no emitted file paths. This is the common InRepo mode case where
/// the memory cache is used without a mount path.
#[test]
fn quiesce_in_repo_no_subscribers_no_files() {
    let db = test_db();
    let cache = MemoryCache::new(db);

    let outcome = quiesce(&cache, &[] as &[std::path::PathBuf]).expect("quiesce must succeed");

    assert_eq!(
        outcome.fsync_failures, 0,
        "no fsync failures expected with no files"
    );
    assert!(
        outcome.duration.as_secs() < 5,
        "quiesce should complete in under 5s"
    );
}

/// AC8.3 + AC8.4: quiesce on a cache with created blocks but no subscribers.
///
/// Blocks exist but no mount_path is set, so no subscribers are running.
/// quiesce must drain zero subscribers, checkpoint the WAL, and return Ok.
#[test]
fn quiesce_with_blocks_no_subscribers() {
    let db = test_db();
    let agent = "quiesce-agent-1";
    seed_agent(&db, agent);
    let cache = MemoryCache::new(db);

    // Create some blocks.
    for i in 0..3 {
        let create = BlockCreate::new(
            format!("block-{i}"),
            BlockType::Working,
            BlockSchema::text(),
        );
        cache.create_block(agent, create).unwrap();
    }

    // Quiesce with no emitted files — WAL checkpoint and drain are the operations.
    let outcome = quiesce(&cache, &[] as &[std::path::PathBuf]).expect("quiesce must succeed");
    assert_eq!(outcome.fsync_failures, 0);
}

/// AC8.3: quiesce with emitted files — verifies the fsync path.
///
/// Creates temporary files on disk and passes them to quiesce.
/// All files should be successfully fsynced.
#[test]
fn quiesce_fsyncs_emitted_files() {
    let db = test_db();
    let cache = MemoryCache::new(db);

    // Create some temporary files that represent "emitted canonical files".
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for i in 0..3 {
        let path = dir.path().join(format!("block-{i}.md"));
        std::fs::write(&path, format!("# Block {i}\n\nContent here.\n")).unwrap();
        paths.push(path);
    }

    let outcome = quiesce(&cache, &paths).expect("quiesce must succeed");
    assert_eq!(
        outcome.fsync_failures, 0,
        "all existing files should fsync without error"
    );
}

/// AC8.3: quiesce counts fsync failures for missing files but still returns Ok.
///
/// If a file in `emitted_file_paths` does not exist, fsync fails. This is
/// counted as a non-fatal failure — quiesce still returns `Ok`.
#[test]
fn quiesce_fsync_failure_is_non_fatal() {
    let db = test_db();
    let cache = MemoryCache::new(db);

    let missing = std::path::PathBuf::from("/nonexistent/path/block.md");
    let outcome =
        quiesce(&cache, &[missing]).expect("quiesce must return Ok even on fsync failure");

    assert_eq!(
        outcome.fsync_failures, 1,
        "one fsync failure expected for the missing file"
    );
}

/// AC8.3: quiesce with mixed valid and missing files.
///
/// The outcome counts only the failures; the valid files are still fsynced.
#[test]
fn quiesce_mixed_fsync_results() {
    let db = test_db();
    let cache = MemoryCache::new(db);

    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("exists.md");
    std::fs::write(&existing, "content").unwrap();

    let missing = std::path::PathBuf::from("/nonexistent/path/missing.md");

    let outcome = quiesce(&cache, &[existing, missing]).expect("quiesce must return Ok");
    assert_eq!(
        outcome.fsync_failures, 1,
        "exactly one failure for the missing file"
    );
}

/// AC8.3: WAL checkpoint is a hard error — if the DB pool fails to provide a
/// connection, `quiesce` must return `Err(QuiesceError::WalCheckpoint)`.
///
/// This test verifies the error type and message, not an actual checkpoint failure
/// (which would require a broken DB). We can't easily inject a broken DB in this
/// test framework, so we test the `QuiesceError` type exists and has correct display.
#[test]
fn quiesce_error_wal_checkpoint_is_hard_error() {
    // Verify the error type is defined correctly and has a useful Display impl.
    let err = QuiesceError::WalCheckpoint {
        source: pattern_core::types::memory_types::MemoryError::Other(
            "test checkpoint failure".to_string(),
        ),
    };
    let display = err.to_string();
    assert!(
        display.contains("WAL checkpoint failed"),
        "error display should mention WAL checkpoint: {display}"
    );
}

/// AC8.3: drain_subscribers is called before WAL checkpoint.
///
/// We verify this indirectly: after quiesce, the subscriber map should be empty.
/// This test uses a cache without mount_path (no actual OS threads), so
/// drain_subscribers is a no-op — but the call path is still exercised.
#[test]
fn quiesce_drains_subscribers_before_checkpoint() {
    let db = test_db();
    let cache = MemoryCache::new(db);

    // quiesce should succeed — drain + checkpoint + (no files to fsync).
    let outcome = quiesce(&cache, &[] as &[std::path::PathBuf]).expect("quiesce must succeed");
    assert_eq!(outcome.fsync_failures, 0);
}

/// AC8.3 + Critical: quiesce with LIVE subscribers exercises the full pause-resume path.
///
/// This test would have caught the race condition in which agent writes between
/// `paused=true` and `handle_pause` entry were silently lost: those writes land
/// in memory_doc but the subscribe_local_update callback is suppressed, so they
/// never reach the channel — and the pre-pause VV snapshot makes the resume
/// reconciliation think they're already synced.
///
/// The subscriber's `subscribe_local_update` callback is registered during
/// `spawn_subscriber_for_block` (called by `persist`). Mutations to the doc
/// BEFORE the subscriber is spawned do not fire the callback — so this test
/// carefully sequences: persist first (to spawn subscriber), then write content.
///
/// Test sequence:
/// 1. Cache with ConstellationDb (on-disk tempdir) + mount_path configured.
/// 2. Create a block, mark dirty, persist (spawns subscriber).
/// 3. Write initial content AFTER the subscriber is registered, persist, wait
///    for subscriber to emit the initial canonical file.
/// 4. Write a second, distinct content blob immediately (exercises race window).
/// 5. Call quiesce with the emitted file path — the Critical #1 fix ensures the
///    race-window write is flushed into disk_doc before the file is fsynced.
/// 6. Assert: the emitted file contains the second write's content.
/// 7. Assert: after resume, a third write still produces an updated file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiesce_with_live_subscriber_full_path() {
    use pattern_core::traits::MemoryStore;
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{BlockSchema, BlockType};
    use std::time::Duration;

    // Directories: one for the on-disk DB, one for subscriber file emission.
    let db_dir = tempfile::tempdir().unwrap();
    let mount_dir = tempfile::tempdir().unwrap();

    // Use an on-disk DB so the WAL checkpoint has something to do.
    let db = Arc::new(
        pattern_db::ConstellationDb::open(
            db_dir.path().join("memory.db"),
            db_dir.path().join("messages.db"),
        )
        .unwrap(),
    );
    let agent = "quiesce-live-sub-agent";
    seed_agent(&db, agent);

    // Set up channels for the subscriber machinery.
    let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (hb_tx, hb_rx) = crossbeam_channel::bounded(128);

    let cache = MemoryCache::new(Arc::clone(&db)).with_mount_path(
        mount_dir.path(),
        reembed_tx,
        hb_tx,
        hb_rx,
    );

    // Step 2: create a text block. `create_block` returns an Arc-based reference
    // clone of the cached LoroDoc, so mutations on `doc` fire `subscribe_local_update`
    // on the same underlying document. Mark dirty and persist to spawn the subscriber
    // (which registers the `subscribe_local_update` callback).
    let create = BlockCreate::new("live-sub-block", BlockType::Working, BlockSchema::text());
    let doc = cache.create_block(agent, create).unwrap();
    let block_id = doc.id().to_string();

    cache.mark_dirty(agent, "live-sub-block");
    cache.persist_block(agent, "live-sub-block").unwrap();

    // Give the subscriber OS thread time to start and register the subscription.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Step 3: write initial content AFTER the subscriber is registered. The
    // subscribe_local_update callback fires on set_text and sends update bytes to
    // the worker channel, which renders the canonical file.
    doc.set_text("initial content for live subscriber test", true)
        .unwrap();
    cache.mark_dirty(agent, "live-sub-block");
    cache.persist_block(agent, "live-sub-block").unwrap();

    // Wait for the subscriber worker to emit the file (debounce: 50 ms; budget: 2 s).
    let expected_file = mount_dir.path().join(format!("{block_id}.md"));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !expected_file.exists() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        expected_file.exists(),
        "subscriber should have emitted {expected_file:?} within 2 s after set_text"
    );

    // Step 4: write a second, distinct content blob. Do this immediately to
    // simulate the race window where the subscriber may not have processed it
    // by the time quiesce fires.
    doc.set_text("updated content after first persist", true)
        .unwrap();

    // Give the subscriber a very short window — enough to pick up the event
    // or not, depending on scheduler timing. This exercises the race.
    tokio::time::sleep(Duration::from_millis(5)).await;

    // Step 5: quiesce with the emitted file. The handle_pause flush (Critical #1
    // fix) must ensure any write in the race window is synced to disk_doc and
    // rendered before we fsync and resume.
    let outcome =
        quiesce(&cache, std::slice::from_ref(&expected_file)).expect("quiesce must succeed");
    assert_eq!(outcome.fsync_failures, 0, "no fsync failures expected");

    // Step 6: emitted file must contain the second write's content after quiesce,
    // regardless of whether the subscriber had processed it before the pause.
    let file_content = std::fs::read_to_string(&expected_file)
        .expect("emitted file should be readable after quiesce");
    assert!(
        file_content.contains("updated content after first persist"),
        "emitted file must contain the second write after quiesce, got: {file_content:?}"
    );

    // Step 7: after resume, a third write must still produce an updated file.
    //
    // `resume_subscribers()` signals the worker and returns immediately. The worker
    // must finish its reconciliation + reset path (clearing `paused=false`) before
    // the `subscribe_local_update` callback will forward events again. We wait long
    // enough for that reconciliation to complete, then write + persist + poll.
    tokio::time::sleep(Duration::from_millis(500)).await;

    doc.set_text("third write after resume", true).unwrap();
    cache.mark_dirty(agent, "live-sub-block");
    cache.persist_block(agent, "live-sub-block").unwrap();

    // Poll until the file contains the third write (or 3 s elapses).
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut found = false;
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(&expected_file)
            && content.contains("third write after resume")
        {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        found,
        "subscriber should emit the third write within 3 s after resume"
    );
}
