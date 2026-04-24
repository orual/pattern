//! Handler for `Pattern.Tasks` — task-graph operations (create, update, link, query).
//!
//! The handler wires eight methods: create_task, update_task, transition_status,
//! link, unlink, list_tasks, query_graph, and add_comment. The list/query
//! surface (Task 9) goes through `pattern_db` directly; mutations (Tasks 7+8)
//! mutate the TaskList block's LoroDoc via `MemoryStore::get_block` and let
//! the subscriber reconcile the SQL index on its own schedule.

use std::collections::HashMap;

use loro::{LoroDoc, LoroValue};
use serde_json::Value as JsonValue;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use pattern_core::AgentAuthor;
use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block::{BlockWrite, BlockWriteKind};
use pattern_core::types::ids::{TaskItemId, new_snowflake_id};
use pattern_core::types::memory_types::{
    BlockFilter, BlockSchema, MemoryError, TaskEdgeRef, TaskStatus,
    task_query::{GraphQuery, GraphSlice, TaskFilter, TaskPatch, TaskSpec, TaskView},
};
use pattern_core::types::origin::Author;
use smol_str::SmolStr;

use crate::memory::MemoryStoreAdapter;
use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::tasks::TasksReq;
use crate::session::SessionContext;

/// Handler for `Pattern.Tasks`.
///
/// Unit-struct (mirrors [`crate::sdk::handlers::MemoryHandler`]). The per-call
/// memory store comes from `cx.user().adapter()`, which respects the active
/// `IsolatePolicy` scope routing.
#[derive(Clone)]
pub struct TasksHandler;

impl std::fmt::Debug for TasksHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksHandler").finish_non_exhaustive()
    }
}

impl DescribeEffect for TasksHandler {
    fn effect_decl() -> EffectDecl {
        EffectDecl {
            type_name: "Tasks",
            description: "Task-graph operations: create, update, transition, link, unlink, list, query, comment",
            constructors: &[
                "Create         :: BlockHandle -> TaskSpec -> Tasks TaskItemId",
                "Update         :: TaskEdgeRef -> TaskPatch -> Tasks ()",
                "Transition     :: TaskEdgeRef -> TaskStatus -> Tasks ()",
                "Link           :: TaskEdgeRef -> TaskEdgeRef -> Tasks ()",
                "Unlink         :: TaskEdgeRef -> TaskEdgeRef -> Tasks ()",
                "List           :: Maybe BlockHandle -> TaskFilter -> Tasks [TaskView]",
                "QueryGraph     :: TaskEdgeRef -> GraphQuery -> Tasks GraphSlice",
                "AddComment     :: TaskEdgeRef -> Text -> Tasks ()",
            ],
            type_defs: &[
                "type BlockHandle = Text",
                "type TaskItemId = Text  -- snowflake, base32-encoded",
                "type TaskEdgeRef = Text  -- \"block-handle#item-id\" (item form) or \"block-handle\" (block form)",
                // JSON payload schemas. Agents build these as JSON Text via
                // Pattern.Aeson (ToJSON) and the handlers decode on the Rust
                // side. A future follow-up (B-full) replaces these with
                // proper typed records flowing through the Core VM.
                "type TaskSpec = Text  -- JSON: {subject:Text, description:Text, status?:TaskStatus-kebab, owner?:AgentId, active_form?:Text, metadata:Value}",
                "type TaskPatch = Text  -- JSON: {subject?:Text, description?:Text, status?:TaskStatus-kebab, owner??:AgentId|null, active_form??:Text|null, metadata?:Value} -- `??` = omit (no change), null (clear), or value (set)",
                "type TaskStatus = Text  -- kebab-case: \"pending\"|\"in-progress\"|\"blocked\"|\"completed\"|\"cancelled\"",
                "type TaskFilter = Text  -- JSON: {status?:[TaskStatus-kebab], owner?:AgentId, has_blockers?:Bool, keyword?:Text, blocks?:[BlockHandle]}",
                "type TaskView = Text  -- JSON: {block_ref:TaskEdgeRef, subject:Text, status:TaskStatus-kebab, owner?:AgentId, blocker_count:Int, blocks_count:Int}",
                "type GraphQuery = Text  -- JSON: {direction:Direction-kebab, depth?:Int, max_nodes?:Int}",
                "type Direction = Text  -- kebab-case: \"forward\"|\"reverse\"|\"both\"",
                "type GraphSlice = Text  -- JSON: {nodes:[TaskEdgeRef], edges:[[TaskEdgeRef,TaskEdgeRef]], truncated:Bool}",
            ],
            helpers: &[
                "create :: Member Tasks effs => BlockHandle -> TaskSpec -> Eff effs TaskItemId\ncreate block spec = send (Create block spec)",
                "update :: Member Tasks effs => TaskEdgeRef -> TaskPatch -> Eff effs ()\nupdate ref patch = send (Update ref patch)",
                "transition :: Member Tasks effs => TaskEdgeRef -> TaskStatus -> Eff effs ()\ntransition ref status = send (Transition ref status)",
                "link :: Member Tasks effs => TaskEdgeRef -> TaskEdgeRef -> Eff effs ()\nlink src tgt = send (Link src tgt)",
                "unlink :: Member Tasks effs => TaskEdgeRef -> TaskEdgeRef -> Eff effs ()\nunlink src tgt = send (Unlink src tgt)",
                "list :: Member Tasks effs => Maybe BlockHandle -> TaskFilter -> Eff effs [TaskView]\nlist block filt = send (List block filt)",
                "queryGraph :: Member Tasks effs => TaskEdgeRef -> GraphQuery -> Eff effs GraphSlice\nqueryGraph root query = send (QueryGraph root query)",
                "addComment :: Member Tasks effs => TaskEdgeRef -> Text -> Eff effs ()\naddComment ref txt = send (AddComment ref txt)",
            ],
        }
    }
}

impl EffectHandler<SessionContext> for TasksHandler {
    type Request = TasksReq;

    fn handle(
        &mut self,
        req: TasksReq,
        cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        let agent_id = cx.user().agent_id().to_string();
        let adapter = cx.user().adapter().clone();
        let store = cx.user().memory_store();

        // Helper closure: record the post-mutation write on the adapter's
        // pending-buffer so TurnOutput.block_writes reflects the change.
        // Called AFTER the mutation landed on the LoroDoc + persist_block.
        let record = |block: &str, kind: BlockWriteKind| -> Result<(), EffectError> {
            record_task_write(&adapter, &agent_id, &*store, block, kind).map_err(EffectError::from)
        };

        match req {
            TasksReq::Create(block, spec_json) => {
                let id = handle_create(&*store, &agent_id, &block, &spec_json)?;
                // `Updated` kind: the block itself was already created
                // upstream; we appended a new task item to its movable list.
                record(&block, BlockWriteKind::Updated)?;
                cx.respond(id.to_string())
            }
            TasksReq::Update(edge_ref, patch_json) => {
                handle_update(&*store, &agent_id, &edge_ref, &patch_json)?;
                if let Ok((block, _)) = parse_item_ref(&edge_ref) {
                    record(&block, BlockWriteKind::Updated)?;
                }
                cx.respond(())
            }
            TasksReq::Transition(edge_ref, status_json) => {
                handle_transition(&*store, &agent_id, &edge_ref, &status_json)?;
                if let Ok((block, _)) = parse_item_ref(&edge_ref) {
                    record(&block, BlockWriteKind::Updated)?;
                }
                cx.respond(())
            }
            TasksReq::AddComment(edge_ref, text) => {
                handle_add_comment(&*store, &agent_id, &edge_ref, &text)?;
                if let Ok((block, _)) = parse_item_ref(&edge_ref) {
                    record(&block, BlockWriteKind::Updated)?;
                }
                cx.respond(())
            }
            TasksReq::Link(source_ref, target_ref) => {
                handle_link(&*store, &agent_id, &source_ref, &target_ref)?;
                if let Ok((block, _)) = parse_item_ref(&source_ref) {
                    record(&block, BlockWriteKind::Updated)?;
                }
                cx.respond(())
            }
            TasksReq::Unlink(source_ref, target_ref) => {
                handle_unlink(&*store, &agent_id, &source_ref, &target_ref)?;
                if let Ok((block, _)) = parse_item_ref(&source_ref) {
                    record(&block, BlockWriteKind::Updated)?;
                }
                cx.respond(())
            }
            TasksReq::List(block_opt, filter_json) => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Tasks::List: db connection: {e}"))
                })?;
                let views = handle_list_tasks(
                    &*store,
                    &conn,
                    &agent_id,
                    block_opt.as_deref(),
                    &filter_json,
                )?;
                // Haskell return type is [TaskView] where TaskView = Text:
                // serialize each TaskView as JSON, pass as a list of strings.
                let view_strs: Vec<String> = views
                    .iter()
                    .map(|v| serde_json::to_string(v).unwrap_or_default())
                    .collect();
                cx.respond(view_strs)
            }
            TasksReq::QueryGraph(root_ref, query_json) => {
                let conn = cx.user().db().get().map_err(|e| {
                    EffectError::Handler(format!("Pattern.Tasks::QueryGraph: db connection: {e}"))
                })?;
                let slice = handle_query_graph(&*store, &conn, &agent_id, &root_ref, &query_json)?;
                // Return type is GraphSlice = Text (JSON-encoded).
                cx.respond(serde_json::to_string(&slice).unwrap_or_default())
            }
        }
    }
}

// region: internal error type

/// Errors raised by task handlers. Converted to `EffectError::Handler` at the
/// dispatch boundary, but kept structured internally so unit tests can match
/// on `TaskHandlerError::TaskNotFound { .. }` precisely.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TaskHandlerError {
    /// Block doesn't exist for this agent.
    #[error("no block {block:?} for agent {agent:?}")]
    BlockNotFound { agent: String, block: String },
    /// Block exists but its schema isn't TaskList.
    #[error(transparent)]
    Memory(#[from] MemoryError),
    /// Request payload failed to JSON-decode.
    #[error("Pattern.Tasks: invalid {what} JSON: {source}")]
    Json {
        /// Which payload (TaskSpec / TaskPatch / TaskStatus) failed.
        what: &'static str,
        source: serde_json::Error,
    },
    /// The target ref couldn't be parsed.
    #[error("Pattern.Tasks: invalid TaskEdgeRef {ref_str:?}: {source}")]
    BadEdgeRef {
        ref_str: String,
        source: pattern_core::types::memory_types::TaskEdgeRefParseError,
    },
    /// TaskEdgeRef addresses a block rather than an item.
    #[error("Pattern.Tasks: operation requires a task-item ref (got block-level ref {ref_str:?})")]
    MissingItemId { ref_str: String },
    /// Loro-level write failed.
    #[error("Pattern.Tasks: loro mutation: {0}")]
    Loro(String),
    /// Underlying MemoryStore call failed.
    #[error("Pattern.Tasks: memory store: {0}")]
    Store(String),
    /// `list_tasks` received a `block=Some(h)` scope that is excluded by the
    /// caller's own `filter.blocks` set. The request is self-contradictory.
    #[error(
        "Pattern.Tasks::List: scoped to block {scoped_block:?}, but filter.blocks={filter_blocks:?} excludes it"
    )]
    ConflictingBlockScope {
        scoped_block: String,
        filter_blocks: Vec<String>,
    },
}

impl From<TaskHandlerError> for EffectError {
    fn from(e: TaskHandlerError) -> Self {
        EffectError::Handler(format!("{e}"))
    }
}

// endregion: internal error type

// region: helpers

/// Parse a "handle#item" (or plain "handle") `TaskEdgeRef` string, returning
/// the block handle + item-id pair. Requires the item component — callers that
/// need to accept block-level refs should handle that explicitly.
fn parse_item_ref(ref_str: &str) -> Result<(String, String), TaskHandlerError> {
    let parsed: TaskEdgeRef =
        ref_str
            .parse::<TaskEdgeRef>()
            .map_err(|e| TaskHandlerError::BadEdgeRef {
                ref_str: ref_str.to_string(),
                source: e,
            })?;
    let item = parsed
        .task_item
        .ok_or_else(|| TaskHandlerError::MissingItemId {
            ref_str: ref_str.to_string(),
        })?;
    Ok((parsed.block.to_string(), item.to_string()))
}

/// Parse a `TaskEdgeRef` string without requiring the item component. Used
/// for link/unlink targets, which may address either a specific item or an
/// entire block.
fn parse_edge_ref_any(ref_str: &str) -> Result<(String, Option<String>), TaskHandlerError> {
    let parsed: TaskEdgeRef =
        ref_str
            .parse::<TaskEdgeRef>()
            .map_err(|e| TaskHandlerError::BadEdgeRef {
                ref_str: ref_str.to_string(),
                source: e,
            })?;
    Ok((
        parsed.block.to_string(),
        parsed.task_item.map(|s| s.to_string()),
    ))
}

/// Fetch a block's StructuredDocument and verify its schema is TaskList.
fn fetch_task_list(
    store: &dyn MemoryStore,
    agent_id: &str,
    block: &str,
) -> Result<StructuredDocument, TaskHandlerError> {
    let sdoc = store
        .get_block(agent_id, block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?
        .ok_or_else(|| TaskHandlerError::BlockNotFound {
            agent: agent_id.to_string(),
            block: block.to_string(),
        })?;
    if !matches!(sdoc.schema(), BlockSchema::TaskList { .. }) {
        return Err(TaskHandlerError::Memory(MemoryError::NotATaskList {
            block: block.into(),
        }));
    }
    Ok(sdoc)
}

/// Convert a `serde_json::Value` to a `loro::LoroValue`. Skips unrepresentable
/// numbers (u64 > i64::MAX) to `Null`.
fn json_to_loro(value: &JsonValue) -> LoroValue {
    match value {
        JsonValue::Null => LoroValue::Null,
        JsonValue::Bool(b) => LoroValue::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                LoroValue::I64(i)
            } else if let Some(f) = n.as_f64() {
                LoroValue::Double(f)
            } else {
                LoroValue::Null
            }
        }
        JsonValue::String(s) => LoroValue::String(s.clone().into()),
        JsonValue::Array(arr) => {
            LoroValue::List(arr.iter().map(json_to_loro).collect::<Vec<_>>().into())
        }
        JsonValue::Object(obj) => {
            let map: HashMap<String, LoroValue> = obj
                .iter()
                .map(|(k, v)| (k.clone(), json_to_loro(v)))
                .collect();
            LoroValue::Map(map.into())
        }
    }
}

/// Convert a `loro::LoroValue` snapshot back to `serde_json::Value`. Container
/// references (which shouldn't appear in deep-value reads) collapse to `Null`.
fn loro_to_json(value: &LoroValue) -> JsonValue {
    match value {
        LoroValue::Null => JsonValue::Null,
        LoroValue::Bool(b) => JsonValue::Bool(*b),
        LoroValue::I64(i) => serde_json::json!(*i),
        LoroValue::Double(f) => serde_json::json!(*f),
        LoroValue::String(s) => JsonValue::String(s.to_string()),
        LoroValue::List(l) => JsonValue::Array(l.iter().map(loro_to_json).collect()),
        LoroValue::Map(m) => {
            let obj: serde_json::Map<String, JsonValue> = m
                .iter()
                .map(|(k, v)| (k.clone(), loro_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        _ => JsonValue::Null,
    }
}

/// Serialize a `TaskStatus` as its kebab-case string form (matches the JSON
/// serde representation and the form the subscriber expects).
///
/// Returns a `TaskHandlerError` if a future `TaskStatus` variant is ever
/// added that doesn't serialize to a bare JSON string (e.g., a struct
/// variant). Today all variants are unit-kebab, so this error is unreachable
/// via normal use.
fn task_status_kebab(status: TaskStatus) -> Result<String, TaskHandlerError> {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(TaskHandlerError::Loro(format!(
            "TaskStatus serialization produced non-string: {other}"
        ))),
        Err(e) => Err(TaskHandlerError::Loro(format!(
            "TaskStatus serialization failed: {e}"
        ))),
    }
}

/// Find the index of a task item by id in a TaskList doc's `items` movable
/// list. Returns `None` if no matching item exists.
fn find_item_index(doc: &LoroDoc, item_id: &str) -> Option<usize> {
    let list = doc.get_movable_list("items");
    let deep = list.get_deep_value();
    let LoroValue::List(items) = deep else {
        return None;
    };
    items.iter().enumerate().find_map(|(i, v)| match v {
        LoroValue::Map(m) => match m.get("id") {
            Some(LoroValue::String(s)) if s.as_str() == item_id => Some(i),
            _ => None,
        },
        _ => None,
    })
}

/// Read the item at `index` as a JSON object. Returns `None` if the item is
/// not a Map (shouldn't happen for a well-formed TaskList). Used by tests
/// to inspect the deep-value materialized shape; production code paths go
/// through `item_map_at` for direct container mutation.
#[cfg(test)]
fn read_item_as_json(doc: &LoroDoc, index: usize) -> Option<serde_json::Map<String, JsonValue>> {
    let list = doc.get_movable_list("items");
    let deep = list.get_deep_value();
    let LoroValue::List(items) = deep else {
        return None;
    };
    let item_val = items.get(index)?;
    let LoroValue::Map(m) = item_val else {
        return None;
    };
    let map: serde_json::Map<String, JsonValue> = m
        .iter()
        .map(|(k, v)| (k.clone(), loro_to_json(v)))
        .collect();
    Some(map)
}

/// Get the `LoroMap` container for the item at `index` in the TaskList's
/// `items` movable list. Returns `None` when the item is not a container-backed
/// map (either index out of bounds or — shouldn't happen post review fix I3 —
/// a legacy value-map snapshot). Fresh docs and docs round-tripped through
/// `apply_json_to_loro_doc` / `import_from_json` both produce containers.
fn item_map_at(doc: &LoroDoc, index: usize) -> Option<loro::LoroMap> {
    let list = doc.get_movable_list("items");
    let entry = list.get(index)?;
    entry.into_container().ok()?.into_map().ok()
}

/// Get the nested `LoroList` container for a named field on a task item
/// (typically `"comments"` or `"blocks"`). Returns `None` if the field is
/// missing or not a container-backed list.
fn item_field_list(item_map: &loro::LoroMap, key: &str) -> Option<loro::LoroList> {
    let entry = item_map.get(key)?;
    entry.into_container().ok()?.into_list().ok()
}

/// Convert a `serde_json::Map` into a `LoroValue::Map` (snapshot value).
/// Used for inserting immutable records like `TaskComment` and edge refs
/// into their parent nested lists — those records are not expected to mutate
/// post-insertion, so full-value storage is appropriate.
fn json_map_to_loro_value(map: serde_json::Map<String, JsonValue>) -> LoroValue {
    json_to_loro(&JsonValue::Object(map))
}

/// Record a task-block mutation on the adapter's pending `BlockWrite` buffer.
///
/// The adapter drains this buffer at turn close to populate
/// `TurnOutput.block_writes`, which drives Phase 5's pseudo-message emission
/// and mid-batch delta snapshot attachments. Without this call, agents'
/// task-block writes would be invisible in subsequent composed requests.
///
/// `rendered_content` is the JSON-serialized deep value of the block's
/// LoroDoc — a canonical, stable textual form suitable for snapshot
/// attachments. Phase 5 may refine this to a more compact rendering.
fn record_task_write(
    adapter: &MemoryStoreAdapter,
    agent_id: &str,
    store: &dyn MemoryStore,
    block_handle: &str,
    kind: BlockWriteKind,
) -> Result<(), TaskHandlerError> {
    let sdoc = store
        .get_block(agent_id, block_handle)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?
        .ok_or_else(|| TaskHandlerError::BlockNotFound {
            agent: agent_id.to_string(),
            block: block_handle.to_string(),
        })?;
    let memory_id = SmolStr::new(sdoc.id());
    let block_type = sdoc.block_type();
    let deep = sdoc.inner().get_deep_value();
    let rendered_content = serde_json::to_string(&loro_to_json(&deep)).unwrap_or_default();

    adapter.record_write(BlockWrite {
        handle: SmolStr::new(block_handle),
        memory_id,
        block_type,
        rendered_content,
        kind,
        previous_content_hash: None,
        previous_rendered_content: None,
        at: jiff::Timestamp::now(),
        author: Author::Agent(AgentAuthor {
            agent_id: SmolStr::new(agent_id),
        }),
    });
    Ok(())
}

// endregion: helpers

// region: handlers

/// Create a new task item in the given block. Returns the minted item id.
pub fn handle_create(
    store: &dyn MemoryStore,
    agent_id: &str,
    block: &str,
    spec_json: &str,
) -> Result<TaskItemId, TaskHandlerError> {
    let spec: TaskSpec =
        serde_json::from_str(spec_json).map_err(|source| TaskHandlerError::Json {
            what: "TaskSpec",
            source,
        })?;
    let sdoc = fetch_task_list(store, agent_id, block)?;

    let item_id: TaskItemId = new_snowflake_id();
    let now = jiff::Timestamp::now();

    // Push a nested LoroMap container (NOT a value-map) so subsequent
    // in-place mutations on individual fields (status, subject, comments,
    // blocks...) produce proper CRDT ops and merge correctly under
    // concurrent edits. See review finding I3.
    let doc = sdoc.inner();
    let list = doc.get_movable_list("items");
    let item_map = list
        .push_container(loro::LoroMap::new())
        .map_err(|e| TaskHandlerError::Loro(format!("push_container: {e}")))?;

    let insert = |key: &str, value: &str| -> Result<(), TaskHandlerError> {
        item_map
            .insert(key, value)
            .map_err(|e| TaskHandlerError::Loro(format!("insert {key}: {e}")))
            .map(|_| ())
    };
    insert("id", item_id.as_str())?;
    insert("subject", &spec.subject)?;
    if !spec.description.is_empty() {
        insert("description", &spec.description)?;
    }
    let status = spec.status.unwrap_or(TaskStatus::Pending);
    insert("status", &task_status_kebab(status)?)?;
    if let Some(ref owner) = spec.owner {
        insert("owner", owner.as_str())?;
    }
    if let Some(ref active) = spec.active_form {
        insert("active_form", active)?;
    }
    insert("created_at", &now.to_string())?;
    insert("updated_at", &now.to_string())?;
    if !spec.metadata.is_null() {
        item_map
            .insert("metadata", json_to_loro(&spec.metadata))
            .map_err(|e| TaskHandlerError::Loro(format!("insert metadata: {e}")))?;
    }
    // `comments` and `blocks` are nested LoroList containers so future
    // add_comment / link / unlink calls produce append/delete ops rather
    // than wholesale replacements.
    item_map
        .insert_container("comments", loro::LoroList::new())
        .map_err(|e| TaskHandlerError::Loro(format!("insert_container comments: {e}")))?;
    item_map
        .insert_container("blocks", loro::LoroList::new())
        .map_err(|e| TaskHandlerError::Loro(format!("insert_container blocks: {e}")))?;

    doc.commit();

    store.mark_dirty(agent_id, block);
    store
        .persist_block(agent_id, block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(item_id)
}

/// Apply a partial patch to an existing task item. Each field is set
/// in-place on the item's `LoroMap` container so concurrent edits to
/// different fields merge correctly.
pub fn handle_update(
    store: &dyn MemoryStore,
    agent_id: &str,
    edge_ref: &str,
    patch_json: &str,
) -> Result<(), TaskHandlerError> {
    let patch: TaskPatch =
        serde_json::from_str(patch_json).map_err(|source| TaskHandlerError::Json {
            what: "TaskPatch",
            source,
        })?;
    let (block, item_id) = parse_item_ref(edge_ref)?;
    let sdoc = fetch_task_list(store, agent_id, &block)?;

    let doc = sdoc.inner();
    let index = find_item_index(doc, &item_id).ok_or_else(|| {
        TaskHandlerError::Memory(MemoryError::TaskNotFound {
            block: block.as_str().into(),
            item: item_id.as_str().into(),
        })
    })?;
    let item_map = item_map_at(doc, index).ok_or_else(|| {
        TaskHandlerError::Loro(format!("item at index {index} is not a LoroMap container"))
    })?;

    apply_patch_to_item_map(&item_map, patch)?;
    item_map
        .insert("updated_at", jiff::Timestamp::now().to_string().as_str())
        .map_err(|e| TaskHandlerError::Loro(format!("insert updated_at: {e}")))?;

    doc.commit();

    store.mark_dirty(agent_id, &block);
    store
        .persist_block(agent_id, &block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Transition a task's status, with optional `completed_at` stamping when
/// moving to `Completed`.
pub fn handle_transition(
    store: &dyn MemoryStore,
    agent_id: &str,
    edge_ref: &str,
    status_json: &str,
) -> Result<(), TaskHandlerError> {
    let status: TaskStatus =
        serde_json::from_str(status_json).map_err(|source| TaskHandlerError::Json {
            what: "TaskStatus",
            source,
        })?;
    let (block, item_id) = parse_item_ref(edge_ref)?;
    let sdoc = fetch_task_list(store, agent_id, &block)?;

    let doc = sdoc.inner();
    let index = find_item_index(doc, &item_id).ok_or_else(|| {
        TaskHandlerError::Memory(MemoryError::TaskNotFound {
            block: block.as_str().into(),
            item: item_id.as_str().into(),
        })
    })?;

    let item_map = item_map_at(doc, index).ok_or_else(|| {
        TaskHandlerError::Loro(format!("item at index {index} is not a LoroMap container"))
    })?;

    let now = jiff::Timestamp::now();
    item_map
        .insert("status", task_status_kebab(status)?.as_str())
        .map_err(|e| TaskHandlerError::Loro(format!("insert status: {e}")))?;
    item_map
        .insert("updated_at", now.to_string().as_str())
        .map_err(|e| TaskHandlerError::Loro(format!("insert updated_at: {e}")))?;
    if status == TaskStatus::Completed {
        item_map
            .insert("completed_at", now.to_string().as_str())
            .map_err(|e| TaskHandlerError::Loro(format!("insert completed_at: {e}")))?;
    } else {
        // Reverse transition (Completed → anything else) clears the stale
        // completed_at stamp so the LoroDoc state matches the new status.
        // `LoroMap::delete` tolerates missing keys, so unconditional delete
        // is safe when the task was never completed in the first place.
        item_map
            .delete("completed_at")
            .map_err(|e| TaskHandlerError::Loro(format!("delete completed_at: {e}")))?;
    }

    doc.commit();

    store.mark_dirty(agent_id, &block);
    store
        .persist_block(agent_id, &block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Append a comment to a task. The comment's `author` is the calling agent and
/// `timestamp` is captured at handler time.
pub fn handle_add_comment(
    store: &dyn MemoryStore,
    agent_id: &str,
    edge_ref: &str,
    text: &str,
) -> Result<(), TaskHandlerError> {
    let (block, item_id) = parse_item_ref(edge_ref)?;
    let sdoc = fetch_task_list(store, agent_id, &block)?;

    let doc = sdoc.inner();
    let index = find_item_index(doc, &item_id).ok_or_else(|| {
        TaskHandlerError::Memory(MemoryError::TaskNotFound {
            block: block.as_str().into(),
            item: item_id.as_str().into(),
        })
    })?;

    let item_map = item_map_at(doc, index).ok_or_else(|| {
        TaskHandlerError::Loro(format!("item at index {index} is not a LoroMap container"))
    })?;
    let comments = item_field_list(&item_map, "comments").ok_or_else(|| {
        TaskHandlerError::Loro("task item is missing its `comments` LoroList container".to_string())
    })?;

    let now = jiff::Timestamp::now();
    let mut comment_map = serde_json::Map::new();
    comment_map.insert("author".into(), JsonValue::String(agent_id.to_string()));
    comment_map.insert("timestamp".into(), JsonValue::String(now.to_string()));
    comment_map.insert("text".into(), JsonValue::String(text.to_string()));
    comments
        .push(json_map_to_loro_value(comment_map))
        .map_err(|e| TaskHandlerError::Loro(format!("push comment: {e}")))?;

    item_map
        .insert("updated_at", now.to_string().as_str())
        .map_err(|e| TaskHandlerError::Loro(format!("insert updated_at: {e}")))?;

    doc.commit();

    store.mark_dirty(agent_id, &block);
    store
        .persist_block(agent_id, &block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Add a directed dependency edge from `source_ref` (which must address a
/// specific item) to `target_ref` (block-level or item-level). The edge lives
/// on the source item's `blocks` list; the target's LoroDoc is never touched.
///
/// If an identical edge already exists, this is a no-op (dedup keeps the
/// canonical .kdl file tidy and prevents duplicate rows on reconcile).
pub fn handle_link(
    store: &dyn MemoryStore,
    agent_id: &str,
    source_ref: &str,
    target_ref: &str,
) -> Result<(), TaskHandlerError> {
    let (src_block, src_item) = parse_item_ref(source_ref)?;
    let (tgt_block, tgt_item) = parse_edge_ref_any(target_ref)?;

    let sdoc = fetch_task_list(store, agent_id, &src_block)?;
    let doc = sdoc.inner();
    let index = find_item_index(doc, &src_item).ok_or_else(|| {
        TaskHandlerError::Memory(MemoryError::TaskNotFound {
            block: src_block.as_str().into(),
            item: src_item.as_str().into(),
        })
    })?;

    let item_map = item_map_at(doc, index).ok_or_else(|| {
        TaskHandlerError::Loro(format!("item at index {index} is not a LoroMap container"))
    })?;
    let blocks = item_field_list(&item_map, "blocks").ok_or_else(|| {
        TaskHandlerError::Loro("task item is missing its `blocks` LoroList container".to_string())
    })?;

    // Dedup: skip if an identical edge already exists. Read via deep_value
    // once to avoid per-element SQL-shaped comparisons.
    let already_present = matches!(blocks.get_deep_value(), LoroValue::List(ref list)
        if list.iter().any(|v| loro_edge_matches(v, &tgt_block, tgt_item.as_deref())));
    if already_present {
        return Ok(());
    }

    blocks
        .push(json_map_to_loro_value(build_edge_map(
            &tgt_block,
            tgt_item.as_deref(),
        )))
        .map_err(|e| TaskHandlerError::Loro(format!("push edge: {e}")))?;

    item_map
        .insert("updated_at", jiff::Timestamp::now().to_string().as_str())
        .map_err(|e| TaskHandlerError::Loro(format!("insert updated_at: {e}")))?;

    doc.commit();

    store.mark_dirty(agent_id, &src_block);
    store
        .persist_block(agent_id, &src_block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Remove a directed edge from the source item's `blocks` list. If no matching
/// edge exists, this is a silent no-op (no LoroDoc mutation, no dirty mark).
pub fn handle_unlink(
    store: &dyn MemoryStore,
    agent_id: &str,
    source_ref: &str,
    target_ref: &str,
) -> Result<(), TaskHandlerError> {
    let (src_block, src_item) = parse_item_ref(source_ref)?;
    let (tgt_block, tgt_item) = parse_edge_ref_any(target_ref)?;

    let sdoc = fetch_task_list(store, agent_id, &src_block)?;
    let doc = sdoc.inner();
    // Idempotent: if the source item doesn't exist, there's no edge to remove.
    // Matches the "no-op if edge doesn't exist" contract — generalized to the
    // whole operation, so `unlink` never errors on absent inputs.
    let Some(index) = find_item_index(doc, &src_item) else {
        return Ok(());
    };

    let item_map = item_map_at(doc, index).ok_or_else(|| {
        // Item exists at this index but is not a LoroMap container. Post-I3
        // this shouldn't happen for any production write path (handle_create,
        // apply_json_to_loro_doc, import_from_json all push containers), but
        // if legacy on-disk content survives a partial rollout we surface the
        // mismatch instead of silently claiming success — the caller's agent
        // thought they removed the edge, but it would have stayed.
        TaskHandlerError::Loro(format!("item at index {index} is not a LoroMap container"))
    })?;
    let Some(blocks) = item_field_list(&item_map, "blocks") else {
        // No `blocks` container on this item; no edges to remove.
        return Ok(());
    };

    // Scan for matching edges and delete them from the end backward to
    // avoid index-shift bugs.
    let LoroValue::List(list_snapshot) = blocks.get_deep_value() else {
        return Ok(());
    };
    let to_delete: Vec<usize> = list_snapshot
        .iter()
        .enumerate()
        .filter_map(|(i, v)| loro_edge_matches(v, &tgt_block, tgt_item.as_deref()).then_some(i))
        .collect();
    if to_delete.is_empty() {
        return Ok(());
    }
    for idx in to_delete.iter().rev() {
        blocks
            .delete(*idx, 1)
            .map_err(|e| TaskHandlerError::Loro(format!("delete edge at {idx}: {e}")))?;
    }

    item_map
        .insert("updated_at", jiff::Timestamp::now().to_string().as_str())
        .map_err(|e| TaskHandlerError::Loro(format!("insert updated_at: {e}")))?;

    doc.commit();

    store.mark_dirty(agent_id, &src_block);
    store
        .persist_block(agent_id, &src_block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Whether a Loro edge value matches `(target_block, target_item)`. Edges are
/// shaped as `{ "block": String, "task_item": String | Null }` stored as
/// `LoroValue::Map` snapshots inside the item's `blocks` `LoroList` container.
fn loro_edge_matches(edge: &LoroValue, block: &str, item: Option<&str>) -> bool {
    let LoroValue::Map(map) = edge else {
        return false;
    };
    let e_block = match map.get("block") {
        Some(LoroValue::String(s)) => s.as_str(),
        _ => return false,
    };
    if e_block != block {
        return false;
    }
    let e_item = match map.get("task_item") {
        Some(LoroValue::String(s)) => Some(s.as_str()),
        Some(LoroValue::Null) | None => None,
        _ => return false,
    };
    e_item == item
}

/// Construct a new edge record as a `serde_json::Map` — the caller wraps it
/// in a LoroValue::Map for insertion into an item's `blocks` `LoroList`.
fn build_edge_map(block: &str, item: Option<&str>) -> serde_json::Map<String, JsonValue> {
    let mut edge = serde_json::Map::new();
    edge.insert("block".into(), JsonValue::String(block.to_string()));
    edge.insert(
        "task_item".into(),
        item.map(|s| JsonValue::String(s.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    edge
}

/// List tasks visible to `agent_id`, optionally scoped to a single block.
///
/// When `block` is `Some`, the handler verifies the block exists and is a
/// TaskList schema (returning `NotATaskList` otherwise) and restricts the
/// query to that handle. When `block` is `None`, the handler enumerates all
/// TaskList-schema blocks visible via `MemoryStore::list_blocks` for the
/// caller — the underlying `MemoryScope` handles `IsolatePolicy` routing.
///
/// The caller's `filter.blocks` (if set) is intersected with the visible set;
/// an empty intersection short-circuits to `Ok(vec![])` without touching SQL.
///
/// `blocker_count` / `blocks_count` are batched via two aggregate queries on
/// `task_edges` rather than N+1 lookups.
pub fn handle_list_tasks(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    agent_id: &str,
    block: Option<&str>,
    filter_json: &str,
) -> Result<Vec<TaskView>, TaskHandlerError> {
    let mut filter: TaskFilter =
        serde_json::from_str(filter_json).map_err(|source| TaskHandlerError::Json {
            what: "TaskFilter",
            source,
        })?;

    let visible_blocks: Vec<smol_str::SmolStr> = match block {
        Some(h) => {
            // Existence + schema enforcement.
            fetch_task_list(store, agent_id, h)?;
            // Reject a self-contradictory request where the caller scopes
            // to block `h` but supplies a `filter.blocks` set that excludes it.
            if let Some(user_blocks) = &filter.blocks
                && !user_blocks.iter().any(|b| b.as_str() == h)
            {
                return Err(TaskHandlerError::ConflictingBlockScope {
                    scoped_block: h.to_string(),
                    filter_blocks: user_blocks.iter().map(|b| b.to_string()).collect(),
                });
            }
            vec![smol_str::SmolStr::new(h)]
        }
        None => {
            let metas = store
                .list_blocks(BlockFilter::by_agent(agent_id))
                .map_err(|e| TaskHandlerError::Store(e.to_string()))?;
            metas
                .into_iter()
                .filter(|m| matches!(m.schema, BlockSchema::TaskList { .. }))
                .map(|m| smol_str::SmolStr::new(&m.label))
                .collect()
        }
    };

    // Intersect with any caller-supplied block constraint.
    filter.blocks = Some(match filter.blocks.take() {
        Some(user_blocks) => {
            let visible_set: std::collections::HashSet<_> =
                visible_blocks.iter().cloned().collect();
            user_blocks
                .into_iter()
                .filter(|b| visible_set.contains(b))
                .collect()
        }
        None => visible_blocks,
    });

    if filter.blocks.as_ref().is_some_and(|v| v.is_empty()) {
        return Ok(Vec::new());
    }

    let rows = pattern_db::queries::list_tasks_filtered(conn, &filter)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    project_rows_to_views(conn, rows)
}

/// Perform a BFS graph traversal from `root_ref`, honouring `GraphQuery`'s
/// direction, depth, and max-nodes caps. Scope-checks the root's block AND
/// every node the BFS returns: nodes whose blocks are not visible to the
/// caller are dropped from the result (along with their incident edges).
/// This prevents an information leak where an edge from a visible block A
/// to a hidden block B would expose B's task identities via BFS results
/// (review finding C4).
pub fn handle_query_graph(
    store: &dyn MemoryStore,
    conn: &rusqlite::Connection,
    agent_id: &str,
    root_ref: &str,
    query_json: &str,
) -> Result<GraphSlice, TaskHandlerError> {
    let root: TaskEdgeRef =
        root_ref
            .parse::<TaskEdgeRef>()
            .map_err(|source| TaskHandlerError::BadEdgeRef {
                ref_str: root_ref.to_string(),
                source,
            })?;
    let query: GraphQuery =
        serde_json::from_str(query_json).map_err(|source| TaskHandlerError::Json {
            what: "GraphQuery",
            source,
        })?;

    // Scope-check: the root block must be accessible to the caller.
    fetch_task_list(store, agent_id, root.block.as_str())?;

    let raw = pattern_db::queries::query_task_graph_bfs(conn, &root, &query)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    // Compute the visible block set (TaskList-schema blocks owned by this
    // agent — MemoryScope handles IsolatePolicy routing upstream so this
    // already reflects the caller's persona/project visibility).
    let visible: std::collections::HashSet<smol_str::SmolStr> = store
        .list_blocks(BlockFilter::by_agent(agent_id))
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?
        .into_iter()
        .filter(|m| matches!(m.schema, BlockSchema::TaskList { .. }))
        .map(|m| smol_str::SmolStr::new(&m.label))
        .collect();

    // Short-circuit: if every raw node is already visible, return as-is.
    if raw.nodes.iter().all(|n| visible.contains(&n.block)) {
        return Ok(raw);
    }

    // Otherwise, filter nodes + drop edges touching filtered-out blocks.
    let kept_nodes: Vec<TaskEdgeRef> = raw
        .nodes
        .into_iter()
        .filter(|n| visible.contains(&n.block))
        .collect();
    let kept_set: std::collections::HashSet<&TaskEdgeRef> = kept_nodes.iter().collect();
    let kept_edges: Vec<(TaskEdgeRef, TaskEdgeRef)> = raw
        .edges
        .into_iter()
        .filter(|(src, tgt)| kept_set.contains(src) && kept_set.contains(tgt))
        .collect();

    Ok(GraphSlice {
        nodes: kept_nodes,
        edges: kept_edges,
        // Preserve the original truncation flag — the BFS decision to cut
        // off is independent of the post-hoc scope filter.
        truncated: raw.truncated,
    })
}

/// Project `TaskRow`s into `TaskView`s with batched blocker/blocks count
/// aggregates. Rows missing either `block_handle` or `task_item_id` are
/// filtered out (legacy pre-v3 rows not tied to a TaskList block).
fn project_rows_to_views(
    conn: &rusqlite::Connection,
    rows: Vec<pattern_db::queries::task_row::TaskRow>,
) -> Result<Vec<TaskView>, TaskHandlerError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let keys: Vec<(String, String)> = rows
        .iter()
        .filter_map(|r| r.block_handle.clone().zip(r.task_item_id.clone()))
        .collect();

    let in_degrees = aggregate_edge_counts(conn, &keys, /*as_target=*/ true)?;
    let out_degrees = aggregate_edge_counts(conn, &keys, /*as_target=*/ false)?;

    let views = rows
        .into_iter()
        .filter_map(|r| {
            let block = r.block_handle?;
            let item = r.task_item_id?;
            let key = (block.clone(), item.clone());
            let blocker_count = in_degrees.get(&key).copied().unwrap_or(0);
            let blocks_count = out_degrees.get(&key).copied().unwrap_or(0);
            Some(TaskView {
                block_ref: TaskEdgeRef {
                    block: block.into(),
                    task_item: Some(item.into()),
                },
                subject: r.subject,
                status: r.status,
                owner: r.owner_agent_id.map(smol_str::SmolStr::new),
                blocker_count,
                blocks_count,
            })
        })
        .collect();

    Ok(views)
}

/// Run a single aggregate query to count edges keyed on `(block, item)`.
/// `as_target == true` counts incoming edges (blocker_count); `false` counts
/// outgoing edges (blocks_count). NULL `target_item` values are never in our
/// key set (callers only pass item-level keys), so no sentinel handling is
/// required.
fn aggregate_edge_counts(
    conn: &rusqlite::Connection,
    keys: &[(String, String)],
    as_target: bool,
) -> Result<std::collections::HashMap<(String, String), usize>, TaskHandlerError> {
    if keys.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let (block_col, item_col) = if as_target {
        ("target_block", "target_item")
    } else {
        ("source_block", "source_item")
    };

    // Tuple-IN clause: `WHERE (block, item) IN ((?, ?), (?, ?), ...)`.
    let placeholders = vec!["(?, ?)"; keys.len()].join(", ");
    let sql = format!(
        "SELECT {block_col}, {item_col}, COUNT(*) \
         FROM task_edges \
         WHERE ({block_col}, {item_col}) IN ({placeholders}) \
         GROUP BY {block_col}, {item_col}"
    );

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    // Flatten keys into a param sequence.
    let mut flat: Vec<String> = Vec::with_capacity(keys.len() * 2);
    for (b, i) in keys {
        flat.push(b.clone());
        flat.push(i.clone());
    }

    let rows = stmt
        .query_map(rusqlite::params_from_iter(flat.iter()), |row| {
            let block: String = row.get(0)?;
            let item: String = row.get(1)?;
            let count: i64 = row.get(2)?;
            Ok(((block, item), count as usize))
        })
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    let mut result = std::collections::HashMap::new();
    for r in rows {
        let (key, count) = r.map_err(|e| TaskHandlerError::Store(e.to_string()))?;
        result.insert(key, count);
    }
    Ok(result)
}

/// Apply a `TaskPatch` to the given item's `LoroMap` container, using
/// in-place field inserts/deletes so concurrent edits merge at the field
/// level instead of LWW-overwriting the whole item (review finding I3).
///
/// Double-option fields (`owner`, `active_form`):
/// - Outer `None` → leave the field untouched.
/// - `Some(None)` → delete the key entirely (clear).
/// - `Some(Some(v))` → set to the new value.
///
/// Plain-option fields (`subject`, `description`, `status`, `metadata`):
/// - `None` → untouched.
/// - `Some(v)` → set.
fn apply_patch_to_item_map(
    item_map: &loro::LoroMap,
    patch: TaskPatch,
) -> Result<(), TaskHandlerError> {
    if let Some(subject) = patch.subject {
        item_map
            .insert("subject", subject.as_str())
            .map_err(|e| TaskHandlerError::Loro(format!("insert subject: {e}")))?;
    }
    if let Some(description) = patch.description {
        item_map
            .insert("description", description.as_str())
            .map_err(|e| TaskHandlerError::Loro(format!("insert description: {e}")))?;
    }
    if let Some(status) = patch.status {
        item_map
            .insert("status", task_status_kebab(status)?.as_str())
            .map_err(|e| TaskHandlerError::Loro(format!("insert status: {e}")))?;
    }
    if let Some(metadata) = patch.metadata {
        item_map
            .insert("metadata", json_to_loro(&metadata))
            .map_err(|e| TaskHandlerError::Loro(format!("insert metadata: {e}")))?;
    }
    if let Some(owner) = patch.owner {
        match owner {
            None => {
                item_map
                    .delete("owner")
                    .map_err(|e| TaskHandlerError::Loro(format!("delete owner: {e}")))?;
            }
            Some(id) => {
                item_map
                    .insert("owner", id.as_str())
                    .map_err(|e| TaskHandlerError::Loro(format!("insert owner: {e}")))?;
            }
        }
    }
    if let Some(active_form) = patch.active_form {
        match active_form {
            None => {
                item_map
                    .delete("active_form")
                    .map_err(|e| TaskHandlerError::Loro(format!("delete active_form: {e}")))?;
            }
            Some(s) => {
                item_map
                    .insert("active_form", s.as_str())
                    .map_err(|e| TaskHandlerError::Loro(format!("insert active_form: {e}")))?;
            }
        }
    }
    Ok(())
}

// endregion: handlers

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::MemoryBlockType;
    use smol_str::SmolStr;

    use crate::testing::in_memory_store::InMemoryMemoryStore;

    fn task_list_schema() -> BlockSchema {
        BlockSchema::TaskList {
            default_status: None,
            default_owner: None,
            display_limit: None,
        }
    }

    fn text_schema() -> BlockSchema {
        BlockSchema::Text { viewport: None }
    }

    fn seed_task_list(store: &dyn MemoryStore, agent_id: &str, label: &str) -> StructuredDocument {
        let create = BlockCreate::new(
            label.to_string(),
            MemoryBlockType::Working,
            task_list_schema(),
        )
        .with_description("test".to_string())
        .with_char_limit(4096);
        store
            .create_block(agent_id, create)
            .expect("create TaskList block")
    }

    fn sample_spec(subject: &str) -> String {
        serde_json::to_string(&TaskSpec {
            subject: subject.to_string(),
            description: String::new(),
            active_form: None,
            status: None,
            owner: None,
            metadata: JsonValue::Null,
        })
        .unwrap()
    }

    #[test]
    fn create_pushes_item_into_movable_list() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");

        let item_id = handle_create(&*store, "agent-a", "tasks", &sample_spec("fix bug"))
            .expect("create succeeds");

        // Re-fetch and inspect the movable list.
        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let list = sdoc.inner().get_movable_list("items");
        assert_eq!(list.len(), 1, "one item pushed");

        let LoroValue::List(items) = list.get_deep_value() else {
            panic!("items must be a list");
        };
        let LoroValue::Map(item) = &items[0] else {
            panic!("item must be a map");
        };
        let id_field = item.get("id").expect("id present");
        assert!(matches!(id_field, LoroValue::String(s) if s.as_str() == item_id.as_str()));
        let subj = item.get("subject").expect("subject present");
        assert!(matches!(subj, LoroValue::String(s) if s.as_str() == "fix bug"));
        let status = item.get("status").expect("status present");
        assert!(matches!(status, LoroValue::String(s) if s.as_str() == "pending"));
    }

    #[test]
    fn create_on_non_tasklist_returns_not_a_task_list() {
        let store = Arc::new(InMemoryMemoryStore::new());
        // Seed a Text-schema block with the same label.
        let create = BlockCreate::new("notes".to_string(), MemoryBlockType::Working, text_schema())
            .with_description("text".to_string())
            .with_char_limit(4096);
        store.create_block("agent-a", create).unwrap();

        let err = handle_create(&*store, "agent-a", "notes", &sample_spec("x"))
            .expect_err("schema mismatch must fail");
        assert!(
            matches!(
                err,
                TaskHandlerError::Memory(MemoryError::NotATaskList { .. })
            ),
            "expected NotATaskList, got {err:?}"
        );
    }

    #[test]
    fn update_patches_specified_fields_only() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let item_id = handle_create(
            &*store,
            "agent-a",
            "tasks",
            &sample_spec("original subject"),
        )
        .unwrap();

        // Patch only the subject; description should remain untouched (empty).
        let patch = TaskPatch {
            subject: Some("new subject".to_string()),
            description: None,
            active_form: None,
            status: None,
            owner: None,
            metadata: None,
        };
        let patch_json = serde_json::to_string(&patch).unwrap();
        let edge_ref = format!("tasks#{item_id}");
        handle_update(&*store, "agent-a", &edge_ref, &patch_json).expect("update ok");

        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), 0).unwrap();
        assert_eq!(
            item.get("subject").and_then(|v| v.as_str()),
            Some("new subject")
        );
        // description was never set → still absent.
        assert!(item.get("description").is_none(), "description untouched");
        // updated_at must be present.
        assert!(
            item.get("updated_at").is_some(),
            "updated_at must be refreshed"
        );
    }

    #[test]
    fn transition_away_from_completed_clears_completed_at() {
        // M-c contract: when transitioning AWAY from Completed, the stale
        // completed_at timestamp is removed from the item's LoroMap. The
        // LoroDoc state must match the new status — otherwise cross-peer
        // merge and KDL rendering carry a bogus completed_at.
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let item_id = handle_create(&*store, "agent-a", "tasks", &sample_spec("task")).unwrap();
        let edge_ref = format!("tasks#{item_id}");

        // First complete it to populate completed_at.
        let completed = serde_json::to_string(&TaskStatus::Completed).unwrap();
        handle_transition(&*store, "agent-a", &edge_ref, &completed).unwrap();
        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), 0).unwrap();
        assert!(item.get("completed_at").is_some());

        // Now reverse: go back to InProgress. completed_at must be gone.
        let in_progress = serde_json::to_string(&TaskStatus::InProgress).unwrap();
        handle_transition(&*store, "agent-a", &edge_ref, &in_progress).unwrap();
        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), 0).unwrap();
        assert!(
            item.get("completed_at").is_none(),
            "completed_at must be cleared on reverse transition, got: {:?}",
            item.get("completed_at")
        );
    }

    #[test]
    fn transition_to_completed_sets_completed_at() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let item_id = handle_create(&*store, "agent-a", "tasks", &sample_spec("task")).unwrap();
        let edge_ref = format!("tasks#{item_id}");

        let status_json = serde_json::to_string(&TaskStatus::Completed).unwrap();
        handle_transition(&*store, "agent-a", &edge_ref, &status_json).unwrap();

        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), 0).unwrap();
        assert_eq!(
            item.get("status").and_then(|v| v.as_str()),
            Some("completed")
        );
        assert!(
            item.get("completed_at").is_some(),
            "completed_at must be set when transitioning to Completed"
        );
    }

    #[test]
    fn add_comment_appends_in_order() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let item_id = handle_create(&*store, "agent-a", "tasks", &sample_spec("t")).unwrap();
        let edge_ref = format!("tasks#{item_id}");

        handle_add_comment(&*store, "agent-a", &edge_ref, "first").unwrap();
        handle_add_comment(&*store, "agent-a", &edge_ref, "second").unwrap();
        handle_add_comment(&*store, "agent-a", &edge_ref, "third").unwrap();

        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), 0).unwrap();
        let comments = item
            .get("comments")
            .and_then(|v| v.as_array())
            .expect("comments present");
        assert_eq!(comments.len(), 3);
        assert_eq!(
            comments[0].get("text").and_then(|v| v.as_str()),
            Some("first")
        );
        assert_eq!(
            comments[1].get("text").and_then(|v| v.as_str()),
            Some("second")
        );
        assert_eq!(
            comments[2].get("text").and_then(|v| v.as_str()),
            Some("third")
        );
        // Each comment carries author = current agent.
        for c in comments {
            assert_eq!(
                c.get("author").and_then(|v| v.as_str()),
                Some("agent-a"),
                "author must be caller"
            );
        }
    }

    #[test]
    fn update_on_missing_ref_returns_task_not_found() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        // Well-formed edge ref but the item doesn't exist.
        let patch_json = serde_json::to_string(&TaskPatch {
            subject: Some("x".to_string()),
            description: None,
            active_form: None,
            status: None,
            owner: None,
            metadata: None,
        })
        .unwrap();
        let err = handle_update(&*store, "agent-a", "tasks#01HQZZZBOGUS01", &patch_json)
            .expect_err("must fail on missing item");
        assert!(
            matches!(
                err,
                TaskHandlerError::Memory(MemoryError::TaskNotFound { .. })
            ),
            "expected TaskNotFound, got {err:?}"
        );
    }

    #[test]
    fn update_applies_owner_clear_via_double_option() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        // Create with an owner.
        let spec = serde_json::to_string(&TaskSpec {
            subject: "t".to_string(),
            description: String::new(),
            active_form: None,
            status: None,
            owner: Some(SmolStr::new("agent-original")),
            metadata: JsonValue::Null,
        })
        .unwrap();
        let item_id = handle_create(&*store, "agent-a", "tasks", &spec).unwrap();
        let edge_ref = format!("tasks#{item_id}");

        // Patch owner to Some(None) → clear.
        let patch = TaskPatch {
            subject: None,
            description: None,
            active_form: None,
            status: None,
            owner: Some(None),
            metadata: None,
        };
        handle_update(
            &*store,
            "agent-a",
            &edge_ref,
            &serde_json::to_string(&patch).unwrap(),
        )
        .unwrap();

        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), 0).unwrap();
        assert!(item.get("owner").is_none(), "owner must be cleared");
    }

    // region: link / unlink tests

    /// Helper: read the blocks edge list on the item at `index` in `block`.
    fn edges_at(store: &dyn MemoryStore, agent: &str, block: &str, index: usize) -> Vec<JsonValue> {
        let sdoc = store.get_block(agent, block).unwrap().unwrap();
        let item = read_item_as_json(sdoc.inner(), index).unwrap();
        item.get("blocks")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn link_appends_edge_to_source_item_blocks() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "tasks", &sample_spec("B")).unwrap();

        let a_ref = format!("tasks#{a}");
        let b_ref = format!("tasks#{b}");
        handle_link(&*store, "agent-a", &a_ref, &b_ref).unwrap();

        // A's blocks list has exactly one edge pointing at B.
        let edges = edges_at(&*store, "agent-a", "tasks", 0);
        assert_eq!(edges.len(), 1, "exactly one edge");
        assert_eq!(
            edges[0].get("block").and_then(|v| v.as_str()),
            Some("tasks")
        );
        assert_eq!(
            edges[0].get("task_item").and_then(|v| v.as_str()),
            Some(b.as_str())
        );
    }

    #[test]
    fn link_twice_is_idempotent_dedup() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "tasks", &sample_spec("B")).unwrap();

        let a_ref = format!("tasks#{a}");
        let b_ref = format!("tasks#{b}");
        handle_link(&*store, "agent-a", &a_ref, &b_ref).unwrap();
        handle_link(&*store, "agent-a", &a_ref, &b_ref).unwrap();

        let edges = edges_at(&*store, "agent-a", "tasks", 0);
        assert_eq!(edges.len(), 1, "dedup keeps a single entry");
    }

    #[test]
    fn self_edge_allowed() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();

        let a_ref = format!("tasks#{a}");
        handle_link(&*store, "agent-a", &a_ref, &a_ref).expect("self-edge allowed");

        let edges = edges_at(&*store, "agent-a", "tasks", 0);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].get("task_item").and_then(|v| v.as_str()),
            Some(a.as_str()),
            "self-edge addresses itself"
        );
    }

    #[test]
    fn link_cross_block_does_not_touch_target_block_doc() {
        // Two distinct TaskList blocks. link(A@L1, B@L2) must only mutate L1.
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "l1");
        seed_task_list(&*store, "agent-a", "l2");
        let a = handle_create(&*store, "agent-a", "l1", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "l2", &sample_spec("B")).unwrap();

        // Snapshot L2's frontier before the link.
        let l2_before = {
            let sdoc = store.get_block("agent-a", "l2").unwrap().unwrap();
            sdoc.inner().state_frontiers()
        };

        let a_ref = format!("l1#{a}");
        let b_ref = format!("l2#{b}");
        handle_link(&*store, "agent-a", &a_ref, &b_ref).unwrap();

        // L2's frontier unchanged — we never touched its LoroDoc.
        let l2_after = {
            let sdoc = store.get_block("agent-a", "l2").unwrap().unwrap();
            sdoc.inner().state_frontiers()
        };
        assert_eq!(
            l2_before, l2_after,
            "target block's LoroDoc must not advance"
        );

        // And the edge IS in L1.
        let l1_edges = edges_at(&*store, "agent-a", "l1", 0);
        assert_eq!(l1_edges.len(), 1);
        assert_eq!(
            l1_edges[0].get("block").and_then(|v| v.as_str()),
            Some("l2")
        );
        assert_eq!(
            l1_edges[0].get("task_item").and_then(|v| v.as_str()),
            Some(b.as_str())
        );
    }

    #[test]
    fn unlink_removes_edge_from_blocks_list() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "tasks", &sample_spec("B")).unwrap();

        let a_ref = format!("tasks#{a}");
        let b_ref = format!("tasks#{b}");
        handle_link(&*store, "agent-a", &a_ref, &b_ref).unwrap();
        assert_eq!(edges_at(&*store, "agent-a", "tasks", 0).len(), 1);

        handle_unlink(&*store, "agent-a", &a_ref, &b_ref).unwrap();
        assert_eq!(
            edges_at(&*store, "agent-a", "tasks", 0).len(),
            0,
            "edge removed after unlink"
        );
    }

    #[test]
    fn unlink_nonexistent_edge_is_noop() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "tasks", &sample_spec("B")).unwrap();

        let a_ref = format!("tasks#{a}");
        let b_ref = format!("tasks#{b}");
        // No prior link — unlink must succeed silently.
        handle_unlink(&*store, "agent-a", &a_ref, &b_ref).expect("no-op unlink must not error");

        assert_eq!(edges_at(&*store, "agent-a", "tasks", 0).len(), 0);
    }

    #[test]
    fn link_missing_source_item_returns_task_not_found() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        // Bogus source item id.
        let err = handle_link(
            &*store,
            "agent-a",
            "tasks#01HQZZZBOGUS01",
            "tasks#any-target",
        )
        .expect_err("must fail");
        assert!(
            matches!(
                err,
                TaskHandlerError::Memory(MemoryError::TaskNotFound { .. })
            ),
            "expected TaskNotFound, got {err:?}"
        );
    }

    #[test]
    fn container_based_item_preserves_field_level_crdt_merge() {
        // I3 contract: concurrent edits to different fields of the same task
        // item merge correctly when items are stored as LoroMap containers
        // (not LoroValue::Map snapshots). With the old delete+insert approach,
        // one peer's edit would LWW-stomp the other's; with containers, both
        // survive.
        use loro::{LoroDoc, LoroMap};

        let doc_a = LoroDoc::new();
        doc_a.set_peer_id(1).expect("peer id");
        let list = doc_a.get_movable_list("items");
        let map = list.push_container(LoroMap::new()).expect("push container");
        map.insert("id", "t1").unwrap();
        map.insert("status", "pending").unwrap();
        map.insert("subject", "original").unwrap();
        doc_a.commit();

        // Fork to a second replica (doc_b).
        let doc_b = doc_a.fork();
        doc_b.set_peer_id(2).expect("peer id");

        // Concurrent edits: A changes status, B changes subject.
        let map_a = doc_a
            .get_movable_list("items")
            .get(0)
            .unwrap()
            .into_container()
            .ok()
            .unwrap()
            .into_map()
            .ok()
            .unwrap();
        map_a.insert("status", "in-progress").unwrap();
        doc_a.commit();

        let map_b = doc_b
            .get_movable_list("items")
            .get(0)
            .unwrap()
            .into_container()
            .ok()
            .unwrap()
            .into_map()
            .ok()
            .unwrap();
        map_b.insert("subject", "refined subject").unwrap();
        doc_b.commit();

        // Merge B's updates into A.
        let snap_b = doc_b.export(loro::ExportMode::all_updates()).unwrap();
        doc_a.import(&snap_b).expect("import updates");

        // Both edits survived — container merge preserved field-level writes.
        let merged = doc_a.get_movable_list("items").get_deep_value();
        let LoroValue::List(items) = merged else {
            panic!("items must be a list");
        };
        let LoroValue::Map(merged_item) = &items[0] else {
            panic!("item must be a map");
        };
        let field = |k: &str| match merged_item.get(k) {
            Some(LoroValue::String(s)) => Some(s.to_string()),
            _ => None,
        };
        assert_eq!(field("status").as_deref(), Some("in-progress"));
        assert_eq!(field("subject").as_deref(), Some("refined subject"));
        // Untouched field survived as well.
        assert_eq!(field("id").as_deref(), Some("t1"));
    }

    #[test]
    fn record_task_write_enqueues_block_write_on_adapter() {
        // I2 contract: mutations flow through adapter.record_write, so
        // TurnOutput.block_writes is populated at turn close and Phase 5's
        // pseudo-message emitter sees task-block changes.
        use std::sync::Arc;
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        handle_create(&*store, "agent-a", "tasks", &sample_spec("T1")).unwrap();

        let adapter = MemoryStoreAdapter::new(store.clone(), "agent-a");
        record_task_write(
            &adapter,
            "agent-a",
            &*store,
            "tasks",
            BlockWriteKind::Updated,
        )
        .unwrap();

        let drained = adapter.drain_pending();
        assert_eq!(drained.len(), 1, "one BlockWrite enqueued");
        assert_eq!(drained[0].handle.as_str(), "tasks");
        assert_eq!(drained[0].kind, BlockWriteKind::Updated);
        // rendered_content is the JSON deep-value of the block — for a
        // TaskList that means the items array; verify it's non-empty JSON.
        assert!(
            drained[0].rendered_content.starts_with('{'),
            "rendered_content must be a JSON object: {}",
            drained[0].rendered_content
        );
        assert!(
            drained[0].rendered_content.contains("T1"),
            "rendered_content must reflect the task we just created"
        );
    }

    #[test]
    fn unlink_missing_source_item_is_noop() {
        // M3 contract: unlink is fully idempotent — silent on missing source
        // item AND missing edge. Matches "remove if present" semantics.
        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        handle_unlink(
            &*store,
            "agent-a",
            "tasks#01HQZZZBOGUS01",
            "tasks#any-target",
        )
        .expect("missing source item must be a silent no-op, not an error");
    }

    // endregion: link / unlink tests

    // region: list / query-graph tests

    use pattern_core::types::memory_types::task_query::{Direction, GraphQuery};
    use pattern_db::ConstellationDb;

    fn open_db() -> ConstellationDb {
        ConstellationDb::open_in_memory().expect("in-memory db")
    }

    /// Seed a task row directly into the `tasks` table (bypassing the subscriber).
    fn seed_task_row(
        db: &ConstellationDb,
        block: &str,
        item_id: &str,
        subject: &str,
        status: TaskStatus,
        owner: Option<&str>,
    ) {
        let now = chrono::Utc::now();
        let row = pattern_db::queries::task_row::TaskRow {
            rowid: 0,
            id: format!("tk-{item_id}"),
            agent_id: None,
            subject: subject.to_string(),
            description: None,
            status,
            due_at: None,
            scheduled_at: None,
            completed_at: None,
            parent_task_id: None,
            block_handle: Some(block.to_string()),
            task_item_id: Some(item_id.to_string()),
            owner_agent_id: owner.map(|s| s.to_string()),
            comments_json: "[]".to_string(),
            created_at: now,
            updated_at: now,
        };
        let mut conn = db.get().expect("pool conn");
        let tx = conn.transaction().unwrap();
        pattern_db::queries::upsert_task_row(&tx, &row).unwrap();
        tx.commit().unwrap();
    }

    /// Seed a single edge from (src_block, src_item) to (tgt_block, tgt_item).
    fn seed_edge(
        db: &ConstellationDb,
        src_block: &str,
        src_item: &str,
        tgt_block: &str,
        tgt_item: Option<&str>,
    ) {
        let mut conn = db.get().unwrap();
        let tx = conn.transaction().unwrap();
        // upsert_task_edges replaces ALL edges for this source; pre-read + merge
        // so we don't clobber previously-seeded edges from the same source.
        let existing: Vec<(String, Option<String>)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT target_block, target_item FROM task_edges \
                     WHERE source_block = ?1 AND source_item = ?2",
                )
                .unwrap();
            stmt.query_map(rusqlite::params![src_block, src_item], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };
        let mut merged = existing;
        merged.push((tgt_block.to_string(), tgt_item.map(|s| s.to_string())));
        pattern_db::queries::upsert_task_edges(&tx, src_block, src_item, &merged).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn list_tasks_scoped_to_single_block_only_returns_that_block() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "l1");
        seed_task_list(&*store, "agent-a", "l2");
        seed_task_row(&db, "l1", "i1", "task 1", TaskStatus::Pending, None);
        seed_task_row(&db, "l1", "i2", "task 2", TaskStatus::InProgress, None);
        seed_task_row(&db, "l2", "i3", "task 3", TaskStatus::Pending, None);

        let conn = db.get().unwrap();
        let views =
            handle_list_tasks(&*store, &conn, "agent-a", Some("l1"), "{}").expect("list ok");
        assert_eq!(views.len(), 2, "only l1's tasks");
        for v in &views {
            assert_eq!(v.block_ref.block.as_str(), "l1");
        }
    }

    #[test]
    fn list_tasks_no_block_enumerates_all_visible() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "l1");
        seed_task_list(&*store, "agent-a", "l2");
        seed_task_row(&db, "l1", "i1", "one", TaskStatus::Pending, None);
        seed_task_row(&db, "l2", "i2", "two", TaskStatus::Pending, None);
        // And a task row for a block the agent does NOT own — must be invisible.
        seed_task_row(
            &db,
            "other-block",
            "i3",
            "hidden",
            TaskStatus::Pending,
            None,
        );

        let conn = db.get().unwrap();
        let views = handle_list_tasks(&*store, &conn, "agent-a", None, "{}").unwrap();
        assert_eq!(views.len(), 2, "agent sees only their own blocks' tasks");
        let blocks: std::collections::HashSet<&str> =
            views.iter().map(|v| v.block_ref.block.as_str()).collect();
        assert!(blocks.contains("l1") && blocks.contains("l2"));
        assert!(!blocks.contains("other-block"));
    }

    #[test]
    fn list_tasks_status_filter_matches_subset() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "a", "a", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "b", "b", TaskStatus::InProgress, None);
        seed_task_row(&db, "tasks", "c", "c", TaskStatus::Blocked, None);
        seed_task_row(&db, "tasks", "d", "d", TaskStatus::Completed, None);
        seed_task_row(&db, "tasks", "e", "e", TaskStatus::Cancelled, None);

        let filter = TaskFilter {
            status: Some(vec![TaskStatus::InProgress, TaskStatus::Blocked]),
            ..Default::default()
        };
        let filter_json = serde_json::to_string(&filter).unwrap();

        let conn = db.get().unwrap();
        let views = handle_list_tasks(&*store, &conn, "agent-a", None, &filter_json).unwrap();
        assert_eq!(views.len(), 2);
        for v in &views {
            assert!(matches!(
                v.status,
                TaskStatus::InProgress | TaskStatus::Blocked
            ));
        }
    }

    #[test]
    fn list_tasks_keyword_filter_matches_fts5() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(
            &db,
            "tasks",
            "a",
            "fix authentication bug",
            TaskStatus::Pending,
            None,
        );
        seed_task_row(&db, "tasks", "b", "write docs", TaskStatus::Pending, None);
        seed_task_row(
            &db,
            "tasks",
            "c",
            "refactor auth flow",
            TaskStatus::Pending,
            None,
        );

        let filter = TaskFilter {
            keyword: Some("auth*".to_string()),
            ..Default::default()
        };
        let filter_json = serde_json::to_string(&filter).unwrap();

        let conn = db.get().unwrap();
        let views = handle_list_tasks(&*store, &conn, "agent-a", None, &filter_json).unwrap();
        let subjects: std::collections::HashSet<&str> =
            views.iter().map(|v| v.subject.as_str()).collect();
        assert_eq!(views.len(), 2, "two matches for 'auth*'");
        assert!(subjects.contains("fix authentication bug"));
        assert!(subjects.contains("refactor auth flow"));
    }

    #[test]
    fn list_tasks_has_blockers_filter() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "a", "a", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "b", "b", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "c", "c", TaskStatus::Pending, None);
        // b is blocked by a (edge from a → b, so b has an incoming edge).
        seed_edge(&db, "tasks", "a", "tasks", Some("b"));

        let filter = TaskFilter {
            has_blockers: Some(true),
            ..Default::default()
        };
        let filter_json = serde_json::to_string(&filter).unwrap();

        let conn = db.get().unwrap();
        let views = handle_list_tasks(&*store, &conn, "agent-a", None, &filter_json).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(
            views[0].block_ref.task_item.as_ref().map(|s| s.as_str()),
            Some("b")
        );
    }

    #[test]
    fn list_tasks_projects_blocker_and_blocks_counts() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "a", "a", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "b", "b", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "c", "c", TaskStatus::Pending, None);
        // a → b and a → c: a has 2 outgoing (blocks_count=2)
        seed_edge(&db, "tasks", "a", "tasks", Some("b"));
        seed_edge(&db, "tasks", "a", "tasks", Some("c"));
        // c gets incoming from a (blocker_count=1 for c).

        let conn = db.get().unwrap();
        let views = handle_list_tasks(&*store, &conn, "agent-a", Some("tasks"), "{}").unwrap();
        let by_item: std::collections::HashMap<&str, &TaskView> = views
            .iter()
            .filter_map(|v| v.block_ref.task_item.as_deref().map(|s| (s, v)))
            .collect();
        assert_eq!(by_item["a"].blocks_count, 2);
        assert_eq!(by_item["a"].blocker_count, 0);
        assert_eq!(by_item["b"].blocker_count, 1);
        assert_eq!(by_item["c"].blocker_count, 1);
    }

    #[test]
    fn list_tasks_on_non_tasklist_returns_not_a_task_list() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        // Seed a Text block instead.
        let create = BlockCreate::new("notes".to_string(), MemoryBlockType::Working, text_schema())
            .with_description("notes".to_string())
            .with_char_limit(4096);
        store.create_block("agent-a", create).unwrap();

        let conn = db.get().unwrap();
        let err = handle_list_tasks(&*store, &conn, "agent-a", Some("notes"), "{}")
            .expect_err("must fail on non-TaskList block");
        assert!(matches!(
            err,
            TaskHandlerError::Memory(MemoryError::NotATaskList { .. })
        ));
    }

    #[test]
    fn list_tasks_conflicting_block_and_filter_blocks_errors() {
        // I5 contract: a caller that scopes `block=Some("tasks")` but sets
        // `filter.blocks=Some(["other"])` is self-contradictory. The handler
        // must reject with ConflictingBlockScope rather than silently return
        // an empty result.
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_list(&*store, "agent-a", "other");

        let filter = TaskFilter {
            blocks: Some(vec![smol_str::SmolStr::new("other")]),
            ..Default::default()
        };
        let filter_json = serde_json::to_string(&filter).unwrap();

        let conn = db.get().unwrap();
        let err = handle_list_tasks(&*store, &conn, "agent-a", Some("tasks"), &filter_json)
            .expect_err("must reject self-contradictory block+filter.blocks");
        assert!(
            matches!(err, TaskHandlerError::ConflictingBlockScope { .. }),
            "expected ConflictingBlockScope, got {err:?}"
        );
    }

    #[test]
    fn list_tasks_scoped_block_in_filter_blocks_is_accepted() {
        // Caller redundantly specifies the same block via both — valid.
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "i1", "t1", TaskStatus::Pending, None);

        let filter = TaskFilter {
            blocks: Some(vec![smol_str::SmolStr::new("tasks")]),
            ..Default::default()
        };
        let filter_json = serde_json::to_string(&filter).unwrap();

        let conn = db.get().unwrap();
        let views = handle_list_tasks(&*store, &conn, "agent-a", Some("tasks"), &filter_json)
            .expect("redundant but consistent scope is accepted");
        assert_eq!(views.len(), 1);
    }

    #[test]
    fn query_graph_forward_chain_of_5() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        for id in ["a", "b", "c", "d", "e"] {
            seed_task_row(&db, "tasks", id, id, TaskStatus::Pending, None);
        }
        // a → b → c → d → e
        seed_edge(&db, "tasks", "a", "tasks", Some("b"));
        seed_edge(&db, "tasks", "b", "tasks", Some("c"));
        seed_edge(&db, "tasks", "c", "tasks", Some("d"));
        seed_edge(&db, "tasks", "d", "tasks", Some("e"));

        let root = TaskEdgeRef {
            block: "tasks".into(),
            task_item: Some("a".into()),
        };
        let query = GraphQuery {
            direction: Direction::Forward,
            depth: None,
            max_nodes: None,
        };
        let conn = db.get().unwrap();
        let slice = handle_query_graph(
            &*store,
            &conn,
            "agent-a",
            &root.to_string(),
            &serde_json::to_string(&query).unwrap(),
        )
        .unwrap();

        assert_eq!(slice.nodes.len(), 5, "5 nodes in chain");
        assert_eq!(slice.edges.len(), 4, "4 edges in chain");
        assert!(!slice.truncated);

        // Edges are in original source→target orientation.
        let edge_pairs: std::collections::HashSet<(&str, &str)> = slice
            .edges
            .iter()
            .filter_map(|(src, tgt)| Some((src.task_item.as_deref()?, tgt.task_item.as_deref()?)))
            .collect();
        for (from, to) in [("a", "b"), ("b", "c"), ("c", "d"), ("d", "e")] {
            assert!(
                edge_pairs.contains(&(from, to)),
                "expected edge ({from}, {to}), got {edge_pairs:?}"
            );
        }
    }

    #[test]
    fn query_graph_depth_zero_returns_root_only() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "a", "a", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "b", "b", TaskStatus::Pending, None);
        seed_edge(&db, "tasks", "a", "tasks", Some("b"));

        let root = TaskEdgeRef {
            block: "tasks".into(),
            task_item: Some("a".into()),
        };
        let query = GraphQuery {
            direction: Direction::Forward,
            depth: Some(0),
            max_nodes: None,
        };
        let conn = db.get().unwrap();
        let slice = handle_query_graph(
            &*store,
            &conn,
            "agent-a",
            &root.to_string(),
            &serde_json::to_string(&query).unwrap(),
        )
        .unwrap();

        assert_eq!(slice.nodes.len(), 1, "only root node");
        assert_eq!(slice.edges.len(), 0, "no edges at depth 0");
    }

    #[test]
    fn query_graph_reverse_direction_walks_incoming_edges() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "a", "a", TaskStatus::Pending, None);
        seed_task_row(&db, "tasks", "b", "b", TaskStatus::Pending, None);
        // a → b, querying B with Reverse should find A.
        seed_edge(&db, "tasks", "a", "tasks", Some("b"));

        let root = TaskEdgeRef {
            block: "tasks".into(),
            task_item: Some("b".into()),
        };
        let query = GraphQuery {
            direction: Direction::Reverse,
            depth: None,
            max_nodes: None,
        };
        let conn = db.get().unwrap();
        let slice = handle_query_graph(
            &*store,
            &conn,
            "agent-a",
            &root.to_string(),
            &serde_json::to_string(&query).unwrap(),
        )
        .unwrap();

        assert_eq!(slice.nodes.len(), 2, "B + A (reverse reachable)");
        assert_eq!(slice.edges.len(), 1);

        // I1 contract: edges keep source→target orientation even under
        // Reverse traversal. The SQL-level edge is a → b, and the BFS walks
        // backward from b; the returned edge is still (a, b).
        let (src, tgt) = &slice.edges[0];
        assert_eq!(src.task_item.as_deref(), Some("a"), "edge source is a");
        assert_eq!(tgt.task_item.as_deref(), Some("b"), "edge target is b");
    }

    #[test]
    fn query_graph_cycle_terminates_within_depth() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        for id in ["a", "b", "c"] {
            seed_task_row(&db, "tasks", id, id, TaskStatus::Pending, None);
        }
        // Cycle: a → b → c → a
        seed_edge(&db, "tasks", "a", "tasks", Some("b"));
        seed_edge(&db, "tasks", "b", "tasks", Some("c"));
        seed_edge(&db, "tasks", "c", "tasks", Some("a"));

        let root = TaskEdgeRef {
            block: "tasks".into(),
            task_item: Some("a".into()),
        };
        let query = GraphQuery {
            direction: Direction::Forward,
            depth: None,
            max_nodes: None,
        };
        let conn = db.get().unwrap();
        let slice = handle_query_graph(
            &*store,
            &conn,
            "agent-a",
            &root.to_string(),
            &serde_json::to_string(&query).unwrap(),
        )
        .unwrap();

        // BFS with visited-set termination: exactly 3 nodes + 3 edges.
        assert_eq!(slice.nodes.len(), 3);
        assert_eq!(slice.edges.len(), 3);
        assert!(!slice.truncated, "bounded by graph size, not caps");
    }

    #[test]
    fn query_graph_filters_nodes_in_hidden_blocks() {
        // C4 contract: an edge from a visible block to an invisible block
        // (one not owned by the caller's agent under the active scope) must
        // not leak the target's TaskEdgeRef via BFS results. The filter
        // drops nodes + incident edges whose blocks are outside the
        // visible set.
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();

        // agent-a owns "visible" block and can see it.
        seed_task_list(&*store, "agent-a", "visible");
        // agent-b owns "hidden" block. agent-a cannot see it.
        seed_task_list(&*store, "agent-b", "hidden");

        // Seed tasks + an edge crossing from agent-a's block to agent-b's.
        seed_task_row(&db, "visible", "v1", "v1", TaskStatus::Pending, None);
        seed_task_row(&db, "hidden", "h1", "h1", TaskStatus::Pending, None);
        seed_edge(&db, "visible", "v1", "hidden", Some("h1"));

        let root = TaskEdgeRef {
            block: "visible".into(),
            task_item: Some("v1".into()),
        };
        let query = GraphQuery {
            direction: Direction::Forward,
            depth: None,
            max_nodes: None,
        };
        let conn = db.get().unwrap();
        let slice = handle_query_graph(
            &*store,
            &conn,
            "agent-a",
            &root.to_string(),
            &serde_json::to_string(&query).unwrap(),
        )
        .unwrap();

        // Only the root (visible) survives; the hidden node + its edge are
        // dropped post-BFS.
        assert_eq!(
            slice.nodes.len(),
            1,
            "hidden node must be filtered, got nodes: {:?}",
            slice.nodes
        );
        assert_eq!(
            slice.nodes[0].block.as_str(),
            "visible",
            "only visible node remains"
        );
        assert!(
            slice.edges.is_empty(),
            "edge touching hidden node must be dropped, got edges: {:?}",
            slice.edges
        );
    }

    #[test]
    fn query_graph_max_nodes_truncates_large_graph() {
        // Star topology: one root with 100 direct children. Depth 1 reaches all
        // children; max_nodes=50 caps the traversal before it finishes.
        // Using a star (not a chain) avoids interaction with the default
        // depth=16 cap — we want to verify the max_nodes truncation path.
        let store = Arc::new(InMemoryMemoryStore::new());
        let db = open_db();
        seed_task_list(&*store, "agent-a", "tasks");
        seed_task_row(&db, "tasks", "root", "root", TaskStatus::Pending, None);
        for i in 0..100 {
            let id = format!("c{i:03}");
            seed_task_row(&db, "tasks", &id, &id, TaskStatus::Pending, None);
            seed_edge(&db, "tasks", "root", "tasks", Some(&id));
        }

        let root = TaskEdgeRef {
            block: "tasks".into(),
            task_item: Some("root".into()),
        };
        let query = GraphQuery {
            direction: Direction::Forward,
            depth: Some(2),
            max_nodes: Some(50),
        };
        let conn = db.get().unwrap();
        let slice = handle_query_graph(
            &*store,
            &conn,
            "agent-a",
            &root.to_string(),
            &serde_json::to_string(&query).unwrap(),
        )
        .unwrap();

        assert!(
            slice.nodes.len() <= 50,
            "capped at 50, got {}",
            slice.nodes.len()
        );
        assert!(slice.truncated, "max_nodes cap must flag truncation");
    }

    // endregion: list / query-graph tests

    // region: integration-style tests (C2/C3/I4)

    /// C3 / AC5.5 — wrap the in-memory store in a MemoryScope with
    /// `IsolatePolicy::Full`. Seed TaskList blocks under persona, project,
    /// AND a third "rogue" agent. `handle_list_tasks(None, ...)` under Full
    /// must return ONLY project tasks regardless of who the caller is;
    /// persona and rogue blocks are both filtered out.
    ///
    /// This actively proves the Full gate engages (as opposed to merely
    /// passing through the filter): the persona caller's own blocks are
    /// hidden from them, and unrelated agents' blocks are hidden too. A
    /// passthrough implementation of MemoryScope would return all three
    /// blocks and fail the test.
    #[test]
    fn list_tasks_respects_full_isolation_hides_persona_tasklist() {
        use pattern_core::types::memory_types::IsolatePolicy;
        use pattern_memory::scope::{MemoryScope, ScopeBinding};

        let inner = InMemoryMemoryStore::new();

        // Seed tasks under three agents.
        let db = open_db();
        seed_task_list(&inner, "persona", "persona-tasks");
        seed_task_row(
            &db,
            "persona-tasks",
            "p1",
            "persona-only",
            TaskStatus::Pending,
            None,
        );
        seed_task_list(&inner, "project", "project-tasks");
        seed_task_row(
            &db,
            "project-tasks",
            "q1",
            "project-only",
            TaskStatus::Pending,
            None,
        );
        seed_task_list(&inner, "rogue", "rogue-tasks");
        seed_task_row(
            &db,
            "rogue-tasks",
            "r1",
            "rogue-only",
            TaskStatus::Pending,
            None,
        );

        // Wrap in MemoryScope with Full isolation.
        let scope = MemoryScope::new(
            inner,
            ScopeBinding::with_project("persona", "project", IsolatePolicy::Full),
        );
        let conn = db.get().unwrap();

        // Caller is the PERSONA agent. Under Full isolation, MemoryScope
        // overrides list_blocks' agent-id filter to the project's id, so
        // even the persona itself cannot see its own TaskList blocks.
        // This is the key assertion: the Full gate is what hides persona
        // from itself — a passthrough implementation would return the
        // persona's block here.
        let views = handle_list_tasks(&scope, &conn, "persona", None, "{}")
            .expect("list_tasks under Full isolation (persona caller)");
        assert_eq!(
            views.len(),
            1,
            "persona caller under Full sees only project tasks, got {views:?}"
        );
        assert_eq!(views[0].block_ref.block.as_str(), "project-tasks");
        assert_eq!(views[0].subject, "project-only");

        // Sanity: the rogue agent's block is also hidden.
        let blocks_seen: std::collections::HashSet<&str> =
            views.iter().map(|v| v.block_ref.block.as_str()).collect();
        assert!(!blocks_seen.contains("rogue-tasks"));
        assert!(!blocks_seen.contains("persona-tasks"));
    }

    /// C2 / AC4.4 + I4 — handler→subscriber roundtrip for link. Drive
    /// handle_create + handle_link through the actual handler surface, then
    /// run the subscriber reconciler against a real DB, and assert the
    /// resulting `task_edges` row has the expected shape.
    ///
    /// Without this test, a subtle mismatch between the handler's LoroDoc
    /// shape (field names, null vs missing, container vs value) and the
    /// subscriber's extraction logic would pass both suites silently while
    /// breaking the end-to-end flow.
    #[test]
    fn handler_to_subscriber_roundtrip_link_produces_task_edge_row() {
        use pattern_memory::subscriber::task::reconcile_task_list;

        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "tasks", &sample_spec("B")).unwrap();
        handle_link(
            &*store,
            "agent-a",
            &format!("tasks#{a}"),
            &format!("tasks#{b}"),
        )
        .unwrap();

        // Now reconcile the LoroDoc into a fresh DB via the real subscriber
        // path. This is the cross-crate shape contract the review flagged —
        // any field rename on either side would be caught here.
        let db = open_db();
        let mut conn = db.get().unwrap();
        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        {
            let tx = conn.transaction().unwrap();
            reconcile_task_list(&tx, "tasks", sdoc.inner())
                .expect("reconcile must accept handler-produced LoroDoc shape");
            tx.commit().unwrap();
        }

        // `tasks` table: two rows (A and B), both with block_handle=tasks.
        let tasks_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE block_handle = 'tasks'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tasks_count, 2, "two tasks reconciled");

        // `task_edges` table: exactly one row, source=A, target=B.
        let mut stmt = conn
            .prepare(
                "SELECT source_block, source_item, target_block, target_item \
                 FROM task_edges WHERE source_block = 'tasks'",
            )
            .unwrap();
        let edges: Vec<(String, String, String, Option<String>)> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(edges.len(), 1, "one edge row after link+reconcile");
        let (sb, si, tb, ti) = &edges[0];
        assert_eq!(sb, "tasks");
        assert_eq!(si, a.as_str());
        assert_eq!(tb, "tasks");
        assert_eq!(ti.as_deref(), Some(b.as_str()));
    }

    /// C2 / AC4.5 — handler→subscriber roundtrip for unlink. After linking
    /// and reconciling, unlinking + re-reconciling deletes the edge row.
    #[test]
    fn handler_to_subscriber_roundtrip_unlink_removes_edge_row() {
        use pattern_memory::subscriber::task::reconcile_task_list;

        let store = Arc::new(InMemoryMemoryStore::new());
        seed_task_list(&*store, "agent-a", "tasks");
        let a = handle_create(&*store, "agent-a", "tasks", &sample_spec("A")).unwrap();
        let b = handle_create(&*store, "agent-a", "tasks", &sample_spec("B")).unwrap();
        let a_ref = format!("tasks#{a}");
        let b_ref = format!("tasks#{b}");

        handle_link(&*store, "agent-a", &a_ref, &b_ref).unwrap();

        // First reconcile: edge appears.
        let db = open_db();
        let mut conn = db.get().unwrap();
        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        {
            let tx = conn.transaction().unwrap();
            reconcile_task_list(&tx, "tasks", sdoc.inner()).unwrap();
            tx.commit().unwrap();
        }
        let pre: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pre, 1);

        // Unlink + re-reconcile: edge gone.
        handle_unlink(&*store, "agent-a", &a_ref, &b_ref).unwrap();
        let sdoc = store.get_block("agent-a", "tasks").unwrap().unwrap();
        {
            let tx = conn.transaction().unwrap();
            reconcile_task_list(&tx, "tasks", sdoc.inner()).unwrap();
            tx.commit().unwrap();
        }
        let post: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(post, 0, "edge row removed after unlink+reconcile");
    }

    // endregion: integration-style tests (C2/C3/I4)
}
