//! Database query functions.
//!
//! Organized by domain:
//! - `agent`: Agent CRUD and queries
//! - `atproto_endpoints`: Agent ATProto identity mapping
//! - `memory`: Memory block operations
//! - `message`: Message history operations
//! - `coordination`: Cross-agent coordination queries
//! - `source`: Data source configuration
//! - `task`: ADHD task management
//! - `event`: Calendar events and reminders
//! - `folder`: File access management

mod agent;
mod atproto_endpoints;
mod coordination;
mod event;
mod folder;
mod memory;
mod message;
mod queue;
mod source;
mod stats;
mod task;

pub use agent::*;
pub use atproto_endpoints::*;
pub use coordination::*;
pub use event::*;
pub use folder::*;
pub use memory::*;
pub use message::*;
pub use queue::*;
pub use source::*;
pub use stats::*;
pub use task::*;
