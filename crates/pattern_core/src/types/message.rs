//! Message value type: a thin wrapper around `genai::chat::ChatMessage` with
//! pattern-specific identity, ownership, ordering, batch membership, and
//! response metadata.
//!
//! ## Attachments
//!
//! [`MessageAttachment`]s are pattern-level metadata that render as content onto
//! the wire at compose-time but are NOT part of the stored `ChatMessage`
//! structure. This keeps the conversational record clean while the wire still
//! gets ephemeral context reminders (e.g. memory snapshots). Attachments are
//! only set on batch-initiating user messages; other messages carry empty
//! attachment vecs.

use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::types::block_ref::BlockRef;
use crate::types::ids::{AgentId, BatchId, MessageId};
use crate::types::memory_types::MemoryBlockType;
use genai::ModelIden;
use genai::chat::Usage;

/// A message in the agent's conversation log.
///
/// Wraps a `genai::chat::ChatMessage` (which carries role/content/options) and
/// adds pattern-specific metadata: identity, ownership, ordering, batch
/// membership, optional per-response metadata for assistant messages, and
/// memory block references to load when this message is in-context.
///
/// ## Identifier fields
///
/// - `id` — unique identifier (UUID). Used for deduplication and DB primary key.
/// - `position` — lex-sortable ordering key (snowflake, base32-encoded). Used
///   by pattern_db's `messages.position` column for absolute ordering and by
///   `archive_messages` for range comparisons. Generated via
///   [`crate::types::ids::new_snowflake_id`] at message creation.
/// - `created_at` — human-readable wall-clock timestamp (nanosecond precision).
///   Retained for display and auditing; the snowflake timestamp has only
///   millisecond resolution.
///
/// Ordering:
/// - Within a batch: by `position`.
/// - Across batches: by the first message's `position`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub chat_message: genai::chat::ChatMessage,
    /// Unique identifier (UUID). Used for deduplication and DB primary key.
    pub id: MessageId,
    /// Lex-sortable ordering key (snowflake, base32-encoded). Populated via
    /// [`crate::types::ids::new_snowflake_id`] at message creation. Used by pattern_db for
    /// absolute ordering (`messages.position` column) and by
    /// `archive_messages` for range comparisons.
    pub position: SmolStr,
    pub owner_id: AgentId,
    /// Human-readable wall-clock timestamp (nanosecond precision). Retained
    /// for display and auditing; snowflake timestamp has only ms resolution.
    pub created_at: Timestamp,
    pub batch: BatchId,
    /// Populated for assistant messages that originated from a `ChatResponse`.
    /// `None` for user and tool messages.
    pub response_meta: Option<ResponseMeta>,
    /// Memory blocks to load for this message's context.
    /// `skip_serializing_if` deliberately omitted — Message crosses postcard wire
    /// (via `MessageAttachment` reachable from `pattern_core::wire::ui`) and
    /// postcard is positional, so skipping fields corrupts the decoder.
    #[serde(default)]
    pub block_refs: Vec<BlockRef>,
    /// Pattern-level attachments. Rendered into `ChatMessage.content` at
    /// compose-time. NOT persisted in `ChatMessage` itself — keeps the
    /// conversational record clean. Only set on batch-initiating user
    /// messages; other messages have empty attachments.
    ///
    /// `skip_serializing_if` deliberately omitted — postcard-positional wire compat.
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
}

/// Output event from a spawned shell process, carried by
/// [`MessageAttachment::ShellOutput`].
///
/// Defined next to `MessageAttachment` for locality. `Backgrounded` is
/// forward-compat for the future per-execute subshell model where
/// `Shell.Execute` timeout transitions to background rather than kill; it
/// is **never enqueued** by any code path under the current v2-semantics
/// decision (Amendment 2026-04-26, phase_03.md). Keep it defined so a future
/// phase can emit it without a breaking schema change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShellOutputKind {
    /// Streaming output chunk from a spawned process.
    Output(String),
    /// Process exited; final delivery on the bridge. Always the last chunk
    /// for a given `task_id`.
    Exit {
        /// OS exit code. `None` if the process was killed or the code could
        /// not be parsed.
        code: Option<i32>,
        /// Wall-clock elapsed since the process was spawned, in milliseconds.
        duration_ms: u64,
    },
    /// Forward-compat sentinel for the future per-execute subshell model
    /// where `Shell.Execute` timeout transitions to background. Currently
    /// unused — no code path enqueues this variant under the v2-semantics
    /// decision (phase_03.md AC3.7 amendment 2026-04-26). Until then, agents
    /// that need long-running execution should use `Shell.Spawn`.
    Backgrounded {
        /// Output captured before the timeout fired.
        partial_output: String,
    },
}

/// Pattern-level metadata that renders as content onto the wire at compose-time
/// but is not part of the stored `ChatMessage` structure. Exists so the
/// conversational record stays uncontaminated by ephemeral context reminders,
/// while the wire still receives them.
///
/// Attachments are **write-once**: once attached to a `Message`, they are
/// never updated. The splice machinery in `agent_loop` renders attachment
/// content deterministically into wire-content at compose-time. This is the
/// cache-stability story — a message's wire bytes stay stable across turns
/// because the attachments don't mutate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessageAttachment {
    /// Memory snapshot attached to a batch-initiating user message
    /// (or to mid-batch tool_result messages when external memory
    /// changes are detected).
    BatchOpeningSnapshot {
        /// Whether this is a full dump or a delta since a prior batch.
        kind: SnapshotKind,
        /// All blocks' labels currently available to this agent. Always
        /// present in both Full and Delta so the model knows the
        /// complete block namespace.
        block_names: Vec<SmolStr>,
        /// For Full: rendered content of ALL blocks.
        /// For Delta: rendered content of blocks that changed since
        /// prior batch.
        blocks: Vec<RenderedBlock>,
        /// For Delta: labels of blocks edited since prior batch. Empty
        /// for Full.
        edited_blocks: Vec<SmolStr>,
    },
    /// A skill became autonomously available to the agent (e.g. a plugin
    /// auto-installed it). Renders as a `<system-reminder>`-wrapped
    /// `[skill:available]` marker showing the frontmatter so the agent
    /// learns it exists and can decide to call `Skills.Load`. Carries
    /// metadata only — NOT the body — to keep wire bytes small and the
    /// attachment cache-stable.
    SkillAvailable {
        /// The skill's block handle, used for subsequent `Skills.Load` calls.
        handle: SmolStr,
        /// Author-declared name from the skill's YAML frontmatter.
        name: String,
        /// Effective trust tier (post-policy enforcement, kebab-case
        /// when rendered).
        trust_tier: crate::types::memory_types::SkillTrustTier,
        /// Optional one-line description from frontmatter.
        description: Option<String>,
        /// Keywords from frontmatter.
        keywords: Vec<String>,
    },
    /// Caller-rendered text. The splice path inlines `content` verbatim
    /// onto the host message; the caller is responsible for any wrapping
    /// (e.g. `<system-reminder>` markers) it wants.
    ///
    /// Use this for one-off notifications that don't fit a typed variant.
    /// New recurring patterns should get their own typed variant for
    /// refactoring resistance and structured analytics.
    Custom {
        /// Pre-rendered text. Spliced verbatim into the host message's
        /// content. Caller handles all formatting.
        content: String,
    },
    /// An external edit was detected on a file the agent has open or is
    /// watching. Queued by file-manager listener threads into the
    /// between-turn async-reminder buffer; the compose-time drain
    /// splices it onto the next turn's first user message.
    ///
    /// The renderer (Task 8) converts this into a `<system-reminder>`
    /// block showing the path and edit kind.
    FileEdit {
        /// Absolute path to the changed file.
        path: std::path::PathBuf,
        /// Whether the file was opened for editing or watched read-only.
        kind: FileEditKind,
        /// When the external edit was detected.
        at: jiff::Timestamp,
        /// Optional unified diff of the change. `None` for watch-only
        /// files and until Task 8 wires the diff payload.
        diff: Option<String>,
    },
    /// An external edit conflicted with the agent's unsaved CRDT state
    /// under `RejectAndNotify` policy. The agent must call `File.Reload`
    /// or `File.ForceWrite` to resolve.
    ///
    /// The renderer (Task 8) converts this into a `<system-reminder>`
    /// block showing the path and conflict details.
    FileConflict {
        /// Absolute path to the conflicted file.
        path: std::path::PathBuf,
        /// When the conflict was detected.
        at: jiff::Timestamp,
    },
    /// Memory block writes that occurred during a turn. Attached to the
    /// message that executed the writes (typically the tool_result that
    /// closed out the dispatch). Replaces the old pseudo-message path
    /// where `Segment2Pass` rendered `BlockWrite`s as standalone
    /// synthetic `ChatMessage`s.
    ///
    /// The compose-time renderer converts this into a
    /// `<system-reminder>` block showing what changed, using the same
    /// body format as the retired `render_change_events` pseudo-message
    /// renderer.
    BlockWriteNotifications {
        /// The block writes that occurred. Rendered as a group into a
        /// single `<system-reminder>` block at compose time.
        writes: Vec<crate::types::block::BlockWrite>,
    },
    /// One shell output event from a spawned process. The bridge thread
    /// (Task 7) enqueues one of these per `OutputChunk` arriving from the
    /// PTY; the compose-time drain splices them onto the next turn's first
    /// user message.
    ///
    /// `Output` chunks carry live stdout/stderr text. `Exit` is the final
    /// chunk signalling process completion. `Backgrounded` is forward-compat
    /// and is currently never enqueued (see [`ShellOutputKind`]).
    ShellOutput {
        /// Stable task identifier assigned at `Shell.Spawn` time.
        task_id: String,
        /// The event kind: streaming output, exit, or (future) background
        /// sentinel.
        kind: ShellOutputKind,
        /// When this event was enqueued by the bridge thread.
        at: jiff::Timestamp,
    },

    /// One subscription event delivered by a `Pattern.Port.Subscribe` stream
    /// (Phase 4). The dispatcher actor's per-subscription drain task builds
    /// these from the `BoxStream<PortEvent>` returned by the `Port` impl's
    /// `subscribe()` and pushes them onto the session's async-reminder
    /// buffer; compose-time drain on the next turn splices them onto the
    /// first user message and `Segment2Pass` renders each one as a
    /// `<system-reminder>` block.
    ///
    /// The `port_id` is the registered port handle (string form of
    /// `pattern_core::types::port::PortId`) — not the raw event source's
    /// internal id, in case those ever diverge.
    PortEvent {
        /// Registered port id (e.g. `"http"`, `"slack"`, `"weather-api"`).
        port_id: String,
        /// Opaque event payload. Interpretation is port-specific.
        payload: serde_json::Value,
        /// When the event was enqueued by the dispatcher's drain task.
        at: jiff::Timestamp,
    },
    /// Provenance hint for a user message — who authored it and from which
    /// surface. Built by `build_turn_input` from the user-message's
    /// [`MessageOrigin`] when `transport_hint` is set (typically from
    /// non-TUI surfaces like the discord plugin).
    ///
    /// Composer renders this as a structured fenced block alongside the
    /// message content so the LLM can distinguish source (DM vs channel,
    /// TUI vs discord, partner vs human) without prompt-injection risk
    /// from inlining surface-supplied strings into the content itself.
    OriginHint {
        /// Typed author (Partner / Human / Agent / Plugin / System).
        /// Rendered from the trusted-runtime variant; doesn't carry
        /// surface-supplied text into the prompt.
        author: crate::types::origin::Author,
        /// Surface label (e.g. `"discord:DM:orual"`, `"discord:channel:123"`).
        /// Plugin-supplied — rendered as data inside the fenced block,
        /// not as content. Newlines stripped at render time.
        transport_hint: Option<smol_str::SmolStr>,
    },
}

/// Whether an external edit notification is for a file the agent has
/// opened for editing or is watching read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileEditKind {
    /// File was opened via `File.Open` — agent has an active CRDT doc.
    Open,
    /// File was registered via `File.Watch` — read-only observation.
    Watch,
}

/// Whether a [`MessageAttachment::BatchOpeningSnapshot`] is a full memory
/// dump or a delta since a prior batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SnapshotKind {
    /// Full memory dump. Used for: first batch of session,
    /// post-compaction, periodic refresh every N batches.
    Full,
    /// Delta since prior batch. Used in normal case.
    Delta {
        /// The batch_id that this delta is expressed against.
        since_batch: BatchId,
    },
}

/// A pre-rendered memory block carried inside a [`MessageAttachment`].
/// Contains the label, rendered text, and a content hash for
/// delta-comparison across batches.
///
/// Uses `Arc<str>` for content (O(1) clone since block content can be
/// large) and `SmolStr` for label (inlines short strings). This struct
/// is immutable once constructed -- unlike `StructuredDocument` it does
/// not share a live `LoroDoc` with the memory cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedBlock {
    /// Block label (e.g. "persona", "task_list"). SmolStr for cheap
    /// inline storage of short labels.
    pub label: SmolStr,
    /// Block type at snapshot time.
    pub block_type: MemoryBlockType,
    /// Rendered content when this block is meant to be surfaced on the
    /// wire. `None` means "tracked but silent" -- hash is present for
    /// delta detection but wire rendering skips this block.
    /// `Arc<str>` for O(1) clone when present.
    pub rendered: Option<Arc<str>>,
    /// Stable content hash of the rendered text, used for delta
    /// comparison. Computed from the rendered bytes at snapshot time.
    pub content_hash: u64,
}

/// Policy for selecting which blocks appear in a snapshot attachment.
///
/// Applied during both Full and Delta construction. Default includes
/// Core and Working blocks; Archival (searchable on-demand) and Log
/// (high-volume) are excluded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSelection {
    /// Block types to include. Default: `[Core, Working]`.
    pub include_types: Vec<MemoryBlockType>,
    /// Explicit block-label allowlist. If empty, include all blocks
    /// matching `include_types`. If non-empty, restrict to these
    /// labels regardless of type.
    pub include_labels: Vec<SmolStr>,
    /// Explicit label exclusions (applied after include_types /
    /// include_labels). Useful for opting specific blocks out.
    pub exclude_labels: Vec<SmolStr>,
}

impl Default for SnapshotSelection {
    fn default() -> Self {
        Self {
            include_types: vec![MemoryBlockType::Core, MemoryBlockType::Working],
            include_labels: Vec::new(),
            exclude_labels: Vec::new(),
        }
    }
}

/// Full snapshot policy: which blocks to include + how to handle mid-batch
/// deltas on tool_use continuation turns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotPolicy {
    /// Block-selection filter for both Full and Delta snapshot construction.
    pub selection: SnapshotSelection,
    /// Controls whether a turn's own tool-initiated block writes trigger
    /// mid-batch delta attachments.
    pub mid_batch: MidBatchDeltaBehavior,
}

/// How to handle memory changes detected mid-batch (between wire turns
/// within a single `Session::step`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidBatchDeltaBehavior {
    /// Emit delta for ALL changes detected mid-batch, including this
    /// turn's own tool-initiated writes. Gives the agent post-edit block
    /// state so it can verify its changes landed correctly. Cache-costly:
    /// every memory-editing turn busts segment 3 for that turn. Choose
    /// this when agents don't trust minimal tool_result confirmations.
    ///
    /// Default — preserves current behavior + strongest agent trust signal
    /// pending empirical data on whether agents need it.
    #[default]
    IncludeSelfEdits,

    /// Emit delta only for changes NOT attributable to this turn's own
    /// `block_writes` (i.e., changes from other agents, data sources, or
    /// operator edits). Cache-efficient: intra-batch turns stay cacheable
    /// unless something external happens. Agent relies on tool_result
    /// content to verify edits landed.
    FilterSelfEdits,
}

impl SnapshotSelection {
    /// Test whether a block with the given label and type passes the
    /// selection filter.
    pub fn accepts(&self, label: &str, block_type: MemoryBlockType) -> bool {
        // Check exclude list first.
        if self.exclude_labels.iter().any(|l| l.as_str() == label) {
            return false;
        }
        // If include_labels is non-empty, restrict to those.
        if !self.include_labels.is_empty() {
            return self.include_labels.iter().any(|l| l.as_str() == label);
        }
        // Otherwise, check include_types.
        self.include_types.contains(&block_type)
    }
}

/// Per-response metadata harvested from `genai::chat::ChatResponse` at the
/// moment the assistant message was constructed.
///
/// One `ChatResponse` per assistant message; a batch may contain many assistant
/// messages (one per tool-call round-trip), each carrying its own metadata.
///
/// Note: genai does not expose a stop reason or response ID in its current
/// API. Fields here reflect what `ChatResponse` actually provides. If genai
/// adds these in future, this type should be updated accordingly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub usage: Usage,
    pub reasoning_content: Option<String>,
    pub model_iden: ModelIden,
    pub provider_model_iden: ModelIden,
}
