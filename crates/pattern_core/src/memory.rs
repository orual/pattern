//! Memory system document types.
//!
//! The `StructuredDocument` wrapper lives here because it appears in
//! [`MemoryStore`](crate::traits::MemoryStore) trait signatures. Moving it
//! to `pattern_memory` would create a circular dependency.
//!
//! Trait-signature value types (block metadata, schemas, search options)
//! live in [`crate::types::memory_types`]. The canonical `MemoryStore`
//! implementation (`MemoryCache`) lives in `pattern_memory`.

mod document;

pub use document::*;
