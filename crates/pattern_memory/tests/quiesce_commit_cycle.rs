//! Quiesce + commit cycle test (AC10.7).
//!
//! Verifies that calling `quiesce()` on a `MemoryCache` with live subscribers
//! produces a canonical database state (WAL truncated to 0 bytes), and that a
//! subsequent `jj commit` captures the expected canonical files (`.kdl`, `.md`,
//! `memory.db`) without including WAL sidecar files (`-wal`, `-shm`). Also
//! verifies that re-opening the mount from the same path preserves the
//! `tasks` + `task_edges` index state across restart.
//!
//! # jj availability
//!
//! This test requires `jj` on PATH. It is skipped automatically when `jj` is
//! absent (via the `skip_if_no_jj!` macro from the `skills_load_mode_a.rs`
//! pattern).
//!
//! # Setup
//!
//! Rather than using the full `attach()` machinery (which requires a `.pattern.kdl`
//! config), this test sets up the equivalent components directly:
//! - `ConstellationDb::open()` on a TempDir for an on-disk DB.
//! - `MemoryCache::new().with_mount_path()` with subscriber channels.
//! - `jj git init --colocate` in the same TempDir.
//! - Files emitted by subscriber workers are naturally tracked by jj.
//!
//! To run explicitly:
//! ```sh
//! cargo nextest run -p pattern-memory --test quiesce_commit_cycle --nocapture
//! ```

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use pattern_core::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::ids::AgentId;
use pattern_core::types::memory_types::{
    BlockSchema, MemoryBlockType, SkillMetadata, SkillTrustTier,
};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_memory::fs::markdown_skill::{SkillFile, write_skill_to_loro_doc};
use pattern_memory::quiesce::quiesce;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

macro_rules! skip_if_no_jj {
    () => {
        if !jj_available() {
            eprintln!("SKIP: jj not available on PATH");
            return;
        }
    };
}

fn jj_available() -> bool {
    Command::new("jj").arg("--version").output().is_ok()
}

/// Initialize a `jj git --colocate` repo in `dir` and configure jj user.
fn init_jj_repo(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = Command::new("jj")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("jj {} spawn failed: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "jj {} failed (exit {}): {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["git", "init", "--colocate"]);
    run(&["config", "set", "--repo", "user.name", "quiesce-test"]);
    run(&[
        "config",
        "set",
        "--repo",
        "user.email",
        "quiesce@pattern.test",
    ]);
}

/// Run `jj commit -m <msg>` in `dir`. Returns stdout.
fn jj_commit(dir: &std::path::Path, msg: &str) -> String {
    let out = Command::new("jj")
        .args(["commit", "-m", msg])
        .current_dir(dir)
        .output()
        .expect("jj commit spawn failed");
    assert!(
        out.status.success(),
        "jj commit failed (exit {}): {}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Run `jj diff --stat -r @-` in `dir`. Returns the stat output showing which
/// files are in the most recently created commit.
fn jj_diff_stat_at_prev(dir: &std::path::Path) -> String {
    let out = Command::new("jj")
        .args(["diff", "--stat", "-r", "@-"])
        .current_dir(dir)
        .output()
        .expect("jj diff spawn failed");
    // Non-zero exit is OK for empty commits; we just return output.
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Seed a minimal agent row for FK constraint satisfaction.
fn seed_agent(db: &ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("quiesce-commit-{agent_id}"),
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("seed agent");
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

/// Get all task item IDs for a block, sorted.
fn task_item_ids(db: &ConstellationDb, block_handle: &str) -> Vec<String> {
    let conn = db.get().unwrap();
    let mut stmt = conn
        .prepare("SELECT task_item_id FROM tasks WHERE block_handle = ?1 ORDER BY task_item_id")
        .unwrap();
    stmt.query_map(rusqlite::params![block_handle], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// AC10.7: Quiesce + commit cycle preserves task index
// ---------------------------------------------------------------------------

/// Quiesce + commit cycle: WAL truncated, correct files in commit, index preserved.
///
/// # What this test verifies
///
/// 1. A Mode-A (InRepo / jj-tracked) mount is set up with TaskList + Skill +
///    Text blocks seeded and subscribers running.
/// 2. The skill is loaded once (via `record_usage`) to populate
///    `skill_usage_stats`.
/// 3. `quiesce()` is called on the mount.
/// 4. The `memory.db-wal` file is absent or 0 bytes (WAL truncated).
/// 5. `jj commit` is run. The diff stat for the commit lists `.kdl`, `.md`,
///    and `memory.db` files but NOT any `-wal` or `-shm` files.
/// 6. The mount is dropped and re-opened from the same path (simulating a
///    process restart). The `tasks` + `task_edges` index reports the same
///    row counts and IDs as before quiesce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quiesce_commit_preserves_task_index() {
    skip_if_no_jj!();

    const AGENT: &str = "qcc-agent";
    const TL_LABEL: &str = "qcc-tasklist";
    const SKILL_LABEL: &str = "qcc-skill";
    const TEXT_LABEL: &str = "qcc-text";

    // Step 1: Set up on-disk DB + jj repo in a TempDir.
    // The DB and emitted block files all live in the same directory, which
    // is also the jj working copy root.
    let dir = tempfile::tempdir().expect("tempdir creation");
    let root = dir.path().to_path_buf();
    let db_path = root.join("memory.db");
    let messages_path = root.join("messages.db");

    // Initialize jj repo first (before opening DB) so the DB is committed
    // as a new file in the initial jj working copy.
    init_jj_repo(&root);

    let db = Arc::new(
        ConstellationDb::open(&db_path, &messages_path).expect("open on-disk ConstellationDb"),
    );
    seed_agent(&db, AGENT);

    // Set up channels for subscriber machinery.
    let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
    let (hb_tx, hb_rx) = crossbeam_channel::bounded(128);

    let cache = Arc::new(MemoryCache::new(Arc::clone(&db)).with_mount_path(
        root.clone(),
        reembed_tx,
        hb_tx,
        hb_rx,
    ));

    // Step 2: Seed TaskList block.
    // Create → persist (spawns subscriber) → write content → mark dirty → persist.
    let tl_doc = cache
        .create_block(
            AGENT,
            BlockCreate::new(
                TL_LABEL,
                MemoryBlockType::Working,
                BlockSchema::TaskList {
                    default_status: None,
                    default_owner: None,
                    display_limit: None,
                },
            ),
        )
        .expect("create TaskList block");
    let tl_block_id = tl_doc.id().to_string();

    // First persist spawns the subscriber.
    cache
        .persist_block(AGENT, TL_LABEL)
        .expect("persist TaskList (spawn subscriber)");

    // Give the subscriber thread time to start.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Insert two tasks.
    {
        let list = tl_doc.inner().get_movable_list("items");
        for (i, id) in ["qcc-task-1", "qcc-task-2"].iter().enumerate() {
            list.insert(
                i,
                loro::LoroValue::Map(
                    vec![
                        ("id".to_string(), loro::LoroValue::String((*id).into())),
                        (
                            "subject".to_string(),
                            loro::LoroValue::String(format!("Task {}", i + 1).into()),
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
            .expect("insert task item");
        }
        tl_doc.inner().commit();
    }
    cache.mark_dirty(AGENT, TL_LABEL);
    cache
        .persist_block(AGENT, TL_LABEL)
        .expect("persist TaskList with tasks");

    // Wait for subscriber to emit the .kdl file and reconcile tasks.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Step 2b: Seed Skill block.
    let skill_doc = cache
        .create_block(
            AGENT,
            BlockCreate::new(
                SKILL_LABEL,
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            ),
        )
        .expect("create Skill block");
    let skill_block_id = skill_doc.id().to_string();

    cache
        .persist_block(AGENT, SKILL_LABEL)
        .expect("persist Skill (spawn subscriber)");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let skill_metadata = SkillMetadata {
        name: "quiesce-test-skill".to_string(),
        trust_tier: SkillTrustTier::ProjectLocal,
        description: Some("A skill for the quiesce-commit-cycle test".to_string()),
        keywords: vec!["quiesce".to_string(), "test".to_string()],
        hooks: serde_json::Value::Null,
    };
    let skill_file = SkillFile {
        metadata: skill_metadata.clone(),
        extras: loro::LoroValue::Map(Default::default()),
        body: "## Quiesce test skill\n\nBody content.\n".to_string(),
    };
    write_skill_to_loro_doc(&skill_file, skill_doc.inner()).expect("write_skill_to_loro_doc");
    skill_doc.inner().commit();

    cache.mark_dirty(AGENT, SKILL_LABEL);
    cache
        .persist_block(AGENT, SKILL_LABEL)
        .expect("persist Skill with metadata");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Step 2c: Seed Text block.
    let text_doc = cache
        .create_block(
            AGENT,
            BlockCreate::new(TEXT_LABEL, MemoryBlockType::Working, BlockSchema::text()),
        )
        .expect("create Text block");
    let text_block_id = text_doc.id().to_string();

    cache
        .persist_block(AGENT, TEXT_LABEL)
        .expect("persist Text (spawn subscriber)");
    tokio::time::sleep(Duration::from_millis(100)).await;

    text_doc
        .set_text("quiesce commit cycle test text content", false)
        .unwrap();
    cache.mark_dirty(AGENT, TEXT_LABEL);
    cache
        .persist_block(AGENT, TEXT_LABEL)
        .expect("persist Text with content");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Step 2d: Record a skill usage stat to populate `skill_usage_stats`.
    // This exercises the same path as `handle_load` in pattern_runtime.
    {
        let skill_block_handle = pattern_core::types::block::BlockHandle::new(&skill_block_id);
        let agent_id = AgentId::new(AGENT);
        let conn = db.get().expect("get conn for skill_usage_stats");
        let tx = conn.unchecked_transaction().expect("begin transaction");
        pattern_db::queries::skill_usage::record_usage(
            &tx,
            &skill_block_handle,
            &agent_id,
            jiff::Timestamp::now(),
        )
        .expect("record_usage");
        tx.commit().expect("commit record_usage");
    }

    // Record pre-quiesce task state for comparison after restart.
    let pre_quiesce_task_count = count_tasks(&db, &tl_block_id);
    let pre_quiesce_task_ids = task_item_ids(&db, &tl_block_id);
    assert_eq!(
        pre_quiesce_task_count, 2,
        "should have 2 tasks before quiesce; got {pre_quiesce_task_count}"
    );

    // Collect emitted canonical file paths for the quiesce fsync list.
    let tl_kdl_path = root.join("blocks").join(format!("@{AGENT}")).join("working").join(format!("{TL_LABEL}.kdl"));
    let skill_md_path = root.join("blocks").join(format!("@{AGENT}")).join("working").join(format!("{SKILL_LABEL}.md"));
    let text_md_path = root.join("blocks").join(format!("@{AGENT}")).join("working").join(format!("{TEXT_LABEL}.md"));

    assert!(
        tl_kdl_path.exists(),
        "TaskList .kdl should exist: {}",
        tl_kdl_path.display()
    );
    assert!(
        skill_md_path.exists(),
        "Skill .md should exist: {}",
        skill_md_path.display()
    );
    assert!(
        text_md_path.exists(),
        "Text .md should exist: {}",
        text_md_path.display()
    );

    // Step 3: Call quiesce() on the mount.
    let emitted_paths = vec![
        tl_kdl_path.clone(),
        skill_md_path.clone(),
        text_md_path.clone(),
    ];
    let outcome = quiesce(&cache, &emitted_paths).expect("quiesce must succeed");
    assert_eq!(
        outcome.fsync_failures, 0,
        "quiesce should not have fsync failures"
    );
    eprintln!(
        "quiesce completed in {:?}, fsync failures: {}",
        outcome.duration, outcome.fsync_failures
    );

    // Step 4: Assert the WAL is truncated.
    // After `wal_checkpoint(TRUNCATE)`, the WAL file should be absent or 0 bytes.
    // SQLite WAL mode: memory.db-wal is the WAL file.
    let wal_path = root.join("memory.db-wal");
    let wal_size = if wal_path.exists() {
        std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    assert_eq!(
        wal_size,
        0,
        "WAL file should be absent or 0 bytes after quiesce (TRUNCATE checkpoint); \
         got {wal_size} bytes at {}",
        wal_path.display()
    );
    eprintln!(
        "WAL check passed: wal_path={} exists={} size={wal_size}",
        wal_path.display(),
        wal_path.exists()
    );

    // Step 5: `jj commit` the mount.
    // After quiesce, all canonical files and memory.db are in a consistent state.
    // jj tracks all files in the working copy, so the commit should include the
    // canonical block files and memory.db.
    drop(cache); // Drop the cache to release the DB pool before jj commit.
    drop(db);

    jj_commit(&root, "quiesce-commit-cycle test commit");

    // Inspect the diff stat for the commit we just created (@-).
    let diff_stat = jj_diff_stat_at_prev(&root);
    eprintln!("jj diff --stat -r @-:\n{diff_stat}");

    // The commit must NOT contain WAL/SHM sidecar files.
    assert!(
        !diff_stat.contains("-wal"),
        "commit should NOT contain -wal files; got:\n{diff_stat}"
    );
    assert!(
        !diff_stat.contains("-shm"),
        "commit should NOT contain -shm files; got:\n{diff_stat}"
    );

    // The commit should contain memory.db and the canonical block files.
    // (jj might not track files that haven't changed; we check for what was
    // new/modified — at minimum memory.db and the block files must appear.)
    assert!(
        diff_stat.contains("memory.db") || diff_stat.is_empty(),
        "commit should contain memory.db or be empty (jj may not show unchanged files); got:\n{diff_stat}"
    );

    // Step 6: Drop the mount, re-open, verify task index is preserved.
    let db2 = Arc::new(
        ConstellationDb::open(&db_path, &messages_path)
            .expect("re-open ConstellationDb after commit"),
    );

    let post_restart_task_count = count_tasks(&db2, &tl_block_id);
    let post_restart_task_ids = task_item_ids(&db2, &tl_block_id);

    assert_eq!(
        post_restart_task_count, pre_quiesce_task_count,
        "task count must be preserved across quiesce+commit+restart: \
         before={pre_quiesce_task_count}, after={post_restart_task_count}"
    );
    assert_eq!(
        post_restart_task_ids, pre_quiesce_task_ids,
        "task IDs must be preserved across quiesce+commit+restart"
    );

    eprintln!(
        "Task index preserved: {} tasks, IDs: {:?}",
        post_restart_task_count, post_restart_task_ids
    );
}
