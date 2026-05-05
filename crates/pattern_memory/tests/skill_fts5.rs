//! FTS5 indexing coverage tests for Skill blocks.
//!
//! Verifies that Skill block metadata (name, description, keywords) and body
//! text are all indexed in `memory_blocks_fts` so that `ctx.skills.search`
//! can discover skills by any of these fields.
//!
//! The preview string written to FTS5 is produced by
//! `StructuredDocument::render()` for `BlockSchema::Skill`. These tests drive
//! that path through `MemoryCache::persist_block`, which calls
//! `update_block_preview` — the same path used by the subscriber worker.
//!
//! Test pattern: create a Skill block → write metadata via
//! `write_skill_to_loro_doc` on the LoroDoc returned by `get_block` →
//! mark dirty → persist → search via `MemoryCache::search`.

use std::sync::Arc;

use pattern_core::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    BlockSchema, MemoryBlockType, MemorySearchScope, Scope, SearchContentType, SearchMode,
    SearchOptions, SkillMetadata, SkillTrustTier,
};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_memory::fs::markdown_skill::write_skill_to_loro_doc;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn test_dbs() -> (tempfile::TempDir, Arc<ConstellationDb>) {
    let dir = tempfile::tempdir().unwrap();
    let dbs = Arc::new(ConstellationDb::open_in_memory().unwrap());
    (dir, dbs)
}

fn create_test_agent(dbs: &ConstellationDb, agent_id: &str) {
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

fn setup() -> (tempfile::TempDir, Arc<ConstellationDb>, MemoryCache) {
    let (dir, dbs) = test_dbs();
    create_test_agent(&dbs, "agent_1");
    let cache = MemoryCache::new(dbs.clone());
    (dir, dbs, cache)
}

/// Create a Skill block, populate it with the given metadata and body, persist,
/// and return. The caller can then search for content in this block.
fn create_skill_block(cache: &MemoryCache, label: &str, metadata: SkillMetadata, body: &str) {
    cache
        .create_block(
            &Scope::global("agent_1"),
            BlockCreate::new(
                label,
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            )
            .with_description(&metadata.name),
        )
        .unwrap();

    let doc = cache
        .get_block(&Scope::global("agent_1"), label)
        .unwrap()
        .expect("block should exist after create");

    let skill_file = pattern_memory::fs::markdown_skill::SkillFile {
        metadata,
        extras: loro::LoroValue::Map(Default::default()),
        body: body.to_string(),
    };
    write_skill_to_loro_doc(&skill_file, doc.inner()).unwrap();
    doc.inner().commit();

    cache.mark_dirty(&Scope::global("agent_1").to_db_key(), label);
    cache.persist_block(&Scope::global("agent_1"), label).unwrap();
}

fn fts_search(
    cache: &MemoryCache,
    query: &str,
) -> Vec<pattern_core::types::memory_types::MemorySearchResult> {
    let opts = SearchOptions {
        mode: SearchMode::Fts,
        content_types: vec![SearchContentType::Blocks],
        limit: 20,
    };
    cache
        .search(
            query,
            opts,
            MemorySearchScope::Scope(Scope::global("agent_1")),
        )
        .unwrap()
}

// ---------------------------------------------------------------------------
// fts5_skill_search_by_name
//
// A Skill block with name "fix-authentication" must be discoverable by
// searching for "authentication".
// ---------------------------------------------------------------------------

#[test]
fn fts5_skill_search_by_name() {
    let (_dir, _dbs, cache) = setup();

    create_skill_block(
        &cache,
        "auth-skill",
        SkillMetadata {
            name: "fix-authentication".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "No special body content.\n",
    );

    // Unrelated skill to confirm we don't get false positives.
    create_skill_block(
        &cache,
        "other-skill",
        SkillMetadata {
            name: "unrelated-task".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Nothing relevant here.\n",
    );

    let results = fts_search(&cache, "authentication");
    assert_eq!(
        results.len(),
        1,
        "expected exactly one result for 'authentication'; got {}: {results:?}",
        results.len()
    );
    let content = results[0].content.as_deref().unwrap_or("");
    assert!(
        content.contains("fix-authentication"),
        "result content should contain skill name; got: {content:?}"
    );
}

// ---------------------------------------------------------------------------
// fts5_skill_search_by_description
//
// A Skill block with a description containing "token-refresh" must be
// discoverable by searching for "token".
// ---------------------------------------------------------------------------

#[test]
fn fts5_skill_search_by_description() {
    let (_dir, _dbs, cache) = setup();

    create_skill_block(
        &cache,
        "desc-skill",
        SkillMetadata {
            name: "some-skill".to_string(),
            trust_tier: SkillTrustTier::ProjectLocal,
            description: Some("Handles token-refresh for expired sessions".to_string()),
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Generic body text.\n",
    );

    // Decoy — no description mentioning token.
    create_skill_block(
        &cache,
        "decoy-skill",
        SkillMetadata {
            name: "unrelated-skill".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: Some("Nothing relevant".to_string()),
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Also irrelevant.\n",
    );

    let results = fts_search(&cache, "token");
    assert_eq!(
        results.len(),
        1,
        "expected exactly one result for 'token'; got {}: {results:?}",
        results.len()
    );
    let content = results[0].content.as_deref().unwrap_or("");
    assert!(
        content.contains("token-refresh") || content.contains("token"),
        "result should contain description text; got: {content:?}"
    );
}

// ---------------------------------------------------------------------------
// fts5_skill_search_by_keyword
//
// A Skill block with keyword "oauth2" must be discoverable by searching for
// "oauth2".
// ---------------------------------------------------------------------------

#[test]
fn fts5_skill_search_by_keyword() {
    let (_dir, _dbs, cache) = setup();

    create_skill_block(
        &cache,
        "kw-skill",
        SkillMetadata {
            name: "session-manager".to_string(),
            trust_tier: SkillTrustTier::FirstParty,
            description: None,
            keywords: vec!["oauth2".to_string(), "auth".to_string()],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Manages user sessions.\n",
    );

    // Decoy with different keywords.
    create_skill_block(
        &cache,
        "decoy-kw",
        SkillMetadata {
            name: "file-manager".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec!["filesystem".to_string(), "io".to_string()],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Manages files.\n",
    );

    let results = fts_search(&cache, "oauth2");
    assert_eq!(
        results.len(),
        1,
        "expected exactly one result for 'oauth2'; got {}: {results:?}",
        results.len()
    );
    let content = results[0].content.as_deref().unwrap_or("");
    assert!(
        content.contains("oauth2"),
        "result should contain the keyword; got: {content:?}"
    );
}

// ---------------------------------------------------------------------------
// fts5_skill_search_by_body
//
// A Skill block with body text "Revokes all active sessions gracefully" must be
// discoverable by searching for "Revokes".
// ---------------------------------------------------------------------------

#[test]
fn fts5_skill_search_by_body() {
    let (_dir, _dbs, cache) = setup();

    create_skill_block(
        &cache,
        "body-skill",
        SkillMetadata {
            name: "logout-handler".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Revokes all active sessions gracefully.\n",
    );

    // Decoy with different body.
    create_skill_block(
        &cache,
        "body-decoy",
        SkillMetadata {
            name: "login-handler".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Creates a new session for the user.\n",
    );

    let results = fts_search(&cache, "Revokes");
    assert_eq!(
        results.len(),
        1,
        "expected exactly one result for 'Revokes'; got {}: {results:?}",
        results.len()
    );
    let content = results[0].content.as_deref().unwrap_or("");
    assert!(
        content.contains("Revokes"),
        "result should contain body text; got: {content:?}"
    );
}

// ---------------------------------------------------------------------------
// fts5_skill_content_snapshot
//
// Three skills with distinct content share the term "security". Snapshot the
// BM25 ordering to detect regressions in the scoring pipeline.
// ---------------------------------------------------------------------------

#[test]
fn fts5_skill_content_snapshot() {
    let (_dir, _dbs, cache) = setup();

    // Skill A: "security" appears in the name only.
    create_skill_block(
        &cache,
        "skill-a",
        SkillMetadata {
            name: "security-audit".to_string(),
            trust_tier: SkillTrustTier::FirstParty,
            description: Some("Runs a security audit on the codebase".to_string()),
            keywords: vec!["security".to_string(), "audit".to_string()],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Checks for vulnerabilities and misconfigurations. security baseline.\n",
    );

    // Skill B: "security" appears in keywords and body.
    create_skill_block(
        &cache,
        "skill-b",
        SkillMetadata {
            name: "access-control".to_string(),
            trust_tier: SkillTrustTier::ProjectLocal,
            description: None,
            keywords: vec!["security".to_string(), "rbac".to_string()],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Manages role-based access control for security enforcement.\n",
    );

    // Skill C: "security" appears in description and body.
    create_skill_block(
        &cache,
        "skill-c",
        SkillMetadata {
            name: "credential-rotation".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: Some("Rotates credentials for security compliance".to_string()),
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        "Automates certificate and API key security rotation.\n",
    );

    let results = fts_search(&cache, "security");
    assert_eq!(
        results.len(),
        3,
        "all three skills should be findable by 'security'; got {}: {results:?}",
        results.len()
    );

    // Collect labels in BM25 order for snapshot.
    // content_preview contains the name so we can identify which skill it is.
    let ordered_names: Vec<&str> = results
        .iter()
        .map(|r| {
            let content = r.content.as_deref().unwrap_or("");
            if content.contains("security-audit") {
                "security-audit"
            } else if content.contains("access-control") {
                "access-control"
            } else if content.contains("credential-rotation") {
                "credential-rotation"
            } else {
                "unknown"
            }
        })
        .collect();

    insta::assert_snapshot!("fts5_skill_content_snapshot", ordered_names.join("\n"));
}
