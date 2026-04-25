//! Child session spawn infrastructure.
//!
//! This module houses the `SpawnRegistry` and related types used by the
//! parent session to track child session handles, enforce per-parent
//! concurrency limits, and propagate cancellation when the parent ends.
//!
//! # Module layout
//!
//! - `registry` — `SpawnRegistry`, `ChildSessionHandle`, `SpawnKind`,
//!   `SpawnResult`, `SpawnError`.

pub mod registry;

pub use registry::{
    ChildSessionHandle, SpawnError, SpawnKind, SpawnRegistry, SpawnResult, TerminationReason,
};
