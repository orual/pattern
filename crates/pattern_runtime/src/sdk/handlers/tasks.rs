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

use pattern_core::memory::StructuredDocument;
use pattern_core::traits::MemoryStore;
use pattern_core::types::ids::{TaskItemId, new_snowflake_id};
use pattern_core::types::memory_types::{
    BlockFilter, BlockSchema, MemoryError, TaskEdgeRef, TaskStatus,
    task_query::{GraphQuery, GraphSlice, TaskFilter, TaskPatch, TaskSpec, TaskView},
};

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
                "type TaskItemId = Text",
                "type TaskEdgeRef = Text",
                "type TaskSpec = Text",
                "type TaskPatch = Text",
                "type TaskStatus = Text",
                "type TaskFilter = Text",
                "type TaskView = Text",
                "type GraphQuery = Text",
                "type GraphSlice = Text",
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
        let store = cx.user().memory_store();

        match req {
            TasksReq::Create(block, spec_json) => {
                let id = handle_create(&*store, &agent_id, &block, &spec_json)?;
                cx.respond(id.to_string())
            }
            TasksReq::Update(edge_ref, patch_json) => {
                handle_update(&*store, &agent_id, &edge_ref, &patch_json)?;
                cx.respond(())
            }
            TasksReq::Transition(edge_ref, status_json) => {
                handle_transition(&*store, &agent_id, &edge_ref, &status_json)?;
                cx.respond(())
            }
            TasksReq::AddComment(edge_ref, text) => {
                handle_add_comment(&*store, &agent_id, &edge_ref, &text)?;
                cx.respond(())
            }
            TasksReq::Link(source_ref, target_ref) => {
                handle_link(&*store, &agent_id, &source_ref, &target_ref)?;
                cx.respond(())
            }
            TasksReq::Unlink(source_ref, target_ref) => {
                handle_unlink(&*store, &agent_id, &source_ref, &target_ref)?;
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
pub(crate) enum TaskHandlerError {
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
fn task_status_kebab(status: TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .expect("TaskStatus serde representation is a string enum")
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
/// not a Map (shouldn't happen for a well-formed TaskList).
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

/// Replace the item at `index` with a new map value. Uses delete-then-insert
/// because `LoroMovableList::set` with a `LoroValue::Map` has subtle semantics
/// around container ids — delete+insert produces a fresh value unambiguously.
fn replace_item_at(
    doc: &LoroDoc,
    index: usize,
    new_item: serde_json::Map<String, JsonValue>,
) -> Result<(), TaskHandlerError> {
    let list = doc.get_movable_list("items");
    list.delete(index, 1)
        .map_err(|e| TaskHandlerError::Loro(format!("delete at {index}: {e}")))?;
    let loro_val = json_to_loro(&JsonValue::Object(new_item));
    list.insert(index, loro_val)
        .map_err(|e| TaskHandlerError::Loro(format!("insert at {index}: {e}")))?;
    Ok(())
}

// endregion: helpers

// region: handlers

/// Create a new task item in the given block. Returns the minted item id.
pub(crate) fn handle_create(
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

    let mut item = serde_json::Map::new();
    item.insert("id".into(), JsonValue::String(item_id.to_string()));
    item.insert("subject".into(), JsonValue::String(spec.subject));
    if !spec.description.is_empty() {
        item.insert("description".into(), JsonValue::String(spec.description));
    }
    let status = spec.status.unwrap_or(TaskStatus::Pending);
    item.insert(
        "status".into(),
        JsonValue::String(task_status_kebab(status)),
    );
    if let Some(owner) = spec.owner {
        item.insert("owner".into(), JsonValue::String(owner.to_string()));
    }
    if let Some(active) = spec.active_form {
        item.insert("active_form".into(), JsonValue::String(active));
    }
    item.insert("created_at".into(), JsonValue::String(now.to_string()));
    item.insert("updated_at".into(), JsonValue::String(now.to_string()));
    if !spec.metadata.is_null() {
        item.insert("metadata".into(), spec.metadata);
    }
    item.insert("comments".into(), JsonValue::Array(vec![]));
    item.insert("blocks".into(), JsonValue::Array(vec![]));

    let doc = sdoc.inner();
    let list = doc.get_movable_list("items");
    let loro_val = json_to_loro(&JsonValue::Object(item));
    list.push(loro_val)
        .map_err(|e| TaskHandlerError::Loro(format!("push: {e}")))?;
    doc.commit();

    store.mark_dirty(agent_id, block);
    store
        .persist_block(agent_id, block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(item_id)
}

/// Apply a partial patch to an existing task item.
pub(crate) fn handle_update(
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

    let mut item = read_item_as_json(doc, index).ok_or_else(|| {
        // Should not happen — find_item_index just returned Some.
        TaskHandlerError::Loro(format!("item at index {index} not a map"))
    })?;
    apply_patch(&mut item, patch);
    item.insert(
        "updated_at".into(),
        JsonValue::String(jiff::Timestamp::now().to_string()),
    );

    replace_item_at(doc, index, item)?;
    doc.commit();

    store.mark_dirty(agent_id, &block);
    store
        .persist_block(agent_id, &block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Transition a task's status, with optional `completed_at` stamping when
/// moving to `Completed`.
pub(crate) fn handle_transition(
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

    let mut item = read_item_as_json(doc, index)
        .ok_or_else(|| TaskHandlerError::Loro(format!("item at index {index} not a map")))?;
    let now = jiff::Timestamp::now();
    item.insert(
        "status".into(),
        JsonValue::String(task_status_kebab(status)),
    );
    item.insert("updated_at".into(), JsonValue::String(now.to_string()));
    if status == TaskStatus::Completed {
        item.insert("completed_at".into(), JsonValue::String(now.to_string()));
    }

    replace_item_at(doc, index, item)?;
    doc.commit();

    store.mark_dirty(agent_id, &block);
    store
        .persist_block(agent_id, &block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Append a comment to a task. The comment's `author` is the calling agent and
/// `timestamp` is captured at handler time.
pub(crate) fn handle_add_comment(
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

    let mut item = read_item_as_json(doc, index)
        .ok_or_else(|| TaskHandlerError::Loro(format!("item at index {index} not a map")))?;
    let now = jiff::Timestamp::now();
    let comment = serde_json::json!({
        "author": agent_id,
        "timestamp": now.to_string(),
        "text": text,
    });
    match item.get_mut("comments") {
        Some(JsonValue::Array(arr)) => arr.push(comment),
        _ => {
            item.insert("comments".into(), JsonValue::Array(vec![comment]));
        }
    }
    item.insert("updated_at".into(), JsonValue::String(now.to_string()));

    replace_item_at(doc, index, item)?;
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
pub(crate) fn handle_link(
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

    let mut item = read_item_as_json(doc, index)
        .ok_or_else(|| TaskHandlerError::Loro(format!("item at index {index} not a map")))?;

    // Grab or create the `blocks` array.
    let blocks_arr = match item.entry("blocks") {
        serde_json::map::Entry::Occupied(mut e) => {
            if !e.get().is_array() {
                e.insert(JsonValue::Array(vec![]));
            }
            e.into_mut().as_array_mut().expect("just inserted array")
        }
        serde_json::map::Entry::Vacant(e) => e
            .insert(JsonValue::Array(vec![]))
            .as_array_mut()
            .expect("just inserted array"),
    };

    // Dedup: skip if an identical edge already exists.
    if edge_matches_any(blocks_arr, &tgt_block, tgt_item.as_deref()) {
        return Ok(());
    }
    blocks_arr.push(build_edge_value(&tgt_block, tgt_item.as_deref()));

    item.insert(
        "updated_at".into(),
        JsonValue::String(jiff::Timestamp::now().to_string()),
    );

    replace_item_at(doc, index, item)?;
    doc.commit();

    store.mark_dirty(agent_id, &src_block);
    store
        .persist_block(agent_id, &src_block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Remove a directed edge from the source item's `blocks` list. If no matching
/// edge exists, this is a silent no-op (no LoroDoc mutation, no dirty mark).
pub(crate) fn handle_unlink(
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

    let mut item = read_item_as_json(doc, index)
        .ok_or_else(|| TaskHandlerError::Loro(format!("item at index {index} not a map")))?;

    let removed_any = match item.get_mut("blocks") {
        Some(JsonValue::Array(arr)) => {
            let before = arr.len();
            arr.retain(|e| !edge_matches(e, &tgt_block, tgt_item.as_deref()));
            before != arr.len()
        }
        _ => false,
    };

    if !removed_any {
        return Ok(());
    }

    item.insert(
        "updated_at".into(),
        JsonValue::String(jiff::Timestamp::now().to_string()),
    );

    replace_item_at(doc, index, item)?;
    doc.commit();

    store.mark_dirty(agent_id, &src_block);
    store
        .persist_block(agent_id, &src_block)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))?;

    Ok(())
}

/// Whether an edge JSON value matches `(target_block, target_item)`. Edges are
/// shaped as `{ "block": String, "task_item": String | Null }`.
fn edge_matches(edge: &JsonValue, block: &str, item: Option<&str>) -> bool {
    let e_block = edge.get("block").and_then(|v| v.as_str());
    if e_block != Some(block) {
        return false;
    }
    let e_item = edge.get("task_item").and_then(|v| v.as_str());
    e_item == item
}

/// Whether any edge in `edges` matches `(target_block, target_item)`.
fn edge_matches_any(edges: &[JsonValue], block: &str, item: Option<&str>) -> bool {
    edges.iter().any(|e| edge_matches(e, block, item))
}

/// Construct a new edge JSON value pointing at `(target_block, target_item)`.
fn build_edge_value(block: &str, item: Option<&str>) -> JsonValue {
    let mut edge = serde_json::Map::new();
    edge.insert("block".into(), JsonValue::String(block.to_string()));
    edge.insert(
        "task_item".into(),
        item.map(|s| JsonValue::String(s.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(edge)
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
pub(crate) fn handle_list_tasks(
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
/// direction, depth, and max-nodes caps. Scope-checks the root's block via
/// [`fetch_task_list`] so agents can't snoop into blocks they don't own.
pub(crate) fn handle_query_graph(
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

    pattern_db::queries::query_task_graph_bfs(conn, &root, &query)
        .map_err(|e| TaskHandlerError::Store(e.to_string()))
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

/// Apply a `TaskPatch` to a mutable JSON map representing a task item.
///
/// Double-option fields (`owner`, `active_form`):
/// - Outer `None` → leave the field untouched.
/// - `Some(None)` → remove the field entirely (clear).
/// - `Some(Some(v))` → set to the new value.
///
/// Plain-option fields (`subject`, `description`, `status`, `metadata`):
/// - `None` → untouched.
/// - `Some(v)` → set.
fn apply_patch(item: &mut serde_json::Map<String, JsonValue>, patch: TaskPatch) {
    if let Some(subject) = patch.subject {
        item.insert("subject".into(), JsonValue::String(subject));
    }
    if let Some(description) = patch.description {
        item.insert("description".into(), JsonValue::String(description));
    }
    if let Some(status) = patch.status {
        item.insert(
            "status".into(),
            JsonValue::String(task_status_kebab(status)),
        );
    }
    if let Some(metadata) = patch.metadata {
        item.insert("metadata".into(), metadata);
    }
    if let Some(owner) = patch.owner {
        match owner {
            None => {
                item.remove("owner");
            }
            Some(id) => {
                item.insert("owner".into(), JsonValue::String(id.to_string()));
            }
        }
    }
    if let Some(active_form) = patch.active_form {
        match active_form {
            None => {
                item.remove("active_form");
            }
            Some(s) => {
                item.insert("active_form".into(), JsonValue::String(s));
            }
        }
    }
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
}
