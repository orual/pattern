//! Mirror of `Pattern.Tasks` (`haskell/Pattern/Tasks.hs`).
//!
//! This module is scaffolding for the ctx.tasks SDK surface. Variants are
//! added in Phase 3 Task 5 to support the eight task-operation methods:
//! `create_task`, `update_task`, `transition_status`, `link`, `unlink`,
//! `list_tasks`, `query_graph`, and `add_comment`.
//!
//! The placeholder variant below is removed during Task 5 once all eight
//! variants are wired.

use tidepool_bridge_derive::FromCore;

/// Rust mirror of the Haskell `Tasks` GADT.
///
/// Variants added in Phase 3 Task 5.
#[derive(Debug, FromCore)]
pub enum TasksReq {
    /// Placeholder variant — removed in Task 5 when actual variants are added.
    #[core(module = "Pattern.Tasks", name = "_Placeholder")]
    _Placeholder,
}
