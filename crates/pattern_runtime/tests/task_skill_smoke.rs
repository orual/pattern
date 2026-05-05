//! End-to-end smoke tests for the full Tasks + Skills SDK surface.
//!
//! Verifies AC10.1, AC10.2, AC10.3, AC10.4, AC10.5, AC10.8.
//!
//! Each test function uses its own isolated fixtures (fresh in-memory sqlite,
//! fresh in-memory store, no shared static state) so the tests run safely
//! under `--test-threads=N` (AC10.5).
//!
//! **Design note:** these tests call the internal handler functions directly
//! (e.g. `handle_create`, `handle_list`, `handle_load`) rather than going
//! through the Haskell eval path. This avoids a `preflight::check()` gate
//! while still exercising the full Rust SDK surface that the Haskell GADT
//! dispatches into. The handler functions were made `pub` specifically to
//! enable this level of integration testing. For the file location rationale
//! (pattern_runtime rather than pattern_memory), see the plan deviation note
//! in the task spec.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    BlockFilter, BlockSchema, IsolatePolicy, MemoryBlockType, MemorySearchScope, Scope,
    SearchContentType, SearchMode, SearchOptions, SkillMetadata, SkillTrustTier, TaskStatus,
};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_memory::fs::markdown_skill::{SkillFile, write_skill_to_loro_doc};
use pattern_memory::scope::{MemoryScope, ScopeBinding};
use pattern_runtime::sdk::handlers::skills::{
    handle_get_metadata, handle_list, handle_load, handle_search,
};
use pattern_runtime::sdk::handlers::tasks::{
    handle_create, handle_link, handle_list_tasks, handle_query_graph, handle_transition,
    handle_update,
};
use pattern_runtime::testing::in_memory_store::InMemoryMemoryStore;

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

const COMMON_KEYWORD: &str = "hydration";

/// Create a fresh agent row in the DB for FK constraint satisfaction.
fn create_agent(db: &ConstellationDb, agent_id: &str) {
    let agent = pattern_db::models::Agent {
        id: agent_id.to_string(),
        name: format!("Smoke Agent {agent_id}"),
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
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
        .expect("create_agent: FK seed must succeed");
}

/// Open a fresh in-memory `ConstellationDb` with all migrations applied.
fn open_db() -> ConstellationDb {
    ConstellationDb::open_in_memory().expect("open in-memory ConstellationDb")
}

/// Open a fresh in-memory `ConstellationDb` and a `MemoryCache` backed by it.
/// Seed an agent row so FK constraints are satisfied.
fn open_cache(agent_id: &str) -> (Arc<ConstellationDb>, Arc<MemoryCache>) {
    let db = Arc::new(open_db());
    create_agent(&db, agent_id);
    let cache = Arc::new(MemoryCache::new(db.clone()));
    (db, cache)
}

/// Create a TaskList block in the store using a global scope.
fn seed_task_list(store: &dyn MemoryStore, agent_id: &str, label: &str) {
    seed_task_list_scoped(store, &Scope::global(agent_id), label);
}

/// Create a TaskList block in the store using an explicit scope.
fn seed_task_list_scoped(store: &dyn MemoryStore, scope: &Scope, label: &str) {
    store
        .create_block(
            scope,
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
        .unwrap_or_else(|e| panic!("seed_task_list_scoped: create must succeed for {label}: {e}"));
}

/// Build a `TaskSpec` JSON string with the given subject.
fn task_spec(subject: &str) -> String {
    serde_json::to_string(&serde_json::json!({
        "subject": subject,
        "description": "",
        "metadata": null
    }))
    .unwrap()
}

/// Build a `TaskPatch` JSON string to update the subject.
fn task_patch_subject(new_subject: &str) -> String {
    serde_json::to_string(&serde_json::json!({ "subject": new_subject })).unwrap()
}

/// Seed a Skill block into a `MemoryCache`, wire its LoroDoc, then persist
/// so the FTS5 index is updated and `handle_search` can find it.
fn seed_skill_in_cache(
    cache: &MemoryCache,
    agent_id: &str,
    label: &str,
    metadata: SkillMetadata,
    body: &str,
) {
    let agent_scope = Scope::global(agent_id);
    cache
        .create_block(
            &agent_scope,
            BlockCreate::new(
                label,
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            ),
        )
        .unwrap_or_else(|e| panic!("seed_skill_in_cache: create failed for {label}: {e}"));

    let doc = cache
        .get_block(&agent_scope, label)
        .unwrap()
        .unwrap_or_else(|| panic!("seed_skill_in_cache: block {label} missing after create"));

    let skill_file = SkillFile {
        metadata,
        extras: loro::LoroValue::Map(Default::default()),
        body: body.to_string(),
    };
    write_skill_to_loro_doc(&skill_file, doc.inner()).unwrap_or_else(|e| {
        panic!("seed_skill_in_cache: write_skill_to_loro_doc for {label}: {e}")
    });
    doc.inner().commit();

    cache.mark_dirty(&Scope::global(agent_id).to_db_key(), label);
    cache
        .persist_block(&agent_scope, label)
        .unwrap_or_else(|e| panic!("seed_skill_in_cache: persist_block for {label}: {e}"));
}

/// Open a fresh in-memory usage-stats connection (separate from ConstellationDb,
/// as used by `handle_load` for skill stat writes).
fn open_usage_conn() -> rusqlite::Connection {
    let mut conn =
        rusqlite::Connection::open_in_memory().expect("open in-memory usage-stats connection");
    pattern_db::migrations::run_memory_migrations(&mut conn)
        .expect("run_memory_migrations on usage conn");
    conn
}

/// Reconcile a TaskList block's LoroDoc into the DB `tasks` + `task_edges` tables
/// so that `handle_list_tasks` and `handle_query_graph` return real results.
///
/// `handle_create/link/transition` mutate the LoroDoc (via InMemoryMemoryStore)
/// but the DB tables are only populated by the subscriber reconciler. In tests we
/// drive reconciliation explicitly so the list/query surface has data to return.
fn reconcile(store: &dyn MemoryStore, agent_id: &str, block: &str, db: &ConstellationDb) {
    reconcile_scoped(store, &Scope::global(agent_id), block, db);
}

fn reconcile_scoped(store: &dyn MemoryStore, scope: &Scope, block: &str, db: &ConstellationDb) {
    let sdoc = store
        .get_block(scope, block)
        .expect("reconcile_scoped: get_block must succeed")
        .unwrap_or_else(|| panic!("reconcile_scoped: block {block} must exist"));
    let loro_doc = sdoc.inner();

    let mut conn = db.get().unwrap();
    let tx = conn.transaction().unwrap();
    pattern_memory::subscriber::task::reconcile_task_list(&tx, block, loro_doc)
        .unwrap_or_else(|e| panic!("reconcile_scoped: reconcile_task_list for {block}: {e}"));
    tx.commit().unwrap();
}

// ---------------------------------------------------------------------------
// Test 1: smoke_tasks_surface
// ---------------------------------------------------------------------------

/// Full Tasks SDK round-trip.
///
/// Exercises: create_task → update_task → transition_status → link →
/// list_tasks → query_graph.
///
/// Asserts each result correctly populates LoroDoc state (via the handler
/// functions) and DB state (via explicit subscriber reconcile).
#[test]
fn smoke_tasks_surface() {
    const AGENT: &str = "tasks-surface-agent";
    const BLOCK: &str = "tasks-smoke";

    let store = Arc::new(InMemoryMemoryStore::new());
    let db = open_db();
    let agent_scope = Scope::global(AGENT);

    // Seed the TaskList block.
    seed_task_list(&*store, AGENT, BLOCK);

    // --- create_task ---

    let id_a = handle_create(&*store, &agent_scope, AGENT, BLOCK, &task_spec("Fix auth flow"))
        .expect("smoke_tasks_surface[step:create_a]: create must succeed");
    assert!(
        !id_a.as_str().is_empty(),
        "smoke_tasks_surface[step:create_a]: new task id must be non-empty"
    );

    let id_b = handle_create(&*store, &agent_scope, AGENT, BLOCK, &task_spec("Write docs"))
        .expect("smoke_tasks_surface[step:create_b]: create must succeed");
    assert_ne!(
        id_a, id_b,
        "smoke_tasks_surface[step:create_b]: two creates must produce distinct ids"
    );

    let id_c = handle_create(&*store, &agent_scope, AGENT, BLOCK, &task_spec("Deploy to staging"))
        .expect("smoke_tasks_surface[step:create_c]: create must succeed");

    // Verify LoroDoc has 3 items.
    {
        let sdoc = store.get_block(&agent_scope, BLOCK).unwrap().unwrap();
        let items = sdoc.inner().get_movable_list("items");
        assert_eq!(
            items.len(),
            3,
            "smoke_tasks_surface[step:create]: LoroDoc must have 3 task items"
        );
    }

    // --- update_task ---

    let edge_a = format!("{BLOCK}#{}", id_a.as_str());
    handle_update(
        &*store,
        &agent_scope,
        AGENT,
        &edge_a,
        &task_patch_subject("Fix OAuth2 flow"),
    )
    .expect("smoke_tasks_surface[step:update]: update must succeed");

    // Verify the LoroDoc reflects the updated subject.
    {
        let sdoc = store.get_block(&agent_scope, BLOCK).unwrap().unwrap();
        let items = sdoc.inner().get_movable_list("items");
        let loro::LoroValue::List(list) = items.get_deep_value() else {
            panic!("smoke_tasks_surface[step:update]: items must be a list");
        };
        let item_a = list
            .iter()
            .find_map(|v| {
                if let loro::LoroValue::Map(m) = v
                    && let Some(loro::LoroValue::String(id)) = m.get("id")
                    && id.as_str() == id_a.as_str()
                {
                    return Some(m.clone());
                }
                None
            })
            .expect("smoke_tasks_surface[step:update]: item_a must exist in list");
        let subject = item_a
            .get("subject")
            .expect("smoke_tasks_surface[step:update]: subject field must exist");
        let loro::LoroValue::String(subject_str) = subject else {
            panic!("subject must be a string");
        };
        assert_eq!(
            subject_str.as_str(),
            "Fix OAuth2 flow",
            "smoke_tasks_surface[step:update]: subject must reflect patch"
        );
    }

    // --- transition_status ---

    let status_completed = serde_json::to_string(&"completed").unwrap();
    handle_transition(&*store, &agent_scope, AGENT, &edge_a, &status_completed)
        .expect("smoke_tasks_surface[step:transition]: transition must succeed");

    // Verify LoroDoc item_a has status "completed".
    {
        let sdoc = store.get_block(&agent_scope, BLOCK).unwrap().unwrap();
        let items = sdoc.inner().get_movable_list("items");
        let loro::LoroValue::List(list) = items.get_deep_value() else {
            panic!("smoke_tasks_surface[step:transition]: items must be a list");
        };
        let item_a = list
            .iter()
            .find_map(|v| {
                if let loro::LoroValue::Map(m) = v
                    && let Some(loro::LoroValue::String(id)) = m.get("id")
                    && id.as_str() == id_a.as_str()
                {
                    return Some(m.clone());
                }
                None
            })
            .expect("smoke_tasks_surface[step:transition]: item_a must exist");
        let status = item_a
            .get("status")
            .expect("smoke_tasks_surface[step:transition]: status field must exist");
        let loro::LoroValue::String(status_str) = status else {
            panic!("status must be a string");
        };
        assert_eq!(
            status_str.as_str(),
            "completed",
            "smoke_tasks_surface[step:transition]: status must be 'completed'"
        );
    }

    // --- link ---

    let edge_b = format!("{BLOCK}#{}", id_b.as_str());
    let edge_c = format!("{BLOCK}#{}", id_c.as_str());
    handle_link(&*store, &agent_scope, AGENT, &edge_b, &edge_c)
        .expect("smoke_tasks_surface[step:link]: link must succeed");

    // --- list_tasks (via DB after reconcile) ---

    reconcile(&*store, AGENT, BLOCK, &db);

    let conn = db.get().unwrap();
    let views = handle_list_tasks(&*store, &conn, &agent_scope, Some(BLOCK), "{}")
        .expect("smoke_tasks_surface[step:list_tasks]: list must succeed");

    assert_eq!(
        views.len(),
        3,
        "smoke_tasks_surface[step:list_tasks]: list must return all 3 tasks; got {views:?}"
    );

    // item_a should be "completed".
    let view_a = views
        .iter()
        .find(|v| v.block_ref.task_item.as_deref() == Some(id_a.as_str()))
        .expect("smoke_tasks_surface[step:list_tasks]: item_a must appear in list");
    assert_eq!(
        view_a.subject, "Fix OAuth2 flow",
        "smoke_tasks_surface[step:list_tasks]: item_a subject must reflect update"
    );
    assert_eq!(
        view_a.status,
        TaskStatus::Completed,
        "smoke_tasks_surface[step:list_tasks]: item_a status must be Completed"
    );

    // item_b should have a link to item_c (blocks_count >= 1).
    let view_b = views
        .iter()
        .find(|v| v.block_ref.task_item.as_deref() == Some(id_b.as_str()))
        .expect("smoke_tasks_surface[step:list_tasks]: item_b must appear in list");
    assert_eq!(
        view_b.blocks_count, 1,
        "smoke_tasks_surface[step:list_tasks]: item_b must link to 1 task"
    );

    // --- query_graph ---

    let graph_query = serde_json::to_string(&serde_json::json!({
        "direction": "forward",
        "depth": 3,
        "max_nodes": 50
    }))
    .unwrap();

    let slice = handle_query_graph(&*store, &conn, &agent_scope, &edge_b, &graph_query)
        .expect("smoke_tasks_surface[step:query_graph]: query_graph must succeed");

    assert!(
        slice
            .nodes
            .iter()
            .any(|n| n.task_item.as_deref() == Some(id_b.as_str())),
        "smoke_tasks_surface[step:query_graph]: graph must include root node (item_b)"
    );
    assert!(
        slice
            .nodes
            .iter()
            .any(|n| n.task_item.as_deref() == Some(id_c.as_str())),
        "smoke_tasks_surface[step:query_graph]: graph must include linked target (item_c)"
    );
    // The edge (b → c) must appear in the returned edge set.
    let has_bc_edge = slice.edges.iter().any(|(src, tgt)| {
        src.task_item.as_deref() == Some(id_b.as_str())
            && tgt.task_item.as_deref() == Some(id_c.as_str())
    });
    assert!(
        has_bc_edge,
        "smoke_tasks_surface[step:query_graph]: edge b→c must appear in graph slice; \
         edges: {:?}",
        slice.edges
    );
}

// ---------------------------------------------------------------------------
// Test 2: smoke_skills_surface
// ---------------------------------------------------------------------------

/// Full Skills SDK round-trip.
///
/// Exercises: list → get_metadata → search → load.
///
/// Asserts:
/// - list contains seeded skill with correct trust_tier (AC8.1).
/// - get_metadata returns SkillMetadata with hooks JSON intact (AC8.2).
/// - search returns the seeded skill (AC8.4).
/// - load returns the rendered `[skill:loaded] … [skill:loaded:end]` text
///   directly as the tool_result body (AC9.1).
/// - canonical `.md` blake3 hash is unchanged before/after load (AC9.3 /
///   AC9.6 — content-hash invariant).
#[test]
fn smoke_skills_surface() {
    const AGENT: &str = "skills-surface-agent";

    let (db, cache) = open_cache(AGENT);
    let mut usage_conn = open_usage_conn();
    let agent_scope = Scope::global(AGENT);

    let hooks_value = serde_json::json!({
        "on_turn_start": [{"inject_context": "Check the auth flow."}],
        "on_tool_use": [{"log": "tool invoked"}]
    });
    let metadata = SkillMetadata {
        name: "oauth2-helper".to_string(),
        trust_tier: SkillTrustTier::FirstParty,
        description: Some("Handles OAuth2 authorization code flow".to_string()),
        keywords: vec!["oauth2".to_string(), "auth".to_string()],
        hooks: hooks_value.clone(),
        source_plugin_id: None,
    };
    let skill_body = "## OAuth2 Helper\n\nThis skill handles PKCE and token refresh.\n";

    seed_skill_in_cache(&cache, AGENT, "oauth2-helper", metadata.clone(), skill_body);

    // Also seed a decoy non-Skill block to verify list filtering.
    cache
        .create_block(
            &agent_scope,
            BlockCreate::new(
                "notes",
                MemoryBlockType::Working,
                BlockSchema::Text { viewport: None },
            ),
        )
        .expect("smoke_skills_surface: create decoy Text block");

    // --- list ---

    let conn = db.get().unwrap();
    let infos =
        handle_list(&*cache, &conn, &agent_scope).expect("smoke_skills_surface[step:list]: must succeed");

    assert_eq!(
        infos.len(),
        1,
        "smoke_skills_surface[step:list]: must return exactly 1 Skill (Text decoy excluded); \
         got {infos:?}"
    );
    assert_eq!(
        infos[0].name, "oauth2-helper",
        "smoke_skills_surface[step:list]: skill name must match"
    );
    assert_eq!(
        infos[0].trust_tier,
        SkillTrustTier::FirstParty,
        "smoke_skills_surface[step:list]: trust_tier must match seeded value"
    );
    assert_eq!(
        infos[0].keywords,
        vec!["oauth2", "auth"],
        "smoke_skills_surface[step:list]: keywords must match"
    );
    assert!(
        infos[0].last_used.is_none(),
        "smoke_skills_surface[step:list]: last_used must be None before any load"
    );

    // --- get_metadata ---

    let returned_meta = handle_get_metadata(&*cache, &agent_scope, "oauth2-helper")
        .expect("smoke_skills_surface[step:get_metadata]: must not error")
        .expect("smoke_skills_surface[step:get_metadata]: must return Some");

    assert_eq!(
        returned_meta.name, "oauth2-helper",
        "smoke_skills_surface[step:get_metadata]: name must round-trip"
    );
    assert_eq!(
        returned_meta.trust_tier,
        SkillTrustTier::FirstParty,
        "smoke_skills_surface[step:get_metadata]: trust_tier must round-trip"
    );
    assert_eq!(
        returned_meta.description.as_deref(),
        Some("Handles OAuth2 authorization code flow"),
        "smoke_skills_surface[step:get_metadata]: description must round-trip"
    );
    assert_eq!(
        returned_meta.hooks, hooks_value,
        "smoke_skills_surface[step:get_metadata]: hooks JSON must be preserved intact \
         through the LoroDoc bridge (AC8.2)"
    );

    // --- get_metadata on non-Skill returns None (AC8.3) ---

    let none_result = handle_get_metadata(&*cache, &agent_scope, "notes")
        .expect("smoke_skills_surface[step:get_metadata_text]: must not error");
    assert!(
        none_result.is_none(),
        "smoke_skills_surface[step:get_metadata_text]: Text block must return None (AC8.3)"
    );

    // --- search ---

    let search_results = handle_search(&*cache, &conn, &agent_scope, "oauth2")
        .expect("smoke_skills_surface[step:search]: must succeed");

    assert!(
        !search_results.is_empty(),
        "smoke_skills_surface[step:search]: search must return at least 1 result for 'oauth2'"
    );
    assert!(
        search_results.iter().any(|r| r.name == "oauth2-helper"),
        "smoke_skills_surface[step:search]: seeded skill must appear in results (AC8.4)"
    );

    // --- canonical body hash before load ---

    let body_before = {
        let sdoc = cache
            .get_block(&agent_scope, "oauth2-helper")
            .unwrap()
            .expect("smoke_skills_surface: block must exist before load");
        sdoc.inner().get_text("body").to_string()
    };
    let hash_before = blake3::hash(body_before.as_bytes());

    // --- load ---
    // handle_load returns the rendered [skill:loaded] text directly as the
    // tool_result body (AC9.1). Persistence across turns is structurally
    // guaranteed because tool_result messages flow through active_messages().
    let rendered = handle_load(&*cache, &mut usage_conn, &agent_scope, AGENT, "oauth2-helper")
        .expect("smoke_skills_surface[step:load]: load must succeed (AC9.1)");

    assert!(
        rendered.contains("[skill:loaded]"),
        "smoke_skills_surface[step:load]: rendered text must contain [skill:loaded] marker"
    );
    assert!(
        rendered.contains("[skill:loaded:end]"),
        "smoke_skills_surface[step:load]: rendered text must contain [skill:loaded:end] marker"
    );
    assert!(
        rendered.contains("oauth2-helper"),
        "smoke_skills_surface[step:load]: rendered text must contain skill name"
    );
    assert!(
        rendered.contains("OAuth2 Helper"),
        "smoke_skills_surface[step:load]: rendered text must contain body heading"
    );
    assert!(
        !rendered.contains("<system-reminder>"),
        "smoke_skills_surface[step:load]: rendered text must NOT be wrapped in \
         <system-reminder> (tool_result has its own role-based framing)"
    );

    // --- canonical body hash after load — must be unchanged (AC9.3 / AC9.6) ---

    let body_after = {
        let sdoc = cache
            .get_block(&agent_scope, "oauth2-helper")
            .unwrap()
            .expect("smoke_skills_surface: block must exist after load");
        sdoc.inner().get_text("body").to_string()
    };
    let hash_after = blake3::hash(body_after.as_bytes());

    assert_eq!(
        hash_before, hash_after,
        "smoke_skills_surface[step:load]: LoroDoc body blake3 hash must be unchanged after load \
         (AC9.3 / Mode-A invariant)"
    );
    assert_eq!(
        body_before, body_after,
        "smoke_skills_surface[step:load]: body strings must be identical before and after load"
    );

    // --- usage stats updated ---

    let bh = pattern_core::types::block::BlockHandle::new("oauth2-helper");
    let stats = pattern_db::queries::skill_usage::get_usage_stats(&usage_conn, &bh)
        .expect("smoke_skills_surface[step:load]: get_usage_stats must succeed");
    assert_eq!(
        stats.use_count, 1,
        "smoke_skills_surface[step:load]: use_count must be 1 after one load (AC9.3)"
    );
    assert!(
        stats.last_used.is_some(),
        "smoke_skills_surface[step:load]: last_used must be populated after load"
    );
}

// ---------------------------------------------------------------------------
// Test 3: smoke_cross_schema_fts
// ---------------------------------------------------------------------------

/// Cross-schema FTS5 coverage.
///
/// Seeds one Text block, one TaskList block (with task content), and one Skill
/// block — all containing the keyword "hydration". Runs a single search query
/// that matches at least one of each type. Asserts all three appear in results.
///
/// Note on BM25 ordering: FTS5 BM25 scoring varies across SQLite versions.
/// We verify that all three block types appear in results and assert the result
/// count is >= 3, but do not snapshot a fixed ordering. The existing
/// `search_relevance_ranked` insta snapshot in `handlers/skills.rs` already
/// pins skill-only ordering; cross-schema ordering is left unsnapshotted to
/// avoid CI fragility.
///
/// Verifies AC10.8.
#[test]
fn smoke_cross_schema_fts() {
    const AGENT: &str = "fts-smoke-agent";

    let (db, cache) = open_cache(AGENT);
    let agent_scope = Scope::global(AGENT);

    // --- Text block ---
    cache
        .create_block(
            &agent_scope,
            BlockCreate::new(
                "text-hydration",
                MemoryBlockType::Core,
                BlockSchema::Text { viewport: None },
            ),
        )
        .expect("smoke_cross_schema_fts: create text block");

    {
        let sdoc = cache.get_block(&agent_scope, "text-hydration").unwrap().unwrap();
        sdoc.set_text(
            &format!("The {COMMON_KEYWORD} protocol keeps agents in sync."),
            false,
        )
        .expect("set_text");
    }
    cache.mark_dirty(&Scope::global(AGENT).to_db_key(), "text-hydration");
    cache
        .persist_block(&agent_scope, "text-hydration")
        .expect("smoke_cross_schema_fts: persist text block");

    // --- Skill block ---
    seed_skill_in_cache(
        &cache,
        AGENT,
        "skill-hydration",
        SkillMetadata {
            name: format!("{COMMON_KEYWORD}-monitor"),
            trust_tier: SkillTrustTier::ProjectLocal,
            description: Some(format!("Monitors {COMMON_KEYWORD} levels")),
            keywords: vec![COMMON_KEYWORD.to_string()],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        },
        &format!("## {COMMON_KEYWORD} Monitor\n\nTracks fluid intake.\n"),
    );

    // --- TaskList block (FTS5 indexed via persist_block on MemoryCache) ---
    cache
        .create_block(
            &agent_scope,
            BlockCreate::new(
                "tasks-hydration",
                MemoryBlockType::Working,
                BlockSchema::TaskList {
                    default_status: None,
                    default_owner: None,
                    display_limit: None,
                },
            ),
        )
        .expect("smoke_cross_schema_fts: create task list block");

    // Write task item content mentioning COMMON_KEYWORD via LoroDoc directly so
    // the FTS index picks it up on persist_block.
    {
        let sdoc = cache.get_block(&agent_scope, "tasks-hydration").unwrap().unwrap();
        let doc = sdoc.inner();
        let list = doc.get_movable_list("items");
        let item_map = list
            .push_container(loro::LoroMap::new())
            .expect("smoke_cross_schema_fts: push_container for task item");
        item_map
            .insert("id", "task-hydration-01")
            .expect("smoke_cross_schema_fts: insert task id");
        item_map
            .insert(
                "subject",
                format!("Implement {COMMON_KEYWORD} tracking feature").as_str(),
            )
            .expect("smoke_cross_schema_fts: insert task subject");
        item_map
            .insert("status", "pending")
            .expect("smoke_cross_schema_fts: insert task status");
        doc.commit();
    }
    cache.mark_dirty(&Scope::global(AGENT).to_db_key(), "tasks-hydration");
    cache
        .persist_block(&agent_scope, "tasks-hydration")
        .expect("smoke_cross_schema_fts: persist task list block");

    // --- Search across all block types ---

    let opts = SearchOptions {
        mode: SearchMode::Fts,
        content_types: vec![SearchContentType::Blocks],
        limit: 50,
    };
    let results = cache
        .search(
            COMMON_KEYWORD,
            opts,
            MemorySearchScope::Scope(Scope::global(AGENT)),
        )
        .expect("smoke_cross_schema_fts[step:search]: must succeed");

    // Verify at least 3 results (one per block type).
    assert!(
        results.len() >= 3,
        "smoke_cross_schema_fts[step:search]: must return >= 3 results for '{}'; \
         got {}: {results:?}",
        COMMON_KEYWORD,
        results.len()
    );

    // All three blocks must appear in results (AC10.8).
    // MemorySearchResult.id is the memory_blocks DB UUID. To check which
    // block labels are present, we look up the block metadata by id via list_blocks.
    // Use an encoded scope key so the filter matches blocks stored with
    // agent_id = Scope::global(AGENT).to_db_key() (i.e. "global:<AGENT>").
    let all_metas = cache
        .list_blocks(BlockFilter::by_scope(&agent_scope))
        .expect("smoke_cross_schema_fts: list_blocks must succeed");
    let id_to_label: std::collections::HashMap<&str, &str> = all_metas
        .iter()
        .map(|m| (m.id.as_str(), m.label.as_str()))
        .collect();

    let result_labels: Vec<&str> = results
        .iter()
        .filter_map(|r| id_to_label.get(r.id.as_str()).copied())
        .collect();

    assert!(
        result_labels.contains(&"text-hydration"),
        "smoke_cross_schema_fts[step:search]: Text block must appear in results (AC10.8); \
         got {result_labels:?}"
    );
    assert!(
        result_labels.contains(&"skill-hydration"),
        "smoke_cross_schema_fts[step:search]: Skill block must appear in results (AC10.8); \
         got {result_labels:?}"
    );
    assert!(
        result_labels.contains(&"tasks-hydration"),
        "smoke_cross_schema_fts[step:search]: TaskList block must appear in results (AC10.8); \
         got {result_labels:?}"
    );

    // Verify schema diversity in the result set.
    let schemas_present: std::collections::HashSet<String> = all_metas
        .iter()
        .filter(|m| result_labels.contains(&m.label.as_str()))
        .map(|m| format!("{:?}", m.schema))
        .collect();
    assert_eq!(
        schemas_present.len(),
        3,
        "smoke_cross_schema_fts[step:search]: results must span 3 distinct block schemas \
         (Text, TaskList, Skill); got {schemas_present:?}"
    );

    let _ = db; // db lifetime bound to cache; silence unused warning.
}

// ---------------------------------------------------------------------------
// Test 4: smoke_scope_enforcement
// ---------------------------------------------------------------------------

/// Scope enforcement: `IsolatePolicy::Full` hides persona blocks from everyone
/// while project blocks remain visible to all callers.
///
/// Scenario:
/// - Project agent "project-a" owns a TaskList + Skill block (project context).
/// - Persona agent "persona-a" owns its own TaskList + Skill block (persona context).
/// - Both are mounted under `MemoryScope::Full`.
///
/// Under `IsolatePolicy::Full` semantics (verified by the unit test
/// `list_tasks_respects_full_isolation_hides_persona_tasklist`):
/// - The scope ignores the `agent_id` filter on `list_blocks` and returns
///   **only project blocks** for all callers.
/// - Persona blocks are invisible — even to the persona itself.
/// - `handle_list_tasks` and `handle_list` called with `agent_id=PERSONA`
///   both return only PROJECT-scoped results (not empty, not persona-scoped).
/// - The persona cannot create new blocks (IsolationDenied).
///
/// This satisfies AC10.3: persona-scoped TaskList + Skill blocks are invisible
/// under Full isolation.
#[test]
fn smoke_scope_enforcement() {
    const PERSONA: &str = "persona-a";
    const PROJECT: &str = "project-a";

    let inner_store = InMemoryMemoryStore::new();
    let db = open_db();
    // Project blocks are stored under Scope::local so that MemoryScope::list_blocks
    // under Full isolation (which filters by Scope::Local(project_id).to_db_key())
    // can find them.
    let project_scope = Scope::local(PROJECT);
    let persona_scope = Scope::global(PERSONA);

    // Seed a TaskList block under the project agent (project context).
    seed_task_list_scoped(&inner_store, &project_scope, "project-tasks");
    let project_task_id = handle_create(
        &inner_store,
        &project_scope,
        PROJECT,
        "project-tasks",
        &task_spec("Project-scoped task"),
    )
    .expect("smoke_scope_enforcement: create project task must succeed");

    // Also seed a TaskList block under the persona agent (persona context).
    // This will be invisible under Full isolation — even to the persona itself.
    seed_task_list(&inner_store, PERSONA, "persona-tasks");
    handle_create(
        &inner_store,
        &persona_scope,
        PERSONA,
        "persona-tasks",
        &task_spec("Persona-scoped task (must be hidden)"),
    )
    .expect("smoke_scope_enforcement: create persona task must succeed");

    // Reconcile both TaskLists into the DB.
    reconcile_scoped(&inner_store, &project_scope, "project-tasks", &db);
    reconcile(&inner_store, PERSONA, "persona-tasks", &db);

    // Seed a Skill block under the project agent (visible under Full).
    inner_store
        .create_block(
            &project_scope,
            BlockCreate::new(
                "project-skill",
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            ),
        )
        .expect("smoke_scope_enforcement: create project skill block");
    {
        let sdoc = inner_store
            .get_block(&project_scope, "project-skill")
            .unwrap()
            .unwrap();
        let skill_file = SkillFile {
            metadata: SkillMetadata {
                name: "project-only-skill".to_string(),
                trust_tier: SkillTrustTier::ProjectLocal,
                description: Some("Visible only in project scope".to_string()),
                keywords: vec!["scoped".to_string()],
                hooks: serde_json::Value::Null,
                source_plugin_id: None,
            },
            extras: loro::LoroValue::Map(Default::default()),
            body: "## Project Skill\n\nFor project use only.\n".to_string(),
        };
        write_skill_to_loro_doc(&skill_file, sdoc.inner())
            .expect("smoke_scope_enforcement: write_skill_to_loro_doc (project skill)");
        sdoc.inner().commit();
    }

    // Seed a Skill block under the persona agent (invisible under Full).
    inner_store
        .create_block(
            &persona_scope,
            BlockCreate::new(
                "persona-skill",
                MemoryBlockType::Working,
                BlockSchema::Skill {
                    expected_keys: vec![],
                },
            ),
        )
        .expect("smoke_scope_enforcement: create persona skill block");
    {
        let sdoc = inner_store
            .get_block(&persona_scope, "persona-skill")
            .unwrap()
            .unwrap();
        let skill_file = SkillFile {
            metadata: SkillMetadata {
                name: "persona-only-skill".to_string(),
                trust_tier: SkillTrustTier::AdHoc,
                description: Some("Must be hidden under Full isolation".to_string()),
                keywords: vec!["persona".to_string()],
                hooks: serde_json::Value::Null,
                source_plugin_id: None,
            },
            extras: loro::LoroValue::Map(Default::default()),
            body: "## Persona Skill\n\nPersona-private.\n".to_string(),
        };
        write_skill_to_loro_doc(&skill_file, sdoc.inner())
            .expect("smoke_scope_enforcement: write_skill_to_loro_doc (persona skill)");
        sdoc.inner().commit();
    }

    // Wrap with MemoryScope::Full:
    // - Persona blocks are invisible to everyone (including the persona).
    // - Project blocks are visible to everyone (including the persona).
    // Under Full isolation, `list_blocks(agent_id=*)` always returns project blocks.
    let scope = MemoryScope::new(
        inner_store,
        ScopeBinding::with_project(PERSONA, PROJECT, IsolatePolicy::Full),
    );

    let db_conn = db.get().unwrap();

    // --- Persona caller: list_tasks → sees ONLY project tasks (not persona tasks) ---
    // Under Full isolation the scope returns project TaskList blocks for any caller.
    // The persona's own TaskList is invisible; only project-tasks rows appear.
    let persona_caller_tasks = handle_list_tasks(&scope, &db_conn, &persona_scope, None, "{}")
        .expect("smoke_scope_enforcement[step:list_tasks_persona_caller]: must not error");
    assert_eq!(
        persona_caller_tasks.len(),
        1,
        "smoke_scope_enforcement[step:list_tasks_persona_caller]: persona caller under Full isolation \
         must see exactly 1 task (the project task, not the persona task); \
         got {persona_caller_tasks:?}"
    );
    assert_eq!(
        persona_caller_tasks[0].block_ref.block.as_str(),
        "project-tasks",
        "smoke_scope_enforcement[step:list_tasks_persona_caller]: visible task must be in \
         project-tasks, not persona-tasks — persona blocks are invisible (AC10.3)"
    );
    assert_eq!(
        persona_caller_tasks[0].block_ref.task_item.as_deref(),
        Some(project_task_id.as_str()),
        "smoke_scope_enforcement[step:list_tasks_persona_caller]: task id must match project task"
    );

    // --- Verify persona-scoped task block_ref is NOT in the visible set ---
    let visible_blocks: std::collections::HashSet<&str> = persona_caller_tasks
        .iter()
        .map(|v| v.block_ref.block.as_str())
        .collect();
    assert!(
        !visible_blocks.contains("persona-tasks"),
        "smoke_scope_enforcement[step:persona_block_hidden]: persona-tasks block must be \
         hidden under Full isolation (AC10.3); visible: {visible_blocks:?}"
    );

    // --- Persona caller: skills.list → sees ONLY project skill (not persona skill) ---
    let persona_caller_skills = handle_list(&scope, &db_conn, &persona_scope)
        .expect("smoke_scope_enforcement[step:list_skills_persona_caller]: must not error");
    assert_eq!(
        persona_caller_skills.len(),
        1,
        "smoke_scope_enforcement[step:list_skills_persona_caller]: persona caller under Full \
         isolation must see exactly 1 skill (the project skill, not the persona skill); \
         got {persona_caller_skills:?}"
    );
    assert_eq!(
        persona_caller_skills[0].name, "project-only-skill",
        "smoke_scope_enforcement[step:list_skills_persona_caller]: visible skill must be \
         'project-only-skill', not 'persona-only-skill' — persona skill is invisible (AC10.3)"
    );

    // --- Project caller: list_tasks → sees project tasks (same as persona caller) ---
    let project_caller_tasks = handle_list_tasks(&scope, &db_conn, &project_scope, None, "{}")
        .expect("smoke_scope_enforcement[step:list_tasks_project_caller]: must not error");
    assert_eq!(
        project_caller_tasks.len(),
        1,
        "smoke_scope_enforcement[step:list_tasks_project_caller]: project caller must see \
         1 task; got {project_caller_tasks:?}"
    );
    assert_eq!(
        project_caller_tasks[0].subject, "Project-scoped task",
        "smoke_scope_enforcement[step:list_tasks_project_caller]: task subject must match seeded value"
    );

    // --- Project caller: skills.list → sees project skill ---
    let project_caller_skills = handle_list(&scope, &db_conn, &project_scope)
        .expect("smoke_scope_enforcement[step:list_skills_project_caller]: must not error");
    assert_eq!(
        project_caller_skills.len(),
        1,
        "smoke_scope_enforcement[step:list_skills_project_caller]: project caller must see 1 skill; \
         got {project_caller_skills:?}"
    );
    assert_eq!(
        project_caller_skills[0].name, "project-only-skill",
        "smoke_scope_enforcement[step:list_skills_project_caller]: skill name must match seeded value"
    );

    // --- Write isolation: persona caller cannot create blocks (IsolationDenied) ---
    // Under Full isolation, writing to the persona scope is denied.
    let write_result = scope.create_block(
        &persona_scope,
        BlockCreate::new(
            "new-persona-block",
            MemoryBlockType::Working,
            BlockSchema::text(),
        ),
    );
    assert!(
        write_result.is_err(),
        "smoke_scope_enforcement[step:write_isolation]: persona write must be denied under \
         Full isolation (AC10.3)"
    );
    let err = write_result.unwrap_err();
    assert!(
        matches!(
            err,
            pattern_core::types::memory_types::MemoryError::IsolationDenied { .. }
        ),
        "smoke_scope_enforcement[step:write_isolation]: error must be IsolationDenied, got {err:?}"
    );
}
