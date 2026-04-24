//! Trait-signature types for the memory subsystem.
//!
//! These types appear in [`crate::traits::MemoryStore`] method signatures and
//! are shared across crate boundaries. Implementation-only types (e.g.
//! `CachedBlock`, `ChangeSource`) live in `pattern_memory::types_internal`.

mod core_types;
mod metadata;
mod schema;
mod search;
mod task;

pub use core_types::*;
pub use metadata::*;
pub use schema::*;
pub use search::*;
pub use task::*;

// `TaskItemId` is a SmolStr alias defined alongside the other id aliases
// in `crate::types::ids`. Re-exported here for import convenience since
// it appears on `TaskItem` and `TaskEdgeRef` in this module.
pub use crate::types::ids::TaskItemId;
