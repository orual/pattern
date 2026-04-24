//! Task item types for task list memory blocks.
//!
//! This module defines the core types used in `BlockSchema::TaskList` blocks:
//! [`TaskStatus`] for item state, [`TaskComment`] for inline commentary,
//! [`BlockRef`] for typed outgoing edges, and [`TaskItem`] for the full
//! per-item record stored in a `LoroMovableList`.
//!
//! ## Edge model
//!
//! [`TaskItem::blocks`] is the *only* edge storage. Outgoing edges (items that
//! this item blocks) are stored here as [`BlockRef`] values. Reverse lookups
//! (items blocked by this item) happen via the `task_edges` index, which is
//! built in Phase 2 and is not part of this module.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::types::{
    block::BlockHandle,
    ids::AgentId,
    memory_types::task_item_id::{TaskItemId, TaskItemIdError},
};

// region: TaskStatus

/// The lifecycle state of a task item.
///
/// Serialized as kebab-case strings (`"pending"`, `"in-progress"`, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    /// The task has not been started.
    Pending,
    /// The task is actively being worked on.
    InProgress,
    /// The task cannot proceed until an external dependency is resolved.
    Blocked,
    /// The task has been finished successfully.
    Completed,
    /// The task will not be done.
    Cancelled,
}

// endregion: TaskStatus

// region: TaskComment

/// An inline comment on a task item from a specific agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskComment {
    /// The agent who authored this comment.
    pub author: AgentId,
    /// When the comment was written.
    pub timestamp: jiff::Timestamp,
    /// The comment text (may contain markdown).
    pub text: String,
}

// endregion: TaskComment

// region: BlockRef

/// A typed edge from one task item to another block (or a specific item within
/// a block).
///
/// ## Display format
///
/// - Block-level ref: `"<handle>"`
/// - Item-level ref: `"<handle>#<item_id>"`
///
/// ## Serde format
///
/// JSON struct form: `{"block": "...", "task_item": null}` or
/// `{"block": "...", "task_item": "..."}`.
///
/// KDL encoding (for canonical `.kdl` files) is handled separately in
/// `pattern_memory::fs::kdl_task_list` — serde and KDL are distinct surfaces.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockRef {
    /// The handle of the target block.
    pub block: BlockHandle,
    /// If present, narrows the reference to a specific item within the block.
    pub task_item: Option<TaskItemId>,
}

impl fmt::Display for BlockRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.task_item {
            None => write!(f, "{}", self.block),
            Some(id) => write!(f, "{}#{}", self.block, id),
        }
    }
}

impl FromStr for BlockRef {
    type Err = BlockRefParseError;

    /// Parse a [`BlockRef`] from its display form.
    ///
    /// - `"handle"` → block-level ref (no `#` separator).
    /// - `"handle#item_id"` → item-level ref.
    ///
    /// # Errors
    ///
    /// - [`BlockRefParseError::EmptyHandle`] if the handle portion is empty
    ///   (including the bare `""` case and the `"#id"` case).
    /// - [`BlockRefParseError::EmptyItemId`] if a `#` separator is present but
    ///   the item-id portion is empty (`"handle#"`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(hash_pos) = s.find('#') {
            let handle_part = &s[..hash_pos];
            let item_part = &s[hash_pos + 1..];

            if handle_part.is_empty() {
                return Err(BlockRefParseError::EmptyHandle);
            }
            if item_part.is_empty() {
                return Err(BlockRefParseError::EmptyItemId);
            }

            // TaskItemId::parse only fails for empty strings, which we've
            // already guarded against above.
            let task_item = TaskItemId::parse(item_part)
                .map_err(|_: TaskItemIdError| BlockRefParseError::EmptyItemId)?;

            Ok(BlockRef {
                block: SmolStr::new(handle_part),
                task_item: Some(task_item),
            })
        } else {
            // No '#' — this is a block-level ref.
            if s.is_empty() {
                return Err(BlockRefParseError::EmptyHandle);
            }
            Ok(BlockRef {
                block: SmolStr::new(s),
                task_item: None,
            })
        }
    }
}

/// Errors that can occur when parsing a [`BlockRef`] from its display form.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockRefParseError {
    /// The block handle portion was empty.
    #[error("block handle must not be empty")]
    EmptyHandle,
    /// A `#` separator was present but the item-id portion was empty.
    #[error("task item id after '#' must not be empty")]
    EmptyItemId,
}

// endregion: BlockRef

// region: TaskItem

/// A single item within a [`crate::types::memory_types::BlockSchema::TaskList`]
/// block.
///
/// Items are stored in a `LoroMovableList` (keyed by the `id` field) so that
/// agents can reorder them without losing identity.
///
/// ## Edge model
///
/// [`TaskItem::blocks`] is the *only* edge storage. Each [`BlockRef`] in this
/// field represents an outgoing "blocks" relationship: completing this item
/// unblocks the referenced item. Reverse lookups (what blocks *this* item) are
/// provided by the `task_edges` index built in Phase 2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskItem {
    /// Unique identifier for this item (Snowflake; lexicographically sortable).
    pub id: TaskItemId,
    /// Brief imperative description of what needs to be done.
    pub subject: String,
    /// Extended markdown body with context, details, and notes.
    pub description: String,
    /// Current active/working form of the subject (what is actively happening).
    pub active_form: Option<String>,
    /// Lifecycle state of this item.
    pub status: TaskStatus,
    /// Agent responsible for this item (inherits `TaskList.default_owner` when
    /// absent).
    pub owner: Option<AgentId>,
    /// Outgoing "blocks" edges — items that cannot proceed until this one is
    /// done. See module-level note on the single-source-of-truth edge model.
    pub blocks: Vec<BlockRef>,
    /// Freeform JSON metadata (tags, priority, estimates, etc.).
    pub metadata: serde_json::Value,
    /// Inline comments, append-mostly; no deduplication is performed.
    pub comments: Vec<TaskComment>,
    /// When this item was first created.
    pub created_at: jiff::Timestamp,
    /// When this item was last modified.
    pub updated_at: jiff::Timestamp,
}

// endregion: TaskItem

// region: tests

#[cfg(test)]
mod tests {
    use super::*;

    // --- TaskStatus kebab-case round-trips ---

    #[test]
    fn status_pending_round_trips_as_kebab() {
        let status = TaskStatus::Pending;
        let json = serde_json::to_string(&status).expect("serialize TaskStatus::Pending");
        assert_eq!(json, r#""pending""#);
        let recovered: TaskStatus = serde_json::from_str(&json).expect("deserialize pending");
        assert_eq!(recovered, status);
    }

    #[test]
    fn status_in_progress_round_trips_as_kebab() {
        let status = TaskStatus::InProgress;
        let json = serde_json::to_string(&status).expect("serialize TaskStatus::InProgress");
        assert_eq!(json, r#""in-progress""#);
        let recovered: TaskStatus = serde_json::from_str(&json).expect("deserialize in-progress");
        assert_eq!(recovered, status);
    }

    #[test]
    fn status_blocked_round_trips_as_kebab() {
        let status = TaskStatus::Blocked;
        let json = serde_json::to_string(&status).expect("serialize TaskStatus::Blocked");
        assert_eq!(json, r#""blocked""#);
        let recovered: TaskStatus = serde_json::from_str(&json).expect("deserialize blocked");
        assert_eq!(recovered, status);
    }

    #[test]
    fn status_completed_round_trips_as_kebab() {
        let status = TaskStatus::Completed;
        let json = serde_json::to_string(&status).expect("serialize TaskStatus::Completed");
        assert_eq!(json, r#""completed""#);
        let recovered: TaskStatus = serde_json::from_str(&json).expect("deserialize completed");
        assert_eq!(recovered, status);
    }

    #[test]
    fn status_cancelled_round_trips_as_kebab() {
        let status = TaskStatus::Cancelled;
        let json = serde_json::to_string(&status).expect("serialize TaskStatus::Cancelled");
        assert_eq!(json, r#""cancelled""#);
        let recovered: TaskStatus = serde_json::from_str(&json).expect("deserialize cancelled");
        assert_eq!(recovered, status);
    }

    // --- BlockRef::from_str parsing ---

    #[test]
    fn block_ref_from_str_handle_only_yields_block_level() {
        let br: BlockRef = "handle".parse().expect("parse block-level ref");
        assert_eq!(br.block.as_str(), "handle");
        assert!(
            br.task_item.is_none(),
            "block-level ref must have no task_item"
        );
    }

    #[test]
    fn block_ref_from_str_handle_hash_id_yields_item_level() {
        let br: BlockRef = "handle#id123".parse().expect("parse item-level ref");
        assert_eq!(br.block.as_str(), "handle");
        assert_eq!(br.task_item.as_ref().map(|id| id.as_str()), Some("id123"));
    }

    #[test]
    fn block_ref_from_str_empty_string_returns_empty_handle_error() {
        let result: Result<BlockRef, _> = "".parse();
        assert!(
            matches!(result, Err(BlockRefParseError::EmptyHandle)),
            "expected EmptyHandle, got {result:?}"
        );
    }

    #[test]
    fn block_ref_from_str_hash_only_returns_empty_handle_error() {
        let result: Result<BlockRef, _> = "#id".parse();
        assert!(
            matches!(result, Err(BlockRefParseError::EmptyHandle)),
            "expected EmptyHandle for '#id', got {result:?}"
        );
    }

    #[test]
    fn block_ref_from_str_handle_hash_empty_returns_empty_item_id_error() {
        let result: Result<BlockRef, _> = "handle#".parse();
        assert!(
            matches!(result, Err(BlockRefParseError::EmptyItemId)),
            "expected EmptyItemId for 'handle#', got {result:?}"
        );
    }

    // --- BlockRef round-trip via Display + FromStr ---

    #[test]
    fn block_ref_display_parse_round_trips_block_level() {
        let original = BlockRef {
            block: SmolStr::new("my-block"),
            task_item: None,
        };
        let s = original.to_string();
        let recovered: BlockRef = s.parse().expect("round-trip parse must succeed");
        assert_eq!(
            original, recovered,
            "block-level BlockRef must round-trip through Display + FromStr"
        );
    }

    #[test]
    fn block_ref_display_parse_round_trips_item_level() {
        let original = BlockRef {
            block: SmolStr::new("my-block"),
            task_item: Some(TaskItemId::parse("item-abc").unwrap()),
        };
        let s = original.to_string();
        assert_eq!(s, "my-block#item-abc");
        let recovered: BlockRef = s.parse().expect("round-trip parse must succeed");
        assert_eq!(
            original, recovered,
            "item-level BlockRef must round-trip through Display + FromStr"
        );
    }
}

// endregion: tests
