//! Wire types for plugin IRPC protocols (Phase 6 of v3-extensibility).
//!
//! These types serialize through postcard at the IRPC boundary. They mirror
//! pattern_core's domain types but with two constraints:
//!
//! 1. **Postcard-compatible**: `serde_json::Value` cannot serialize through
//!    postcard (no static schema), so dynamic JSON fields use [`WireJson`].
//! 2. **Natural-keyed addressing**: blocks are addressed by
//!    `(scope, label)` via [`BlockAddr`] — `Scope` already encodes the
//!    ownership boundary (Global(agent_id) or Local(project_id)). No
//!    internal uuid `block_id: String` ever crosses the wire.
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

// Re-export of canonical, non-feature-gated BlockAddr. Lives here for
// back-compat with wire-side callers; the source of truth is
// `crate::types::memory_types::BlockAddr`.
pub use crate::types::memory_types::BlockAddr;

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
    /// Mount path this plugin instance is scoped to. See
    /// [`crate::traits::plugin::PluginContext::mount_path`].
    #[serde(default)]
    pub mount_path: Option<std::path::PathBuf>,
    /// Project id derived from `.pattern.kdl` in `mount_path`. Plugins use this
    /// to construct `Scope::Local(project_id)` for shared-block addressing without
    /// re-parsing the mount config.
    #[serde(default)]
    pub project_id: Option<SmolStr>,
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

/// Agent → plugin port unsubscribe (via Port.unsubscribe effect → IRPC).
/// Symmetric pair with [`WirePortSubscribeRequest`]; no config payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WirePortUnsubscribeRequest {
    pub port_id: PortId,
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

/// Loro `VersionVector` in wire-friendly form (loro's encode/decode bytes).
///
/// Used in [`SyncRequest`] for resume-from-version semantics: plugin remembers
/// each block's VV across restarts, and on next sync sends them so host can
/// stream only the missing deltas instead of re-snapshotting everything.
///
/// Construct via [`Self::from_loro`] / decode via [`Self::to_loro`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireVersionVector(pub Vec<u8>);

impl WireVersionVector {
    pub fn from_loro(vv: &loro::VersionVector) -> Self { Self(vv.encode()) }
    pub fn to_loro(&self) -> Result<loro::VersionVector, loro::LoroError> {
        loro::VersionVector::decode(&self.0)
    }
}

/// Initialization payload for a [`crate::plugin::protocol::MemorySyncProtocol::Sync`] session.
///
/// Plugin picks one of two subscription shapes; both carry optional per-addr
/// version vectors so host can skip re-snapshotting blocks the plugin already has.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SyncRequest {
    /// Subscribe to all blocks matching `filter`. Newly-created blocks that
    /// match the filter auto-stream as `BlockAvailable` events.
    Filter {
        filter: crate::types::memory_types::BlockFilter,
        /// Optional per-addr version vectors. For any addr present here, host
        /// emits only deltas since that VV; for addrs not present (or new),
        /// host sends a fresh BlockAvailable snapshot.
        #[serde(default)]
        known: Vec<(BlockAddr, WireVersionVector)>,
    },
    /// Subscribe to a specific set of addresses. Use
    /// [`crate::traits::plugin::wire::WireMemoryEdit::Subscribe`] /
    /// `Unsubscribe` to mutate the watched set mid-session.
    Addrs {
        addrs: Vec<BlockAddr>,
        /// Optional per-addr version vectors (same semantics as Filter.known).
        #[serde(default)]
        known: Vec<(BlockAddr, WireVersionVector)>,
    },
}

/// Runtime → plugin event on the memory-sync bidi stream.
///
/// Plugin opens [`crate::plugin::protocol::MemorySyncProtocol::Sync`] with a
/// [`SyncRequest`], receives an initial set of `BlockAvailable` events with
/// snapshots (or `Delta` events for blocks the plugin already had per VV),
/// then `Delta` events as the runtime observes loro changes on the watched blocks.
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
    /// Block metadata changed host-side (pinned / type / schema / description).
    /// These fields live outside the loro CRDT so Delta events don't carry them;
    /// plugins observing metadata need this distinct signal.
    MetadataChanged {
        addr: BlockAddr,
        metadata: crate::types::memory_types::BlockMetadata,
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

/// Plugin → runtime edit-or-control message on the memory-sync bidi stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WireMemoryEdit {
    /// Plugin pushed a local edit. Runtime applies to its loro doc, fires
    /// downstream subscribers, persists per scope wrapper's policy.
    Delta {
        addr: BlockAddr,
        payload: DeltaPayload,
    },
    /// Add addrs to the watched set without re-opening the session. Host
    /// responds with `BlockAvailable` for each newly-watched addr (or `Delta`
    /// since `known` VV if provided).
    Subscribe {
        addrs: Vec<BlockAddr>,
        #[serde(default)]
        known: Vec<(BlockAddr, WireVersionVector)>,
    },
    /// Drop addrs from the watched set. Host sends `BlockGone { reason: OutOfScope }`
    /// for each, then stops emitting events for them.
    Unsubscribe {
        addrs: Vec<BlockAddr>,
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

/// Plugin → runtime memory search request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSearchQuery {
    /// Free-text query (matched against block content via FTS5).
    pub query: String,
    /// Search scope. `None` defaults to the session's default scope (single scope).
    /// `Some(MemorySearchScope::Scope(...))` targets one specific scope; `Some(Constellation)`
    /// iterates across every scope visible to the session.
    pub scope: Option<crate::types::memory_types::MemorySearchScope>,
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

/// Plugin → runtime outbound message. Same shape as [`crate::wire::ui::AgentMessage`]
/// for everything EXCEPT origin: the plugin self-reports `plugin_id` +
/// `partner_authority`, and the daemon constructs `Author::Plugin {...}` server-side.
/// Plugins literally cannot encode Partner/Human/Agent/System authorship via this
/// wire — the type doesn't expose those variants. Use the TUI protocol
/// (`pattern/1` ALPN) for callers that need full origin control.
#[cfg(feature = "plugin-transport")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAgentMessage {
    /// Client-minted batch ID (snowflake) for correlating TurnEvents.
    pub batch_id: crate::types::ids::BatchId,
    /// Routing directive (Direct/Auto/Address).
    pub recipient: crate::wire::ui::Recipient,
    /// Message content parts (multi-modal capable).
    pub parts: Vec<crate::types::provider::ContentPart>,
    /// Plugin authoring this message. Daemon constructs
    /// `Author::Plugin { plugin_id, partner_authority }` server-side.
    pub plugin_id: SmolStr,
    /// Whether the plugin is acting with partner-level authority.
    /// Plugins installed by the partner default to true; remote/untrusted false.
    #[serde(default)]
    pub partner_authority: bool,
    /// Sphere for this message. Defaults to Internal (plugin→agent channel).
    #[serde(default = "default_plugin_sphere")]
    pub sphere: crate::types::origin::Sphere,
    /// Optional transport hint for display/attribution (e.g. "discord:channel:xxx").
    #[serde(default)]
    pub transport_hint: Option<SmolStr>,
}

#[cfg(feature = "plugin-transport")]
fn default_plugin_sphere() -> crate::types::origin::Sphere { crate::types::origin::Sphere::Internal }
