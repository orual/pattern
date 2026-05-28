// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Task item types for task list memory blocks.
//!
//! Defines the core types stored inside `BlockSchema::TaskList` blocks:
//! [`TaskStatus`] for item lifecycle state, [`TaskComment`] for inline
//! commentary, [`TaskEdgeRef`] for typed outgoing block/item references
//! (the task dependency graph), and [`TaskItem`] for the full per-item
//! record stored in a `LoroMovableList`.
//!
//! ## Edge model
//!
//! [`TaskItem::blocks`] is the *only* edge storage. Outgoing edges (items
//! that this item blocks) are stored here as [`TaskEdgeRef`] values.
//! Reverse lookups (items blocked by this item) happen via the
//! `task_edges` index built in Phase 2; that table is a derived view.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::types::{
    block::BlockHandle,
    ids::{AgentId, TaskItemId},
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

impl TaskStatus {
    /// Returns the canonical kebab-case string stored in SQLite.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in-progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = UnknownTaskStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in-progress" => Ok(Self::InProgress),
            "blocked" => Ok(Self::Blocked),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(UnknownTaskStatusError(other.to_owned())),
        }
    }
}

/// Error returned when an unknown task status string is encountered.
#[derive(Debug)]
pub struct UnknownTaskStatusError(pub String);

impl std::fmt::Display for UnknownTaskStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown task status '{}'", self.0)
    }
}

impl std::error::Error for UnknownTaskStatusError {}

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

// region: TaskEdgeRef

/// A typed edge from one task item to another block (or a specific item
/// within a block). Represents the task dependency graph: "this item
/// blocks the referenced target."
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
/// KDL encoding (for canonical `.kdl` files) uses a typed annotation
/// `(block)"handle"` / `(block)"handle#item_id"` and is handled
/// separately in `pattern_memory::fs::kdl_task_list` — serde and KDL
/// are distinct surfaces.
///
/// ## Name
///
/// Named `TaskEdgeRef` rather than `BlockRef` because
/// `pattern_core::types::block_ref::BlockRef` already exists for a
/// different purpose (referencing memory blocks to load into agent
/// context). Keep these straight at import time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskEdgeRef {
    /// The handle of the target block.
    pub block: BlockHandle,
    /// If present, narrows the reference to a specific item within the block.
    pub task_item: Option<TaskItemId>,
}

impl fmt::Display for TaskEdgeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.task_item {
            None => write!(f, "{}", self.block),
            Some(id) => write!(f, "{}#{}", self.block, id),
        }
    }
}

impl FromStr for TaskEdgeRef {
    type Err = TaskEdgeRefParseError;

    /// Parse a [`TaskEdgeRef`] from its display form.
    ///
    /// - `"handle"` → block-level ref.
    /// - `"handle#item_id"` → item-level ref.
    ///
    /// # Errors
    ///
    /// - [`TaskEdgeRefParseError::EmptyHandle`] if the handle is empty
    ///   (bare `""` or `"#id"`).
    /// - [`TaskEdgeRefParseError::EmptyItemId`] if a `#` separator is
    ///   present but the item-id chunk is empty (`"handle#"`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(hash_pos) = s.find('#') {
            let handle_part = &s[..hash_pos];
            let item_part = &s[hash_pos + 1..];
            if handle_part.is_empty() {
                return Err(TaskEdgeRefParseError::EmptyHandle);
            }
            if item_part.is_empty() {
                return Err(TaskEdgeRefParseError::EmptyItemId);
            }
            Ok(TaskEdgeRef {
                block: SmolStr::new(handle_part),
                task_item: Some(SmolStr::new(item_part)),
            })
        } else {
            if s.is_empty() {
                return Err(TaskEdgeRefParseError::EmptyHandle);
            }
            Ok(TaskEdgeRef {
                block: SmolStr::new(s),
                task_item: None,
            })
        }
    }
}

/// Errors that can occur when parsing a [`TaskEdgeRef`] from its display form.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TaskEdgeRefParseError {
    /// The block handle portion was empty.
    #[error("block handle must not be empty")]
    EmptyHandle,
    /// A `#` separator was present but the item-id portion was empty.
    #[error("task item id after '#' must not be empty")]
    EmptyItemId,
}

// endregion: TaskEdgeRef

// region: TaskItem

/// A single item within a [`crate::types::memory_types::BlockSchema::TaskList`]
/// block.
///
/// Items are stored in a `LoroMovableList` so agents can reorder them
/// without losing identity.
///
/// ## Edge model
///
/// [`TaskItem::blocks`] is the *only* edge storage. Each [`TaskEdgeRef`]
/// in this field represents an outgoing "blocks" relationship:
/// completing this item unblocks the referenced item. Reverse lookups
/// are provided by the `task_edges` index built in Phase 2.
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
    /// Agent responsible for this item (inherits `TaskList.default_owner`
    /// when absent).
    pub owner: Option<AgentId>,
    /// Outgoing "blocks" edges — items that cannot proceed until this one
    /// is done. See module-level note on the single-source-of-truth edge
    /// model.
    pub blocks: Vec<TaskEdgeRef>,
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
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""pending""#);
        assert_eq!(serde_json::from_str::<TaskStatus>(&json).unwrap(), status);
    }

    #[test]
    fn status_in_progress_round_trips_as_kebab() {
        let status = TaskStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""in-progress""#);
        assert_eq!(serde_json::from_str::<TaskStatus>(&json).unwrap(), status);
    }

    #[test]
    fn status_blocked_round_trips_as_kebab() {
        let status = TaskStatus::Blocked;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""blocked""#);
        assert_eq!(serde_json::from_str::<TaskStatus>(&json).unwrap(), status);
    }

    #[test]
    fn status_completed_round_trips_as_kebab() {
        let status = TaskStatus::Completed;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""completed""#);
        assert_eq!(serde_json::from_str::<TaskStatus>(&json).unwrap(), status);
    }

    #[test]
    fn status_cancelled_round_trips_as_kebab() {
        let status = TaskStatus::Cancelled;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, r#""cancelled""#);
        assert_eq!(serde_json::from_str::<TaskStatus>(&json).unwrap(), status);
    }

    // --- TaskEdgeRef::from_str parsing ---

    #[test]
    fn task_edge_ref_from_str_handle_only_yields_block_level() {
        let r: TaskEdgeRef = "handle".parse().unwrap();
        assert_eq!(r.block.as_str(), "handle");
        assert!(r.task_item.is_none());
    }

    #[test]
    fn task_edge_ref_from_str_handle_hash_id_yields_item_level() {
        let r: TaskEdgeRef = "handle#id123".parse().unwrap();
        assert_eq!(r.block.as_str(), "handle");
        assert_eq!(r.task_item.as_deref(), Some("id123"));
    }

    #[test]
    fn task_edge_ref_from_str_empty_string_returns_empty_handle_error() {
        let result: Result<TaskEdgeRef, _> = "".parse();
        assert!(matches!(result, Err(TaskEdgeRefParseError::EmptyHandle)));
    }

    #[test]
    fn task_edge_ref_from_str_hash_only_returns_empty_handle_error() {
        let result: Result<TaskEdgeRef, _> = "#id".parse();
        assert!(matches!(result, Err(TaskEdgeRefParseError::EmptyHandle)));
    }

    #[test]
    fn task_edge_ref_from_str_handle_hash_empty_returns_empty_item_id_error() {
        let result: Result<TaskEdgeRef, _> = "handle#".parse();
        assert!(matches!(result, Err(TaskEdgeRefParseError::EmptyItemId)));
    }

    // --- TaskEdgeRef round-trip via Display + FromStr ---

    #[test]
    fn task_edge_ref_display_parse_round_trips_block_level() {
        let original = TaskEdgeRef {
            block: SmolStr::new("my-block"),
            task_item: None,
        };
        let recovered: TaskEdgeRef = original.to_string().parse().unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn task_edge_ref_display_parse_round_trips_item_level() {
        let original = TaskEdgeRef {
            block: SmolStr::new("my-block"),
            task_item: Some(SmolStr::new("item-abc")),
        };
        let s = original.to_string();
        assert_eq!(s, "my-block#item-abc");
        let recovered: TaskEdgeRef = s.parse().unwrap();
        assert_eq!(original, recovered);
    }

    // --- TaskItem JSON round-trip (AC1.2 coverage) ---

    /// Construct a fixture timestamp at a known epoch second for
    /// deterministic serialization comparison.
    fn fixture_ts(secs: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(secs).expect("valid fixture timestamp")
    }

    #[test]
    fn task_item_full_json_round_trip() {
        // TaskItem with every field populated, including both a block-level
        // TaskEdgeRef and an item-level TaskEdgeRef.
        let item = TaskItem {
            id: SmolStr::new("01JADT00000FULLITEM00000001"),
            subject: String::from("write the spec"),
            description: String::from("Draft the initial architecture document."),
            active_form: Some(String::from("drafting architecture section 3")),
            status: TaskStatus::InProgress,
            owner: Some(SmolStr::new("agent-r")),
            blocks: vec![
                TaskEdgeRef {
                    block: SmolStr::new("alpha-block"),
                    task_item: None,
                },
                TaskEdgeRef {
                    block: SmolStr::new("beta-block"),
                    task_item: Some(SmolStr::new("01JADT00000BETAITEM0000001")),
                },
            ],
            metadata: serde_json::json!({"priority": "high", "estimate_hours": 3.5}),
            comments: vec![TaskComment {
                author: SmolStr::new("agent-r"),
                timestamp: fixture_ts(1_750_000_000),
                text: String::from("blocking on design review"),
            }],
            created_at: fixture_ts(1_749_000_000),
            updated_at: fixture_ts(1_750_000_000),
        };

        let json = serde_json::to_string(&item).expect("serialise TaskItem");
        let recovered: TaskItem = serde_json::from_str(&json).expect("deserialise TaskItem");
        assert_eq!(item, recovered);

        // Spot-check key fields survive the wire.
        assert_eq!(recovered.blocks.len(), 2);
        assert!(recovered.blocks[0].task_item.is_none());
        assert!(recovered.blocks[1].task_item.is_some());
        assert_eq!(recovered.status, TaskStatus::InProgress);
        assert_eq!(recovered.comments.len(), 1);
    }

    #[test]
    fn task_item_empty_blocks_and_comments_round_trip() {
        // Empty slices must not produce null or missing fields in JSON.
        let item = TaskItem {
            id: SmolStr::new("01JADT00000EMPTY00000000001"),
            subject: String::from("a bare task"),
            description: String::new(),
            active_form: None,
            status: TaskStatus::Pending,
            owner: None,
            blocks: vec![],
            metadata: serde_json::Value::Null,
            comments: vec![],
            created_at: fixture_ts(1_749_000_000),
            updated_at: fixture_ts(1_749_000_000),
        };

        let json = serde_json::to_string(&item).expect("serialise TaskItem");
        let recovered: TaskItem = serde_json::from_str(&json).expect("deserialise TaskItem");
        assert_eq!(item, recovered);

        // Confirm the decoded JSON agrees: empty vecs decode as arrays, not null.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse as Value");
        assert!(v["blocks"].is_array(), "blocks field must be a JSON array");
        assert!(
            v["comments"].is_array(),
            "comments field must be a JSON array"
        );
        assert_eq!(v["blocks"].as_array().unwrap().len(), 0);
        assert_eq!(v["comments"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn task_item_self_edge_round_trip() {
        // An item whose blocks list contains a TaskEdgeRef pointing at its
        // own id within its own block. The serde layer is unaware of graph
        // semantics; this must round-trip without error (anchors AC1.6).
        let own_id = SmolStr::new("01JADT00000SELFREF00000001");
        let own_block = SmolStr::new("my-task-list");

        let item = TaskItem {
            id: own_id.clone(),
            subject: String::from("complete self-referential task"),
            description: String::from("This item lists itself as a blocker."),
            active_form: None,
            status: TaskStatus::Blocked,
            owner: None,
            blocks: vec![TaskEdgeRef {
                block: own_block.clone(),
                task_item: Some(own_id.clone()),
            }],
            metadata: serde_json::Value::Null,
            comments: vec![],
            created_at: fixture_ts(1_749_000_000),
            updated_at: fixture_ts(1_749_000_000),
        };

        let json = serde_json::to_string(&item).expect("serialise self-edge TaskItem");
        let recovered: TaskItem =
            serde_json::from_str(&json).expect("deserialise self-edge TaskItem");
        assert_eq!(item, recovered);

        // Confirm the self-reference survives intact.
        let edge = &recovered.blocks[0];
        assert_eq!(edge.block, own_block);
        assert_eq!(edge.task_item.as_deref(), Some(own_id.as_str()));
    }

    // --- TaskComment UTF-8 + multiline round-trip (AC1.2 coverage) ---

    #[test]
    fn task_comment_multiline_utf8_round_trip() {
        // Text contains:
        //   - ASCII + newline (multiline)
        //   - emoji: 🧠 (U+1F9E0, BRAIN)
        //   - combining mark: e\u{030A} (e + combining ring above, looks like å)
        //   - CJK: 考 (U+8003)
        //   - right-to-left: مرحبا (Arabic "hello")
        let complex_text = "line one\nline two 🧠\ne\u{030A}考مرحبا\n";
        let comment = TaskComment {
            author: SmolStr::new("agent-x"),
            timestamp: fixture_ts(1_750_000_000),
            text: String::from(complex_text),
        };

        let json = serde_json::to_string(&comment).expect("serialise TaskComment");
        let recovered: TaskComment = serde_json::from_str(&json).expect("deserialise TaskComment");
        assert_eq!(comment, recovered);

        // Confirm the text survived character-for-character.
        assert_eq!(recovered.text, complex_text);
        assert!(recovered.text.contains('🧠'));
        assert!(recovered.text.contains('\n'));
        // The combining mark sequence must remain intact (not collapsed or escaped).
        assert!(recovered.text.contains("e\u{030A}"));
    }
}

// endregion: tests
