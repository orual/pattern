//! Handler for `Pattern.Skills` — skill-operation surface (list, get_metadata, load, search, get_usage_stats).
//!
//! The handler wires five methods: list, get_metadata, load, search, and get_usage_stats.
//! Methods delegate to MemoryStore and pattern_db for data access.
//!
//! Tasks 5–7 of Phase 5 fill the method bodies. Tasks 5–6 implement the
//! read-only surface (list, get_metadata, get_usage_stats, search). Task 7
//! adds the mutating `load` handler (segment-2 injection + sqlite stat write).

use loro::LoroValue;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockHandle;
use pattern_core::types::memory_types::{
    BlockFilter, BlockMetadata, BlockSchema, MemorySearchScope, SearchContentType, SearchMode,
    SearchOptions, SkillError, SkillInfo, SkillMetadata, SkillUsageStats,
};

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::SkillsReq;
use crate::session::SessionContext;
use crate::timeout::HandlerGuard;

/// Handler for `Pattern.Skills`.
///
/// Unit-struct (mirrors [`crate::sdk::handlers::TasksHandler`]). The per-call
/// memory store comes from `cx.user().memory_store()`, which respects the active
/// `IsolatePolicy` scope routing. The DB connection for usage stats comes from
/// `cx.user().db()`.
#[derive(Clone)]
pub struct SkillsHandler;

impl std::fmt::Debug for SkillsHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillsHandler").finish_non_exhaustive()
    }
}

impl DescribeEffect for SkillsHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Skills",
            description: "Skill-block operations: list, get_metadata, load, search, get_usage_stats",
            constructors: &[
                "List          :: Skills Text",
                "GetMetadata   :: BlockHandle -> Skills Text",
                "Load          :: BlockHandle -> Skills ()",
                "Search        :: Text -> Skills Text",
                "GetUsageStats :: BlockHandle -> Skills Text",
            ],
            type_defs: &[
                "type BlockHandle = Text",
                "type SkillInfo = Text       -- JSON: {handle:BlockHandle, name:Text, description?:Text, trust_tier:Text, keywords:[Text], last_used?:Text}",
                "type SkillMetadata = Text   -- JSON: {name:Text, description?:Text, version?:Text, trust_tier:Text, keywords:[Text], hooks:Value}",
                "type SkillUsageStats = Text -- JSON: {handle:BlockHandle, use_count:Int, last_used?:Text, last_used_by?:Text}",
            ],
            helpers: &[
                "listSkills :: Member Skills effs => Eff effs Text\nlistSkills = send List",
                "getSkillMetadata :: Member Skills effs => BlockHandle -> Eff effs Text\ngetSkillMetadata h = send (GetMetadata h)",
                "loadSkill :: Member Skills effs => BlockHandle -> Eff effs ()\nloadSkill h = send (Load h)",
                "searchSkills :: Member Skills effs => Text -> Eff effs Text\nsearchSkills q = send (Search q)",
                "getSkillUsageStats :: Member Skills effs => BlockHandle -> Eff effs Text\ngetSkillUsageStats h = send (GetUsageStats h)",
            ],
        }
    }
}

/// `EffectHandler<SessionContext>` impl.
///
/// The `SessionContext` bound (tighter than `HasCancelState`) is required because
/// `get_usage_stats` and `list` need `cx.user().db()` to query the sqlite
/// `skill_usage_stats` table. This mirrors `TasksHandler`, which uses the same
/// bound for the same reason.
impl EffectHandler<SessionContext> for SkillsHandler {
    type Request = SkillsReq;

    fn handle(
        &mut self,
        req: SkillsReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let agent_id = cx.user().agent_id().to_string();
        let store = cx.user().memory_store();
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        match req {
            SkillsReq::List => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Skills::List: db connection: {e}"))
                })?;
                let infos = handle_list(&*store, &conn, &agent_id)?;
                let items: Vec<String> = infos
                    .iter()
                    .map(|info| serde_json::to_string(info).unwrap_or_default())
                    .collect();
                cx.respond(items)
            }
            SkillsReq::GetMetadata(handle) => {
                let result = handle_get_metadata(&*store, &agent_id, &handle)?;
                cx.respond(
                    result
                        .map(|m| serde_json::to_string(&m).unwrap_or_default())
                        .unwrap_or_else(|| "null".to_string()),
                )
            }
            SkillsReq::Load(_) => {
                // Task 7: load handler — segment-2 injection + sqlite stat write.
                // Not yet implemented; surface a clear diagnostic until Task 7 lands.
                Err(EffectError::Handler(
                    "Pattern.Skills.Load is not yet implemented \
                     (Phase 5 Task 7). Agent code should not call Load \
                     before the handler implementation lands."
                        .to_string(),
                ))
            }
            SkillsReq::Search(query) => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Skills::Search: db connection: {e}"))
                })?;
                let infos = handle_search(&*store, &conn, &agent_id, &query)?;
                let items: Vec<String> = infos
                    .iter()
                    .map(|info| serde_json::to_string(info).unwrap_or_default())
                    .collect();
                cx.respond(items)
            }
            SkillsReq::GetUsageStats(handle) => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!(
                        "Pattern.Skills::GetUsageStats: db connection: {e}"
                    ))
                })?;
                let stats = handle_get_usage_stats(&*store, &conn, &agent_id, &handle)?;
                cx.respond(serde_json::to_string(&stats).unwrap_or_default())
            }
        }
    }
}

// region: internal error type

/// Errors raised by skill handlers. Converted to `EffectError::Handler` at the
/// dispatch boundary so unit tests can match on `SkillHandlerError` precisely.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum SkillHandlerError {
    /// Block doesn't exist for this agent.
    #[error("no block {block:?} for agent {agent:?}")]
    BlockNotFound { agent: String, block: String },
    /// Block has the wrong schema for this operation.
    #[error(transparent)]
    Skill(#[from] SkillError),
    /// LoroDoc projection or MemoryStore call failed.
    #[error("Pattern.Skills: store error: {0}")]
    Store(String),
    /// The skill's LoroDoc metadata could not be projected.
    #[error("Pattern.Skills: malformed LoroDoc for block {block:?}: {detail}")]
    MalformedLoro { block: String, detail: String },
    /// SQLite call failed.
    #[error("Pattern.Skills: sqlite error: {0}")]
    Sqlite(String),
}

impl From<SkillHandlerError> for EffectError {
    fn from(e: SkillHandlerError) -> Self {
        EffectError::Handler(format!("{e}"))
    }
}

// endregion: internal error type

// region: helpers

/// Project a [`SkillMetadata`] from a block's LoroDoc deep value.
///
/// Calls `project_metadata_from_loro` on the doc's root map; converts the
/// string-error into a [`SkillHandlerError::MalformedLoro`] for structured
/// propagation.
fn project_skill_metadata(
    doc: &loro::LoroDoc,
    handle: &str,
) -> Result<SkillMetadata, SkillHandlerError> {
    let deep = doc.get_deep_value();
    let root_map = match &deep {
        LoroValue::Map(m) => m.clone(),
        other => {
            return Err(SkillHandlerError::MalformedLoro {
                block: handle.to_string(),
                detail: format!("root value is not a LoroMap; got {other:?}"),
            });
        }
    };
    pattern_memory::fs::markdown_skill::project_metadata_from_loro(&root_map).map_err(|e| {
        SkillHandlerError::MalformedLoro {
            block: handle.to_string(),
            detail: e,
        }
    })
}

// endregion: helpers

// region: handlers

/// List all Skill-schema blocks visible to `agent_id`.
///
/// Enumerates blocks via `store.list_blocks`, filters to `BlockSchema::Skill`,
/// projects each block's LoroDoc into `SkillMetadata`, batch-fetches usage
/// stats from sqlite, and assembles `SkillInfo` records.
pub(crate) fn handle_list(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    agent_id: &str,
) -> Result<Vec<SkillInfo>, SkillHandlerError> {
    let all_meta = store
        .list_blocks(BlockFilter::by_agent(agent_id))
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?;

    // Filter to Skill-schema blocks only.
    let skill_meta: Vec<_> = all_meta
        .into_iter()
        .filter(|m| matches!(m.schema, BlockSchema::Skill { .. }))
        .collect();

    if skill_meta.is_empty() {
        return Ok(Vec::new());
    }

    // Collect handles for batch usage-stat query.
    let handles: Vec<BlockHandle> = skill_meta
        .iter()
        .map(|m| BlockHandle::new(&m.label))
        .collect();

    let usage_map = pattern_db::queries::skill_usage::get_usage_stats_batch(conn, &handles)
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;

    // Project each Skill block's LoroDoc into SkillInfo.
    let mut infos = Vec::with_capacity(skill_meta.len());
    for meta in &skill_meta {
        let handle = BlockHandle::new(&meta.label);
        let sdoc = store
            .get_block(agent_id, &meta.label)
            .map_err(|e| SkillHandlerError::Store(e.to_string()))?
            .ok_or_else(|| SkillHandlerError::BlockNotFound {
                agent: agent_id.to_string(),
                block: meta.label.clone(),
            })?;

        let skill_meta_proj = project_skill_metadata(sdoc.inner(), &meta.label)?;
        let last_used = usage_map.get(&handle).and_then(|s| s.last_used);

        infos.push(SkillInfo {
            handle: handle.clone(),
            name: skill_meta_proj.name,
            description: skill_meta_proj.description,
            trust_tier: skill_meta_proj.trust_tier,
            keywords: skill_meta_proj.keywords,
            last_used,
        });
    }

    Ok(infos)
}

/// Fetch typed `SkillMetadata` for a block handle.
///
/// Returns `None` if the block's schema is not Skill (per AC8.3 — this is
/// not an error, just a typed `Option<SkillMetadata>`). Returns an error
/// if the block doesn't exist.
pub(crate) fn handle_get_metadata(
    store: &dyn MemoryStore,
    agent_id: &str,
    handle: &str,
) -> Result<Option<SkillMetadata>, SkillHandlerError> {
    let sdoc = store
        .get_block(agent_id, handle)
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?
        .ok_or_else(|| SkillHandlerError::BlockNotFound {
            agent: agent_id.to_string(),
            block: handle.to_string(),
        })?;

    // If the schema is not Skill, return None per AC8.3.
    if !matches!(sdoc.schema(), BlockSchema::Skill { .. }) {
        return Ok(None);
    }

    let metadata = project_skill_metadata(sdoc.inner(), handle)?;
    Ok(Some(metadata))
}

/// Retrieve usage statistics for a single Skill block.
///
/// Returns `SkillUsageStats::default()` when no row exists (the skill has
/// never been loaded on this install). Returns an error if the block does
/// not exist or is not a Skill block.
pub(crate) fn handle_get_usage_stats(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    agent_id: &str,
    handle: &str,
) -> Result<SkillUsageStats, SkillHandlerError> {
    // Verify the block exists and is visible to this agent.
    let sdoc = store
        .get_block(agent_id, handle)
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?
        .ok_or_else(|| SkillHandlerError::BlockNotFound {
            agent: agent_id.to_string(),
            block: handle.to_string(),
        })?;

    // Scope-check: the block must be a Skill block.
    if !matches!(sdoc.schema(), BlockSchema::Skill { .. }) {
        return Err(SkillHandlerError::Skill(SkillError::NotASkill(
            BlockHandle::new(handle),
        )));
    }

    let bh = BlockHandle::new(handle);
    let stats = pattern_db::queries::skill_usage::get_usage_stats(conn, &bh)
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;

    Ok(stats)
}

/// Search for Skill blocks matching `query`.
///
/// Calls `store.search()` with FTS5 over the agent's blocks, then post-filters
/// to only results whose block schema is `BlockSchema::Skill`. Projects
/// matched blocks into `SkillInfo` with batch usage-stat join.
///
/// Note: `MemoryStore::search` returns `MemorySearchResult` where `id` is the
/// `memory_blocks.id` UUID (not the label). To correlate to block labels, we
/// enumerate all Skill blocks for this agent and intersect. This is O(n) in the
/// number of skill blocks and O(1) DB queries — acceptable for the expected scale.
pub(crate) fn handle_search(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    agent_id: &str,
    query: &str,
) -> Result<Vec<SkillInfo>, SkillHandlerError> {
    let opts = SearchOptions {
        mode: SearchMode::Fts,
        content_types: vec![SearchContentType::Blocks],
        limit: 50,
    };

    let search_results = store
        .search(query, opts, MemorySearchScope::Agent(agent_id.into()))
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?;

    if search_results.is_empty() {
        return Ok(Vec::new());
    }

    // Collect the result IDs (memory_blocks.id UUIDs) so we can correlate.
    // Results are already ordered by BM25 score (descending) from the store.
    let result_ids: Vec<&str> = search_results.iter().map(|r| r.id.as_str()).collect();

    // Enumerate all Skill blocks for this agent to build a label↔id mapping.
    let all_meta = store
        .list_blocks(BlockFilter::by_agent(agent_id))
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?;

    // Build a map from memory_id → BlockMetadata for Skill blocks only.
    // BlockMetadata.id is the memory_blocks DB UUID.
    let skill_by_id: std::collections::HashMap<&str, &BlockMetadata> = all_meta
        .iter()
        .filter(|m| matches!(m.schema, BlockSchema::Skill { .. }))
        .map(|m| (m.id.as_str(), m))
        .collect();

    // Walk search results in BM25 order; keep only Skill hits.
    let mut matched_labels: Vec<String> = Vec::new();
    for id in &result_ids {
        if let Some(meta) = skill_by_id.get(*id) {
            matched_labels.push(meta.label.clone());
        }
    }

    if matched_labels.is_empty() {
        return Ok(Vec::new());
    }

    // Batch-fetch usage stats for matched skill labels.
    let handles: Vec<BlockHandle> = matched_labels.iter().map(BlockHandle::new).collect();
    let usage_map = pattern_db::queries::skill_usage::get_usage_stats_batch(conn, &handles)
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;

    // Project each matched skill into SkillInfo, preserving BM25 order.
    let mut infos = Vec::with_capacity(matched_labels.len());
    for label in &matched_labels {
        let handle = BlockHandle::new(label);
        let sdoc = store
            .get_block(agent_id, label)
            .map_err(|e| SkillHandlerError::Store(e.to_string()))?
            .ok_or_else(|| SkillHandlerError::BlockNotFound {
                agent: agent_id.to_string(),
                block: label.clone(),
            })?;

        let skill_meta = project_skill_metadata(sdoc.inner(), label)?;
        let last_used = usage_map.get(&handle).and_then(|s| s.last_used);

        infos.push(SkillInfo {
            handle: handle.clone(),
            name: skill_meta.name,
            description: skill_meta.description,
            trust_tier: skill_meta.trust_tier,
            keywords: skill_meta.keywords,
            last_used,
        });
    }

    Ok(infos)
}

// endregion: handlers

// region: tests

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pattern_core::traits::MemoryStore;
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{
        BlockSchema, MemoryBlockType, SkillMetadata, SkillTrustTier,
    };

    use pattern_memory::fs::markdown_skill::SkillFile;
    use pattern_memory::fs::markdown_skill::write_skill_to_loro_doc;

    use super::*;

    // ---- Test helpers -------------------------------------------------------

    fn skill_schema() -> BlockSchema {
        BlockSchema::Skill {
            expected_keys: vec![],
        }
    }

    fn text_schema() -> BlockSchema {
        BlockSchema::Text { viewport: None }
    }

    fn make_skill_metadata(name: &str) -> SkillMetadata {
        SkillMetadata {
            name: name.to_string(),
            trust_tier: SkillTrustTier::ProjectLocal,
            description: Some(format!("{name} description")),
            keywords: vec![name.to_string()],
            hooks: serde_json::Value::Null,
        }
    }

    /// Seed a Skill block into `store` for `agent_id` at `label`, with `metadata`
    /// and `body`. The LoroDoc is wired via `write_skill_to_loro_doc` so that
    /// `project_metadata_from_loro` returns valid data.
    fn seed_skill(
        store: &Arc<crate::testing::in_memory_store::InMemoryMemoryStore>,
        agent_id: &str,
        label: &str,
        metadata: SkillMetadata,
        body: &str,
    ) {
        let doc = store
            .create_block(
                agent_id,
                BlockCreate::new(label, MemoryBlockType::Working, skill_schema()),
            )
            .expect("create Skill block");

        let skill_file = SkillFile {
            metadata,
            extras: loro::LoroValue::Map(Default::default()),
            body: body.to_string(),
        };
        write_skill_to_loro_doc(&skill_file, doc.inner())
            .expect("write_skill_to_loro_doc failed in test seed");
        doc.inner().commit();
    }

    /// Open a fresh in-memory SQLite DB with all migrations applied.
    fn open_test_db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        pattern_db::migrations::run_memory_migrations(&mut conn).unwrap();
        conn
    }

    // ---- list_enumerates_skill_blocks ---------------------------------------

    #[test]
    fn list_enumerates_skill_blocks() {
        // Seed 3 Skill blocks + 2 Text blocks. handle_list must return exactly 3.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let conn = open_test_db();
        let agent = "agent-test";

        seed_skill(
            &store,
            agent,
            "skill-a",
            make_skill_metadata("skill-a"),
            "Body A.",
        );
        seed_skill(
            &store,
            agent,
            "skill-b",
            make_skill_metadata("skill-b"),
            "Body B.",
        );
        seed_skill(
            &store,
            agent,
            "skill-c",
            make_skill_metadata("skill-c"),
            "Body C.",
        );

        // Seed two Text blocks (should not appear in list results).
        store
            .create_block(
                agent,
                BlockCreate::new("note-1", MemoryBlockType::Working, text_schema()),
            )
            .unwrap();
        store
            .create_block(
                agent,
                BlockCreate::new("note-2", MemoryBlockType::Working, text_schema()),
            )
            .unwrap();

        let infos = handle_list(&*store, &conn, agent).expect("handle_list should succeed");
        assert_eq!(
            infos.len(),
            3,
            "expected 3 SkillInfo entries, got {}: {infos:?}",
            infos.len()
        );

        // Each SkillInfo must have a valid name.
        let names: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"skill-a"), "skill-a missing");
        assert!(names.contains(&"skill-b"), "skill-b missing");
        assert!(names.contains(&"skill-c"), "skill-c missing");
    }

    // ---- list_populates_last_used_from_sqlite --------------------------------

    #[test]
    fn list_populates_last_used_from_sqlite() {
        // Seed 2 Skill blocks. Call record_usage on one.
        // handle_list must return Some(timestamp) for the loaded skill, None for the other.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let agent = "agent-test";

        seed_skill(
            &store,
            agent,
            "skill-loaded",
            make_skill_metadata("skill-loaded"),
            "body.",
        );
        seed_skill(
            &store,
            agent,
            "skill-fresh",
            make_skill_metadata("skill-fresh"),
            "body.",
        );

        let block = BlockHandle::new("skill-loaded");
        let agent_id = pattern_core::types::ids::AgentId::new(agent);
        let ts = jiff::Timestamp::from_second(1_700_000_000).unwrap();
        {
            let tx = conn.transaction().unwrap();
            pattern_db::queries::skill_usage::record_usage(&tx, &block, &agent_id, ts).unwrap();
            tx.commit().unwrap();
        }

        let infos = handle_list(&*store, &conn, agent).expect("handle_list ok");
        assert_eq!(infos.len(), 2);

        let loaded = infos.iter().find(|i| i.name == "skill-loaded").unwrap();
        let fresh = infos.iter().find(|i| i.name == "skill-fresh").unwrap();

        assert!(
            loaded.last_used.is_some(),
            "loaded skill must have last_used; got None"
        );
        assert!(
            fresh.last_used.is_none(),
            "fresh skill must have last_used=None; got {:?}",
            fresh.last_used
        );
    }

    // ---- get_metadata_returns_typed_frontmatter -----------------------------

    #[test]
    fn get_metadata_returns_typed_frontmatter() {
        // Seed a skill with nested hooks JSON. get_metadata must return the same JSON.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let agent = "agent-test";

        let hooks_value = serde_json::json!({
            "on_turn_start": [{"inject_context": "Remember the checklist."}],
            "on_tool_use": [{"log": "tool used"}]
        });
        let metadata = SkillMetadata {
            name: "hooked-skill".to_string(),
            trust_tier: SkillTrustTier::FirstParty,
            description: Some("A skill with hooks".to_string()),
            keywords: vec!["hook".to_string(), "injection".to_string()],
            hooks: hooks_value.clone(),
        };
        seed_skill(&store, agent, "hooked-skill", metadata, "The skill body.\n");

        let result = handle_get_metadata(&*store, agent, "hooked-skill")
            .expect("get_metadata should succeed");

        let returned = result.expect("expected Some(SkillMetadata), got None");
        assert_eq!(returned.name, "hooked-skill");
        assert_eq!(returned.trust_tier, SkillTrustTier::FirstParty);
        assert_eq!(returned.description.as_deref(), Some("A skill with hooks"));
        assert_eq!(returned.keywords, vec!["hook", "injection"]);
        assert_eq!(
            returned.hooks, hooks_value,
            "hooks field must round-trip through LoroDoc"
        );
    }

    // ---- get_metadata_on_text_block_returns_none ----------------------------

    #[test]
    fn get_metadata_on_text_block_returns_none() {
        // A Text-schema block returns None from get_metadata (not an error).
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let agent = "agent-test";

        store
            .create_block(
                agent,
                BlockCreate::new("my-note", MemoryBlockType::Working, text_schema()),
            )
            .unwrap();

        let result = handle_get_metadata(&*store, agent, "my-note")
            .expect("get_metadata on text block should not error");

        assert!(
            result.is_none(),
            "expected None for non-Skill block; got Some({result:?})"
        );
    }

    // ---- get_usage_stats_default_for_new_skill ------------------------------

    #[test]
    fn get_usage_stats_default_for_new_skill() {
        // A skill that has never been loaded returns SkillUsageStats::default().
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let conn = open_test_db();
        let agent = "agent-test";

        seed_skill(
            &store,
            agent,
            "brand-new",
            make_skill_metadata("brand-new"),
            "body.",
        );

        let stats = handle_get_usage_stats(&*store, &conn, agent, "brand-new")
            .expect("get_usage_stats should succeed for new skill");

        assert_eq!(
            stats,
            SkillUsageStats::default(),
            "new skill must return default stats; got {stats:?}"
        );
        assert_eq!(stats.use_count, 0);
        assert!(stats.last_used.is_none());
    }

    // ---- get_usage_stats_after_three_loads ----------------------------------

    #[test]
    fn get_usage_stats_after_three_loads() {
        // Call record_usage 3 times; handler must return use_count == 3.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let agent = "agent-test";

        seed_skill(
            &store,
            agent,
            "counted-skill",
            make_skill_metadata("counted-skill"),
            "body.",
        );

        let block = BlockHandle::new("counted-skill");
        let agent_id = pattern_core::types::ids::AgentId::new(agent);
        let t1 = jiff::Timestamp::from_second(1_700_000_001).unwrap();
        let t2 = jiff::Timestamp::from_second(1_700_000_002).unwrap();
        let t3 = jiff::Timestamp::from_second(1_700_000_003).unwrap();

        for ts in [t1, t2, t3] {
            let tx = conn.transaction().unwrap();
            pattern_db::queries::skill_usage::record_usage(&tx, &block, &agent_id, ts).unwrap();
            tx.commit().unwrap();
        }

        let stats = handle_get_usage_stats(&*store, &conn, agent, "counted-skill")
            .expect("get_usage_stats should succeed after loads");

        assert_eq!(
            stats.use_count, 3,
            "use_count must be 3 after three loads; got {stats:?}"
        );
        assert_eq!(
            stats.last_used.as_ref().map(|t| t.to_string()),
            Some(t3.to_string()),
            "last_used must be t3"
        );
    }
}

// endregion: tests
