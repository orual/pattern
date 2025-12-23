//! Database query functions.
//!
//! Organized by domain:
//! - `agent`: Agent CRUD and queries
//! - `memory`: Memory block operations
//! - `message`: Message history operations
//! - `coordination`: Cross-agent coordination queries
//! - `source`: Data source configuration
//! - `task`: ADHD task management
//! - `event`: Calendar events and reminders
//! - `folder`: File access management

mod agent;
mod coordination;
mod event;
mod folder;
mod memory;
mod message;
mod source;
mod task;

pub use agent::*;
pub use coordination::*;
pub use event::*;
pub use folder::*;
pub use memory::*;
pub use message::*;
pub use source::*;
pub use task::*;
