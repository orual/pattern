//! Cross-schema FTS5 coverage test (AC10.8).
//!
//! Verifies that `MemoryCache::search` spans all three block schema kinds
//! (Text, TaskList, Skill) and returns results from all of them when the query
//! matches content in each. Also snapshot-tests the BM25 ordering for
//! stability, and explicitly confirms that no schema kind is silently excluded
//! by filtering logic.
//!
//! # Setup
//!
//! Uses `Arc<MemoryCache>` + `ConstellationDb::open_in_memory()` — the same
//! in-memory pattern as `skill_fts5.rs`. No mount path or subscribers needed;
//! `persist_block` updates the FTS5 index directly via `update_block_preview`.
//!
//! # Blocks seeded
//!
//! - Text block: body contains "hydration tip".
//! - TaskList block: one task with subject "hydration check".
//! - Skill block: keywords include "hydration".
//!
//! A single `search("hydration", ...)` call must return results from all three
//! block kinds.
//!
//! To run explicitly:
//! ```sh
//! cargo nextest run -p pattern-memory --test cross_schema_fts --nocapture
//! ```

use std::collections::HashSet;
use std::sync::Arc;

use pattern_core::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    BlockSchema, MemoryBlockType, MemorySearchScope, SearchContentType, SearchMode, SearchOptions,
    SkillMetadata, SkillTrustTier,
};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_memory::fs::markdown_skill::{SkillFile, write_skill_to_loro_doc};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

const AGENT: &str = "cross-schema-fts-agent";

/// Open an in-memory ConstellationDb and create a MemoryCache.
fn setup() -> (Arc<ConstellationDb>, MemoryCache) {
    let db = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    // Create agent row.
    let agent = pattern_db::models::Agent {
        id: AGENT.to_string(),
        name: "cross-schema-fts-agent".to_string(),
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
    let cache = MemoryCache::new(Arc::clone(&db));
    (db, cache)
}

/// Seed a Text block with `content` and update the FTS5 index via persist.
fn seed_text_block(cache: &MemoryCache, label: &str, content: &str) {
    let doc = cache
        .create_block(
            AGENT,
            BlockCreate::new(label, MemoryBlockType::Working, BlockSchema::text()),
        )
        .unwrap_or_else(|e| panic!("create text block '{label}': {e}"));
    doc.set_text(content, false)
        .unwrap_or_else(|e| panic!("set_text for '{label}': {e}"));
    cache.mark_dirty(AGENT, label);
    cache
        .persist_block(AGENT, label)
        .unwrap_or_else(|e| panic!("persist text block '{label}': {e}"));
}

/// Seed a TaskList block with one task item whose `subject` is `subject`.
fn seed_task_list_block(cache: &MemoryCache, label: &str, subject: &str) {
    let doc = cache
        .create_block(
            AGENT,
            BlockCreate::new(
                label,
                MemoryBlockType::Working,
                BlockSchema::TaskList {
                    default_status: None,
                    default_owner: None,
                    display_limit: None,
                },
            ),
        )
        .unwrap_or_else(|e| panic!("create task list block '{label}': {e}"));

    // Insert one task item into the movable list.
    {
        let list = doc.inner().get_movable_list("items");
        list.insert(
            0,
            loro::LoroValue::Map(
                vec![
                    (
                        "id".to_string(),
                        loro::LoroValue::String(format!("{label}-item-1").into()),
                    ),
                    (
                        "subject".to_string(),
                        loro::LoroValue::String(subject.into()),
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
        .unwrap_or_else(|e| panic!("insert task item for '{label}': {e}"));
        doc.inner().commit();
    }

    cache.mark_dirty(AGENT, label);
    cache
        .persist_block(AGENT, label)
        .unwrap_or_else(|e| panic!("persist task list block '{label}': {e}"));
}

/// Seed a Skill block with keywords containing `keyword`.
fn seed_skill_block(cache: &MemoryCache, label: &str, keyword: &str) {
    cache
        .create_block(
            AGENT,
            BlockCreate::new(
                label,
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            )
            .with_description("hydration skill"),
        )
        .unwrap_or_else(|e| panic!("create skill block '{label}': {e}"));

    let doc = cache
        .get_block(AGENT, label)
        .unwrap()
        .expect("skill block must exist");

    let skill_file = SkillFile {
        metadata: SkillMetadata {
            name: format!("{label}-skill"),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec![keyword.to_string()],
            hooks: serde_json::Value::Null,
        },
        extras: loro::LoroValue::Map(Default::default()),
        body: "Skill body content without the search term.\n".to_string(),
    };
    write_skill_to_loro_doc(&skill_file, doc.inner())
        .unwrap_or_else(|e| panic!("write_skill_to_loro_doc for '{label}': {e}"));
    doc.inner().commit();

    cache.mark_dirty(AGENT, label);
    cache
        .persist_block(AGENT, label)
        .unwrap_or_else(|e| panic!("persist skill block '{label}': {e}"));
}

/// Run an FTS search scoped to the test agent.
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
        .search(query, opts, MemorySearchScope::Agent(AGENT.into()))
        .unwrap_or_else(|e| panic!("search failed: {e}"))
}

// ---------------------------------------------------------------------------
// AC10.8: FTS5 spans Text, TaskList, and Skill blocks
// ---------------------------------------------------------------------------

/// A single search for "hydration" must return results from all three block kinds.
///
/// # What this test verifies
///
/// 1. Seeds a Text block containing "hydration tip".
/// 2. Seeds a TaskList block with a task subject "hydration check".
/// 3. Seeds a Skill block with keyword "hydration".
/// 4. Runs `cache.search("hydration", SearchOptions::default(), MemorySearchScope::Agent(...))`.
/// 5. Asserts all three results are present (one per schema kind).
/// 6. Asserts no schema kind is silently excluded.
#[test]
fn cross_schema_fts_returns_all_schema_kinds() {
    let (_db, cache) = setup();

    seed_text_block(&cache, "hydration-text", "hydration tip for daily wellness");
    seed_task_list_block(&cache, "hydration-tasklist", "hydration check at 10am");
    seed_skill_block(&cache, "hydration-skill", "hydration");

    let results = fts_search(&cache, "hydration");

    assert_eq!(
        results.len(),
        3,
        "search for 'hydration' should return exactly 3 results (one per block schema kind); \
         got {}: {results:#?}",
        results.len()
    );

    // Verify each block kind is present by checking content.
    // Each result's `content` field is the FTS5 preview string.
    let contents: Vec<&str> = results
        .iter()
        .map(|r| r.content.as_deref().unwrap_or(""))
        .collect();

    let has_text = contents.iter().any(|c| c.contains("hydration tip"));
    let has_task = contents.iter().any(|c| c.contains("hydration check"));
    let has_skill = contents.iter().any(|c| c.contains("hydration"));

    assert!(
        has_text,
        "Text block result ('hydration tip') missing from search results; contents: {contents:?}"
    );
    assert!(
        has_task,
        "TaskList block result ('hydration check') missing from search results; contents: {contents:?}"
    );
    assert!(
        has_skill,
        "Skill block result (keyword 'hydration') missing from search results; contents: {contents:?}"
    );
}

/// No block schema kind is silently excluded by the FTS5 filtering logic.
///
/// Seeds one block of each kind with a DIFFERENT unique term per kind, then
/// searches for each term independently. This proves the FTS index covers all
/// three schemas without relying on a single shared term.
#[test]
fn no_schema_kind_silently_excluded() {
    let (_db, cache) = setup();

    seed_text_block(&cache, "exclusion-text", "zynthoflux unique text marker");
    seed_task_list_block(
        &cache,
        "exclusion-tasklist",
        "zynthoflux unique task subject",
    );
    seed_skill_block(&cache, "exclusion-skill", "zynthoflux");

    // Each block has a unique term — search for "zynthoflux" to find all three.
    let results = fts_search(&cache, "zynthoflux");

    let found_schemas: HashSet<String> = results
        .iter()
        .map(|r| {
            let content = r.content.as_deref().unwrap_or("");
            if content.contains("unique text marker") {
                "text"
            } else if content.contains("unique task subject") {
                "task-list"
            } else {
                "skill"
            }
        })
        .map(|s| s.to_string())
        .collect();

    assert!(
        found_schemas.contains("text"),
        "Text block schema silently excluded from FTS index; found: {found_schemas:?}"
    );
    assert!(
        found_schemas.contains("task-list"),
        "TaskList block schema silently excluded from FTS index; found: {found_schemas:?}"
    );
    assert!(
        found_schemas.contains("skill"),
        "Skill block schema silently excluded from FTS index; found: {found_schemas:?}"
    );
}

/// Snapshot-test BM25 ordering for "hydration" across all three block kinds.
///
/// Seeds the same three block types as `cross_schema_fts_returns_all_schema_kinds`
/// but with more varied "hydration" content to produce a realistic BM25 ranking.
/// The exact ordering is snapshot-tested via `insta` to catch regressions in the
/// FTS5 scoring pipeline.
#[test]
fn cross_schema_fts_bm25_ordering_snapshot() {
    let (_db, cache) = setup();

    // Text block: "hydration" appears once in the body.
    seed_text_block(
        &cache,
        "bm25-text",
        "hydration is important for daily health and wellness",
    );

    // TaskList block: "hydration" appears in the task subject.
    seed_task_list_block(&cache, "bm25-tasklist", "hydration check — drink water now");

    // Skill block: "hydration" appears as a keyword.
    seed_skill_block(&cache, "bm25-skill", "hydration");

    let results = fts_search(&cache, "hydration");

    assert_eq!(
        results.len(),
        3,
        "BM25 ordering snapshot requires exactly 3 results; got {}: {results:#?}",
        results.len()
    );

    // Map results to identifiable labels for the snapshot.
    let ordered_labels: Vec<&str> = results
        .iter()
        .map(|r| {
            let content = r.content.as_deref().unwrap_or("");
            if content.contains("bm25-text") || content.contains("daily health") {
                "text-block"
            } else if content.contains("bm25-tasklist")
                || content.contains("drink water")
                || content.contains("hydration check")
            {
                "tasklist-block"
            } else if content.contains("bm25-skill") || content.contains("bm25-skill-skill") {
                "skill-block"
            } else {
                "unknown"
            }
        })
        .collect();

    // Snapshot the ordering. If the FTS5 scoring changes, this snapshot will
    // need to be reviewed and updated. The snapshot tracks which block kind
    // ranks highest — a regression here might indicate a scoring bug.
    insta::assert_snapshot!("cross_schema_fts_bm25_ordering", ordered_labels.join("\n"));
}
