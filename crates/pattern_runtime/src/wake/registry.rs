// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use tokio::task::JoinHandle;

use crate::mailbox::{Mailbox, MailboxInput};

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
    /// `True`. The program is evaluated at `period` intervals on a
    /// read-only restricted bundle.
    Custom {
        /// User-supplied identifier from `ctx.wake.register`.
        id: SmolStr,
        /// Source code of the condition program (Haskell).
        program: String,
        /// How often to evaluate the condition. The registry enforces
        /// a minimum of 1 second; sub-second values are rejected at
        /// registration with [`WakeError::PeriodTooShort`].
        period: std::time::Duration,
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
    /// The custom evaluator returned an error during registration
    /// (e.g. min-period violation, capacity limit exceeded).
    #[error("custom evaluator error: {0}")]
    CustomEvaluatorError(String),
}

/// One registered wake condition, holding the evaluator task that
/// fires its activation.
struct RegisteredCondition {
    /// Stable identifier for unregister + re-register flows.
    id: SmolStr,
    /// Owning agent id. Used by `list_for_agent` to scope the listing
    /// to the caller's own wakes. Populated by the handler from
    /// `dispatch_agent_id`; tests that call `register` directly
    /// supply their own test agent id.
    agent_id: SmolStr,
    /// The condition's declaration. Kept for observability +
    /// reflection.
    condition: WakeCondition,
    /// Original wire form of the condition, stashed at register-time
    /// so `list_for_agent` can return wire payloads without needing
    /// a `WakeCondition -> WireWakeCondition` reverse conversion.
    /// `WireWakeCondition` is `Clone` and tiny (primitives only).
    wire: crate::sdk::requests::wake::WireWakeCondition,
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
    /// Session mailbox. Wake evaluators enqueue activations via
    /// [`Mailbox::send_input`] so the `pending` counter stays in sync
    /// with the channel queue depth. Holding the `Arc<Mailbox>` —
    /// rather than a raw [`tokio::sync::mpsc::UnboundedSender`] clone —
    /// closes the underflow bug where wake-driven sends bypassed the
    /// counter and tripped `has_pending()` to permanently true after
    /// the drain loop's `note_consumed` call.
    mailbox: Arc<Mailbox>,
    /// Tokio runtime handle for spawning evaluator tasks. Required because
    /// `register` is called from the eval-worker OS thread, which has no
    /// ambient tokio runtime. Without this, `tokio::spawn` would panic.
    tokio_handle: Handle,
    /// Minimum interval period the registry will accept. Defaults to
    /// 1 minute (the wake system is for long-running tracking, not
    /// real-time polling); tuned via [`Self::with_min_period`] (test path
    /// only — production callers use the default).
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
    /// Default block-storage scope for this session. Used by
    /// [`WakeCondition::TaskDependencyResolved`] to look up the watched
    /// task block under the session's project scope when bound. When
    /// `None`, falls back to `Scope::Global(agent_id)` for the lookup
    /// (matches pre-Phase-1 behaviour for unmounted sessions).
    default_scope: Option<pattern_core::types::memory_types::Scope>,
    /// Optional custom evaluator for Haskell-registered conditions.
    /// When empty, Custom registrations fall back to a parked task
    /// (Phase 4 behaviour). When set, the evaluator spawns real
    /// trigger tasks. Behind a `Mutex` for late-wiring after
    /// construction (the evaluator needs SDK include paths that
    /// aren't known at registry build time).
    custom_evaluator: Mutex<Option<Arc<super::custom::CustomEvaluator>>>,
    /// Optional persistence: when set, register/unregister mirror to
    /// the `wake_registrations` table so wakes survive daemon restarts.
    /// Restored on session open via `restore_for_agent` (see session.rs).
    /// `None` for sessions that don't want persistence (tests, transient
    /// shells).
    persistence: Option<Arc<pattern_db::ConstellationDb>>,
}

impl WakeRegistry {
    /// Construct a new registry that delivers wake activations
    /// through `mailbox`.
    ///
    /// Holds an `Arc<Mailbox>` (not a raw sender clone) so wake
    /// evaluators can call [`Mailbox::send_input`] and keep the
    /// mailbox's `pending` counter in lockstep with the channel.
    ///
    /// `tokio_handle` must be a live runtime handle so that evaluator
    /// tasks can be spawned from the eval-worker OS thread (which has
    /// no ambient tokio context). Callers in production pass
    /// `cx.user().tokio_handle().clone()`; tests pass
    /// `tokio::runtime::Handle::current()` from inside a
    /// `#[tokio::test]`.
    pub fn new(mailbox: Arc<Mailbox>, tokio_handle: Handle) -> Self {
        Self {
            conditions: Mutex::new(Vec::new()),
            mailbox,
            tokio_handle,
            min_period: jiff::Span::new().minutes(1),
            block_change_notifier: None,
            memory_store: None,
            default_scope: None,
            custom_evaluator: Mutex::new(None),
            persistence: None,
        }
    }

    /// Builder-style: wire the session's default block-storage scope so
    /// task-dependency wakes look up watched blocks at the right scope.
    /// Production callers pass `cx.user().default_scope().clone()`.
    #[must_use]
    pub fn with_default_scope(mut self, scope: pattern_core::types::memory_types::Scope) -> Self {
        self.default_scope = Some(scope);
        self
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

    /// Builder-style: wire a [`CustomEvaluator`] for Haskell-registered
    /// custom conditions. Without this, Custom registrations fall back
    /// to a parked task that never fires (Phase 4 behaviour).
    #[must_use]
    pub fn with_custom_evaluator(mut self, evaluator: Arc<super::custom::CustomEvaluator>) -> Self {
        *self.custom_evaluator.get_mut() = Some(evaluator);
        self
    }

    /// Late-wire a [`CustomEvaluator`] after construction. Used when
    /// the SDK include paths aren't known at registry build time
    /// (e.g. `open_with_agent_loop` builds the registry before resolving
    /// include paths).
    pub fn set_custom_evaluator(&self, evaluator: Arc<super::custom::CustomEvaluator>) {
        *self.custom_evaluator.lock() = Some(evaluator);
    }

    /// Builder-style: wire a `ConstellationDb` for persistence. When set,
    /// successful `register`/`unregister` calls mirror to the
    /// `wake_registrations` table so wakes survive daemon restarts.
    #[must_use]
    pub fn with_persistence(mut self, db: Arc<pattern_db::ConstellationDb>) -> Self {
        self.persistence = Some(db);
        self
    }

    /// Register a wake condition. Returns the id used to refer to it
    /// in [`Self::unregister`].
    /// Public register: mirrors to persistence (if wired). Production
    /// callers use this. Restore path uses `register_inner` with
    /// `persist = false` to avoid PK-conflict noise on the row that is
    /// being restored.
    pub fn register(
        &self,
        id: SmolStr,
        condition: WakeCondition,
        agent_id: SmolStr,
        wire: crate::sdk::requests::wake::WireWakeCondition,
    ) -> Result<SmolStr, WakeError> {
        self.register_inner(id, condition, agent_id, wire, true)
    }

    fn register_inner(
        &self,
        id: SmolStr,
        condition: WakeCondition,
        agent_id: SmolStr,
        wire: crate::sdk::requests::wake::WireWakeCondition,
        persist: bool,
    ) -> Result<SmolStr, WakeError> {
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
                    self.mailbox.clone(),
                    &self.tokio_handle,
                )?
            }
            WakeCondition::TaskTimeout { task, deadline } => {
                super::rust_primitives::spawn_task_timeout(
                    task.clone(),
                    deadline.0,
                    self.mailbox.clone(),
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
                    self.mailbox.clone(),
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
                // Look up the watched block at the session's default
                // scope (project-Local when bound, persona-Global
                // otherwise). The condition's `agent_id` identifies
                // who to wake, not who owns the block — tasks may live
                // in project scope and be shared across agents.
                let lookup_scope = self.default_scope.clone().unwrap_or_else(|| {
                    pattern_core::types::memory_types::Scope::Global(agent_id.clone().into())
                });
                let metadata = store
                    .get_block_metadata(&lookup_scope, &parent_label)
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
                    lookup_scope,
                    store.clone(),
                    notifier.clone(),
                    self.mailbox.clone(),
                    &self.tokio_handle,
                )
            }
            WakeCondition::Custom {
                id: custom_id,
                program,
                period,
            } => {
                // Phase 7 Task 6: delegate to the CustomEvaluator if wired.
                // Falls back to a parked task when no evaluator is available
                // (e.g. test sessions without SDK dir). The parked sentinel
                // NEVER fires; the real task lives in CustomEvaluator::tasks.
                //
                // IMPORTANT: we do NOT store the period in the WakeRegistry's
                // own sentinel handle — the period is owned by the CustomEvaluator.
                // On unregister, we delegate to the CustomEvaluator so it can
                // abort its own task (not the sentinel). See unregister() below.
                let maybe_evaluator = self.custom_evaluator.lock().clone();
                if let Some(evaluator) = maybe_evaluator {
                    evaluator
                        .register_interval(custom_id.clone(), program.clone(), *period)
                        .map_err(WakeError::CustomEvaluatorError)?;
                    // The evaluator owns the real task. Store a no-op sentinel
                    // handle so the registry can track the id and enforce the
                    // duplicate-id check. The sentinel is aborted harmlessly on
                    // registry drop; the real task is aborted via
                    // CustomEvaluator::unregister in WakeRegistry::unregister.
                    self.tokio_handle
                        .spawn(async move { std::future::pending::<()>().await })
                } else {
                    tracing::info!(
                        target: "pattern_runtime::wake",
                        custom_wake_id = %custom_id,
                        program_bytes = program.len(),
                        "custom wake condition registered; no evaluator wired (fallback parked)"
                    );
                    self.tokio_handle
                        .spawn(async move { std::future::pending::<()>().await })
                }
            }
        };

        // Clone-before-move so we can mirror to persistence after the
        // local registry is updated.
        let agent_id_for_db = agent_id.clone();
        let wire_for_db = wire.clone();

        let mut conds = self.conditions.lock();
        conds.push(RegisteredCondition {
            id: id.clone(),
            agent_id,
            condition,
            wire,
            handle,
        });
        drop(conds);

        // Mirror to wake_registrations if persistence is wired AND the
        // caller asked for persistence (restore path passes `persist=false`
        // to avoid PK-conflict noise on already-persisted rows). Failures
        // are logged but don't fail the register — the in-memory wake is
        // still active, persistence is best-effort across restart.
        if persist && let Some(db) = &self.persistence {
            match serde_json::to_string(&wire_for_db) {
                Ok(json) => match db.get() {
                    Ok(conn) => {
                        if let Err(e) = pattern_db::queries::insert_wake_registration(
                            &conn,
                            id.as_str(),
                            agent_id_for_db.as_str(),
                            &json,
                        ) {
                            tracing::warn!(
                                target: "pattern_runtime::wake",
                                wake_id = %id,
                                error = %e,
                                "failed to persist wake registration; in-memory wake still active"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        target: "pattern_runtime::wake",
                        wake_id = %id,
                        error = %e,
                        "failed to get db connection for wake persistence"
                    ),
                },
                Err(e) => tracing::warn!(
                    target: "pattern_runtime::wake",
                    wake_id = %id,
                    error = %e,
                    "failed to serialize wire wake condition; skipping persistence"
                ),
            }
        }

        Ok(id)
    }

    /// Unregister a wake condition by id. Aborts its evaluator task.
    /// Returns `true` if the id was registered, `false` otherwise.
    ///
    /// For [`WakeCondition::Custom`] conditions, this delegates to the
    /// `CustomEvaluator` (which owns the real evaluator task) in addition
    /// to removing the sentinel from the registry. Without this delegation,
    /// unregistering a custom condition would only abort the no-op sentinel
    /// and leave the real evaluator task running — leaking tasks across
    /// register/unregister cycles and eventually exhausting the 32-condition cap.
    pub fn unregister(&self, id: &SmolStr) -> bool {
        let mut conds = self.conditions.lock();
        if let Some(idx) = conds.iter().position(|c| &c.id == id) {
            let removed = conds.remove(idx);
            // Abort the registry-side handle (real task for most conditions;
            // no-op sentinel for Custom conditions — see register()).
            removed.handle.abort();
            // For Custom conditions: also delegate to the CustomEvaluator so
            // it can abort the real evaluator task and free the condition slot.
            // The evaluator is held behind a Mutex so we take a snapshot here
            // and release the conditions lock before calling into it, avoiding
            // a potential lock-order inversion.
            if matches!(removed.condition, WakeCondition::Custom { .. }) {
                let maybe_evaluator = self.custom_evaluator.lock().clone();
                if let Some(evaluator) = maybe_evaluator {
                    evaluator.unregister(id);
                }
            }
            // Mirror the removal to persistence. Best-effort; the in-memory
            // unregister already succeeded. After 0019 the composite PK
            // requires both agent_id and wake_id; we have agent_id from the
            // removed RegisteredCondition.
            if let Some(db) = &self.persistence {
                match db.get() {
                    Ok(conn) => {
                        if let Err(e) = pattern_db::queries::delete_wake_registration(
                            &conn,
                            removed.agent_id.as_str(),
                            id.as_str(),
                        ) {
                            tracing::warn!(
                                target: "pattern_runtime::wake",
                                wake_id = %id,
                                error = %e,
                                "failed to delete wake registration row"
                            );
                        }
                    }
                    Err(e) => tracing::warn!(
                        target: "pattern_runtime::wake",
                        wake_id = %id,
                        error = %e,
                        "failed to get db connection for wake persistence delete"
                    ),
                }
            }
            true
        } else {
            false
        }
    }

    /// Restore wake registrations for `agent_id` from persistence.
    /// Called at session-open time after the registry has been wired with
    /// `with_persistence` + any required notifiers/evaluators. Replays each
    /// persisted row through `register` using the SAME wake_id so external
    /// references (e.g. agent-block-stored ids) stay valid across restart.
    ///
    /// Rows that fail to deserialize or whose register call fails are logged
    /// and skipped — restoration is best-effort, partial restore is better
    /// than failing session-open.
    ///
    /// Returns the number of rows successfully restored.
    pub fn restore_for_agent(&self, agent_id: &SmolStr) -> usize {
        let Some(db) = &self.persistence else {
            return 0;
        };
        let conn = match db.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    target: "pattern_runtime::wake",
                    agent_id = %agent_id,
                    error = %e,
                    "failed to get db connection for wake restore"
                );
                return 0;
            }
        };
        let rows = match pattern_db::queries::list_wakes_for_agent(&conn, agent_id.as_str()) {
            Ok(rs) => rs,
            Err(e) => {
                tracing::warn!(
                    target: "pattern_runtime::wake",
                    agent_id = %agent_id,
                    error = %e,
                    "failed to list persisted wakes for agent; restoring zero"
                );
                return 0;
            }
        };
        // Release the conn before calling register (which takes its own
        // conn for the mirror-write). r2d2 pools tolerate concurrent gets
        // but releasing early is cheap and avoids holding a connection across
        // a potentially long compile path for WakeCustom restore.
        drop(conn);

        let mut restored = 0usize;
        for row in rows {
            let wire: crate::sdk::requests::wake::WireWakeCondition = match serde_json::from_str(&row.condition_json) {
                Ok(w) => w,
                Err(e) => {
                    tracing::warn!(
                        target: "pattern_runtime::wake",
                        wake_id = %row.wake_id,
                        error = %e,
                        "failed to deserialize persisted wake condition; skipping"
                    );
                    continue;
                }
            };
            let condition = wire.clone().into_condition(SmolStr::from(row.agent_id.as_str()));
            let id = SmolStr::from(row.wake_id.as_str());
            // Use register_inner with persist=false to avoid double-writing
            // the row we just read.
            match self.register_inner(id.clone(), condition, SmolStr::from(row.agent_id.as_str()), wire, false) {
                Ok(_) => restored += 1,
                Err(e) => tracing::warn!(
                    target: "pattern_runtime::wake",
                    wake_id = %id,
                    error = %e,
                    "failed to re-register persisted wake; skipping"
                ),
            }
        }
        if restored > 0 {
            tracing::info!(
                target: "pattern_runtime::wake",
                agent_id = %agent_id,
                count = restored,
                "restored persisted wake registrations"
            );
        }
        restored
    }

    /// Test/harness shim. Production callers (the `Pattern.Wake` handler)
    /// use `register` directly with the real agent_id + wire form; tests
    /// and bench harnesses don't care about the listing metadata, so this
    /// fills both in with synthetic values (`"test-agent"` + a placeholder
    /// wire). Do not use from production code paths — the wire form
    /// stashed here would mislead any caller of `list_for_agent`.
    pub fn register_test(
        &self,
        id: SmolStr,
        condition: WakeCondition,
    ) -> Result<SmolStr, WakeError> {
        let wire = crate::sdk::requests::wake::WireWakeCondition::Interval(0);
        self.register(id, condition, SmolStr::from("test-agent"), wire)
    }

    /// List wake conditions registered under `agent_id`, in the
    /// order they were registered. Returns `(wake_id, wire-condition)`
    /// pairs so handlers can hand the wire form straight back to the
    /// agent without a reverse conversion.
    ///
    /// Scoped to a single agent because cross-agent listing would
    /// expose other personas' wake state — agents only need to see
    /// the conditions they themselves registered.
    pub fn list_for_agent(
        &self,
        agent_id: &SmolStr,
    ) -> Vec<(SmolStr, crate::sdk::requests::wake::WireWakeCondition)> {
        let conds = self.conditions.lock();
        conds
            .iter()
            .filter(|c| &c.agent_id == agent_id)
            .map(|c| (c.id.clone(), c.wire.clone()))
            .collect()
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
    MailboxInput::new(from, msg).with_delivery(crate::mailbox::DeliveryMode::Queue)
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
    use crate::mailbox::Mailbox;
    use pattern_core::types::ids::PersonaId;
    use pattern_core::types::memory_types::TaskEdgeRef;

    fn fresh_registry() -> (WakeRegistry, Arc<Mailbox>) {
        let (mailbox, _) = Mailbox::new(PersonaId::from("wake-registry-test"));
        let handle = tokio::runtime::Handle::current();
        (WakeRegistry::new(mailbox.clone(), handle), mailbox)
    }

    /// Regression: wake evaluators must keep [`Mailbox::pending`] in sync
    /// with the channel queue depth.
    ///
    /// Before the fix, every wake spawn helper sent activations through a
    /// raw `mpsc::UnboundedSender<MailboxInput>`, bypassing
    /// [`Mailbox::send_input`] (which bumps `pending`). The mailbox drain
    /// task's `note_consumed` call then underflowed the `AtomicUsize`
    /// from `0` to `usize::MAX`, leaving `has_pending()` permanently
    /// true. The flag is consulted between continuation turns in
    /// [`crate::agent_loop::drive_step`] — once tripped, the runtime
    /// silently locked into "single-turn mode" for the lifetime of the
    /// session, breaking every tool_use chain after the first wake
    /// fired.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wake_drain_does_not_underflow_pending_counter() {
        use crate::mailbox::Mailbox;
        use pattern_core::types::ids::PersonaId;

        let (mailbox, _) = Mailbox::new(PersonaId::from("wake-pending"));
        let handle = tokio::runtime::Handle::current();
        let reg = WakeRegistry::new(mailbox.clone(), handle)
            .with_min_period(jiff::Span::new().milliseconds(10));
        reg.register_test(
            "iv-pending".into(),
            WakeCondition::Interval {
                period: SpanCompare(jiff::Span::new().milliseconds(50)),
            },
        )
        .expect("register interval");

        // Wait for the first interval tick to land in the mailbox.
        let mut rx = mailbox.lock_rx().await;
        let _input = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
            .await
            .expect("wake should fire within 500ms")
            .expect("mailbox channel open");
        drop(rx);

        // Simulate the drain loop's bookkeeping (mailbox.rs:305).
        mailbox.note_consumed();

        assert!(
            !mailbox.has_pending(),
            "after draining a wake-fired message, has_pending() must be \
             false; underflow here means a wake evaluator bypassed \
             Mailbox::send_input — pending counter and channel depth \
             have diverged"
        );
    }

    #[tokio::test]
    async fn task_dep_without_notifier_is_subscriber_not_configured() {
        let (reg, _rx) = fresh_registry();
        let edge = TaskEdgeRef {
            block: SmolStr::new("tasks"),
            task_item: Some(SmolStr::new("item-1")),
        };
        let err = reg
            .register_test(
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
        let (mailbox, _) = Mailbox::new(PersonaId::from("task-dep-no-store"));
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let reg = WakeRegistry::new(mailbox, tokio::runtime::Handle::current())
            .with_block_change_notifier(notifier);
        let edge = TaskEdgeRef {
            block: SmolStr::new("tasks"),
            task_item: Some(SmolStr::new("item-1")),
        };
        let err = reg
            .register_test(
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
        let (mailbox, _) = Mailbox::new(PersonaId::from("task-dep-block-level"));
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let reg = WakeRegistry::new(mailbox, tokio::runtime::Handle::current())
            .with_block_change_notifier(notifier)
            .with_memory_store(store);
        let edge = TaskEdgeRef {
            block: SmolStr::new("tasks"),
            task_item: None,
        };
        let err = reg
            .register_test(
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
        let (mailbox, _) = Mailbox::new(PersonaId::from("task-dep-ghost"));
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let reg = WakeRegistry::new(mailbox, tokio::runtime::Handle::current())
            .with_block_change_notifier(notifier)
            .with_memory_store(store);
        let edge = TaskEdgeRef {
            block: SmolStr::new("ghost-block"),
            task_item: Some(SmolStr::new("item-1")),
        };
        let err = reg
            .register_test(
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
        let (mailbox, _) = Mailbox::new(PersonaId::from("custom-parked"));
        let reg = WakeRegistry::new(mailbox.clone(), tokio::runtime::Handle::current());
        let id = reg
            .register_test(
                SmolStr::new("custom-1"),
                WakeCondition::Custom {
                    id: SmolStr::new("user-id"),
                    program: "pure True".to_string(),
                    period: std::time::Duration::from_secs(1),
                },
            )
            .expect("custom registration should succeed in Phase 4");
        assert_eq!(id.as_str(), "custom-1");
        assert_eq!(reg.len(), 1);

        // The parked evaluator must NOT fire any wake activation.
        // Hold the mailbox receiver and assert nothing arrives for 100ms.
        let no_fire = tokio::time::timeout(std::time::Duration::from_millis(100), async {
            mailbox.lock_rx().await.recv().await
        })
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
        let (mailbox, _) = Mailbox::new(PersonaId::from("sync-thread"));
        let reg = WakeRegistry::new(mailbox, handle)
            .with_min_period(jiff::Span::new().milliseconds(100));

        // Call register from a plain OS thread — no ambient tokio context.
        // This must not panic with "no reactor running".
        std::thread::spawn(move || {
            reg.register_test(
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

    /// Persistence roundtrip: register a wake, simulate restart by
    /// dropping the registry, then build a fresh registry on the same
    /// db + call `restore_for_agent` and verify the wake comes back
    /// with the SAME id.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persistence_roundtrip_restores_wake_with_same_id() {
        let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().unwrap());
        let agent = SmolStr::from("persist-agent");

        // First registry: register a wake.
        let original_id = {
            let (mailbox, _) = Mailbox::new(PersonaId::from("persist-1"));
            let handle = tokio::runtime::Handle::current();
            let reg = WakeRegistry::new(mailbox, handle)
                .with_min_period(jiff::Span::new().milliseconds(10))
                .with_persistence(db.clone());
            let id = SmolStr::from("my-wake-id");
            let condition = WakeCondition::Interval {
                period: SpanCompare(jiff::Span::new().milliseconds(50)),
            };
            let wire = crate::sdk::requests::wake::WireWakeCondition::Interval(50);
            reg.register(id.clone(), condition, agent.clone(), wire).expect("register")
        };
        assert_eq!(original_id.as_str(), "my-wake-id");

        // Second registry on same db: should find + restore the persisted row.
        let (mailbox, _) = Mailbox::new(PersonaId::from("persist-2"));
        let handle = tokio::runtime::Handle::current();
        let reg2 = WakeRegistry::new(mailbox, handle)
            .with_min_period(jiff::Span::new().milliseconds(10))
            .with_persistence(db.clone());
        let restored = reg2.restore_for_agent(&agent);
        assert_eq!(restored, 1, "one wake should be restored");

        let listed = reg2.list_for_agent(&agent);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.as_str(), "my-wake-id");

        let removed = reg2.unregister(&SmolStr::from("my-wake-id"));
        assert!(removed);

        let (mailbox, _) = Mailbox::new(PersonaId::from("persist-3"));
        let handle = tokio::runtime::Handle::current();
        let reg3 = WakeRegistry::new(mailbox, handle)
            .with_persistence(db);
        assert_eq!(reg3.restore_for_agent(&agent), 0, "no rows after unregister");
    }
}
