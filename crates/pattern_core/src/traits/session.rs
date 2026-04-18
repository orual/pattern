//! Per-turn session trait: executes agent turns and captures checkpoints.
//!
//! A `Session` is produced by [`crate::traits::AgentRuntime::open_session`]
//! and lives for the duration of one or more agent turns. Sessions hold the
//! execution environment (Haskell interpreter state in Phase 3's Tidepool
//! bridge), dispatch side effects, and can be checkpointed for replay or
//! analysis.
//!
//! # Checkpoint / restore semantics
//!
//! Both `checkpoint` and `restore` are `async` because concrete
//! implementations may need to quiesce in-flight effects (e.g. flush pending
//! CRDT writes, drain outbound message queues) before producing or consuming
//! a snapshot. The async-ness is a forward-compatibility hedge: a synchronous
//! stub impl is trivially satisfiable, but callers must treat these methods
//! as potentially-awaiting.
//!
//! **Restore is nondestructive.** A `restore` call seeds in-memory working
//! state from the snapshot; it must not mutate any persistent store that
//! other sessions or the live runtime observe. This makes restore safe
//! mid-turn (for checkpoint-and-replay debugging) and safe from a forked
//! analysis session.
//!
//! **Mid-turn constraints.** `checkpoint` MAY be called mid-turn, but the
//! resulting snapshot captures only the committed portion of the turn —
//! in-flight effects are not guaranteed to be included. A session that needs
//! strict mid-turn checkpointability must drive commits explicitly in its
//! `checkpoint` implementation.

use async_trait::async_trait;

use crate::error::RuntimeError;
use crate::types::snapshot::SessionSnapshot;
use crate::types::turn::{StepReply, TurnInput};

/// Per-turn agent execution.
///
/// # Example
///
/// See the trait-level doctest on [`crate::traits::AgentRuntime`] for a
/// dummy impl that satisfies both traits together.
#[async_trait]
pub trait Session: Send {
    /// Execute one user-visible exchange against the given input.
    ///
    /// An "exchange" is the user-visible unit: the caller sends a
    /// message (or tool_results from a prior exchange's continuation,
    /// though that's internal); the agent loop may issue multiple
    /// **wire-level** provider turns (chained via `ToolUse` → next
    /// turn's tool_results) before producing a terminal response.
    ///
    /// Every wire turn appears in order in [`StepReply::turns`]; the
    /// final turn's `stop_reason` is also surfaced as
    /// `final_stop_reason`. Streaming consumers observe mid-exchange
    /// progress via the session's [`crate::traits::TurnSink`]; this
    /// method returns only the aggregated tail-end.
    ///
    /// Per-wire-turn [`TurnOutput`](crate::types::turn::TurnOutput)s
    /// are the checkpoint granularity — a `step` call that produces
    /// three wire turns writes three `TurnRecord` entries + three
    /// checkpoints before returning.
    async fn step(&mut self, input: TurnInput) -> Result<StepReply, RuntimeError>;

    /// Capture the session's environment for later restore.
    ///
    /// Returns a snapshot of committed state only. See module docs for the
    /// mid-turn guarantees.
    async fn checkpoint(&self) -> Result<SessionSnapshot, RuntimeError>;

    /// Restore a captured environment into this session.
    ///
    /// Nondestructive: seeds in-memory working state only. The caller is
    /// responsible for ensuring `snapshot` is compatible with this session's
    /// persona (typically by confirming agent-id equality).
    async fn restore(&mut self, snapshot: SessionSnapshot) -> Result<(), RuntimeError>;
}
