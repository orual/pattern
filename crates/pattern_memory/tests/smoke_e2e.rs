//! Capstone end-to-end smoke test.
//!
//! Exercises the full v3-memory-rework DoD flow deterministically with no live
//! provider calls. Runs in CI.
//!
//! Flow:
//!   1. Create Mode A project in a tempdir git repo.
//!   2. Attach the mount.
//!   3. Write Core text block + Map block + Log block.
//!   4. Verify canonical files emitted (.md, .kdl, .jsonl) with expected content.
//!   5. External edit to the .md file (simulated human editor).
//!   6. Wait for notify watcher → loro CRDT merge.
//!   7. Verify reconciled content via `get_rendered_content`.
//!   8. Quiesce + commit via host `git commit`.
//!   9. Detach + simulate process restart.
//!   10. Re-attach; read blocks; assert matches committed state.
//!   11. Create messages.db backup via `create_snapshot`.
//!   12. Insert messages, then truncate messages.db.
//!   13. Restore from backup; verify message count.
//!
//! Verifies: v3-memory-rework.AC15.1, AC15.2, AC15.5.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, BlockType};
use pattern_db::{ConstellationDb, Json, models};
use pattern_memory::backup::restore::restore_snapshot;
use pattern_memory::backup::snapshot::create_snapshot;
use pattern_memory::mount::{MountedStore, attach_with_paths};
use pattern_memory::paths::PatternPaths;
use pattern_memory::quiesce::quiesce;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Initialize a git repo with user config and initial commit.
fn git_init(project_root: &Path) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(project_root)
            .output()
            .expect("git command must execute");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init"]);
    run(&["config", "user.name", "Pattern Smoke"]);
    run(&["config", "user.email", "smoke@pattern.test"]);
}

/// Stage all and commit with a message.
fn git_commit(project_root: &Path, msg: &str) {
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(project_root)
            .output()
            .expect("git command must execute");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["add", "-A"]);
    run(&["commit", "-m", msg, "--allow-empty"]);
}

/// Seed a minimal agent row for FK constraint satisfaction.
fn seed_agent(db: &ConstellationDb, agent_id: &str) {
    let agent = models::Agent {
        id: agent_id.to_string(),
        name: format!("smoke-test-{agent_id}"),
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("seed agent");
}

/// Insert a scripted message into messages.db.
fn insert_message(db: &ConstellationDb, agent_id: &str, content_preview: &str) {
    let conn = db.get().unwrap();
    let msg = models::Message {
        id: format!("msg-{}", uuid::Uuid::new_v4().simple()),
        agent_id: agent_id.to_string(),
        position: format!("{:020}", Timestamp::now().as_millisecond()),
        batch_id: None,
        sequence_in_batch: None,
        role: models::MessageRole::User,
        content_json: Json(serde_json::json!({"text": content_preview})),
        content_preview: Some(content_preview.to_string()),
        batch_type: None,
        source: Some("test".to_string()),
        source_metadata: None,
        is_archived: false,
        is_deleted: false,
        created_at: Timestamp::now(),
    };
    pattern_db::queries::create_message(&conn, &msg).expect("insert message");
}

/// Count all messages for an agent.
fn count_messages(db: &ConstellationDb, agent_id: &str) -> i64 {
    pattern_db::queries::count_all_messages(&db.get().unwrap(), agent_id).expect("count messages")
}

/// Recursively collect all files with a given extension under a directory.
fn collect_files_with_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if !dir.is_dir() {
        return result;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_files_with_ext(&path, ext));
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            result.push(path);
        }
    }
    result
}

/// Collect all emitted canonical files under `<mount>/blocks/`.
fn collect_emitted_paths(mount: &MountedStore) -> Vec<PathBuf> {
    let blocks_dir = mount.mount_path.join("blocks");
    let mut paths = Vec::new();
    for ext in &["md", "kdl", "jsonl"] {
        paths.extend(collect_files_with_ext(&blocks_dir, ext));
    }
    paths
}

// ---------------------------------------------------------------------------
// Capstone smoke test
// ---------------------------------------------------------------------------

/// Full DoD end-to-end test exercising attach → write → file emission →
/// external edit → merge → quiesce → detach → re-attach → backup → restore.
///
/// This test requires a tokio runtime for the subscriber supervisor and
/// filesystem watcher, but the main flow is synchronous.
#[tokio::test]
async fn smoke_e2e() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().to_owned();
    let paths = PatternPaths::with_base(tmp.path());

    // --- Step 1: git init + Mode A project ---
    git_init(&project_root);
    pattern_memory::modes::mode_a::init(&project_root).expect("Mode A init");
    git_commit(&project_root, "baseline: init Mode A project");

    // --- Step 2: attach ---
    let mount = attach_with_paths(&project_root, &paths).expect("attach");
    assert!(
        mount.mount_path.exists(),
        "mount path should exist: {}",
        mount.mount_path.display()
    );

    let agent_id = "smoke-agent";
    seed_agent(&mount.db, agent_id);

    // --- Step 3: create blocks ---
    // First create + persist (empty) to spawn subscribers, then write content.
    // Subscribers are spawned on first persist. The subscribe_local_update hook
    // only fires for writes AFTER the subscriber exists, so we split creation
    // (which spawns the subscriber) from content writes (which trigger events).

    let text_doc = mount
        .cache
        .create_block(
            agent_id,
            BlockCreate::new("notes", BlockType::Core, BlockSchema::text()),
        )
        .expect("create notes block");
    let notes_block_id = text_doc.id().to_string();
    // Persist to spawn the subscriber.
    mount.cache.persist_block(agent_id, "notes").unwrap();

    let map_doc = mount
        .cache
        .create_block(
            agent_id,
            BlockCreate::new(
                "config",
                BlockType::Working,
                BlockSchema::Map { fields: vec![] },
            ),
        )
        .expect("create config block");
    let config_block_id = map_doc.id().to_string();
    mount.cache.persist_block(agent_id, "config").unwrap();

    let log_doc = mount
        .cache
        .create_block(
            agent_id,
            BlockCreate::new(
                "events",
                BlockType::Working,
                BlockSchema::Log {
                    display_limit: 100,
                    entry_schema: pattern_core::types::memory_types::LogEntrySchema {
                        timestamp: true,
                        agent_id: true,
                        fields: vec![],
                    },
                },
            ),
        )
        .expect("create events block");
    let events_block_id = log_doc.id().to_string();
    mount.cache.persist_block(agent_id, "events").unwrap();

    // Brief sleep to let subscriber threads start.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now write actual content — these writes trigger subscribe_local_update
    // callbacks that send CommitEvents to the subscriber workers.
    text_doc.set_text("hello pattern", false).unwrap();
    mount.cache.mark_dirty(agent_id, "notes");
    mount.cache.persist_block(agent_id, "notes").unwrap();

    map_doc
        .set_field("key1", serde_json::json!("value1"), false)
        .unwrap();
    mount.cache.mark_dirty(agent_id, "config");
    mount.cache.persist_block(agent_id, "config").unwrap();

    log_doc
        .append_log_entry(serde_json::json!({"event": "started"}), false)
        .unwrap();
    mount.cache.mark_dirty(agent_id, "events");
    mount.cache.persist_block(agent_id, "events").unwrap();

    // --- Step 4: wait for subscriber debounce + verify files ---
    // Subscribers are lazy-spawned on first persist when mount_path is set.
    // Give them time to emit canonical files.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Files are emitted as `<mount_path>/<block_id>.<ext>`.
    let notes_md = mount.mount_path.join(format!("{notes_block_id}.md"));
    assert!(
        notes_md.exists(),
        "notes .md should exist at {}",
        notes_md.display()
    );
    let md_content = std::fs::read_to_string(&notes_md).unwrap();
    assert!(
        md_content.contains("hello pattern"),
        "notes .md should contain 'hello pattern', got: {md_content:?}"
    );

    let config_kdl = mount.mount_path.join(format!("{config_block_id}.kdl"));
    assert!(
        config_kdl.exists(),
        "config .kdl should exist at {}",
        config_kdl.display()
    );

    let events_jsonl = mount.mount_path.join(format!("{events_block_id}.jsonl"));
    assert!(
        events_jsonl.exists(),
        "events .jsonl should exist at {}",
        events_jsonl.display()
    );

    // --- Step 5-6: external edit + wait for watcher merge ---
    std::fs::write(&notes_md, "hello pattern — externally edited\n")
        .expect("external edit to notes.md");
    // Wait for notify event + subscriber merge cycle.
    tokio::time::sleep(Duration::from_millis(700)).await;

    // --- Step 7: verify merged content ---
    let merged = mount
        .cache
        .get_rendered_content(agent_id, "notes")
        .expect("get merged content")
        .expect("notes should exist after merge");
    assert!(
        merged.contains("externally edited"),
        "merged content should reflect external edit, got: {merged:?}"
    );

    // Persist the merged state to DB so it survives detach/re-attach.
    mount.cache.mark_dirty(agent_id, "notes");
    mount.cache.persist_block(agent_id, "notes").unwrap();

    // --- Step 8: quiesce + git commit ---
    let emitted = collect_emitted_paths(&mount);
    quiesce(&mount.cache, &emitted).expect("quiesce");
    git_commit(&project_root, "smoke: write blocks");

    // --- Step 9: detach (simulate process restart) ---
    mount.detach();

    // --- Step 10: re-attach and verify ---
    let mount2 = attach_with_paths(&project_root, &paths).expect("re-attach");
    let recovered = mount2
        .cache
        .get_rendered_content(agent_id, "notes")
        .expect("get after re-attach")
        .expect("notes should exist after re-attach");
    assert!(
        recovered.contains("externally edited"),
        "recovered content should match committed state, got: {recovered:?}"
    );

    // --- Step 11: messages.db backup ---
    let messages_db_path = mount2.db.messages_path().to_owned();
    let project_id = &mount2.config.project.name;

    // Insert known messages before snapshot.
    let pre_snapshot_count = 5;
    for i in 0..pre_snapshot_count {
        insert_message(&mount2.db, agent_id, &format!("pre-snapshot-{i}"));
    }
    assert_eq!(count_messages(&mount2.db, agent_id), pre_snapshot_count);

    let snapshot =
        create_snapshot(&messages_db_path, &paths, project_id).expect("create backup snapshot");
    assert!(
        snapshot.path.exists(),
        "snapshot file should exist at {}",
        snapshot.path.display()
    );

    // --- Step 12: insert more messages + corrupt ---
    for i in 0..3 {
        insert_message(&mount2.db, agent_id, &format!("post-snapshot-{i}"));
    }
    assert_eq!(count_messages(&mount2.db, agent_id), pre_snapshot_count + 3);

    // Must drop the DB pool before truncating the file so the restore can
    // open it cleanly (no active connections).
    drop(mount2.db);
    // Also drop the cache so no stale refs hold the pool.
    drop(mount2.cache);

    // Corrupt messages.db.
    std::fs::write(&messages_db_path, b"").expect("truncate messages.db");

    // --- Step 13: restore + verify ---
    let _pre_restore_path = restore_snapshot(&messages_db_path, &snapshot.path).expect("restore");

    // Re-open the restored DB and verify message count.
    let restored_db = ConstellationDb::open(
        // memory.db path — same as mount's path.
        mount2.mount_path.join("memory.db"),
        &messages_db_path,
    )
    .expect("open restored db");
    let restored_count = count_messages(&restored_db, agent_id);
    assert_eq!(
        restored_count, pre_snapshot_count,
        "restored count should be {pre_snapshot_count}, got {restored_count}"
    );
}
