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
use crate::types::turn::{TurnInput, TurnOutput};

/// Per-turn agent execution.
///
/// # Example
///
/// See the trait-level doctest on [`crate::traits::AgentRuntime`] for a
/// dummy impl that satisfies both traits together.
#[async_trait]
pub trait Session: Send {
    /// Execute one agent turn against the given input.
    ///
    /// A turn begins with the caller-provided [`TurnInput`] and ends when
    /// the agent loop produces a [`TurnOutput`]. Partial results are not
    /// exposed through this method; streaming consumers observe them via
    /// the runtime's endpoint registry instead.
    async fn step(&mut self, input: TurnInput) -> Result<TurnOutput, RuntimeError>;

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
