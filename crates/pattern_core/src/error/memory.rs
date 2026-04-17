//! Memory errors for block storage operations.
//!
//! This file defines errors that occur when reading, writing, or resolving
//! memory blocks. Surfaced through [`super::core::CoreError::Memory`].
//!
//! # Pre-v3 CoreError variants replaced by this file
//!
//! - `MemoryNotFound` → [`MemoryError::BlockNotFound`] (generalised from
//!   string-keyed agent/block_name to typed [`BlockHandle`]).
//! - `DataSourceError` (storage-related operations) → [`MemoryError::StoreCorrupted`]
//!   where appropriate.
//! - New: [`MemoryError::ConcurrentWriteConflict`] (no pre-v3 equivalent).

use miette::Diagnostic;
use thiserror::Error;

use crate::types::block::BlockHandle;

/// Errors from the memory block store.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum MemoryError {
    /// The requested memory block does not exist.
    ///
    /// `available` lists the handles that *do* exist so callers can give
    /// actionable feedback without a separate list call.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::MemoryError;
    /// use pattern_core::types::block::BlockHandle;
    ///
    /// let err = MemoryError::BlockNotFound {
    ///     handle: BlockHandle::new("persona"),
    ///     available: vec![BlockHandle::new("task_list")],
    /// };
    /// assert!(err.to_string().contains("persona"));
    /// ```
    #[error("block not found: {handle}")]
    #[diagnostic(
        code(pattern_core::memory::block_not_found),
        help("available blocks: {available:?}")
    )]
    BlockNotFound {
        /// The handle that was requested but not found.
        handle: BlockHandle,
        /// All handles currently available in the same scope.
        available: Vec<BlockHandle>,
    },

    /// The backing store returned data that cannot be parsed or is internally
    /// inconsistent.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::MemoryError;
    ///
    /// let err = MemoryError::StoreCorrupted { detail: "checksum mismatch".to_string() };
    /// assert!(err.to_string().contains("checksum"));
    /// ```
    #[error("memory store corrupted: {detail}")]
    #[diagnostic(
        code(pattern_core::memory::store_corrupted),
        help("inspect the backing database; a repair or restore from backup may be needed")
    )]
    StoreCorrupted {
        /// Human-readable description of the corruption.
        detail: String,
    },

    /// Two concurrent writers raced on the same block and could not be merged.
    ///
    /// The CRDT layer resolves most concurrent writes automatically; this error
    /// indicates a conflict that requires explicit resolution (e.g., schema
    /// mismatch between concurrent edits).
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::MemoryError;
    /// use pattern_core::types::block::BlockHandle;
    ///
    /// let err = MemoryError::ConcurrentWriteConflict {
    ///     handle: BlockHandle::new("shared_notes"),
    /// };
    /// assert!(err.to_string().contains("shared_notes"));
    /// ```
    #[error("concurrent write conflict on block: {handle}")]
    #[diagnostic(
        code(pattern_core::memory::concurrent_write_conflict),
        help("retry the write; if the conflict persists, a manual merge may be required")
    )]
    ConcurrentWriteConflict {
        /// The block that had a write conflict.
        handle: BlockHandle,
    },
}
