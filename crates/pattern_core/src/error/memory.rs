//! Memory errors for block storage operations.
//!
//! This file defines errors that occur when reading, writing, or resolving
//! memory blocks. Surfaced through [`super::core::CoreError::Memory`].
//!
//! # Unified error
//!
//! This is the single canonical `MemoryError` for all memory operations.
//! Both the `MemoryStore` trait (via `MemoryResult<T>`) and the `CoreError`
//! wrapper point here.
//!
//! # Pre-v3 CoreError variants replaced by this file
//!
//! - `MemoryNotFound` → [`MemoryError::BlockNotFound`] (typed
//!   [`BlockHandle`]) or [`MemoryError::NotFound`] (string-keyed).
//! - `DataSourceError` (storage-related operations) →
//!   [`MemoryError::StoreCorrupted`] where appropriate.
//! - New: [`MemoryError::ConcurrentWriteConflict`] (no pre-v3 equivalent).

use miette::Diagnostic;
use thiserror::Error;

use crate::types::block::BlockHandle;
use crate::types::memory_types::{DocumentError, IsolatePolicy};

/// Errors from the memory block store.
///
/// This is the unified error type for all memory operations. The
/// `MemoryResult<T>` type alias uses this as the error variant.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum MemoryError {
    /// The requested memory block does not exist (typed-handle lookup).
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

    /// The requested memory block does not exist (string-keyed lookup).
    ///
    /// Used by `MemoryCache::get` and other call sites that identify blocks
    /// by `(agent_id, label)` rather than a typed `BlockHandle`.
    #[error("block not found: {agent_id}/{label}")]
    #[diagnostic(code(pattern_core::memory::not_found))]
    NotFound {
        /// The agent that owns the missing block.
        agent_id: String,
        /// The label that was requested but not found.
        label: String,
    },

    /// The block is read-only and cannot be modified.
    #[error("block is read-only: {0}")]
    #[diagnostic(code(pattern_core::memory::read_only))]
    ReadOnly(String),

    /// Permission denied for a memory operation.
    #[error(
        "permission denied for block '{block_label}': required {required:?}, actual {actual:?}"
    )]
    #[diagnostic(code(pattern_core::memory::permission_denied))]
    PermissionDenied {
        /// The label of the block the operation was attempted on.
        block_label: String,
        /// The permission level required for the operation.
        required: pattern_db::models::MemoryPermission,
        /// The permission level the block actually has.
        actual: pattern_db::models::MemoryPermission,
    },

    /// Operation would cross a persona isolation boundary.
    #[error(
        "isolation denied: operation {operation} would cross persona boundary under policy {policy}"
    )]
    #[diagnostic(
        code(pattern_core::memory::isolation_denied),
        help("check the IsolatePolicy for this persona-project binding")
    )]
    IsolationDenied {
        /// The operation that was denied.
        operation: String,
        /// The active isolation policy.
        policy: IsolatePolicy,
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

    /// An error from the underlying database layer.
    #[error("database error: {0}")]
    #[diagnostic(code(pattern_core::memory::database))]
    Database(#[from] pattern_db::DbError),

    /// An error from the Loro CRDT layer.
    #[error("loro error: {0}")]
    #[diagnostic(code(pattern_core::memory::loro))]
    Loro(String),

    /// An error from structured document operations.
    #[error("document error: {0}")]
    #[diagnostic(code(pattern_core::memory::document))]
    Document(#[from] DocumentError),

    /// Catch-all for memory operation failures that don't fit other variants.
    #[error("memory operation failed: {0}")]
    #[diagnostic(code(pattern_core::memory::other))]
    Other(String),
}

/// Convenience `Result` alias using [`MemoryError`] as the error type.
///
/// Used throughout `MemoryStore` trait signatures and implementations.
pub type MemoryResult<T> = Result<T, MemoryError>;
