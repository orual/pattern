// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Trait-signature types for the memory subsystem.
//!
//! These types appear in [`crate::traits::MemoryStore`] method signatures and
//! are shared across crate boundaries. Implementation-only types (e.g.
//! `CachedBlock`, `ChangeSource`) live in `pattern_memory::types_internal`.

pub mod block_schema_kind;
mod core_types;
mod metadata;
mod schema;
mod scope;
mod search;
mod skill;
mod task;
pub mod task_query;

pub use block_schema_kind::*;
pub use core_types::*;
pub use metadata::*;
pub use schema::*;
pub use scope::*;
pub use search::*;
pub use skill::*;
pub use task::*;
pub use task_query::*;

// `TaskItemId` is a SmolStr alias defined alongside the other id aliases
// in `crate::types::ids`. Re-exported here for import convenience since
// it appears on `TaskItem` and `TaskEdgeRef` in this module.
pub use crate::types::ids::TaskItemId;
