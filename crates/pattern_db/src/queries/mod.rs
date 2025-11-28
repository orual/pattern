//! Database query functions.
//!
//! Organized by domain:
//! - `agent`: Agent CRUD and queries
//! - `memory`: Memory block operations
//! - `message`: Message history operations
//! - `coordination`: Cross-agent coordination queries

mod agent;
mod coordination;
mod memory;
mod message;

pub use agent::*;
pub use coordination::*;
pub use memory::*;
pub use message::*;
