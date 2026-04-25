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

pub mod draft;
pub mod ephemeral;
pub mod fork;
pub mod fork_registry;
pub mod merge;
pub mod registry;
pub mod sibling;

pub use ephemeral::{
    MAX_EPHEMERAL_TURNS, build_progress_log_observer, child_include_paths, compute_child_caps,
    create_progress_log_block, run_ephemeral, synthesize_program_lib,
};
pub use fork::{
    ForkError, ForkHandle, ForkIsolationState, WireForkHandle, check_promote_capability,
};
pub use fork_registry::{ForkRegistry, InMemoryForkRegistry};
pub use merge::{ConflictSummary, MergeReport};
pub use registry::{
    ChildSessionHandle, SpawnError, SpawnKind, SpawnRegistry, SpawnResult, TerminationReason,
};
pub use sibling::{
    RegistryError, SiblingExistingOutcome, SiblingPersonaResolver, StubSiblingResolver,
    UnconfiguredSiblingResolver, spawn_sibling_existing, spawn_sibling_new,
};
