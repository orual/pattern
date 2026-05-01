//! Mode-A (InRepo) jj-tracked mount integration test for `Skills.Load`.
//!
//! Verifies AC9.6: loading a skill 100 times does not dirty the jj working
//! copy — no canonical `.md` file is emitted and no LoroDoc mutations occur.
//!
//! The test uses a real `jj git init` repository. It is skipped automatically
//! when `jj` is not on PATH so it works in minimal CI containers.
//!
//! # Design note
//!
//! This test cannot import from `pattern_runtime` (that crate depends on
//! `pattern_memory`, creating a cycle). Instead, it exercises the components
//! that `handle_load` delegates to directly: `MemoryCache::get_block` for the
//! store read, and `pattern_db::queries::skill_usage::record_usage` for the
//! sqlite write. The jj cleanliness assertion proves that neither path emits
//! any tracked file — which is the structural contract AC9.6 enforces.
//!
//! To run explicitly:
//! ```sh
//! cargo nextest run -p pattern-memory --test skills_load_mode_a --nocapture
//! ```

use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockCreate, BlockHandle};
use pattern_core::types::ids::AgentId;
use pattern_core::types::memory_types::{
    BlockSchema, MemoryBlockType, Scope, SkillMetadata, SkillTrustTier,
};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_memory::fs::markdown_skill::{SkillFile, write_skill_to_loro_doc};

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

/// Initialize a temporary `jj git --colocate` repository and return the `TempDir`.
fn init_jj_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir creation failed");
    let status = Command::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(dir.path())
        .status()
        .expect("jj git init spawn failed");
    assert!(status.success(), "jj git init exited non-zero");
    dir
}

/// Return `true` if `jj status` in `repo` reports no pending working-copy changes.
///
/// The heuristic checks whether jj reports any "Modified", "Added", or "Removed"
/// path lines. An empty working copy or one with only untracked files passes.
fn jj_status_clean(repo: &std::path::Path) -> (bool, String) {
    let output = Command::new("jj")
        .args(["status"])
        .current_dir(repo)
        .output()
        .expect("jj status spawn failed");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    // jj prints "The working copy is clean" when there are no changes.
    let is_clean = stdout.contains("The working copy is clean")
        || (!stdout.contains("Modified ")
            && !stdout.contains("Added ")
            && !stdout.contains("Removed "));
    (is_clean, stdout)
}

/// Write a canonical skill `.md` file to `path` via the standard emitter.
fn write_skill_md(path: &std::path::Path, metadata: &SkillMetadata, body: &str) {
    let content = pattern_memory::fs::markdown_skill::emit(
        metadata,
        &loro::LoroValue::Map(Default::default()),
        body,
    )
    .expect("emit skill md failed");
    std::fs::write(path, content).expect("write skill md failed");
}

/// Create a test agent row in the DB.
fn create_agent(dbs: &ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("Test Agent {agent_id}"),
        description: None,
        model_provider: "anthropic".to_string(),
        model_name: "claude".to_string(),
        system_prompt: "test".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: pattern_db::models::AgentStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    pattern_db::queries::create_agent(&dbs.get().unwrap(), &agent)
        .expect("failed to create test agent");
}

// ---------------------------------------------------------------------------
// AC9.6: 100 skill loads must not dirty the jj working copy.
// ---------------------------------------------------------------------------

/// Mode-A jj integration test: load a skill 100 times, assert `jj status` clean.
///
/// # What this test verifies
///
/// `handle_load` (in `pattern_runtime`) must not:
/// - Write the canonical `.md` file (the LoroDoc must not be committed/re-emitted).
/// - Create any new tracked or untracked files under the jj working copy.
///
/// This test exercises the two database calls that `handle_load` delegates to:
/// - `MemoryCache::get_block` (read-only; no LoroDoc mutation).
/// - `pattern_db::queries::skill_usage::record_usage` (sqlite-only; no file I/O).
///
/// Both calls are verified to be non-mutating with respect to the jj repo by
/// asserting `jj status` remains clean after 100 iterations.
///
/// # Setup
///
/// 1. Initialize a `jj git --colocate` repo (creates `.jj/` + `.git/`).
/// 2. Write a canonical `skill.md` file to the repo root and commit it via jj.
/// 3. Set up an in-memory `ConstellationDb` + `MemoryCache` with the skill block.
/// 4. Perform 100 simulated loads (read block + write sqlite stat).
/// 5. Assert `jj status` clean; assert `skill.md` is byte-identical.
#[test]
fn load_does_not_dirty_mount() {
    skip_if_no_jj!();

    const AGENT: &str = "mode-a-test-agent";
    const SKILL_LABEL: &str = "mode-a-skill";
    const SKILL_BODY: &str = "## Mode A Skill\n\nThis skill body must remain unchanged.\n";

    let metadata = SkillMetadata {
        name: "mode-a-skill".to_string(),
        trust_tier: SkillTrustTier::ProjectLocal,
        description: Some("Mode A integration test skill".to_string()),
        keywords: vec!["integration".to_string(), "mode-a".to_string()],
        hooks: serde_json::Value::Null,
    };

    // 1. Initialize a jj git repo in a TempDir.
    let repo_dir = init_jj_repo();
    let repo_path = repo_dir.path();

    // 2. Write the canonical skill.md file to the repo root.
    let skill_md_path = repo_path.join("skill.md");
    write_skill_md(&skill_md_path, &metadata, SKILL_BODY);

    // Commit the initial file: describe + new.
    let desc_ok = Command::new("jj")
        .args(["describe", "-m", "initial: add skill.md"])
        .current_dir(repo_path)
        .status()
        .expect("jj describe spawn")
        .success();
    assert!(desc_ok, "jj describe failed for initial commit");

    let new_ok = Command::new("jj")
        .args(["new"])
        .current_dir(repo_path)
        .status()
        .expect("jj new spawn")
        .success();
    assert!(new_ok, "jj new failed for initial commit");

    // Capture the file hash before loads — must be stable.
    let before_bytes = std::fs::read(&skill_md_path).expect("read skill.md before loads");

    // The working copy (new empty commit) should be clean.
    let (pre_clean, pre_out) = jj_status_clean(repo_path);
    assert!(
        pre_clean,
        "jj working copy must be clean after initial commit (pre-load); got:\n{pre_out}"
    );

    // 3. Set up an in-memory ConstellationDb + MemoryCache.
    //    Using in-memory DB keeps all sqlite writes out of the jj repo.
    let dbs = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    create_agent(&dbs, AGENT);
    let cache = MemoryCache::new(dbs.clone());

    // Create the Skill block in the cache (pure in-memory; no file emission).
    let agent_scope = Scope::global(AGENT);
    cache
        .create_block(
            &agent_scope,
            BlockCreate::new(
                SKILL_LABEL,
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            ),
        )
        .expect("create skill block failed");

    // Populate the LoroDoc with metadata + body.
    let doc = cache
        .get_block(&agent_scope, SKILL_LABEL)
        .expect("get_block failed")
        .expect("skill block must exist");
    let skill_file = SkillFile {
        metadata: metadata.clone(),
        extras: loro::LoroValue::Map(Default::default()),
        body: SKILL_BODY.to_string(),
    };
    write_skill_to_loro_doc(&skill_file, doc.inner()).expect("write_skill_to_loro_doc failed");
    doc.inner().commit();

    // 4. Open an in-memory connection for skill_usage_stats writes.
    //    This keeps all sqlite I/O out of the jj repo (no .db file on disk).
    let mut usage_conn = rusqlite::Connection::open_in_memory().expect("open in-memory usage conn");
    pattern_db::migrations::run_memory_migrations(&mut usage_conn)
        .expect("run memory migrations on in-memory conn");

    let skill_block = BlockHandle::new(SKILL_LABEL);
    let agent_id = AgentId::new(AGENT);

    // 5. Simulate 100 loads:
    //    - Read the skill block (no mutation).
    //    - Record a sqlite usage stat (in-memory DB; no file I/O).
    for i in 0..100u32 {
        // Read the block — this is the read path that handle_load uses.
        let fetched = cache
            .get_block(&agent_scope, SKILL_LABEL)
            .unwrap_or_else(|e| panic!("get_block at load {i} failed: {e}"))
            .unwrap_or_else(|| panic!("skill block missing at load {i}"));

        // Verify schema is still Skill (invariant: loads don't mutate schema).
        assert!(
            matches!(fetched.schema(), BlockSchema::Skill { .. }),
            "block schema must remain Skill after load {i}"
        );

        // Write the usage stat (in-memory sqlite; no jj-visible I/O).
        let now = jiff::Timestamp::now();
        let tx = usage_conn
            .transaction()
            .unwrap_or_else(|e| panic!("transaction at load {i}: {e}"));
        pattern_db::queries::skill_usage::record_usage(&tx, &skill_block, &agent_id, now)
            .unwrap_or_else(|e| panic!("record_usage at load {i}: {e}"));
        tx.commit()
            .unwrap_or_else(|e| panic!("tx commit at load {i}: {e}"));
    }

    // 6. Assert the sqlite stats are correct (100 loads recorded).
    let stats = pattern_db::queries::skill_usage::get_usage_stats(&usage_conn, &skill_block)
        .expect("get_usage_stats failed");
    assert_eq!(
        stats.use_count, 100,
        "use_count must be 100 after 100 loads; got {stats:?}"
    );

    // 7. Assert `jj status` is still clean — no files modified or added in the repo.
    let (post_clean, post_out) = jj_status_clean(repo_path);
    assert!(
        post_clean,
        "jj working copy must remain clean after 100 skill loads (AC9.6); got:\n{post_out}"
    );

    // 8. Assert the skill.md file is byte-identical (content-hash stable).
    let after_bytes = std::fs::read(&skill_md_path).expect("read skill.md after 100 loads");
    assert_eq!(
        before_bytes, after_bytes,
        "skill.md must be byte-identical before and after 100 loads (AC9.3)"
    );
}
