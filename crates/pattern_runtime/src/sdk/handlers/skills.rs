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
    BlockFilter, BlockMetadata, BlockSchema, MemorySearchScope, Scope, SearchContentType,
    SearchMode, SearchOptions, SkillError, SkillInfo, SkillMetadata, SkillUsageStats,
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
            constructors: std::borrow::Cow::Borrowed(&[
                "List          :: Skills Text",
                "GetMetadata   :: BlockHandle -> Skills Text",
                "Load          :: BlockHandle -> Skills Text",
                "Search        :: Text -> Skills Text",
                "GetUsageStats :: BlockHandle -> Skills Text",
            ]),
            type_defs: std::borrow::Cow::Borrowed(&[
                "type BlockHandle = Text",
                "type SkillInfo = Text       -- JSON: {handle:BlockHandle, name:Text, description?:Text, trust_tier:Text, keywords:[Text], last_used?:Text}",
                "type SkillMetadata = Text   -- JSON: {name:Text, description?:Text, version?:Text, trust_tier:Text, keywords:[Text], hooks:Value}",
                "type SkillUsageStats = Text -- JSON: {handle:BlockHandle, use_count:Int, last_used?:Text, last_used_by?:Text}",
            ]),
            helpers: std::borrow::Cow::Borrowed(&[
                "listSkills :: Member Skills effs => Eff effs Text\nlistSkills = send List",
                "getSkillMetadata :: Member Skills effs => BlockHandle -> Eff effs Text\ngetSkillMetadata h = send (GetMetadata h)",
                "loadSkill :: Member Skills effs => BlockHandle -> Eff effs Text\nloadSkill h = send (Load h)",
                "searchSkills :: Member Skills effs => Text -> Eff effs Text\nsearchSkills q = send (Search q)",
                "getSkillUsageStats :: Member Skills effs => BlockHandle -> Eff effs Text\ngetSkillUsageStats h = send (GetUsageStats h)",
            ]),
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
        let scope = cx.user().default_scope().clone();
        let store = cx.user().memory_store();
        let state = cx.user().cancel_state();
        let _guard = HandlerGuard::enter(&state.gate);

        // Effect-class runtime guard. All Skills constructors are Observe/Enforce.
        let constructor_name = match &req {
            SkillsReq::List => "List",
            SkillsReq::GetMetadata(_) => "GetMetadata",
            SkillsReq::Load(_) => "Load",
            SkillsReq::Search(_) => "Search",
            SkillsReq::GetUsageStats(_) => "GetUsageStats",
        };
        crate::sdk::effect_classes::check_effect_class(
            cx.user().capabilities(),
            "Skills",
            constructor_name,
        )?;

        match req {
            SkillsReq::List => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Skills::List: db connection: {e}"))
                })?;
                let infos = handle_list(&*store, &conn, &scope)?;
                let items: Vec<String> = infos
                    .iter()
                    .map(|info| serde_json::to_string(info).unwrap_or_default())
                    .collect();
                cx.respond(serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string()))
            }
            SkillsReq::GetMetadata(handle) => {
                let result = handle_get_metadata(&*store, &scope, &handle)?;
                cx.respond(
                    result
                        .map(|m| serde_json::to_string(&m).unwrap_or_default())
                        .unwrap_or_else(|| "null".to_string()),
                )
            }
            SkillsReq::Load(handle) => {
                let mut conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Skills::Load: db connection: {e}"))
                })?;
                let rendered = handle_load(&*store, &mut conn, &scope, &agent_id, &handle)?;
                cx.respond(rendered)
            }
            SkillsReq::Search(query) => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Skills::Search: db connection: {e}"))
                })?;
                let infos = handle_search(&*store, &conn, &scope, &query)?;
                let items: Vec<String> = infos
                    .iter()
                    .map(|info| serde_json::to_string(info).unwrap_or_default())
                    .collect();
                cx.respond(serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string()))
            }
            SkillsReq::GetUsageStats(handle) => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!(
                        "Pattern.Skills::GetUsageStats: db connection: {e}"
                    ))
                })?;
                let stats = handle_get_usage_stats(&*store, &conn, &scope, &handle)?;
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
pub enum SkillHandlerError {
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

/// List all Skill-schema blocks visible to `scope`.
///
/// Enumerates blocks via `store.list_blocks`, filters to `BlockSchema::Skill`,
/// projects each block's LoroDoc into `SkillMetadata`, batch-fetches usage
/// stats from sqlite, and assembles `SkillInfo` records. The underlying
/// `MemoryScope` handles `IsolatePolicy` routing upstream.
pub fn handle_list(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    scope: &Scope,
) -> Result<Vec<SkillInfo>, SkillHandlerError> {
    // Use an unscoped filter so that MemoryScope (if present) can apply its
    // IsolatePolicy routing. A scoped filter would set filter.agent_id and bypass
    // MemoryScope's routing at line 219 of scope/wrapper.rs.
    let all_meta = store
        .list_blocks(BlockFilter::default())
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

        // Reconstruct the block's own scope from its encoded agent_id (e.g.
        // "local:project-a" or "global:persona-a"). This is necessary because
        // MemoryScope returns project blocks from list_blocks(default()) under
        // Full isolation, but get_block(caller_scope, …) returns None for a
        // Global (persona) scope under Full isolation. Using the block's own
        // scope bypasses that routing asymmetry and fetches the doc directly.
        let block_scope = Scope::from_db_key(&meta.agent_id).unwrap_or_else(|| scope.clone());
        let sdoc = store
            .get_block(&block_scope, &meta.label)
            .map_err(|e| SkillHandlerError::Store(e.to_string()))?
            .ok_or_else(|| SkillHandlerError::BlockNotFound {
                agent: block_scope.id().to_string(),
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
pub fn handle_get_metadata(
    store: &dyn MemoryStore,
    scope: &Scope,
    handle: &str,
) -> Result<Option<SkillMetadata>, SkillHandlerError> {
    let sdoc = store
        .get_block(scope, handle)
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?
        .ok_or_else(|| SkillHandlerError::BlockNotFound {
            agent: scope.id().to_string(),
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
pub fn handle_get_usage_stats(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    scope: &Scope,
    handle: &str,
) -> Result<SkillUsageStats, SkillHandlerError> {
    // Verify the block exists and is visible to this scope.
    let sdoc = store
        .get_block(scope, handle)
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?
        .ok_or_else(|| SkillHandlerError::BlockNotFound {
            agent: scope.id().to_string(),
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
pub fn handle_search(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    scope: &Scope,
    query: &str,
) -> Result<Vec<SkillInfo>, SkillHandlerError> {
    let opts = SearchOptions {
        mode: SearchMode::Fts,
        content_types: vec![SearchContentType::Blocks],
        limit: 50,
    };

    let search_results = store
        .search(
            query,
            opts,
            MemorySearchScope::Scope(Scope::Global(scope.id().into())),
        )
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?;

    if search_results.is_empty() {
        return Ok(Vec::new());
    }

    // Collect the result IDs (memory_blocks.id UUIDs) so we can correlate.
    // Results are already ordered by BM25 score (descending) from the store.
    let result_ids: Vec<&str> = search_results.iter().map(|r| r.id.as_str()).collect();

    // Enumerate all Skill blocks visible to this scope to build a label↔id mapping.
    // Use an unscoped filter so that MemoryScope (if present) can apply its
    // IsolatePolicy routing — same rationale as handle_list.
    let all_meta = store
        .list_blocks(BlockFilter::default())
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?;

    // Build a map from memory_id → BlockMetadata for Skill blocks only.
    // BlockMetadata.id is the memory_blocks DB UUID.
    let skill_by_id: std::collections::HashMap<&str, &BlockMetadata> = all_meta
        .iter()
        .filter(|m| matches!(m.schema, BlockSchema::Skill { .. }))
        .map(|m| (m.id.as_str(), m))
        .collect();

    // Walk search results in BM25 order; keep only Skill hits.
    // Carry the block's own encoded agent_id so we can reconstruct the scope
    // for get_block — necessary for project blocks under Full isolation (see
    // handle_list for the same rationale).
    let mut matched: Vec<(String, Scope)> = Vec::new();
    for id in &result_ids {
        if let Some(meta) = skill_by_id.get(*id) {
            let block_scope =
                Scope::from_db_key(&meta.agent_id).unwrap_or_else(|| scope.clone());
            matched.push((meta.label.clone(), block_scope));
        }
    }

    if matched.is_empty() {
        return Ok(Vec::new());
    }

    // Batch-fetch usage stats for matched skill labels.
    let handles: Vec<BlockHandle> = matched.iter().map(|(l, _)| BlockHandle::new(l)).collect();
    let usage_map = pattern_db::queries::skill_usage::get_usage_stats_batch(conn, &handles)
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;

    // Project each matched skill into SkillInfo, preserving BM25 order.
    let mut infos = Vec::with_capacity(matched.len());
    for (label, block_scope) in &matched {
        let handle = BlockHandle::new(label);
        let sdoc = store
            .get_block(block_scope, label)
            .map_err(|e| SkillHandlerError::Store(e.to_string()))?
            .ok_or_else(|| SkillHandlerError::BlockNotFound {
                agent: block_scope.id().to_string(),
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

/// Load a Skill block: returns the rendered `[skill:loaded]` text (markers +
/// frontmatter line + full body) directly to the agent as the tool result,
/// and records a usage stat row in sqlite. Does NOT mutate the LoroDoc and
/// does NOT touch the canonical `.md` file (AC9.3 / AC9.6 — content-hash
/// stable across loads).
///
/// The returned text becomes the `tool_result_msg` content for the wire turn
/// that called `Skills.Load`. Because tool_result messages naturally flow
/// through `TurnHistory::active_messages()`, the skill content persists
/// across subsequent turns in segment 2 without any special pseudo-message
/// pipe (AC9.2).
///
/// Returns `BlockNotFound` if the handle has no block (AC8.5), or
/// `Skill(SkillError::NotASkill)` if the block exists but is not a Skill
/// (AC8.6). Other errors propagate as Sqlite/Store/MalformedLoro.
pub fn handle_load(
    store: &dyn MemoryStore,
    conn: &mut rusqlite::Connection,
    scope: &Scope,
    agent_id: &str,
    handle: &str,
) -> Result<String, SkillHandlerError> {
    // 1. Fetch block.
    let sdoc = store
        .get_block(scope, handle)
        .map_err(|e| SkillHandlerError::Store(e.to_string()))?
        .ok_or_else(|| SkillHandlerError::BlockNotFound {
            agent: scope.id().to_string(),
            block: handle.to_string(),
        })?;

    // 2. Schema check.
    if !matches!(sdoc.schema(), BlockSchema::Skill { .. }) {
        return Err(SkillHandlerError::Skill(SkillError::NotASkill(
            BlockHandle::new(handle),
        )));
    }

    // 3. Project metadata + 4. read body.
    let metadata = project_skill_metadata(sdoc.inner(), handle)?;
    let body = sdoc.inner().get_text("body").to_string();

    // 5. Render markers + body. No <system-reminder> wrap — tool_result has
    //    its own role; the markers themselves are the framing the agent
    //    pattern-matches on.
    let rendered = pattern_provider::compose::render::render_skill_loaded_text(
        &metadata.name,
        metadata.trust_tier,
        &body,
    );

    // 6. Sqlite stat write inside a transaction. record_usage uses an UPSERT
    //    that increments use_count atomically; wrapping in a transaction is
    //    belt-and-braces but matches the convention from sibling handlers.
    let agent_smol: pattern_core::types::ids::AgentId = agent_id.into();
    let bh = BlockHandle::new(handle);
    let now = jiff::Timestamp::now();
    let tx = conn
        .transaction()
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;
    pattern_db::queries::skill_usage::record_usage(&tx, &bh, &agent_smol, now)
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;
    tx.commit()
        .map_err(|e| SkillHandlerError::Sqlite(e.to_string()))?;

    Ok(rendered)
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
            source_plugin_id: None,
        }
    }

    /// Seed a Skill block into `store` for `scope` at `label`, with `metadata`
    /// and `body`. The LoroDoc is wired via `write_skill_to_loro_doc` so that
    /// `project_metadata_from_loro` returns valid data.
    fn seed_skill(
        store: &Arc<crate::testing::in_memory_store::InMemoryMemoryStore>,
        scope: &Scope,
        label: &str,
        metadata: SkillMetadata,
        body: &str,
    ) {
        let doc = store
            .create_block(
                scope,
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
        let scope = Scope::Global("agent-test".into());

        seed_skill(
            &store,
            &scope,
            "skill-a",
            make_skill_metadata("skill-a"),
            "Body A.",
        );
        seed_skill(
            &store,
            &scope,
            "skill-b",
            make_skill_metadata("skill-b"),
            "Body B.",
        );
        seed_skill(
            &store,
            &scope,
            "skill-c",
            make_skill_metadata("skill-c"),
            "Body C.",
        );

        // Seed two Text blocks (should not appear in list results).
        store
            .create_block(
                &scope,
                BlockCreate::new("note-1", MemoryBlockType::Working, text_schema()),
            )
            .unwrap();
        store
            .create_block(
                &scope,
                BlockCreate::new("note-2", MemoryBlockType::Working, text_schema()),
            )
            .unwrap();

        let infos = handle_list(&*store, &conn, &scope).expect("handle_list should succeed");
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
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "skill-loaded",
            make_skill_metadata("skill-loaded"),
            "body.",
        );
        seed_skill(
            &store,
            &scope,
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

        let infos = handle_list(&*store, &conn, &scope).expect("handle_list ok");
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
        let scope = Scope::Global("agent-test".into());

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
            source_plugin_id: None,
        };
        seed_skill(&store, &scope, "hooked-skill", metadata, "The skill body.\n");

        let result = handle_get_metadata(&*store, &scope, "hooked-skill")
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
        let scope = Scope::Global("agent-test".into());

        store
            .create_block(
                &scope,
                BlockCreate::new("my-note", MemoryBlockType::Working, text_schema()),
            )
            .unwrap();

        let result = handle_get_metadata(&*store, &scope, "my-note")
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
        let scope = Scope::Global("agent-test".into());

        seed_skill(
            &store,
            &scope,
            "brand-new",
            make_skill_metadata("brand-new"),
            "body.",
        );

        let stats = handle_get_usage_stats(&*store, &conn, &scope, "brand-new")
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
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
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

        let stats = handle_get_usage_stats(&*store, &conn, &scope, "counted-skill")
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

    // ---- load handler tests -------------------------------------------------

    #[test]
    fn load_missing_block_returns_block_not_found() {
        // AC8.5: handle that doesn't exist returns BlockNotFound.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        let err = handle_load(&*store, &mut conn, &scope, agent, "no-such-skill")
            .expect_err("must error for missing block");
        assert!(
            matches!(err, SkillHandlerError::BlockNotFound { .. }),
            "expected BlockNotFound, got {err:?}"
        );
    }

    #[test]
    fn load_text_block_returns_not_a_skill() {
        // AC8.6: handle on a Text block returns SkillError::NotASkill.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        store
            .create_block(
                &scope,
                BlockCreate::new("notes", MemoryBlockType::Working, text_schema()),
            )
            .expect("create text block");

        let err = handle_load(&*store, &mut conn, &scope, agent, "notes")
            .expect_err("must error for non-skill block");
        match err {
            SkillHandlerError::Skill(SkillError::NotASkill(h)) => {
                assert_eq!(h.as_str(), "notes");
            }
            other => panic!("expected NotASkill, got {other:?}"),
        }
    }

    #[test]
    fn load_returns_rendered_text_with_markers_and_full_body() {
        // AC9.1: a successful load returns the rendered [skill:loaded] text
        // (markers + frontmatter line + full body) as the tool_result content.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "fix-auth",
            make_skill_metadata("fix-auth"),
            "## Overview\n\nHandles OAuth2.\n",
        );

        let rendered =
            handle_load(&*store, &mut conn, &scope, agent, "fix-auth").expect("load must succeed");

        assert!(
            rendered.contains("[skill:loaded]"),
            "rendered text must contain [skill:loaded] marker; got: {rendered}"
        );
        assert!(
            rendered.contains("[skill:loaded:end]"),
            "rendered text must contain [skill:loaded:end] marker; got: {rendered}"
        );
        assert!(
            rendered.contains("name=\"fix-auth\""),
            "rendered text must contain the skill name; got: {rendered}"
        );
        assert!(
            rendered.contains("trust_tier=\"project-local\""),
            "rendered text must contain kebab-case trust_tier; got: {rendered}"
        );
        // Full body present, not truncated.
        assert!(
            rendered.contains("## Overview\n\nHandles OAuth2.\n"),
            "rendered text must contain the full body; got: {rendered}"
        );
        // No <system-reminder> wrap — that's user-role framing; tool_result
        // has its own role-based framing.
        assert!(
            !rendered.contains("<system-reminder>"),
            "rendered text must NOT be wrapped in <system-reminder>; got: {rendered}"
        );
    }

    #[test]
    fn load_updates_use_count_to_five() {
        // AC9.3: 5 loads → use_count == 5.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "skill-x",
            make_skill_metadata("skill-x"),
            "Body.",
        );

        for _ in 0..5 {
            handle_load(&*store, &mut conn, &scope, agent, "skill-x").expect("load must succeed");
        }

        let bh = BlockHandle::new("skill-x");
        let stats = pattern_db::queries::skill_usage::get_usage_stats(&conn, &bh)
            .expect("get_usage_stats must succeed");
        assert_eq!(stats.use_count, 5, "use_count must be 5 after 5 loads");
        assert!(
            stats.last_used.is_some(),
            "last_used must be populated after first load"
        );
    }

    #[test]
    fn load_does_not_modify_lorodoc_body() {
        // AC9.3: 100 loads must not alter the canonical block content. We hash
        // the body LoroText before and after the loop and assert equal. The
        // canonical-file-on-disk invariant is covered by skills_load_mode_a.rs.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "skill-stable",
            make_skill_metadata("skill-stable"),
            "stable body content\n",
        );

        let body_before = {
            let sdoc = store
                .get_block(&scope, "skill-stable")
                .unwrap()
                .expect("block exists");
            sdoc.inner().get_text("body").to_string()
        };
        let hash_before = blake3::hash(body_before.as_bytes());

        for _ in 0..100 {
            handle_load(&*store, &mut conn, &scope, agent, "skill-stable")
                .expect("load must succeed");
        }

        let body_after = {
            let sdoc = store
                .get_block(&scope, "skill-stable")
                .unwrap()
                .expect("block exists");
            sdoc.inner().get_text("body").to_string()
        };
        let hash_after = blake3::hash(body_after.as_bytes());

        assert_eq!(
            hash_before, hash_after,
            "LoroDoc body must be byte-identical after 100 loads"
        );
        assert_eq!(body_before, body_after, "body strings must match");
    }

    #[test]
    fn load_two_skills_returns_distinct_text_each_call() {
        // AC9.4: load A then B; each call returns its own rendered text.
        // (Buffer-order semantics from the previous design no longer apply —
        // each call's output goes to its own tool_result_msg in the wire turn.)
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "skill-alpha",
            make_skill_metadata("skill-alpha"),
            "Alpha body.",
        );
        seed_skill(
            &store,
            &scope,
            "skill-beta",
            make_skill_metadata("skill-beta"),
            "Beta body.",
        );

        let alpha_text = handle_load(&*store, &mut conn, &scope, agent, "skill-alpha").unwrap();
        let beta_text = handle_load(&*store, &mut conn, &scope, agent, "skill-beta").unwrap();

        assert!(alpha_text.contains("name=\"skill-alpha\""));
        assert!(alpha_text.contains("Alpha body."));
        assert!(!alpha_text.contains("skill-beta"));
        assert!(beta_text.contains("name=\"skill-beta\""));
        assert!(beta_text.contains("Beta body."));
        assert!(!beta_text.contains("skill-alpha"));
    }

    #[test]
    fn load_same_skill_twice_increments_count_and_returns_text_each_time() {
        // AC9.5: loading the same skill twice succeeds twice (no dedup); each
        // call returns its own text. use_count increments by 1 per call.
        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "skill-twice",
            make_skill_metadata("skill-twice"),
            "Body.",
        );

        let first = handle_load(&*store, &mut conn, &scope, agent, "skill-twice").unwrap();
        let second = handle_load(&*store, &mut conn, &scope, agent, "skill-twice").unwrap();

        assert!(first.contains("[skill:loaded]"));
        assert!(second.contains("[skill:loaded]"));
        // Same input → same rendered output (deterministic).
        assert_eq!(first, second);

        let bh = BlockHandle::new("skill-twice");
        let stats = pattern_db::queries::skill_usage::get_usage_stats(&conn, &bh).unwrap();
        assert_eq!(stats.use_count, 2);
    }

    #[test]
    fn load_persists_in_history_via_tool_result() {
        // AC9.2 structural: the rendered text returned by handle_load is
        // intended to be the content of a tool_result_msg in TurnOutput.
        // Once that message is recorded in TurnHistory, it shows up in
        // active_messages() across subsequent turns — proving the skill
        // body survives a non-loading intervening turn.
        use crate::memory::TurnHistory;
        use genai::chat::ChatMessage;
        use jiff::Timestamp;
        use pattern_core::types::ids::{AgentId, MessageId, new_id, new_snowflake_id};
        use pattern_core::types::message::Message;
        use pattern_core::types::turn::{StopReason, TurnInput, TurnOutput};
        use smol_str::SmolStr;

        let store = Arc::new(crate::testing::in_memory_store::InMemoryMemoryStore::new());
        let mut conn = open_test_db();
        let scope = Scope::Global("agent-test".into());
        let agent = "agent-test";

        seed_skill(
            &store,
            &scope,
            "skill-flow",
            make_skill_metadata("skill-flow"),
            "Flow body.",
        );
        let rendered =
            handle_load(&*store, &mut conn, &scope, agent, "skill-flow").expect("load");

        // Synthesize a Message wrapping a tool ChatMessage carrying the
        // rendered text. (Production code synthesizes this in agent_loop's
        // tool_result message; we model the same shape here.)
        let make_msg = |chat: ChatMessage, batch: SmolStr| -> Message {
            Message {
                chat_message: chat,
                id: MessageId::from(new_id()),
                position: new_snowflake_id(),
                owner_id: AgentId::from(agent),
                created_at: Timestamp::now(),
                batch,
                response_meta: None,
                block_refs: vec![],
                attachments: vec![],
            }
        };

        let batch_a = new_snowflake_id();
        let tool_result_msg = make_msg(
            ChatMessage::new(genai::chat::ChatRole::Tool, rendered.clone()),
            batch_a.clone(),
        );

        let mut hist = TurnHistory::empty();

        // Turn 1: a turn that loaded the skill (input is irrelevant for this
        // structural assertion; output carries the tool_result_msg).
        hist.record(
            new_snowflake_id(),
            TurnInput::continuation(batch_a.clone(), AgentId::from(agent)),
            TurnOutput {
                messages: vec![tool_result_msg],
                block_writes: vec![],
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: None,
                cache_metrics: Default::default(),
                completed_at: Timestamp::now(),
            },
        );

        // Turn 2: a non-loading turn (no skill ops).
        let batch_b = new_snowflake_id();
        let unrelated = make_msg(ChatMessage::user("anything"), batch_b.clone());
        hist.record(
            new_snowflake_id(),
            TurnInput::continuation(batch_b, AgentId::from(agent)),
            TurnOutput {
                messages: vec![unrelated],
                block_writes: vec![],
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
                usage: None,
                cache_metrics: Default::default(),
                completed_at: Timestamp::now(),
            },
        );

        // Active history must still surface the skill marker — proving the
        // skill body persists across the intervening non-loading turn.
        let active_text: String = hist
            .active_messages()
            .map(|m| m.chat_message.content.joined_texts().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            active_text.contains("[skill:loaded]"),
            "skill marker must persist across non-loading turn; got: {active_text}"
        );
        assert!(
            active_text.contains("name=\"skill-flow\""),
            "skill name must persist; got: {active_text}"
        );
        assert!(
            active_text.contains("Flow body."),
            "skill body must persist; got: {active_text}"
        );
    }
}

// region: search tests (real FTS5 — MemoryCache + ConstellationDb)
//
// `InMemoryMemoryStore.search()` always returns an empty result set (no FTS
// backend). The following tests use `MemoryCache` + `ConstellationDb::open_in_memory()`
// — the same pattern used by `crates/pattern_memory/tests/skill_fts5.rs` — to
// exercise the full FTS5 code path through `handle_search`.

#[cfg(test)]
mod search_tests {
    use std::sync::Arc;

    use pattern_db::ConstellationDb;
    use pattern_memory::MemoryCache;
    use pattern_memory::fs::markdown_skill::{SkillFile, write_skill_to_loro_doc};

    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{
        BlockSchema, MemoryBlockType, SkillMetadata, SkillTrustTier,
    };

    use super::*;

    const AGENT: &str = "search_agent";

    /// Create a fresh in-memory `ConstellationDb` and matching `MemoryCache`.
    fn setup() -> (Arc<ConstellationDb>, MemoryCache) {
        let dbs = Arc::new(ConstellationDb::open_in_memory().unwrap());
        // Agent row is required for FK constraints on memory_blocks.
        let agent = pattern_db::models::Agent {
            id: AGENT.to_string(),
            name: "Search Test Agent".to_string(),
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
            .expect("create agent for search tests");
        let cache = MemoryCache::new(dbs.clone());
        (dbs, cache)
    }

    /// Seed a Skill block into `cache`, wire the LoroDoc, then persist so
    /// the FTS5 index is updated.
    fn seed_and_persist(cache: &MemoryCache, label: &str, metadata: SkillMetadata, body: &str) {
        let scope = Scope::Global(AGENT.into());
        cache
            .create_block(
                &scope,
                BlockCreate::new(
                    label,
                    MemoryBlockType::Working,
                    BlockSchema::Skill {
                        expected_keys: vec![],
                    },
                ),
            )
            .unwrap();

        let doc = cache
            .get_block(&scope, label)
            .unwrap()
            .expect("block must exist after create");

        let skill_file = SkillFile {
            metadata,
            extras: loro::LoroValue::Map(Default::default()),
            body: body.to_string(),
        };
        write_skill_to_loro_doc(&skill_file, doc.inner()).unwrap();
        doc.inner().commit();

        cache.mark_dirty(&scope.to_db_key(), label);
        cache.persist_block(&scope, label).unwrap();
    }

    // ---- search_matches_skill_name ---------------------------------------

    #[test]
    fn search_matches_skill_name() {
        // Seed a skill whose name contains "authentication". Query must return it
        // without surfacing the decoy.
        let (dbs, cache) = setup();
        let conn = dbs.get().unwrap();

        seed_and_persist(
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
            "Generic skill body.\n",
        );
        seed_and_persist(
            &cache,
            "decoy-skill",
            SkillMetadata {
                name: "unrelated-work".to_string(),
                trust_tier: SkillTrustTier::AdHoc,
                description: None,
                keywords: vec![],
                hooks: serde_json::Value::Null,
                source_plugin_id: None,
            },
            "Nothing here.\n",
        );

        let scope = Scope::Global(AGENT.into());
        let results =
            handle_search(&cache, &conn, &scope, "authentication").expect("search should succeed");

        assert_eq!(
            results.len(),
            1,
            "expected exactly 1 result for 'authentication'; got {}: {results:?}",
            results.len()
        );
        assert_eq!(
            results[0].name, "fix-authentication",
            "wrong skill returned"
        );
    }

    // ---- search_matches_skill_description --------------------------------

    #[test]
    fn search_matches_skill_description() {
        // Skill with description mentioning "token-refresh"; query on "token".
        let (dbs, cache) = setup();
        let conn = dbs.get().unwrap();

        seed_and_persist(
            &cache,
            "desc-skill",
            SkillMetadata {
                name: "session-manager".to_string(),
                trust_tier: SkillTrustTier::ProjectLocal,
                description: Some("Handles token-refresh for expired sessions".to_string()),
                keywords: vec![],
                hooks: serde_json::Value::Null,
                source_plugin_id: None,
            },
            "Generic body.\n",
        );
        seed_and_persist(
            &cache,
            "decoy-skill",
            SkillMetadata {
                name: "file-handler".to_string(),
                trust_tier: SkillTrustTier::AdHoc,
                description: Some("Manages files on disk".to_string()),
                keywords: vec![],
                hooks: serde_json::Value::Null,
                source_plugin_id: None,
            },
            "File body.\n",
        );

        let scope = Scope::Global(AGENT.into());
        let results = handle_search(&cache, &conn, &scope, "token").expect("search should succeed");

        assert_eq!(
            results.len(),
            1,
            "expected 1 result for 'token'; got {}: {results:?}",
            results.len()
        );
        assert_eq!(results[0].name, "session-manager");
    }

    // ---- search_matches_skill_body ---------------------------------------

    #[test]
    fn search_matches_skill_body() {
        // Skill whose body contains "Revokes all active sessions".
        let (dbs, cache) = setup();
        let conn = dbs.get().unwrap();

        seed_and_persist(
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
        seed_and_persist(
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
            "Creates new user sessions.\n",
        );

        let scope = Scope::Global(AGENT.into());
        let results =
            handle_search(&cache, &conn, &scope, "Revokes").expect("search should succeed");

        assert_eq!(
            results.len(),
            1,
            "expected 1 result for 'Revokes'; got {}: {results:?}",
            results.len()
        );
        assert_eq!(results[0].name, "logout-handler");
    }

    // ---- search_relevance_ranked ----------------------------------------

    #[test]
    fn search_relevance_ranked() {
        // Three skills all contain "security". BM25 ordering is snapshotted.
        let (dbs, cache) = setup();
        let conn = dbs.get().unwrap();

        // Skill A: "security" in name, description, keywords, and body.
        seed_and_persist(
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

        // Skill B: "security" in keywords and body only.
        seed_and_persist(
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

        // Skill C: "security" in description and body only.
        seed_and_persist(
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

        let scope = Scope::Global(AGENT.into());
        let results =
            handle_search(&cache, &conn, &scope, "security").expect("search should succeed");

        assert_eq!(
            results.len(),
            3,
            "all three skills should match 'security'; got {}: {results:?}",
            results.len()
        );

        // Snapshot BM25 ordering by name for regression detection.
        let ordered_names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        insta::assert_snapshot!("search_relevance_ranked", ordered_names.join("\n"));
    }
}

// endregion: tests
