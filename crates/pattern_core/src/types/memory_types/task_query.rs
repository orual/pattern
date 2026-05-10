//! Query and mutation types for TaskList memory blocks.
//!
//! These types form the shared vocabulary consumed by:
//! - Phase 2's database query layer (`list_tasks_filtered`,
//!   `query_task_graph_bfs` in `pattern_db::queries::task`).
//! - Phase 3's SDK handlers (create/update/delete/query tool dispatch).
//!
//! Landing them in Phase 2 keeps the query layer self-contained — no forward
//! references to Phase 3 are required, and Phase 3 only adds small
//! handler-local types and helper methods on top.
//!
//! ## `TaskPatch` field conventions
//!
//! Fields that can be explicitly cleared (set back to `None` after having a
//! value) use `Option<Option<T>>`:
//! - Outer `None` → absent from JSON → "leave this field unchanged."
//! - `Some(None)` → JSON `null` → "clear this field."
//! - `Some(Some(v))` → JSON value → "set this field to `v`."
//!
//! Standard serde does NOT correctly distinguish absent vs. `null` for
//! `Option<Option<T>>`: both map to `None` by default. The
//! [`double_option`] module provides `serialize`/`deserialize` helpers that
//! are wired via `#[serde(deserialize_with = "double_option::deserialize")]` on
//! each clearable field. Serialisation uses `skip_serializing_if =
//! "Option::is_none"` so absent stays absent, while `Some(None)` writes `null`.

use serde::{Deserialize, Serialize};

use crate::types::{
    block::BlockHandle,
    ids::AgentId,
    memory_types::{TaskStatus, task::TaskEdgeRef},
};

// region: double_option serde helpers

/// Serde helpers for the three-state `Option<Option<T>>` patch-field pattern.
///
/// Standard serde cannot distinguish between "field absent" and "field is
/// `null`" for `Option<T>` — both produce `None`. For `TaskPatch`'s clearable
/// fields we need all three states:
///
/// | JSON | Rust |
/// |------|------|
/// | absent | `None` (do not touch) |
/// | `null` | `Some(None)` (clear) |
/// | `"value"` | `Some(Some("value"))` (set) |
///
/// Wire this with:
/// ```text
/// #[serde(default, skip_serializing_if = "Option::is_none",
///         deserialize_with = "double_option::deserialize")]
/// pub field: Option<Option<T>>,
/// ```
///
/// - `#[serde(default)]` maps absent fields to `None`.
/// - `skip_serializing_if = "Option::is_none"` suppresses absent fields on
///   output, leaving `Some(None)` to serialize as `null` via the inner `Option`.
/// - `deserialize_with` calls our custom deserialiser, which wraps whatever the
///   inner `Option<T>` deserializer produces in `Some(…)` — so `null` →
///   `Some(None)` and `"value"` → `Some(Some("value"))`.
mod double_option {
    use serde::{Deserialize, Deserializer};

    /// Deserialise `Option<Option<T>>` so that:
    /// - Absent fields (handled by `#[serde(default)]`) → `None`.
    /// - JSON `null` → `Some(None)`.
    /// - JSON `"value"` → `Some(Some("value"))`.
    pub fn deserialize<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
    where
        T: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}

// endregion: double_option serde helpers

// region: TaskSpec

/// Parameters for creating a new task item within a TaskList block.
///
/// Edges are not set at creation time; they are managed separately via the
/// edge-mutation API so the graph remains consistent across concurrent writes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Brief imperative description of what needs to be done.
    pub subject: String,
    /// Extended markdown body with context, details, and notes.
    pub description: String,
    /// Active/working form of the subject (what is currently happening).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// Initial lifecycle state; defaults to `Pending` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    /// Agent responsible for this item; inherits the block's `default_owner`
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AgentId>,
    /// Freeform JSON metadata (tags, priority, estimates, etc.).
    pub metadata: serde_json::Value,
}

// endregion: TaskSpec

// region: TaskCreateRequest

/// Wire format for a `Tasks.create` call: optional block-level metadata
/// (consulted only when this call auto-creates the target block) plus the
/// list of task items to add.
///
/// A single call may seed a fresh TaskList with N items in one operation.
/// If the target block already exists, `block_description` is ignored and
/// the block's existing description is preserved; the items are appended
/// to the existing list.
///
/// `items` must be non-empty — calling `Create` with zero items is a
/// programming error and surfaces as `TaskHandlerError::EmptyCreate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskCreateRequest {
    /// Optional human-readable description for the *block* itself.
    /// Applied only when this Create call auto-creates the underlying
    /// TaskList block. Use this to label the list as a whole (\"auth
    /// refactor tasks\", \"v3 release\") — describing individual items
    /// is the job of each `TaskSpec.subject`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_description: Option<String>,
    /// Task items to add to the block, in order. Each item gets its own
    /// minted `TaskItemId`; ids are returned in the same order as the
    /// input items.
    pub items: Vec<TaskSpec>,
}

// endregion: TaskCreateRequest

// region: TaskPatch

/// Partial update to an existing task item.
///
/// Every field is optional — absent means "leave unchanged." Fields that can
/// be cleared back to `None` (i.e., `owner` and `active_form`) use
/// `Option<Option<T>>`:
/// - Outer `None` (absent in JSON) → do not touch.
/// - `Some(None)` (JSON `null`) → clear the field.
/// - `Some(Some(v))` (JSON value) → set to `v`.
///
/// See the module-level [`double_option`] documentation for the serde
/// mechanics. `status` and the plain-string fields use a single `Option<T>`
/// because they cannot meaningfully be "cleared" to an absent state — `status`
/// always has a value after creation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskPatch {
    /// Replaces the task subject if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Replaces the task description if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Sets or clears the active_form field.
    ///
    /// `None` → do not modify. `Some(None)` → clear. `Some(Some(v))` → set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option::deserialize"
    )]
    pub active_form: Option<Option<String>>,
    /// Replaces the task status if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    /// Sets or clears the owner field.
    ///
    /// `None` → do not modify. `Some(None)` → clear. `Some(Some(v))` → set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "double_option::deserialize"
    )]
    pub owner: Option<Option<AgentId>>,
    /// Replaces the metadata blob if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

// endregion: TaskPatch

// region: TaskFilter

/// Criteria for filtering task items returned by list queries.
///
/// All fields are optional; absent means "no constraint on this dimension."
/// Multiple constraints compose as AND.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskFilter {
    /// Restrict to items whose status is in this set.
    ///
    /// `None` or empty vec → no status filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<TaskStatus>>,
    /// Restrict to items owned by this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AgentId>,
    /// If `Some(true)`, return only items that have at least one incoming
    /// blocker edge (items that are currently blocked). If `Some(false)`,
    /// return only unblocked items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_blockers: Option<bool>,
    /// FTS5 keyword query string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// Restrict to items belonging to one of these block handles.
    ///
    /// - `None` → no block constraint (all blocks included).
    /// - `Some(vec![h])` → only items from block `h`.
    /// - `Some(many)` → items from any block in the set.
    ///
    /// `Some(vec![])` (empty vec) is treated as "no results" — not "all results."
    /// Callers should pass `None` when no block scoping is desired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<BlockHandle>>,
}

// endregion: TaskFilter

// region: TaskView

/// A projected view of a single task item for UI and agent consumption.
///
/// Carries only the fields needed for rendering task lists and dependency
/// summaries — callers that need the full record should load the TaskItem
/// via the CRDT layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskView {
    /// Typed reference to the task item (block + item id pair).
    pub block_ref: TaskEdgeRef,
    /// Brief imperative description.
    pub subject: String,
    /// Current lifecycle state.
    pub status: TaskStatus,
    /// Responsible agent, if assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<AgentId>,
    /// Number of items that must complete before this item can proceed.
    pub blocker_count: usize,
    /// Number of items this item is blocking.
    pub blocks_count: usize,
}

// endregion: TaskView

// region: Direction

/// Direction for graph traversal in [`GraphQuery`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// Follow outgoing "blocks" edges (from blocker to blocked item).
    #[default]
    Forward,
    /// Follow incoming "blocks" edges (find what is blocking this item).
    Reverse,
    /// Follow edges in both directions.
    Both,
}

// endregion: Direction

// region: GraphQuery

/// Parameters for a graph BFS traversal rooted at a [`TaskEdgeRef`].
///
/// Depth and node limits are expressed as `Option<u32>` — callers that omit
/// them receive the implementation's built-in safety caps (depth=16,
/// max_nodes=1000) applied at query time, not at construction time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphQuery {
    /// Which direction to traverse edges.
    pub direction: Direction,
    /// Maximum BFS depth from the root node.
    ///
    /// `None` → use the default cap of 16. Set explicitly for tighter or
    /// looser bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Maximum number of nodes to visit before truncating.
    ///
    /// `None` → use the default cap of 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<u32>,
}

impl Default for GraphQuery {
    fn default() -> Self {
        Self {
            direction: Direction::Forward,
            depth: None,
            max_nodes: None,
        }
    }
}

// endregion: GraphQuery

// region: GraphSlice

/// A bounded subgraph returned by `query_task_graph_bfs`.
///
/// Nodes and edges are expressed as [`TaskEdgeRef`] values so callers can
/// correlate results back to the CRDT layer without additional lookups.
///
/// When `truncated` is `true`, the walk hit the `max_nodes` or `depth` limit
/// before exhausting all reachable nodes; the slice is a prefix of the full
/// reachable graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphSlice {
    /// All nodes visited (including the root).
    pub nodes: Vec<TaskEdgeRef>,
    /// All edges traversed, as `(source, target)` pairs.
    pub edges: Vec<(TaskEdgeRef, TaskEdgeRef)>,
    /// If `true`, the traversal was cut short by a depth or node cap.
    pub truncated: bool,
}

// endregion: GraphSlice

// region: tests

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::types::ids::TaskItemId;

    // region: TaskSpec tests

    #[test]
    fn task_spec_minimal_round_trips() {
        let spec = TaskSpec {
            subject: "fix the build".to_owned(),
            description: String::new(),
            active_form: None,
            status: None,
            owner: None,
            metadata: serde_json::Value::Null,
        };

        let json = serde_json::to_string(&spec).unwrap();
        let recovered: TaskSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, recovered);

        // Optional fields must be absent in JSON when None.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("active_form").is_none(),
            "active_form should be absent"
        );
        assert!(v.get("status").is_none(), "status should be absent");
        assert!(v.get("owner").is_none(), "owner should be absent");
    }

    #[test]
    fn task_spec_full_round_trips() {
        let spec = TaskSpec {
            subject: "land the feature".to_owned(),
            description: "Full description here.".to_owned(),
            active_form: Some("writing tests".to_owned()),
            status: Some(TaskStatus::InProgress),
            owner: Some(SmolStr::new("agent-orual")),
            metadata: serde_json::json!({"priority": "high"}),
        };

        let json = serde_json::to_string(&spec).unwrap();
        let recovered: TaskSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, recovered);
    }

    // endregion: TaskSpec tests

    // region: TaskPatch tests

    /// Verifies the `Option<Option<T>>` double-option pattern for `owner`:
    /// - `None` (outer) → field absent in JSON → "do not touch."
    /// - `Some(None)` → JSON `null` → "clear the field."
    /// - `Some(Some(v))` → JSON value → "set to this value."
    #[test]
    fn task_patch_owner_none_is_absent_in_json() {
        let patch = TaskPatch {
            subject: None,
            description: None,
            active_form: None,
            status: None,
            owner: None,
            metadata: None,
        };

        let json = serde_json::to_string(&patch).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // All fields absent — an empty patch object.
        assert!(
            v.get("owner").is_none(),
            "owner=None must be absent: {json}"
        );

        // Round-trip: absent field → outer None.
        let recovered: TaskPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.owner, None);
    }

    #[test]
    fn task_patch_owner_some_none_serializes_as_null_and_round_trips() {
        let patch = TaskPatch {
            subject: None,
            description: None,
            active_form: None,
            status: None,
            owner: Some(None), // "clear the owner"
            metadata: None,
        };

        let json = serde_json::to_string(&patch).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // `Some(None)` must appear as JSON `null`, not be absent.
        assert!(
            v.get("owner").is_some(),
            "owner=Some(None) must be present in JSON: {json}"
        );
        assert!(
            v["owner"].is_null(),
            "owner=Some(None) must serialize as null: {json}"
        );

        // Round-trip: JSON null → Some(None).
        let recovered: TaskPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.owner, Some(None));
    }

    #[test]
    fn task_patch_owner_some_some_round_trips() {
        let patch = TaskPatch {
            subject: None,
            description: None,
            active_form: None,
            status: None,
            owner: Some(Some(SmolStr::new("agent-new"))),
            metadata: None,
        };

        let json = serde_json::to_string(&patch).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // `Some(Some(v))` must appear as the value.
        assert_eq!(v["owner"].as_str(), Some("agent-new"));

        let recovered: TaskPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.owner, Some(Some(SmolStr::new("agent-new"))));
    }

    #[test]
    fn task_patch_active_form_double_option_round_trips() {
        // Same double-option semantics as owner, for active_form.
        let clear = TaskPatch {
            subject: None,
            description: None,
            active_form: Some(None), // "clear active_form"
            status: None,
            owner: None,
            metadata: None,
        };
        let json = serde_json::to_string(&clear).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v["active_form"].is_null(),
            "Some(None) must be null: {json}"
        );
        let recovered: TaskPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(recovered.active_form, Some(None));

        // Set to a new value.
        let set = TaskPatch {
            subject: None,
            description: None,
            active_form: Some(Some("reviewing PR".to_owned())),
            status: None,
            owner: None,
            metadata: None,
        };
        let json2 = serde_json::to_string(&set).unwrap();
        let recovered2: TaskPatch = serde_json::from_str(&json2).unwrap();
        assert_eq!(
            recovered2.active_form,
            Some(Some("reviewing PR".to_owned()))
        );
    }

    #[test]
    fn task_patch_full_populated_round_trips() {
        let patch = TaskPatch {
            subject: Some("updated subject".to_owned()),
            description: Some("updated description".to_owned()),
            active_form: Some(Some("active now".to_owned())),
            status: Some(TaskStatus::Blocked),
            owner: Some(Some(SmolStr::new("agent-x"))),
            metadata: Some(serde_json::json!({"key": "val"})),
        };

        let json = serde_json::to_string(&patch).unwrap();
        let recovered: TaskPatch = serde_json::from_str(&json).unwrap();
        assert_eq!(patch, recovered);
    }

    // endregion: TaskPatch tests

    // region: TaskFilter tests

    #[test]
    fn task_filter_default_is_all_none() {
        let filter = TaskFilter::default();
        assert!(filter.status.is_none());
        assert!(filter.owner.is_none());
        assert!(filter.has_blockers.is_none());
        assert!(filter.keyword.is_none());
        assert!(filter.blocks.is_none());
    }

    #[test]
    fn task_filter_populated_round_trips() {
        let filter = TaskFilter {
            status: Some(vec![TaskStatus::Pending, TaskStatus::InProgress]),
            owner: Some(SmolStr::new("agent-z")),
            has_blockers: Some(true),
            keyword: Some("auth".to_owned()),
            blocks: None,
        };

        let json = serde_json::to_string(&filter).unwrap();
        let recovered: TaskFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, recovered);
    }

    #[test]
    fn task_filter_empty_json_deserializes_to_all_none() {
        let filter: TaskFilter = serde_json::from_str("{}").unwrap();
        assert_eq!(filter, TaskFilter::default());
    }

    #[test]
    fn task_filter_blocks_field_round_trips() {
        // Some(vec![h]) — single block constraint.
        let filter = TaskFilter {
            status: None,
            owner: None,
            has_blockers: None,
            keyword: None,
            blocks: Some(vec![SmolStr::new("sprint-block"), SmolStr::new("backlog")]),
        };

        let json = serde_json::to_string(&filter).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        // `blocks` must be present and be an array with 2 elements.
        assert!(v.get("blocks").is_some(), "blocks must be present in JSON");
        assert_eq!(
            v["blocks"].as_array().map(|a| a.len()),
            Some(2),
            "blocks array must contain 2 elements"
        );

        let recovered: TaskFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, recovered);
    }

    #[test]
    fn task_filter_blocks_none_absent_in_json() {
        let filter = TaskFilter::default();
        let json = serde_json::to_string(&filter).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(
            v.get("blocks").is_none(),
            "blocks=None must be absent in JSON: {json}"
        );
    }

    #[test]
    fn task_filter_blocks_empty_vec_round_trips() {
        // Some(vec![]) — explicit "no results" sentinel.
        let filter = TaskFilter {
            blocks: Some(vec![]),
            ..Default::default()
        };
        let json = serde_json::to_string(&filter).unwrap();
        let recovered: TaskFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, recovered);
        assert_eq!(recovered.blocks, Some(vec![]));
    }

    // endregion: TaskFilter tests

    // region: TaskView tests

    #[test]
    fn task_view_round_trips() {
        let view = TaskView {
            block_ref: TaskEdgeRef {
                block: SmolStr::new("sprint-block"),
                task_item: Some(SmolStr::new("item-001")),
            },
            subject: "ship the release".to_owned(),
            status: TaskStatus::InProgress,
            owner: Some(SmolStr::new("agent-orual")),
            blocker_count: 2,
            blocks_count: 5,
        };

        let json = serde_json::to_string(&view).unwrap();
        let recovered: TaskView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, recovered);
    }

    #[test]
    fn task_view_no_owner_round_trips() {
        let view = TaskView {
            block_ref: TaskEdgeRef {
                block: SmolStr::new("backlog"),
                task_item: None,
            },
            subject: "triage issues".to_owned(),
            status: TaskStatus::Pending,
            owner: None,
            blocker_count: 0,
            blocks_count: 0,
        };

        let json = serde_json::to_string(&view).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v.get("owner").is_none(), "owner=None must be absent");

        let recovered: TaskView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, recovered);
    }

    // endregion: TaskView tests

    // region: Direction tests

    #[test]
    fn direction_forward_round_trips_as_kebab() {
        let dir = Direction::Forward;
        let json = serde_json::to_string(&dir).unwrap();
        assert_eq!(json, r#""forward""#);
        assert_eq!(serde_json::from_str::<Direction>(&json).unwrap(), dir);
    }

    #[test]
    fn direction_reverse_round_trips_as_kebab() {
        let dir = Direction::Reverse;
        let json = serde_json::to_string(&dir).unwrap();
        assert_eq!(json, r#""reverse""#);
        assert_eq!(serde_json::from_str::<Direction>(&json).unwrap(), dir);
    }

    #[test]
    fn direction_both_round_trips_as_kebab() {
        let dir = Direction::Both;
        let json = serde_json::to_string(&dir).unwrap();
        assert_eq!(json, r#""both""#);
        assert_eq!(serde_json::from_str::<Direction>(&json).unwrap(), dir);
    }

    // endregion: Direction tests

    // region: GraphQuery tests

    #[test]
    fn graph_query_default_is_forward_with_no_caps() {
        let q = GraphQuery::default();
        assert_eq!(q.direction, Direction::Forward);
        assert!(q.depth.is_none());
        assert!(q.max_nodes.is_none());
    }

    #[test]
    fn graph_query_forward_round_trips() {
        let q = GraphQuery {
            direction: Direction::Forward,
            depth: Some(8),
            max_nodes: Some(500),
        };
        let json = serde_json::to_string(&q).unwrap();
        let recovered: GraphQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(q, recovered);
    }

    #[test]
    fn graph_query_reverse_round_trips() {
        let q = GraphQuery {
            direction: Direction::Reverse,
            depth: None,
            max_nodes: None,
        };
        let json = serde_json::to_string(&q).unwrap();
        let recovered: GraphQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(q, recovered);
    }

    #[test]
    fn graph_query_both_round_trips() {
        let q = GraphQuery {
            direction: Direction::Both,
            depth: Some(4),
            max_nodes: Some(100),
        };
        let json = serde_json::to_string(&q).unwrap();
        let recovered: GraphQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(q, recovered);
    }

    // endregion: GraphQuery tests

    // region: GraphSlice tests

    #[test]
    fn graph_slice_empty_round_trips() {
        let root = TaskEdgeRef {
            block: SmolStr::new("root-block"),
            task_item: Some(SmolStr::new("root-item")),
        };
        let slice = GraphSlice {
            nodes: vec![root],
            edges: vec![],
            truncated: false,
        };

        let json = serde_json::to_string(&slice).unwrap();
        let recovered: GraphSlice = serde_json::from_str(&json).unwrap();
        assert_eq!(slice, recovered);
    }

    #[test]
    fn graph_slice_with_edges_round_trips() {
        let a = TaskEdgeRef {
            block: SmolStr::new("block-a"),
            task_item: Some(SmolStr::new("item-a")),
        };
        let b = TaskEdgeRef {
            block: SmolStr::new("block-b"),
            task_item: Some(SmolStr::new("item-b")),
        };
        let c = TaskEdgeRef {
            block: SmolStr::new("block-c"),
            task_item: None,
        };

        let slice = GraphSlice {
            nodes: vec![a.clone(), b.clone(), c.clone()],
            edges: vec![(a.clone(), b.clone()), (b.clone(), c.clone())],
            truncated: false,
        };

        let json = serde_json::to_string(&slice).unwrap();
        let recovered: GraphSlice = serde_json::from_str(&json).unwrap();
        assert_eq!(slice, recovered);
    }

    #[test]
    fn graph_slice_truncated_flag_round_trips() {
        let root = TaskEdgeRef {
            block: SmolStr::new("root"),
            task_item: None,
        };
        let slice = GraphSlice {
            nodes: vec![root],
            edges: vec![],
            truncated: true,
        };

        let json = serde_json::to_string(&slice).unwrap();
        let recovered: GraphSlice = serde_json::from_str(&json).unwrap();
        assert_eq!(slice, recovered);
        assert!(recovered.truncated);
    }

    // endregion: GraphSlice tests

    // region: TaskItemId alias test

    /// Confirms `TaskItemId` is a `SmolStr` alias usable in task_query types.
    #[test]
    fn task_item_id_is_smol_str_alias() {
        let id: TaskItemId = SmolStr::new("01JDT000SNOWFLAKE0001");
        assert_eq!(id.as_str(), "01JDT000SNOWFLAKE0001");
    }

    // endregion: TaskItemId alias test
}

// endregion: tests
