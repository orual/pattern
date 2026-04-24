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
    BlockSchema, MemoryError, TaskEdgeRef, TaskStatus, task_query::TaskPatch, task_query::TaskSpec,
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
            TasksReq::Link(_, _) => Err(EffectError::Handler(
                "Pattern.Tasks::Link — Task 8 implements".into(),
            )),
            TasksReq::Unlink(_, _) => Err(EffectError::Handler(
                "Pattern.Tasks::Unlink — Task 8 implements".into(),
            )),
            TasksReq::List(_, _) => Err(EffectError::Handler(
                "Pattern.Tasks::List — Task 9 implements".into(),
            )),
            TasksReq::QueryGraph(_, _) => Err(EffectError::Handler(
                "Pattern.Tasks::QueryGraph — Task 9 implements".into(),
            )),
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
}
