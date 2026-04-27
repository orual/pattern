//! Database query functions.
//!
//! Organized by domain. All queries use rusqlite directly with
//! inherent `fn from_row` on each row struct.

mod agent;
mod atproto_endpoints;
pub mod constellation;
mod event;
mod folder;
pub mod fronting;
mod memory;
mod message;
mod queue;
pub mod skill_usage;
mod source;
pub mod stats;
mod task;
pub mod task_row;

pub use agent::*;
pub use atproto_endpoints::*;
pub use constellation::ConstellationRegistryDb;
pub use event::*;
pub use folder::*;
pub use fronting::{clear_fronting_set, load_fronting_set, save_fronting_set};
pub use memory::*;
pub use message::*;
pub use queue::*;
pub use skill_usage::{get_usage_stats, get_usage_stats_batch, record_usage};
pub use source::*;
pub use task::*;
pub use task_row::{TaskEdgeRow, TaskRow};
