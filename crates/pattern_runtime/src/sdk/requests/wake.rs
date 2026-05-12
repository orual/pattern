//! Mirror of `Pattern.Wake` (`haskell/Pattern/Wake.hs`).
//!
//! Wake conditions cross the Haskell/Rust boundary as typed Core values
//! — each wire struct derives [`tidepool_bridge_derive::FromCore`] and
//! converts to the corresponding [`crate::wake::WakeCondition`] variant
//! via a `From<Wire*>` impl. The handler attaches the dispatching
//! agent's id at construction time for the `TaskDependencyResolved`
//! variant, so the Haskell-side does not have to carry it.
//!
//! Constructor naming follows the same convention as `Pattern.Spawn`:
//! the wire enum's variants are prefixed (`WakeInterval`,
//! `WakeBlockChanged`, etc.) on the Haskell side to keep them out of
//! the same namespace as the `Wake` GADT constructors (`Register`,
//! `Unregister`) — see `Pattern.Spawn`'s `Cat`/`Flag` precedent.

use jiff::Span;
use smol_str::SmolStr;
use serde::{Deserialize, Serialize};
use tidepool_bridge_derive::{FromCore, ToCore};

use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::memory_types::TaskEdgeRef;
use pattern_core::types::origin::SpanCompare;

use crate::wake::WakeCondition;

// ── BlockRef ─────────────────────────────────────────────────────────────────

/// Wire mirror of [`BlockRef`].
///
/// Local to `Pattern.Wake` — `Pattern.Spawn` declares its own
/// `WireBlockRef`. The two are structurally identical; we don't share
/// because the derive layer keys lookups on
/// `(module, type_name)` and the Haskell module is different here.
#[derive(Debug, Clone, FromCore, ToCore, Serialize, Deserialize)]
#[core(module = "Pattern.Wake", name = "BlockRef")]
pub struct WireBlockRef {
    pub label: String,
    pub block_id: String,
    pub agent_id: String,
}

impl From<WireBlockRef> for BlockRef {
    fn from(w: WireBlockRef) -> Self {
        BlockRef::with_owner(w.label, w.block_id, w.agent_id)
    }
}

// ── TaskEdgeRef ──────────────────────────────────────────────────────────────

/// Wire mirror of [`TaskEdgeRef`].
///
/// Tuple form (block, task_item) matches the `Pattern.Spawn`
/// convention of carrying multi-field domain types as positional
/// records on the wire (avoids the named-field-variant restriction
/// that the `FromCore` derive imposes when these appear inside an
/// enum variant).
#[derive(Debug, Clone, FromCore, ToCore, Serialize, Deserialize)]
#[core(module = "Pattern.Wake", name = "TaskEdgeRef")]
pub struct WireTaskEdgeRef {
    pub block: String,
    pub task_item: Option<String>,
}

impl From<WireTaskEdgeRef> for TaskEdgeRef {
    fn from(w: WireTaskEdgeRef) -> Self {
        TaskEdgeRef {
            block: SmolStr::from(w.block),
            task_item: w.task_item.map(SmolStr::from),
        }
    }
}

// ── WireWakeCondition ────────────────────────────────────────────────────────

/// Wire mirror of [`WakeCondition`]. Constructor names are
/// `Wake`-prefixed on the Haskell side.
///
/// `period_min` / `deadline_min` are wall-clock minute durations
/// converted to [`SpanCompare`] at the conversion boundary. Wake
/// timers are wall-clock-bounded (rejected at registration if they
/// carry calendar units). The Int is minutes (not milliseconds or
/// seconds): wake timers are for long-running tracking, never
/// sub-second polling, so minutes is the granularity agents want
/// to think in. The registry enforces a 1-minute minimum.
///
/// `TaskDependencyResolved` carries only the [`TaskEdgeRef`]; the
/// dispatching agent's id is attached by the handler from
/// [`crate::session::HasPermissionBridge::dispatch_agent_id`].
#[derive(Debug, Clone, FromCore, ToCore, Serialize, Deserialize)]
pub enum WireWakeCondition {
    /// `Interval period_min`. Wall-clock period in minutes; the
    /// registry rejects values below the per-session minimum (default
    /// 1 minute).
    #[core(module = "Pattern.Wake", name = "WakeInterval")]
    Interval(i64),
    /// `TaskTimeout task_block deadline_min`. Fires once after the
    /// deadline elapses (deadline in minutes); the agent then reads
    /// `task` to act on the timeout.
    #[core(module = "Pattern.Wake", name = "WakeTaskTimeout")]
    TaskTimeout(WireBlockRef, i64),
    /// `BlockChanged block`. Fires whenever `block`'s rendered
    /// content changes (any author).
    #[core(module = "Pattern.Wake", name = "WakeBlockChanged")]
    BlockChanged(WireBlockRef),
    /// `TaskDependencyResolved task`. Fires once when the named item
    /// transitions to `Completed`.
    #[core(module = "Pattern.Wake", name = "WakeTaskDependencyResolved")]
    TaskDependencyResolved(WireTaskEdgeRef),
    /// `Custom id program period_min`. Evaluates the Haskell program
    /// every `period_min` minutes against a read-only restricted
    /// bundle; pokes the mailbox when the result is `True`.
    #[core(module = "Pattern.Wake", name = "WakeCustom")]
    Custom(String, String, i64),
}

impl WireWakeCondition {
    /// Convert into a [`WakeCondition`], attaching the dispatching
    /// agent's id for the `TaskDependencyResolved` variant.
    ///
    /// `agent_id` is consumed only when the variant requires it; the
    /// other variants are agent-agnostic.
    pub fn into_condition(self, agent_id: SmolStr) -> WakeCondition {
        match self {
            Self::Interval(period_min) => WakeCondition::Interval {
                period: SpanCompare(Span::new().minutes(period_min)),
            },
            Self::TaskTimeout(task, deadline_min) => WakeCondition::TaskTimeout {
                task: task.into(),
                deadline: SpanCompare(Span::new().minutes(deadline_min)),
            },
            Self::BlockChanged(block) => WakeCondition::BlockChanged {
                block: block.into(),
            },
            Self::TaskDependencyResolved(task) => WakeCondition::TaskDependencyResolved {
                task: task.into(),
                agent_id,
            },
            Self::Custom(id, program, period_min) => WakeCondition::Custom {
                id: SmolStr::from(id),
                program,
                // Clamp negative or zero periods to the minimum; the registry
                // validates and rejects them with WakeError::PeriodTooShort.
                period: std::time::Duration::from_secs((period_min.max(0) as u64) * 60),
            },
        }
    }
}

// ── WireWakeListItem ─────────────────────────────────────────────────────────

/// One row in the `Wake.list` response. Pairs the wake id with the
/// wire form of its condition so agents can see both what is
/// registered and its parameters (period, deadline, watched block,
/// etc.) without needing to interpret an opaque id.
///
/// `condition` carries the same `WireWakeCondition` that was originally
/// passed to `register` — the registry stashes the wire form at
/// register-time so the listing path doesn't need a reverse domain →
/// wire conversion.
#[derive(Debug, ToCore)]
#[core(module = "Pattern.Wake", name = "WakeListItem")]
pub struct WireWakeListItem {
    pub wake_id: String,
    pub condition: WireWakeCondition,
}

// ── WakeReq ──────────────────────────────────────────────────────────────────

/// Rust mirror of the Haskell `Wake` GADT.
#[derive(Debug, FromCore)]
pub enum WakeReq {
    /// Register a wake condition. The optional first field is a
    /// caller-supplied name; when `None` the runtime mints a fresh UUID-shaped
    /// id. Named ids are per-agent (composite PK `(agent_id, wake_id)` from
    /// migration 0019), so two personas can both register a wake called
    /// `social-check` without colliding.
    #[core(module = "Pattern.Wake", name = "Register")]
    Register(Option<String>, WireWakeCondition),
    /// Unregister a previously-registered wake by id. Returns
    /// whether the id was actually registered.
    #[core(module = "Pattern.Wake", name = "Unregister")]
    Unregister(String),
    /// List wake conditions registered by the dispatching agent.
    /// Returns `[(WakeId, WakeCondition)]` — one entry per active
    /// registration. Scoped to the dispatching agent so callers see
    /// only their own wakes; cross-agent listing would expose other
    /// personas' wake state.
    #[core(module = "Pattern.Wake", name = "List")]
    List,
}
