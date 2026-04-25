//! Child session spawn infrastructure.
//!
//! This module houses the `SpawnRegistry` and related types used by the
//! parent session to track child session handles, enforce per-parent
//! concurrency limits, and propagate cancellation when the parent ends.
//! `ephemeral` houses the fork-for-ephemeral construction + the
//! `run_ephemeral` driver that owns the child's wire-turn loop.
//!
//! # Module layout
//!
//! - `registry` — `SpawnRegistry`, `ChildSessionHandle`, `SpawnKind`,
//!   `SpawnResult`, `SpawnError`, `TerminationReason`.
//! - `ephemeral` — `run_ephemeral`, `synthesize_program_lib`,
//!   `compute_child_caps`, `child_include_paths`,
//!   `MAX_EPHEMERAL_TURNS`.

pub mod ephemeral;
pub mod registry;

pub use ephemeral::{
    MAX_EPHEMERAL_TURNS, child_include_paths, compute_child_caps, run_ephemeral,
    synthesize_program_lib,
};
pub use registry::{
    ChildSessionHandle, SpawnError, SpawnKind, SpawnRegistry, SpawnResult, TerminationReason,
};
