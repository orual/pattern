// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Merge reporting types for fork resolution.
//!
//! `MergeReport` is returned by `ForkHandle::merge_back_lightweight` and
//! `ForkHandle::merge_back_persistent` (Tasks 2 and 5). Loro CRDT semantics
//! guarantee convergence — all concurrent ops from both sides are preserved in
//! the merged state. The report exists to let callers inspect how many blocks
//! were reconciled.

/// Summary of a merge-back operation.
///
/// Returned by [`crate::spawn::fork::ForkHandle::merge_back_lightweight`]
/// and [`crate::spawn::fork::ForkHandle::merge_back_persistent`].
///
/// Loro vector-clock CRDT guarantees that all concurrent ops from both the
/// parent and the fork are preserved in the merged state — there is no data
/// loss and no manual conflict resolution required. `blocks_merged` counts
/// how many blocks were reconciled.
#[derive(Debug, Clone, Default)]
pub struct MergeReport {
    /// Number of blocks successfully imported from the fork into the parent.
    pub blocks_merged: u32,
}
