//! Core trait surface for pattern_core.
//!
//! This module collects the abstract contracts every Pattern v3 component
//! implements or consumes. Concrete implementations live in sibling crates
//! (`pattern_runtime`, `pattern_provider`) or inside this crate's own
//! subsystem modules (e.g. memory storage).

pub mod memory_store;

pub use memory_store::MemoryStore;
