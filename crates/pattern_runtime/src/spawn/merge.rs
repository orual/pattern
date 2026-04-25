//! Merge reporting types for fork resolution.
//!
//! `MergeReport` is returned by `ForkHandle::merge_back_lightweight` and
//! `ForkHandle::merge_back_persistent` (Tasks 2 and 5). Conflicts here are
//! informational only — Loro CRDT semantics guarantee convergence, so the
//! report exists to let callers inspect which blocks received concurrent edits
//! rather than to signal failure.

/// Summary of a merge-back operation.
///
/// Returned by [`crate::spawn::fork::ForkHandle::merge_back_lightweight`].
/// Conflicts are informational: the Loro vector-clock CRDT guarantees that
/// all concurrent ops from both the parent and the fork are preserved in the
/// merged state, so `blocks_conflicted` tracks how many blocks had concurrent
/// edits on both sides (non-zero means interleaved timelines) but does NOT
/// indicate data loss.
#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    /// Number of blocks successfully imported from the fork into the parent.
    pub blocks_merged: u32,
    /// Number of blocks that had concurrent edits on both sides.
    ///
    /// A non-zero value means the merged document reflects contributions from
    /// both timelines. Loro CRDT handles the resolution deterministically.
    pub blocks_conflicted: u32,
    /// Informational summaries for blocks that had concurrent edits.
    pub conflicts: Vec<ConflictSummary>,
}

/// Informational summary for a single block that received concurrent edits on
/// both sides of a fork.
///
/// This does not represent a "conflict" in the traditional sense — Loro CRDT
/// will merge both sets of ops automatically. The summary is provided so
/// callers can log or surface which blocks diverged during the fork lifetime.
#[derive(Debug, Clone)]
pub struct ConflictSummary {
    /// Label of the block that had concurrent edits.
    pub label: String,
    /// Approximate number of ops that arrived from the fork side.
    pub fork_ops: u32,
    /// Approximate number of ops that arrived from the parent side.
    pub parent_ops: u32,
}
