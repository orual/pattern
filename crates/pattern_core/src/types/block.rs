//! Block value type and handle for memory storage.
//!
//! A [`Block`] is the content retrieved from memory storage — it carries both
//! the rendered text and the metadata needed for agents to refer to and update
//! that content. A [`BlockHandle`] is the lightweight identifier an agent uses
//! to name a block in tool calls and context references.
//!
//! # Relationship to `memory::store`
//!
//! `BlockMetadata` in `memory::store` is an implementation-level type for the
//! V2 cache/DB layer. `Block` and `BlockHandle` here are the value-level types
//! that cross trait boundaries and appear in `TurnOutput::block_writes`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A lightweight, stable identifier for a memory block as seen by agents.
///
/// Agents refer to blocks by their handle in tool calls and context rendering.
/// The handle is stable across edits; the block's content may change while the
/// handle remains constant.
///
/// # Examples
///
/// ```
/// use pattern_core::types::block::BlockHandle;
///
/// let h = BlockHandle::new("persona");
/// assert_eq!(h.as_str(), "persona");
/// let h2: BlockHandle = "task_list".into();
/// assert_ne!(h, h2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct BlockHandle(pub String);

impl BlockHandle {
    /// Create a new `BlockHandle` from any string label.
    pub fn new(label: impl Into<String>) -> Self {
        BlockHandle(label.into())
    }

    /// Borrow the inner label string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BlockHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for BlockHandle {
    fn from(s: String) -> Self {
        BlockHandle(s)
    }
}

impl From<&str> for BlockHandle {
    fn from(s: &str) -> Self {
        BlockHandle(s.to_string())
    }
}

impl From<BlockHandle> for String {
    fn from(h: BlockHandle) -> Self {
        h.0
    }
}

/// The content and metadata of a memory block retrieved from storage.
///
/// `Block` is the value type that crosses the memory trait boundary — it is
/// what the context composer renders into the agent's prompt and what
/// `TurnOutput::block_writes` references after a turn completes.
///
/// For the mutable, cache-backed document used during editing, see
/// `memory::store::StructuredDocument`.
///
/// # Examples
///
/// ```
/// use pattern_core::types::block::{Block, BlockHandle};
///
/// let block = Block {
///     handle: BlockHandle::new("persona"),
///     label: "Persona".to_string(),
///     content: "I am a helpful assistant.".to_string(),
///     char_limit: Some(2000),
/// };
/// assert_eq!(block.handle.as_str(), "persona");
/// assert!(block.content.contains("helpful"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    /// Stable identifier for this block.
    pub handle: BlockHandle,
    /// Human-readable display label (shown in context headers).
    pub label: String,
    /// Rendered text content ready for prompt injection.
    pub content: String,
    /// Optional character limit; `None` means unlimited.
    pub char_limit: Option<usize>,
}

/// A pending write to a memory block, recorded in `TurnOutput`.
///
/// Block writes are applied after a turn completes so that the change log is
/// available for pseudo-message emission (Phase 5) and checkpointing (Phase 3).
///
/// # Examples
///
/// ```
/// use pattern_core::types::block::{BlockHandle, BlockWrite};
///
/// let write = BlockWrite {
///     handle: BlockHandle::new("task_list"),
///     new_content: "- [ ] Review PR\n- [x] Write tests".to_string(),
/// };
/// assert!(write.new_content.contains("Review PR"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockWrite {
    /// The block that was written to.
    pub handle: BlockHandle,
    /// Replacement content after the write.
    pub new_content: String,
}
