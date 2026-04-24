//! Handler for `Pattern.Tasks` — task-graph operations (create, update, link, query).
//!
//! The handler wires eight methods: create_task, update_task, transition_status,
//! link, unlink, list_tasks, query_graph, and add_comment. Implementation details
//! are filled in during Phase 3 Tasks 7–9.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use tidepool_effect::{EffectContext, EffectError, EffectHandler};
use tidepool_eval::Value;

use crate::sdk::describe::{DescribeEffect, EffectDecl};
use crate::sdk::requests::tasks::TasksReq;
use crate::session::SessionContext;

/// Handler position in the canonical [`crate::sdk::bundle::SdkBundle`]
/// HList. Tasks handler will be tag 14 (after DiagnosticsHandler at tag 13).
const TASKS_HANDLER_TAG: u32 = 14;

/// Handler for `Pattern.Tasks`.
///
/// Holds an Arc to the MemoryStore for CRDT-layer access (LoroDoc mutations).
/// DB queries go through cx.user().db().get() per-call to minimize lifetime chaining.
#[derive(Clone)]
pub struct TasksHandler {
    store: Arc<dyn MemoryStore>,
}

impl std::fmt::Debug for TasksHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TasksHandler").finish_non_exhaustive()
    }
}

impl TasksHandler {
    /// Construct a handler bound to the given store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
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
            helpers: &[],
        }
    }
}

impl EffectHandler<SessionContext> for TasksHandler {
    type Request = TasksReq;

    fn handle(
        &mut self,
        req: TasksReq,
        _cx: &EffectContext<'_, SessionContext>,
    ) -> Result<Value, EffectError> {
        match req {
            TasksReq::_Placeholder => Err(EffectError::Handler(
                "Pattern.Tasks is scaffolding; variants added in Task 5".into(),
            )),
        }
    }
}
