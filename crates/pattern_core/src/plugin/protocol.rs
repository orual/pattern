//! Plugin IRPC protocols (Phase 6 of v3-extensibility).
//!
//! Three protocols multiplexed on the daemon's QUIC endpoint via iroh::Router.
//! Plugin and runtime each run both a server (accepting their inbound protocol)
//! and a client (dialing the other side's protocol):
//!
//! - [`PluginGuestProtocol`] on ALPN [`PLUGIN_GUEST_ALPN`] (`pattern-plugin-guest/1`):
//!   Runtime → Plugin direction. Plugin-side accepts, runtime-side dials.
//!   Lifecycle (OnInstall/Enable/Disable), introspection (DeclarePorts/GetLibrary),
//!   hook events, port operations (PortCall/PortSubscribe).
//! - [`PluginHostProtocol`] on ALPN [`PLUGIN_HOST_ALPN`] (`pattern-plugin-host/1`):
//!   Plugin → Runtime direction. Runtime-side accepts, plugin-side dials.
//!   Host callbacks (HostSendMessage, task ops, skill invoke) and db-poking
//!   memory ops.
//! - [`MemorySyncProtocol`] on ALPN `pattern-plugin-memory-sync/1`: single
//!   bidi-streaming method for loro delta sync. Feature-gated (`memory-sync`
//!   SDK feature). Plugins that don't enable the feature don't register the
//!   ALPN.
//!
//! Splitting host/guest into separate enums + ALPNs gives type-level direction
//! safety — a misbehaving peer can't send a variant that crosses the boundary
//! because the receiving side doesn't know how to decode it on the wrong ALPN.
//!
//! Wire types live in [`pattern_core::traits::plugin::wire`].

use irpc::rpc_requests;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use irpc::channel::{mpsc, oneshot};

use crate::traits::plugin::wire::*;
use crate::types::block::BlockCreate;
use crate::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, UndoRedoOp,
};
use crate::hooks::HookEvent;

/// ALPN for the Plugin Guest protocol (runtime → plugin). Plugin-side accepts.
pub const PLUGIN_GUEST_ALPN: &[u8] = b"pattern-plugin-guest/1";

/// ALPN for the Plugin Host protocol (plugin → runtime). Runtime-side accepts.
pub const PLUGIN_HOST_ALPN: &[u8] = b"pattern-plugin-host/1";

/// ALPN for the bidi memory delta-sync stream.
pub const PLUGIN_MEMORY_SYNC_ALPN: &[u8] = b"pattern-plugin-memory-sync/1";

/// Runtime → Plugin protocol. Plugin-side accepts on [`PLUGIN_GUEST_ALPN`];
/// runtime dials to invoke lifecycle, introspection, hooks, and port ops.
#[rpc_requests(message = PluginGuestMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum PluginGuestProtocol {
    // ═══ Runtime → Plugin: lifecycle ═════════════════════════════════════════
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    #[wrap(OnInstallRequest)]
    OnInstall(WirePluginContext),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    #[wrap(OnEnableRequest)]
    OnEnable(WirePluginContext),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    #[wrap(OnDisableRequest)]
    OnDisable(WirePluginContext),
    #[rpc(tx = oneshot::Sender<Vec<WirePortDeclaration>>)]
    #[wrap(DeclarePortsRequest)]
    DeclarePorts(()),
    #[rpc(tx = oneshot::Sender<Option<SmolStr>>)]
    #[wrap(GetLibraryRequest)]
    GetLibrary(()),

    // ═══ Runtime → Plugin: hook events ═══════════════════════════════════════
    /// Fire-and-forget notification.
    #[rpc(tx = oneshot::Sender<()>)]
    #[wrap(OnHookEventRequest)]
    OnHookEvent(HookEvent),
    /// Blocking hook — emitter waits for response.
    #[rpc(tx = oneshot::Sender<WireHookResponse>)]
    #[wrap(OnHookEventBlockingRequest)]
    OnHookEventBlocking(HookEvent),

    // ═══ Runtime → Plugin: port operations ═══════════════════════════════════
    /// Agent calls plugin port method. `WireJson` carries the response payload.
    #[rpc(tx = oneshot::Sender<Result<WireJson, WirePortError>>)]
    PortCall(WirePortCallRequest),
    /// Agent subscribes to a port. Server-stream returns events until plugin
    /// closes or the client drops its receiver.
    #[rpc(tx = mpsc::Sender<WirePortStreamItem>)]
    PortSubscribe(WirePortSubscribeRequest),

}

/// Plugin → Runtime protocol. Runtime-side accepts on [`PLUGIN_HOST_ALPN`];
/// plugin dials to make host callbacks (send messages, task/skill ops) and
/// db-poking memory operations.
#[rpc_requests(message = PluginHostMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum PluginHostProtocol {
    // ═══ host callbacks ══════════════════════════════════════════════════════
    /// Plugin sends a message to an agent's mailbox.
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    HostSendMessage(WireHostMessage),
    /// Plugin creates a task. Returns the new task_id.
    #[rpc(tx = oneshot::Sender<Result<SmolStr, WirePluginError>>)]
    HostTaskCreate(WireTaskCreate),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    HostTaskTransition(WireTaskTransition),
    #[rpc(tx = oneshot::Sender<Result<(), WirePluginError>>)]
    HostTaskLink(WireTaskLink),
    #[rpc(tx = oneshot::Sender<Vec<WireTaskItem>>)]
    HostTaskQuery(WireTaskQuery),
    #[rpc(tx = oneshot::Sender<Result<WireSkillInvocation, WirePluginError>>)]
    HostSkillInvoke(WireSkillInvoke),

    // ═══ db-poking memory ops ════════════════════════════════════════════════
    /// Create a new memory block. Returns the freshly-minted metadata.
    #[rpc(tx = oneshot::Sender<Result<BlockMetadata, WireMemoryError>>)]
    MemoryCreateBlock(BlockCreate),
    /// Soft-delete a block (idempotent if Memory.create later reactivates).
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    #[wrap(MemoryDeleteBlockRequest)]
    MemoryDeleteBlock(BlockAddr),
    /// FTS5 / vector memory search.
    #[rpc(tx = oneshot::Sender<Vec<WireSearchResult>>)]
    #[wrap(MemorySearchRequest)]
    MemorySearch(WireSearchQuery),
    /// Enumerate blocks matching the filter.
    #[rpc(tx = oneshot::Sender<Vec<BlockMetadata>>)]
    MemoryListBlocks(BlockFilter),
    /// Persist a block to disk (explicit flush).
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    #[wrap(MemoryPersistRequest)]
    MemoryPersist(BlockAddr),
    /// Update block metadata (pinned, type, schema, description).
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    #[wrap(MemoryUpdateMetadataRequest)]
    MemoryUpdateMetadata(MemoryUpdateMetadataArgs),
    /// Undo/redo the last persisted change. Returns whether the op moved the
    /// document (false if nothing to undo/redo).
    #[rpc(tx = oneshot::Sender<Result<bool, WireMemoryError>>)]
    #[wrap(MemoryUndoRedoRequest)]
    MemoryUndoRedo(MemoryUndoRedoArgs),
    /// Get a block shared by another agent (via `share` permission).
    #[rpc(tx = oneshot::Sender<Result<Option<BlockMetadata>, WireMemoryError>>)]
    #[wrap(MemoryGetSharedBlockRequest)]
    MemoryGetSharedBlock(MemoryGetSharedBlockArgs),
    /// Insert an archival entry.
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    MemoryInsertArchival(ArchivalEntry),
    /// Search archival entries by content.
    #[rpc(tx = oneshot::Sender<Vec<ArchivalEntry>>)]
    #[wrap(MemorySearchArchivalRequest)]
    MemorySearchArchival(WireSearchQuery),
    /// Delete a single archival entry by id.
    #[rpc(tx = oneshot::Sender<Result<(), WireMemoryError>>)]
    #[wrap(MemoryDeleteArchivalRequest)]
    MemoryDeleteArchival(SmolStr),
}

/// Memory delta-sync protocol. Single bidi-streaming method on the
/// `pattern-plugin-memory-sync/1` ALPN. Plugin opens [`Self::Sync`] with
/// a `BlockFilter`; runtime sends initial `BlockAvailable` events with
/// snapshots, then a stream of `Delta`s as loro changes land. Plugin's
/// inbound channel carries local edits that runtime applies to its loro
/// docs (fires existing subscribers + scope-wrapper persist policy).
///
/// Dropping either side closes the session.
#[rpc_requests(message = MemorySyncMessage)]
#[derive(Debug, Serialize, Deserialize)]
pub enum MemorySyncProtocol {
    /// Open a bidi delta-sync session. Runtime sends `WireMemoryEvent`s
    /// (initial BlockAvailable + ongoing Delta + final BlockGone) on `tx`;
    /// plugin pushes local edits as `WireMemoryEdit`s on `rx`. Drop either
    /// side closes the session.
    #[rpc(tx = mpsc::Sender<WireMemoryEvent>, rx = mpsc::Receiver<WireMemoryEdit>)]
    Sync(BlockFilter),
}

// ── Multi-field argument structs ─────────────────────────────────────────────
// irpc's `#[rpc_requests]` wants tuple-style variants with a single field.
// Multi-field variants are expressed by wrapping their args in named structs.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUpdateMetadataArgs {
    pub addr: BlockAddr,
    pub patch: BlockMetadataPatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryUndoRedoArgs {
    pub addr: BlockAddr,
    pub op: UndoRedoOp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryGetSharedBlockArgs {
    pub owner: SmolStr,
    pub label: SmolStr,
}

#[cfg(test)]
mod tests {
    //! Postcard roundtrip tests. Covers representative variants across the
    //! axes: unary oneshot, server-stream, bidi-stream, simple-tuple,
    //! multi-field-wrapped, and the chunked-payload forward-compat case.

    use super::*;
    use crate::capability::CapabilitySet;
    use crate::types::memory_types::{BlockMetadata, BlockSchema};

    fn roundtrip<T: Serialize + serde::de::DeserializeOwned + std::fmt::Debug>(v: &T) -> T {
        let bytes = postcard::to_stdvec(v).expect("encode");
        postcard::from_bytes::<T>(&bytes).expect("decode")
    }

    #[test]
    fn wire_json_roundtrips() {
        let v = serde_json::json!({"a": 1, "b": [true, null, "x"]});
        let w = WireJson::from_value(&v).unwrap();
        let back = roundtrip(&w);
        assert_eq!(back.parse().unwrap(), v);
    }

    #[test]
    fn block_addr_roundtrips() {
        let addr = BlockAddr {
            agent_id: "local:pattern".into(),
            label: "scratchpad".into(),
            scope: WireMemoryScope::Personal,
        };
        let back = roundtrip(&addr);
        assert_eq!(back, addr);
    }

    #[test]
    fn snapshot_payload_inline_and_chunked_both_roundtrip() {
        let inline = SnapshotPayload::Inline { bytes: vec![1, 2, 3, 4] };
        let _ = roundtrip(&inline);

        let chunked = SnapshotPayload::Chunked {
            chunk_id: "abc".into(),
            seq: 0,
            final_chunk: false,
            bytes: vec![5, 6, 7],
        };
        let _ = roundtrip(&chunked);
        // forward-compat: receivers must handle both variants even though v1 only
        // emits Inline. compile + roundtrip is the assertion.
    }

    #[test]
    fn block_metadata_roundtrips() {
        let meta = BlockMetadata::standalone(BlockSchema::text());
        let bytes = postcard::to_stdvec(&meta).expect("encode BlockMetadata");
        let _back: BlockMetadata = postcard::from_bytes(&bytes).expect("decode BlockMetadata");
    }

    #[test]
    fn wire_plugin_context_roundtrips() {
        let ctx = WirePluginContext {
            plugin_id: "discord".into(),
            plugin_root: std::path::PathBuf::from("/plugins/discord"),
            user_config: WireJson("{}".into()),
            effective_capabilities: CapabilitySet::default(),
        };
        let bytes = postcard::to_stdvec(&ctx).expect("encode");
        let _back: WirePluginContext = postcard::from_bytes(&bytes).expect("decode");
    }

    #[test]
    fn wire_hook_response_modify_carries_wirejson() {
        let v = serde_json::json!({"replace": "x"});
        let r = WireHookResponse::Modify(WireJson::from_value(&v).unwrap());
        let _back = roundtrip(&r);
    }

    #[test]
    fn wire_memory_event_with_metadata_and_snapshot_roundtrips() {
        let addr = BlockAddr {
            agent_id: "local:pattern".into(),
            label: "persona".into(),
            scope: WireMemoryScope::Personal,
        };
        let meta = BlockMetadata::standalone(BlockSchema::text());
        let ev = WireMemoryEvent::BlockAvailable {
            addr,
            metadata: meta,
            snapshot: SnapshotPayload::Inline { bytes: vec![0u8; 16] },
        };
        let _back = roundtrip(&ev);
    }

    #[test]
    fn multi_field_wrapped_args_roundtrip() {
        // MemoryUpdateMetadataArgs is the canonical multi-field wrapper.
        let args = MemoryUpdateMetadataArgs {
            addr: BlockAddr {
                agent_id: "local:pattern".into(),
                label: "x".into(),
                scope: WireMemoryScope::Personal,
            },
            patch: BlockMetadataPatch::default().pinned(true),
        };
        let _back = roundtrip(&args);
    }
}
