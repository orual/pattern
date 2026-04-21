//! # pattern_memory
//!
//! Implementation crate for Pattern's memory subsystem. Hosts the `MemoryCache`
//! canonical `MemoryStore` implementation, `SharedBlockManager`, schema
//! templates, and (in later phases) filesystem serialization, the loro-native
//! subscriber machinery, the jj CLI adapter, storage-mode handling, and
//! backup/restore.
//!
//! [`StructuredDocument`](pattern_core::memory::StructuredDocument) remains in
//! `pattern_core::memory` because it appears in
//! [`MemoryStore`](pattern_core::traits::MemoryStore) trait signatures and
//! moving it would create a circular dependency.
//!
//! All data-contract types live in [`pattern_core::types::memory_types`].
//! Nothing in `pattern_core` depends on this crate.

pub mod backup;
pub mod cache;
pub mod config;
pub mod fs;
pub mod jj;
pub mod modes;
pub mod mount;
pub mod paths;
pub mod persona;
pub mod quiesce;
pub mod reembed;
pub mod schema_templates;
pub mod scope;
pub mod sharing;
pub mod subscriber;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
mod types_internal;
/// Host VCS detection (git, jj).
pub mod vcs;

pub use cache::{MemoryCache, PauseOutcome};
pub use paths::PatternPaths;
pub use schema_templates::templates;
pub use sharing::{CONSTELLATION_OWNER, SharedBlockManager};
