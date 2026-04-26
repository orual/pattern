//! `WakeRegistry` and the wake-condition declaration types.
//!
//! See the parent module ([`crate::wake`]) for the wake-condition
//! design rationale. This file holds the declarative types
//! ([`WakeCondition`], [`WakeError`]) and the per-session
//! [`WakeRegistry`] that owns the evaluator tasks.

use std::sync::Arc;

use parking_lot::Mutex;
use pattern_core::traits::MemoryStore;
use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::memory_types::TaskEdgeRef;
use pattern_core::types::origin::SpanCompare;
use smol_str::SmolStr;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::mailbox::MailboxInput;


/// A wake-condition declaration, decoupled from its evaluator.
///
/// Each variant pairs with a Rust evaluator (T7/T8/T9) or a Haskell
/// program (Phase 7 T6); registration constructs the appropriate
/// evaluator task and stores its handle in the [`WakeRegistry`].
///
/// `#[non_exhaustive]` so future transports (e.g. Phase 7's
/// `Custom` evaluator surfacing or new system events) can grow the
/// enum without breaking match arms.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum WakeCondition {
    /// Fire once, after `deadline` elapses, with the timeout block.
    TaskTimeout {
        /// The task block whose deadline is being watched.
        task: BlockRef,
        /// How long to wait before firing.
        deadline: SpanCompare,
    },
    /// Fire repeatedly every `period`. Min period 1s — enforced by
    /// the registry to prevent subsecond polling.
    Interval {
        /// The interval between fires.
        period: SpanCompare,
    },
    /// Fire whenever `block`'s content changes (any author). Wired
    /// through the `pattern_memory::subscriber` fan-out (T8).
    BlockChanged {
        /// The block to watch.
        block: BlockRef,
    },
    /// Fire when `task` transitions to `Completed`. Piggybacks on the
    /// BlockChanged subscriber on the task's parent `TaskList` block
    /// and re-reads the item's status on each change (T9).
    ///
    /// `task.task_item` must be `Some(_)`; block-level references are
    /// rejected at register time with [`WakeError::TaskItemRequired`].
    /// `agent_id` scopes the memory-store reads used to check status —
    /// callers should pass `cx.user().dispatch_agent_id()` so the
    /// evaluator sees the same blocks the agent does.
    TaskDependencyResolved {
        /// The task whose completion is being awaited.
        task: TaskEdgeRef,
        /// Agent id for memory-store scoping.
        agent_id: SmolStr,
    },
    /// Fire when a Haskell-registered condition program returns
    /// `True`. Phase 4 stores the program but the evaluator that
    /// runs it on its trigger is scheduled for Phase 7 Task 6.
    Custom {
        /// User-supplied identifier from `ctx.wake.register`.
        id: SmolStr,
        /// Source code of the condition program (Haskell).
        program: String,
    },
}

/// Details for the [`WakeError::PeriodTooShort`] variant.
///
/// Boxed to keep `WakeError` small (clippy `result_large_err`).
#[derive(Debug)]
pub struct PeriodTooShortDetails {
    /// What the caller asked for.
    pub requested: jiff::Span,
    /// The enforced minimum.
    pub minimum: jiff::Span,
}

/// Errors produced by [`WakeRegistry::register`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WakeError {
    /// Caller asked for an interval period below the registry's
    /// minimum (1s). Subsecond polling is rejected to prevent
    /// runaway resource use.
    #[error("interval period {0:?} is below the minimum")]
    PeriodTooShort(Box<PeriodTooShortDetails>),
    /// Span carried a calendar unit (years/months/weeks) that cannot
    /// be converted to a wall-clock duration without a reference
    /// instant. Wake timers are wall-clock events; callers should
    /// supply day/hour/minute/second/sub-second units.
    #[error(
        "span carries calendar units that need a reference instant \
         to convert to wall-clock duration: {0}"
    )]
    NonWallClockSpan(String),
    /// The condition's id is already registered. Caller should
    /// `unregister` first or pick a different id.
    #[error("wake condition with id {id:?} is already registered")]
    DuplicateId {
        /// The conflicting id.
        id: SmolStr,
    },
    /// Returned when the registry was constructed without a
    /// `BlockChangeNotifier` (i.e. no `MemoryCache` is wired) and
    /// the caller tried to register a [`WakeCondition::BlockChanged`]
    /// or [`WakeCondition::TaskDependencyResolved`].
    #[error(
        "block-change subscriber not configured on this registry; \
         BlockChanged and TaskDependencyResolved require a MemoryCache"
    )]
    SubscriberNotConfigured,
    /// Returned when the registry was constructed without a
    /// `MemoryStore` and the caller tried to register a
    /// [`WakeCondition::TaskDependencyResolved`]. The status check
    /// inside the evaluator's callback re-reads the task's parent
    /// block, which requires a store to query.
    #[error(
        "memory store not configured on this registry; \
         TaskDependencyResolved requires it to read task status"
    )]
    MemoryStoreNotConfigured,
    /// Returned when the parent `TaskList` block named by the
    /// supplied [`TaskEdgeRef`] cannot be resolved to a `block_id`
    /// for the supplied agent (no such label, or the block lives in
    /// another agent's scope).
    #[error("parent task-list block {label:?} not found for agent {agent_id:?}")]
    ParentBlockNotFound {
        /// The block label that failed to resolve.
        label: SmolStr,
        /// The agent id used for the lookup.
        agent_id: SmolStr,
    },
    /// Returned when [`MemoryStore::get_block_metadata`] returned an
    /// error while resolving the parent `TaskList` block (e.g. a DB
    /// pool failure). Distinguished from [`Self::ParentBlockNotFound`]
    /// so transient infrastructure failures don't read as "the block
    /// doesn't exist".
    #[error(
        "parent task-list block {label:?} resolution failed for \
         agent {agent_id:?}: {message}"
    )]
    ParentBlockResolveFailed {
        /// The block label that failed to resolve.
        label: SmolStr,
        /// The agent id used for the lookup.
        agent_id: SmolStr,
        /// The underlying error message.
        message: String,
    },
    /// Returned when [`WakeCondition::TaskDependencyResolved`] is
    /// registered with a block-level [`TaskEdgeRef`] (no
    /// `task_item`). The dependency-resolved wake watches a single
    /// item's transition to `Completed`; block-level references are
    /// ambiguous (which item?) and rejected so callers get a clear
    /// signal at registration rather than a wake that never fires.
    #[error("TaskDependencyResolved requires an item-level reference, got block-level {block:?}")]
    TaskItemRequired {
        /// The block handle that was passed without an item id.
        block: SmolStr,
    },
}

/// One registered wake condition, holding the evaluator task that
/// fires its activation.
struct RegisteredCondition {
    /// Stable identifier for unregister + re-register flows.
    id: SmolStr,
    /// The condition's declaration. Kept for observability +
    /// reflection.
    #[allow(dead_code)]
    condition: WakeCondition,
    /// Evaluator task. Aborted on unregister or registry drop.
    handle: JoinHandle<()>,
}

/// Per-session registry of active wake conditions.
///
/// Owns one `JoinHandle<()>` per registered condition. Drop aborts
/// every evaluator task — the registry's lifetime bounds its
/// conditions' lifetimes, so a session ending cleans up all timers
/// and subscribers it spawned.
///
/// Constructed via [`WakeRegistry::new`] with the session's mailbox
/// sender and a tokio runtime handle; evaluator tasks deliver
/// activations through that sender.
///
/// The `tokio_handle` is required because `register` is called from
/// the eval-worker OS thread, which has no ambient tokio runtime.
/// All `spawn` calls go through the stored handle rather than
/// `tokio::spawn` (which would panic outside a runtime context).
///
/// To enable [`WakeCondition::BlockChanged`], wire a
/// [`pattern_memory::subscriber::BlockChangeNotifier`] via
/// [`Self::with_block_change_notifier`] — typically from
/// `MemoryCache::block_change_notifier`. Without it, BlockChanged
/// registrations return [`WakeError::SubscriberNotConfigured`].
pub struct WakeRegistry {
    conditions: Mutex<Vec<RegisteredCondition>>,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
    /// Tokio runtime handle for spawning evaluator tasks. Required because
    /// `register` is called from the eval-worker OS thread, which has no
    /// ambient tokio runtime. Without this, `tokio::spawn` would panic.
    tokio_handle: Handle,
    /// Minimum interval period the registry will accept. Defaults to
    /// 1 second; tuned via [`Self::with_min_period`] (test path only —
    /// production callers use the default).
    min_period: jiff::Span,
    /// Optional block-change notifier. Required for
    /// [`WakeCondition::BlockChanged`] (and Phase 4 Task 9's
    /// [`WakeCondition::TaskDependencyResolved`]). Cheap to clone —
    /// internally `Arc`-shared.
    block_change_notifier: Option<pattern_memory::subscriber::BlockChangeNotifier>,
    /// Optional memory store. Required for
    /// [`WakeCondition::TaskDependencyResolved`] so the evaluator
    /// can resolve the parent block's `block_id` and re-read task
    /// status when the parent block's content changes.
    memory_store: Option<Arc<dyn MemoryStore>>,
}

impl WakeRegistry {
    /// Construct a new registry that delivers wake activations
    /// through `mailbox_tx`.
    ///
    /// `tokio_handle` must be a live runtime handle so that evaluator
    /// tasks can be spawned from the eval-worker OS thread (which has
    /// no ambient tokio context). Callers in production pass
    /// `cx.user().tokio_handle().clone()`; tests pass
    /// `tokio::runtime::Handle::current()` from inside a
    /// `#[tokio::test]`.
    pub fn new(mailbox_tx: mpsc::UnboundedSender<MailboxInput>, tokio_handle: Handle) -> Self {
        Self {
            conditions: Mutex::new(Vec::new()),
            mailbox_tx,
            tokio_handle,
            min_period: jiff::Span::new().seconds(1),
            block_change_notifier: None,
            memory_store: None,
        }
    }

    /// Builder-style: lower the minimum interval period. Test-only
    /// hook; production callers leave the default 1s in place.
    #[must_use]
    pub fn with_min_period(mut self, min_period: jiff::Span) -> Self {
        self.min_period = min_period;
        self
    }

    /// Builder-style: wire a [`pattern_memory::subscriber::BlockChangeNotifier`]
    /// so [`WakeCondition::BlockChanged`] (and Phase 4 Task 9's
    /// [`WakeCondition::TaskDependencyResolved`]) can register
    /// callbacks. Production callers pass
    /// `cache.block_change_notifier().clone()`.
    #[must_use]
    pub fn with_block_change_notifier(
        mut self,
        notifier: pattern_memory::subscriber::BlockChangeNotifier,
    ) -> Self {
        self.block_change_notifier = Some(notifier);
        self
    }

    /// Builder-style: wire an [`Arc<dyn MemoryStore>`] so
    /// [`WakeCondition::TaskDependencyResolved`] evaluators can
    /// resolve parent blocks and read task status. Production
    /// callers pass `cx.user().memory_store()`.
    #[must_use]
    pub fn with_memory_store(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory_store = Some(store);
        self
    }

    /// Register a wake condition. Returns the id used to refer to it
    /// in [`Self::unregister`].
    pub fn register(&self, id: SmolStr, condition: WakeCondition) -> Result<SmolStr, WakeError> {
        // Duplicate-id check.
        {
            let conds = self.conditions.lock();
            if conds.iter().any(|c| c.id == id) {
                return Err(WakeError::DuplicateId { id });
            }
        }

        let handle = match &condition {
            WakeCondition::Interval { period } => {
                super::rust_primitives::validate_period(period.0, self.min_period)?;
                super::rust_primitives::spawn_interval(
                    period.0,
                    self.mailbox_tx.clone(),
                    &self.tokio_handle,
                )?
            }
            WakeCondition::TaskTimeout { task, deadline } => {
                super::rust_primitives::spawn_task_timeout(
                    task.clone(),
                    deadline.0,
                    self.mailbox_tx.clone(),
                    &self.tokio_handle,
                )?
            }
            WakeCondition::BlockChanged { block } => {
                let notifier = self
                    .block_change_notifier
                    .as_ref()
                    .ok_or(WakeError::SubscriberNotConfigured)?;
                super::block_changed::spawn_block_changed(
                    block.clone(),
                    notifier.clone(),
                    self.mailbox_tx.clone(),
                    &self.tokio_handle,
                )
            }
            WakeCondition::TaskDependencyResolved { task, agent_id } => {
                let notifier = self
                    .block_change_notifier
                    .as_ref()
                    .ok_or(WakeError::SubscriberNotConfigured)?;
                let store = self
                    .memory_store
                    .as_ref()
                    .ok_or(WakeError::MemoryStoreNotConfigured)?;
                if task.task_item.is_none() {
                    return Err(WakeError::TaskItemRequired {
                        block: task.block.clone(),
                    });
                }
                // Resolve parent TaskList block label → block_id so the
                // notifier subscription is keyed correctly. Failing
                // here surfaces a clear "no such block" error rather
                // than silently subscribing to a non-existent key.
                let parent_label = task.block.clone();
                let metadata = store
                    .get_block_metadata(agent_id, &parent_label)
                    .map_err(|e| WakeError::ParentBlockResolveFailed {
                        label: parent_label.clone(),
                        agent_id: agent_id.clone(),
                        message: e.to_string(),
                    })?
                    .ok_or_else(|| WakeError::ParentBlockNotFound {
                        label: parent_label.clone(),
                        agent_id: agent_id.clone(),
                    })?;
                let parent_block = BlockRef::new(parent_label, &metadata.id);
                super::task_dep::spawn_task_dependency_resolved(
                    parent_block,
                    task.clone(),
                    agent_id.clone(),
                    store.clone(),
                    notifier.clone(),
                    self.mailbox_tx.clone(),
                    &self.tokio_handle,
                )
            }
            WakeCondition::Custom { id, program } => {
                // Phase 4 stores the program but does not run it; the
                // evaluator that triggers user-supplied conditions
                // ships in Phase 7 Task 6. Spawn a parked task so the
                // registry has a JoinHandle to abort on unregister,
                // preserving the same lifecycle shape as evaluators
                // that *do* fire.
                tracing::info!(
                    target = "pattern_runtime::wake",
                    custom_wake_id = %id,
                    program_bytes = program.len(),
                    "custom wake condition registered; evaluator deferred (Phase 7 Task 6)"
                );
                self.tokio_handle
                    .spawn(async move { std::future::pending::<()>().await })
            }
        };

        let mut conds = self.conditions.lock();
        conds.push(RegisteredCondition {
            id: id.clone(),
            condition,
            handle,
        });
        Ok(id)
    }

    /// Unregister a wake condition by id. Aborts its evaluator task.
    /// Returns `true` if the id was registered, `false` otherwise.
    pub fn unregister(&self, id: &SmolStr) -> bool {
        let mut conds = self.conditions.lock();
        if let Some(idx) = conds.iter().position(|c| &c.id == id) {
            let removed = conds.remove(idx);
            removed.handle.abort();
            true
        } else {
            false
        }
    }

    /// Number of currently-registered conditions. For observability
    /// + tests.
    pub fn len(&self) -> usize {
        self.conditions.lock().len()
    }

    /// True when no conditions are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for WakeRegistry {
    fn drop(&mut self) {
        let mut conds = self.conditions.lock();
        for cond in conds.drain(..) {
            cond.handle.abort();
        }
    }
}

impl std::fmt::Debug for WakeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeRegistry")
            .field("len", &self.len())
            .field("min_period", &self.min_period)
            .finish_non_exhaustive()
    }
}

/// Build a [`MailboxInput`] for a wake activation.
///
/// Wake activations carry an `Author::System { reason: ... }`
/// origin. The body is a short marker text so the agent's
/// composer has *something* to render (even though the structured
/// payload — block ref, span — lives on the origin's `SystemReason`
/// variant where it belongs).
pub(super) fn wake_mailbox_input(
    reason: pattern_core::types::origin::SystemReason,
    body_text: &str,
) -> MailboxInput {
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::message::Message;
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere};

    let from = MessageOrigin::new(Author::System { reason }, Sphere::System);
    let msg = Message {
        chat_message: genai::chat::ChatMessage::new(
            genai::chat::ChatRole::User,
            body_text.to_string(),
        ),
        id: MessageId::from(new_id().to_string()),
        position: new_snowflake_id(),
        // Wake messages don't have a single human owner — attribute
        // to the system. The recipient session's agent_id is what
        // the composer cares about; this field tracks who wrote the
        // message body, not who's receiving it.
        owner_id: AgentId::from("_system_"),
        created_at: jiff::Timestamp::now(),
        batch: BatchId::from(new_snowflake_id()),
        response_meta: None,
        block_refs: vec![],
        attachments: vec![],
    };
    MailboxInput { from, msg }
}

#[cfg(test)]
mod tests {
    //! Phase 4 Task 9 register-time validation.
    //!
    //! End-to-end fire-on-completion testing is in
    //! `tests/wake_task_dep.rs` (integration-style; needs an
    //! `InMemoryMemoryStore` populated with a real `TaskList` block).
    //! These unit tests cover the registry's own preflight errors.

    use super::*;
    use pattern_core::types::memory_types::TaskEdgeRef;

    fn fresh_registry() -> (WakeRegistry, mpsc::UnboundedReceiver<MailboxInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::runtime::Handle::current();
        (WakeRegistry::new(tx, handle), rx)
    }

    #[tokio::test]
    async fn task_dep_without_notifier_is_subscriber_not_configured() {
        let (reg, _rx) = fresh_registry();
        let edge = TaskEdgeRef {
            block: SmolStr::new("tasks"),
            task_item: Some(SmolStr::new("item-1")),
        };
        let err = reg
            .register(
                SmolStr::new("w1"),
                WakeCondition::TaskDependencyResolved {
                    task: edge,
                    agent_id: SmolStr::new("a"),
                },
            )
            .expect_err("should require notifier");
        assert!(matches!(err, WakeError::SubscriberNotConfigured));
    }

    #[tokio::test]
    async fn task_dep_without_store_is_memory_store_not_configured() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let reg = WakeRegistry::new(tx, tokio::runtime::Handle::current())
            .with_block_change_notifier(notifier);
        let edge = TaskEdgeRef {
            block: SmolStr::new("tasks"),
            task_item: Some(SmolStr::new("item-1")),
        };
        let err = reg
            .register(
                SmolStr::new("w1"),
                WakeCondition::TaskDependencyResolved {
                    task: edge,
                    agent_id: SmolStr::new("a"),
                },
            )
            .expect_err("should require store");
        assert!(matches!(err, WakeError::MemoryStoreNotConfigured));
    }

    #[tokio::test]
    async fn task_dep_block_level_ref_is_task_item_required() {
        use crate::testing::in_memory_store::InMemoryMemoryStore;
        let (tx, _rx) = mpsc::unbounded_channel();
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let reg = WakeRegistry::new(tx, tokio::runtime::Handle::current())
            .with_block_change_notifier(notifier)
            .with_memory_store(store);
        let edge = TaskEdgeRef {
            block: SmolStr::new("tasks"),
            task_item: None,
        };
        let err = reg
            .register(
                SmolStr::new("w1"),
                WakeCondition::TaskDependencyResolved {
                    task: edge,
                    agent_id: SmolStr::new("a"),
                },
            )
            .expect_err("should reject block-level ref");
        assert!(matches!(err, WakeError::TaskItemRequired { .. }));
    }

    #[tokio::test]
    async fn task_dep_unknown_parent_block_surfaces_clear_error() {
        use crate::testing::in_memory_store::InMemoryMemoryStore;
        let (tx, _rx) = mpsc::unbounded_channel();
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let reg = WakeRegistry::new(tx, tokio::runtime::Handle::current())
            .with_block_change_notifier(notifier)
            .with_memory_store(store);
        let edge = TaskEdgeRef {
            block: SmolStr::new("ghost-block"),
            task_item: Some(SmolStr::new("item-1")),
        };
        let err = reg
            .register(
                SmolStr::new("w1"),
                WakeCondition::TaskDependencyResolved {
                    task: edge,
                    agent_id: SmolStr::new("a"),
                },
            )
            .expect_err("should report parent missing");
        match err {
            WakeError::ParentBlockNotFound { label, agent_id } => {
                assert_eq!(label.as_str(), "ghost-block");
                assert_eq!(agent_id.as_str(), "a");
            }
            other => panic!("expected ParentBlockNotFound, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn custom_condition_is_accepted_with_parked_evaluator() {
        let (tx, rx) = mpsc::unbounded_channel();
        let reg = WakeRegistry::new(tx, tokio::runtime::Handle::current());
        let id = reg
            .register(
                SmolStr::new("custom-1"),
                WakeCondition::Custom {
                    id: SmolStr::new("user-id"),
                    program: "pure True".to_string(),
                },
            )
            .expect("custom registration should succeed in Phase 4");
        assert_eq!(id.as_str(), "custom-1");
        assert_eq!(reg.len(), 1);

        // Important 1: The parked evaluator must NOT fire any wake activation.
        // Keep `rx` alive and assert nothing arrives for at least 100ms.
        let no_fire = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            tokio::task::spawn(async move {
                // Move rx into a spawned task so the block won't prevent
                // the registry from being used above.
                let mut rx = rx;
                rx.recv().await
            }),
        )
        .await;
        assert!(
            no_fire.is_err(),
            "Custom registration must NOT fire any wake activation; \
             the parked evaluator is wired incorrectly"
        );

        assert!(reg.unregister(&SmolStr::new("custom-1")));
        assert_eq!(reg.len(), 0);
    }

    /// Regression test for Critical 1: `WakeRegistry::register` must not panic
    /// when called from a non-tokio thread (i.e. the eval-worker OS thread).
    ///
    /// Before the fix, every `spawn_*` helper called bare `tokio::spawn(...)`,
    /// which panics with "no reactor running" when invoked outside a tokio
    /// runtime context. The fix stores the runtime `Handle` and calls
    /// `handle.spawn(...)` instead.
    ///
    /// This test is deliberately a plain `#[test]` (NOT `#[tokio::test]`) so
    /// it reproduces the eval-worker path exactly: the registering thread has
    /// no ambient tokio runtime.
    #[test]
    fn register_from_sync_thread_does_not_panic() {
        // Build a real multi-threaded runtime to host evaluator tasks.
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let handle = rt.handle().clone();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let reg = WakeRegistry::new(tx, handle)
            .with_min_period(jiff::Span::new().milliseconds(100));

        // Call register from a plain OS thread — no ambient tokio context.
        // This must not panic with "no reactor running".
        std::thread::spawn(move || {
            reg.register(
                SmolStr::new("interval-sync"),
                WakeCondition::Interval {
                    period: SpanCompare(jiff::Span::new().milliseconds(200)),
                },
            )
            .expect("register from sync thread must not panic");
            assert_eq!(reg.len(), 1);
        })
        .join()
        .expect("sync thread must not panic");
    }
}
