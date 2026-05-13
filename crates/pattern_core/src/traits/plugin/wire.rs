//! Wire types for plugin IRPC protocols (Phase 6 of v3-extensibility).
//!
//! These types serialize through postcard at the IRPC boundary. They mirror
//! pattern_core's domain types but with two constraints:
//!
//! 1. **Postcard-compatible**: `serde_json::Value` cannot serialize through
//!    postcard (no static schema), so dynamic JSON fields use [`WireJson`].
//! 2. **Natural-keyed addressing**: blocks are addressed by
//!    `(agent_id, label, scope)` via [`BlockAddr`] — no internal uuid
//!    `block_id: String` ever crosses the wire.
//!
//! Forward-compat: payload types (`SnapshotPayload`, `DeltaPayload`) carry
//! both `Inline` and `Chunked` variants in v1 even though v1 only emits
//! `Inline`. Receivers MUST handle both shapes — v2+ flips emission without
//! a protocol version bump. (irpc has no built-in versioning; bake
//! forward-compat into the type.)

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// Postcard-friendly wrapper for arbitrary JSON. Round-trips through the
/// wire as a String; decoded on demand via [`Self::parse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireJson(pub String);

impl WireJson {
    /// Encode a `serde_json::Value` into wire form.
    pub fn from_value(v: &serde_json::Value) -> Result<Self, serde_json::Error> {
        Ok(Self(serde_json::to_string(v)?))
    }

    /// Decode the wire form back to a `serde_json::Value`.
    pub fn parse(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.0)
    }
}

/// Snapshot of a block's CRDT state at a point in time.
///
/// v1 emits `Inline` only (Pattern's blocks are well below postcard's 16 MiB
/// limit). v2+ can flip emission to `Chunked` without a wire version bump.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SnapshotPayload {
    /// Single-frame snapshot. v1 always emits this.
    Inline { bytes: Vec<u8> },
    /// Multi-frame, sequence-addressed within `chunk_id`. `final_chunk = true`
    /// completes the snapshot. Receivers in v1 buffer + assemble; emitters
    /// in v1 do not produce this.
    Chunked {
        chunk_id: SmolStr,
        seq: u32,
        final_chunk: bool,
        bytes: Vec<u8>,
    },
}

/// A loro delta — incremental change to a block's CRDT state.
///
/// Same forward-compat shape as [`SnapshotPayload`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DeltaPayload {
    Inline {
        bytes: Vec<u8>,
    },
    Chunked {
        chunk_id: SmolStr,
        seq: u32,
        final_chunk: bool,
        bytes: Vec<u8>,
    },
}

/// Natural-keyed block addressing for wire ops.
///
/// Runtime resolves `(agent_id, label, scope)` → block_id server-side via
/// the MemoryCache index. Internal uuid block_ids never cross the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockAddr {
    pub agent_id: SmolStr,
    pub label: SmolStr,
    pub scope: WireMemoryScope,
}

/// Wire-side mirror of `types::memory_types::Scope` (without crate-internal
/// metadata). Plugins request blocks at a scope; runtime resolves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WireMemoryScope {
    Personal,
    Shared,
    Constellation,
}

// ── Plugin lifecycle wire types ──────────────────────────────────────────────

use jiff::Timestamp;

use crate::capability::CapabilitySet;
use crate::types::port::{PortCapabilities, PortId, PortMetadata};

/// Plugin context passed to `on_install` / `on_enable` / `on_disable`.
///
/// Wire-side mirror of [`crate::traits::plugin::PluginContext`] but with
/// dynamic JSON in [`WireJson`] form and no `Arc<dyn MemoryStore>` —
/// memory access crosses the wire as separate RPC variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePluginContext {
    pub plugin_id: SmolStr,
    pub plugin_root: std::path::PathBuf,
    pub user_config: WireJson,
    pub effective_capabilities: CapabilitySet,
}

/// Wire-side port declaration. Plugin reports its ports via this shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortDeclaration {
    pub id: PortId,
    pub metadata: PortMetadata,
    pub capabilities: PortCapabilities,
    /// Optional Haskell helpers (per Phase 6 Task 1: `Option<SmolStr>`).
    pub library: Option<SmolStr>,
}

// ── Port operation wire types ────────────────────────────────────────────────

/// Agent → plugin port call (via Port.call effect → IRPC → plugin port impl).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortCallRequest {
    pub port_id: PortId,
    pub method: SmolStr,
    pub payload: WireJson,
}

/// Agent → plugin port subscribe (via Port.subscribe effect → IRPC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortSubscribeRequest {
    pub port_id: PortId,
    pub config: WireJson,
}

/// Plugin → agent port event. Streamed through the subscribe response stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortEvent {
    pub port_id: PortId,
    pub payload: WireJson,
    pub at: Timestamp,
}

/// Item type for the PortSubscribe stream. Wraps WirePortEvent so producers
/// can signal graceful close. Drop-without-Done means abnormal termination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WirePortStreamItem {
    Event(WirePortEvent),
    Done { reason: SmolStr },
}

/// Health-style status for a port. Plugins can push status events between
/// port operations (e.g. external service down → `Unavailable`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WirePortStatus {
    Healthy,
    Unavailable { reason: SmolStr },
    RateLimited { retry_after_secs: u32 },
    Reconnecting,
}

// ── Hook wire types ──────────────────────────────────────────────────────────
//
// `HookEvent`, `HookEventMetadata`, and `HookSemantics` are already
// postcard-compatible (HookPayload is wire-safe by construction). Plugin
// protocol can carry them directly. Only `HookResponse::Modify(serde_json::Value)`
// needs a wire mirror because of the embedded `Value`.

/// Wire-side mirror of [`crate::hooks::HookResponse`]. Differs only in that
/// `Modify` carries a [`WireJson`] instead of a `serde_json::Value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WireHookResponse {
    Continue,
    Block { reason: SmolStr },
    Modify(WireJson),
}

// ── Memory-sync event wire types ─────────────────────────────────────────────

/// Reason a block stopped being available on the sync stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BlockGoneReason {
    /// Block was deleted at the source.
    Deleted,
    /// Block fell out of the plugin's declared scope or capability set.
    OutOfScope,
    /// Filter no longer matches (e.g. label changed).
    FilterMismatch,
}

/// Runtime → plugin event on the memory-sync bidi stream.
///
/// Plugin opens [`crate::traits::plugin::wire::SyncRequest`] (TODO Task 3
/// step 8 — needs `WireBlockFilter`), receives an initial set of
/// `BlockAvailable` events with snapshots, then `Delta` events as the
/// runtime observes loro changes on the watched blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WireMemoryEvent {
    BlockAvailable {
        addr: BlockAddr,
        /// Block metadata. Source type is jiff-clean + Serialize-derived
        /// (Phase 6 Task 3 chrono→jiff swap).
        metadata: crate::types::memory_types::BlockMetadata,
        snapshot: SnapshotPayload,
    },
    Delta {
        addr: BlockAddr,
        payload: DeltaPayload,
    },
    BlockGone {
        addr: BlockAddr,
        reason: BlockGoneReason,
    },
    /// Producer signals graceful end-of-stream. After sending Done the
    /// runtime drops its sender; the plugin can distinguish this clean close
    /// ("sync session ending coherently") from a transport-drop (crash /
    /// network loss).
    Done {
        reason: SmolStr,
    },
}

/// Plugin → runtime edit on the memory-sync bidi stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WireMemoryEdit {
    /// Plugin pushed a local edit. Runtime applies to its loro doc, fires
    /// downstream subscribers, persists per scope wrapper's policy.
    Delta {
        addr: BlockAddr,
        payload: DeltaPayload,
    },
    /// Plugin signals graceful end-of-stream on its edit channel.
    Done {
        reason: SmolStr,
    },
}

// ── Error wire types ─────────────────────────────────────────────────────────

/// Plugin-level error, surfaced when lifecycle hooks fail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WirePluginError {
    /// Plugin process not running / connection closed.
    TransportLost { reason: SmolStr },
    /// Plugin returned an error from a lifecycle method.
    PluginReturnedError { message: SmolStr },
    /// Plugin's manifest declares capabilities it doesn't actually have.
    CapabilityMismatch { requested: SmolStr, denied_by: SmolStr },
    /// Plugin process died unexpectedly (out-of-process only).
    ProcessDied { exit_code: Option<i32> },
    /// V1 stub: method received but not yet dispatched into runtime state.
    /// Will be removed when 5c+ wires real plugin-registry dispatch.
    Unimplemented { method: SmolStr },
    /// Generic catch-all for cases not yet enumerated.
    Other { message: SmolStr },
}

/// Port-operation error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WirePortError {
    NotFound { port_id: PortId },
    NotSubscribable { port_id: PortId },
    MethodNotFound { port_id: PortId, method: SmolStr },
    InvalidPayload { reason: SmolStr },
    CallFailed { port_id: PortId, message: SmolStr },
    RateLimited { retry_after_secs: u32 },
}

/// Memory-operation error returned from the db-poking memory variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WireMemoryError {
    BlockNotFound { addr: BlockAddr },
    ScopeDenied { addr: BlockAddr, reason: SmolStr },
    CapabilityDenied { reason: SmolStr },
    PersistenceFailed { addr: BlockAddr, message: SmolStr },
    /// V1 stub: method received but not yet dispatched. Removed when 5c+ lands.
    Unimplemented { method: SmolStr },
    Other { message: SmolStr },
}

/// Plugin → runtime memory search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSearchQuery {
    /// Free-text query (matched against block content via FTS5).
    pub query: String,
    /// Optional per-agent filter (defaults to plugin's declared scope).
    pub agent_id_filter: Option<SmolStr>,
    /// Cap on returned results.
    pub limit: u32,
}

/// Single hit from a memory search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSearchResult {
    pub addr: BlockAddr,
    /// Snippet around the match (FTS5 highlight or fallback head).
    pub snippet: String,
    /// FTS5 rank or vector score (impl-defined; higher = better).
    pub score: f64,
}

// ── Host-callback wire types ─────────────────────────────────────────────────

/// Plugin → runtime outbound message (delivered to an agent's mailbox).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireHostMessage {
    /// Target agent ("local:pattern", etc).
    pub target_agent_id: SmolStr,
    /// Message body. Often plain text; plugins can also pass structured
    /// payloads via JSON.
    pub body: String,
    /// Optional metadata (origin hints, attachments, etc) as JSON.
    pub metadata: Option<WireJson>,
}

// ── Task ops wire types ──────────────────────────────────────────────────────
//
// Plugins use these to create/transition/link/query tasks on the agent's
// TaskList block via Tasks effect parity over the wire.

use crate::types::memory_types::TaskStatus;

/// Plugin-driven task creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTaskCreate {
    /// TaskList block address.
    pub block: BlockAddr,
    /// Human-readable task subject.
    pub subject: SmolStr,
    /// Optional description / details.
    pub description: Option<String>,
    /// Optional initial status (defaults to Pending).
    pub initial_status: Option<TaskStatus>,
}

/// Status-only transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTaskTransition {
    pub block: BlockAddr,
    pub task_id: SmolStr,
    pub to: TaskStatus,
}

/// Link/unlink between two task nodes (forms the task graph).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTaskLink {
    pub block: BlockAddr,
    pub from_task: SmolStr,
    pub to_task: SmolStr,
    /// `true` to link, `false` to unlink.
    pub link: bool,
}

/// Task list / filter query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTaskQuery {
    pub block: Option<BlockAddr>,
    pub status_filter: Option<TaskStatus>,
}

/// Single task surfaced to the plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireTaskItem {
    pub block: BlockAddr,
    pub task_id: SmolStr,
    pub subject: SmolStr,
    pub description: Option<String>,
    pub status: TaskStatus,
}

// ── Skill invocation wire types ──────────────────────────────────────────────

/// Plugin asks the runtime to invoke a skill (Skill block) on its behalf.
/// The skill body runs in the agent's eval context, not the plugin's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSkillInvoke {
    /// Skill block address (label is the skill name).
    pub addr: BlockAddr,
    /// Free-form invocation payload (passed to skill body).
    pub payload: WireJson,
}

/// Result of a skill invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSkillInvocation {
    /// Returned value from the skill body.
    pub output: WireJson,
    /// Optional supplementary text the skill emitted (for logging / display).
    pub log: Option<String>,
}
