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
//!   [`BlockHandle`]) or [`MemoryError::WriteToMissingBlock`]
//!   (string-keyed write/mutation path).
//! - `DataSourceError` (storage-related operations) →
//!   [`MemoryError::StoreCorrupted`] where appropriate.
//! - New: [`MemoryError::ConcurrentWriteConflict`] (no pre-v3 equivalent).
//!
//! # Read vs write missing-block semantics
//!
//! Read-style operations on the [`crate::traits::MemoryStore`] trait
//! (`get_block`, `get_block_metadata`, `get_rendered_content`) all
//! return `MemoryResult<Option<...>>` and signal "block does not exist"
//! by returning `Ok(None)`. The trait contract is honoured by every
//! impl including `pattern_memory::MemoryCache`.
//!
//! Write-style operations that have no `Option` return type
//! (`update_block_metadata`, `persist_block`, `delete_block`,
//! `mark_dirty`-equivalents) signal "block does not exist" by returning
//! [`MemoryError::WriteToMissingBlock`]. This variant is exclusively a
//! write-path error — read paths never produce it.

use miette::Diagnostic;
use thiserror::Error;

use crate::types::block::BlockHandle;
use crate::types::memory_types::{DocumentError, IsolatePolicy, Scope};

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

    /// A non-Option-returning operation targeted a block that does not
    /// exist (string-keyed lookup, no auto-create path).
    ///
    /// Read-style operations (`get_block`, `get_block_metadata`,
    /// `get_rendered_content`) never produce this — they return
    /// `Ok(None)` for missing blocks per their trait contract. This
    /// variant fires on operations that have no `Option` return slot
    /// for "missing": `update_block_metadata`, `persist_block`,
    /// `delete_block`, `undo_redo`, `history_depth`, etc. The `op`
    /// field names which operation raised the error so logs and
    /// diagnostics can disambiguate.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::MemoryError;
    /// use pattern_core::types::memory_types::Scope;
    ///
    /// let err = MemoryError::WriteToMissingBlock {
    ///     scope: Scope::global("agent-7"),
    ///     label: "scratchpad".into(),
    ///     op: "persist_block",
    /// };
    /// assert!(err.to_string().contains("persist_block"));
    /// assert!(err.to_string().contains("scratchpad"));
    /// ```
    #[error("{op}: block does not exist: {scope}/{label}")]
    #[diagnostic(code(pattern_core::memory::write_to_missing_block))]
    WriteToMissingBlock {
        /// The scope that should have owned the (missing) block.
        scope: Scope,
        /// The label that was targeted.
        label: String,
        /// The mutating operation that raised the error
        /// (e.g. `"persist_block"`, `"update_block_metadata"`).
        op: &'static str,
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
        required: crate::types::memory_types::MemoryPermission,
        /// The permission level the block actually has.
        actual: crate::types::memory_types::MemoryPermission,
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
    Database(String),

    /// An error from the Loro CRDT layer.
    #[error("loro error: {0}")]
    #[diagnostic(code(pattern_core::memory::loro))]
    Loro(String),

    /// An error from structured document operations.
    #[error("document error: {0}")]
    #[diagnostic(code(pattern_core::memory::document))]
    Document(#[from] DocumentError),

    /// A task item does not exist in the specified block.
    ///
    /// Returned when attempting to update, transition, or link a task by
    /// reference when the item id does not exist in the target block.
    ///
    /// # Example
    ///
    /// ```
    /// use pattern_core::error::MemoryError;
    /// use pattern_core::types::ids::TaskItemId;
    /// use pattern_core::types::block::BlockHandle;
    ///
    /// let err = MemoryError::TaskNotFound {
    ///     block: BlockHandle::new("sprint-1"),
    ///     item: TaskItemId::from("item-042"),
    /// };
    /// let msg = err.to_string();
    /// assert!(msg.contains("sprint-1"));
    /// assert!(msg.contains("item-042"));
    /// ```
    #[error("task not found: block {block}, item {item}")]
    #[diagnostic(code(pattern_core::memory::task_not_found))]
    TaskNotFound {
        /// The block where the task item was expected.
        block: BlockHandle,
        /// The task item id that was not found.
        item: crate::types::ids::TaskItemId,
    },

    /// The block does not have a TaskList schema.
    ///
    /// Raised when attempting a task-list operation (create_task, update_task,
    /// link, etc.) on a block whose schema is not `BlockSchema::TaskList`.
    #[error("block is not a task list: {block}")]
    #[diagnostic(code(pattern_core::memory::not_a_task_list))]
    NotATaskList {
        /// The block that is not a TaskList.
        block: BlockHandle,
    },

    /// Catch-all for memory operation failures that don't fit other variants.
    #[error("memory operation failed: {0}")]
    #[diagnostic(code(pattern_core::memory::other))]
    Other(String),
}

/// Convenience `Result` alias using [`MemoryError`] as the error type.
///
/// Used throughout `MemoryStore` trait signatures and implementations.
pub type MemoryResult<T> = Result<T, MemoryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_not_found_display_includes_block_and_item() {
        let err = MemoryError::TaskNotFound {
            block: BlockHandle::new("sprint-1"),
            item: crate::types::ids::TaskItemId::from("item-042"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("sprint-1"),
            "error message should contain block: {}",
            msg
        );
        assert!(
            msg.contains("item-042"),
            "error message should contain item: {}",
            msg
        );
    }

    #[test]
    fn not_a_task_list_display_includes_block() {
        let err = MemoryError::NotATaskList {
            block: BlockHandle::new("persona"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("persona"),
            "error message should contain block: {}",
            msg
        );
    }
}
