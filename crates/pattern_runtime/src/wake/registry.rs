//! `WakeRegistry` and the wake-condition declaration types.
//!
//! See the parent module ([`crate::wake`]) for the wake-condition
//! design rationale. This file holds the declarative types
//! ([`WakeCondition`], [`WakeError`]) and the per-session
//! [`WakeRegistry`] that owns the evaluator tasks.

use parking_lot::Mutex;
use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::origin::SpanCompare;
use smol_str::SmolStr;
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
    /// BlockChanged subscriber and re-reads task status on each
    /// parent-block change (T9).
    TaskDependencyResolved {
        /// The task whose completion is being awaited.
        task: BlockRef,
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

/// Errors produced by [`WakeRegistry::register`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WakeError {
    /// Caller asked for an interval period below the registry's
    /// minimum (1s). Subsecond polling is rejected to prevent
    /// runaway resource use.
    #[error("interval period {requested:?} is below the minimum {minimum:?}")]
    PeriodTooShort {
        /// What the caller asked for.
        requested: jiff::Span,
        /// The enforced minimum.
        minimum: jiff::Span,
    },
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
    /// Phase 4 ships only the registration path for [`WakeCondition::Custom`].
    /// Returned when a session not configured with the Phase 7 Task 6
    /// evaluator tries to register a Custom condition. Until that
    /// evaluator lands, agents that call `ctx.wake.register` with a
    /// Custom condition see this error.
    #[error("custom wake-condition evaluator not configured (Phase 7 Task 6)")]
    CustomEvaluatorNotConfigured,
    /// Returned when the registry was constructed without a
    /// `BlockChangeNotifier` (i.e. no `MemoryCache` is wired) and
    /// the caller tried to register a [`WakeCondition::BlockChanged`]
    /// or [`WakeCondition::TaskDependencyResolved`].
    #[error(
        "block-change subscriber not configured on this registry; \
         BlockChanged and TaskDependencyResolved require a MemoryCache"
    )]
    SubscriberNotConfigured,
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
/// sender; evaluator tasks deliver activations through that sender.
///
/// To enable [`WakeCondition::BlockChanged`], wire a
/// [`pattern_memory::subscriber::BlockChangeNotifier`] via
/// [`Self::with_block_change_notifier`] — typically from
/// `MemoryCache::block_change_notifier`. Without it, BlockChanged
/// registrations return [`WakeError::SubscriberNotConfigured`].
pub struct WakeRegistry {
    conditions: Mutex<Vec<RegisteredCondition>>,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
    /// Minimum interval period the registry will accept. Defaults to
    /// 1 second; tuned via [`Self::with_min_period`] (test path only —
    /// production callers use the default).
    min_period: jiff::Span,
    /// Optional block-change notifier. Required for
    /// [`WakeCondition::BlockChanged`] (and Phase 4 Task 9's
    /// [`WakeCondition::TaskDependencyResolved`]). Cheap to clone —
    /// internally `Arc`-shared.
    block_change_notifier: Option<pattern_memory::subscriber::BlockChangeNotifier>,
}

impl WakeRegistry {
    /// Construct a new registry that delivers wake activations
    /// through `mailbox_tx`.
    pub fn new(mailbox_tx: mpsc::UnboundedSender<MailboxInput>) -> Self {
        Self {
            conditions: Mutex::new(Vec::new()),
            mailbox_tx,
            min_period: jiff::Span::new().seconds(1),
            block_change_notifier: None,
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

    /// Sender clone for evaluator tasks. Internal helper for
    /// `rust_primitives` and (later) the loro-subscriber-backed
    /// evaluators.
    pub(super) fn mailbox_tx(&self) -> &mpsc::UnboundedSender<MailboxInput> {
        &self.mailbox_tx
    }

    /// The minimum period accepted by [`Self::register`].
    pub(super) fn min_period(&self) -> jiff::Span {
        self.min_period
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
                super::rust_primitives::spawn_interval(period.0, self.mailbox_tx.clone())?
            }
            WakeCondition::TaskTimeout { task, deadline } => {
                super::rust_primitives::spawn_task_timeout(
                    task.clone(),
                    deadline.0,
                    self.mailbox_tx.clone(),
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
                )
            }
            WakeCondition::TaskDependencyResolved { .. } => {
                // T9 wires this on top of the BlockChanged subscriber:
                // re-reads the task's status on the parent block's
                // change events. Until T9 lands, surface a clear
                // error rather than silently spawning a no-op task.
                return Err(WakeError::CustomEvaluatorNotConfigured);
            }
            WakeCondition::Custom { .. } => {
                return Err(WakeError::CustomEvaluatorNotConfigured);
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
