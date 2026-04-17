//! Block identifier alias and post-turn `BlockWrite` audit record.
//!
//! Pattern agents name memory blocks by a human-chosen label (`"persona"`,
//! `"task_list"`, etc.). That label is the [`BlockHandle`]. The full block
//! state — content, schema, metadata, permissions — lives on
//! [`crate::memory::StructuredDocument`], which the memory trait surface
//! returns directly. The context composer renders blocks via
//! `MemoryStore::get_rendered_content(agent_id, label)` (owned blocks) and
//! `StructuredDocument::render()` (shared blocks); this module therefore does
//! not define a parallel `Block` value type.
//!
//! A [`BlockWrite`] is the post-turn audit record of a memory change,
//! attached to [`crate::types::turn::TurnOutput::block_writes`]. Phase 5's
//! pseudo-message emission renders one `[memory:written]` or
//! `[memory:updated]` pseudo-message per `BlockWrite`; the record is
//! intentionally self-contained so emission does not need to re-query memory
//! at display time.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::memory::BlockType;
use crate::types::ids::MemoryId;
use crate::types::origin::Author;

/// A lightweight, stable identifier for a memory block as seen by agents.
///
/// Agents refer to blocks by handle in tool calls and context references. The
/// handle is the human-chosen label (`"persona"`, `"task_list"`, …), stable
/// across edits; the block's content may change while the handle remains
/// constant. Distinct from [`MemoryId`], which is the DB row identifier.
///
/// Like the other identifier types in [`crate::types::ids`], `BlockHandle` is
/// a [`SmolStr`] alias — cheap to clone (Arc-sharing beyond the inline cap),
/// no newtype ceremony.
///
/// # Examples
///
/// ```
/// use pattern_core::types::block::BlockHandle;
/// use smol_str::SmolStr;
///
/// let h: BlockHandle = SmolStr::new("persona");
/// assert_eq!(h.as_str(), "persona");
/// ```
pub type BlockHandle = SmolStr;

/// Classification of a write recorded by [`BlockWrite`].
///
/// Mirrors the shape Phase 5's pseudo-message emission expects: `Created` and
/// `Replaced` both map to `[memory:written]`; `Appended` and `Updated` map to
/// `[memory:updated]` with diff-style rendering; `Deleted` is reserved for
/// future tombstone emission.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockWriteKind {
    /// Block was newly created this turn.
    Created,
    /// Block's entire content was replaced with a new value.
    Replaced,
    /// New content was appended to the existing content.
    Appended,
    /// A structured-schema block was updated without a full replace.
    Updated,
    /// Block was deleted (soft-delete in storage; see `pattern_db` for
    /// retention semantics).
    Deleted,
}

/// A post-turn audit record of a memory-block write.
///
/// Attached to [`crate::types::turn::TurnOutput::block_writes`] so that
/// pseudo-message emission (Phase 5) and checkpoint replay (Phase 3) can
/// reconstruct what the turn did to memory without re-querying the store at
/// display time. The record is intentionally self-contained:
///
/// - `rendered_content` is the text representation ready for
///   `[memory:written]` / `[memory:updated]` pseudo-message bodies.
/// - `previous_content_hash` (when present) lets diff-style rendering decide
///   between "this content was written fresh" and "this content changed from
///   something else," without requiring the storage layer to be queried for
///   the pre-write state.
/// - Full post-write state can be re-fetched from memory via `memory_id` or
///   `(handle, agent)` lookup when a caller wants the richer
///   [`crate::memory::StructuredDocument`] (with schema, permissions, Loro
///   history, etc.). Detached snapshots can be obtained by forking the
///   document.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use smol_str::SmolStr;
///
/// use pattern_core::memory::BlockType;
/// use pattern_core::types::block::{BlockHandle, BlockWrite, BlockWriteKind};
/// use pattern_core::types::origin::{Author, SystemReason};
///
/// let handle: BlockHandle = SmolStr::new("task_list");
/// let write = BlockWrite {
///     handle,
///     memory_id: SmolStr::new("mem_01HXYZ"),
///     block_type: BlockType::Working,
///     rendered_content: "- [ ] Review PR\n- [x] Write tests".to_string(),
///     kind: BlockWriteKind::Appended,
///     previous_content_hash: Some(0xdead_beef_dead_beef),
///     at: Timestamp::now(),
///     author: Author::System { reason: SystemReason::ToolCall },
/// };
/// assert!(write.rendered_content.contains("Review PR"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockWrite {
    /// Human-chosen label for the block the agent writes to.
    pub handle: BlockHandle,
    /// DB row identifier for the block (for re-fetch of full state).
    pub memory_id: MemoryId,
    /// Whether the block is Core, Working, or Archival.
    pub block_type: BlockType,
    /// Rendered text content after the write, ready for pseudo-message
    /// display. Derived from the underlying [`crate::memory::StructuredDocument`]
    /// at write time so display does not need to re-query memory.
    pub rendered_content: String,
    /// Classification of the write (created / replaced / appended / ...).
    pub kind: BlockWriteKind,
    /// Hash of the content before this write, when applicable. `None` for
    /// [`BlockWriteKind::Created`]; `Some(_)` for updates that carry a
    /// pre-write baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_content_hash: Option<u64>,
    /// Wall-clock time the write occurred (UTC instant via `jiff`).
    pub at: Timestamp,
    /// Who authored the write, using the shared `MessageOrigin` author
    /// surface so anti-loop / trust policies have structural access to the
    /// originator.
    pub author: Author,
}
