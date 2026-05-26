//! Per-session memory-sync handler for the `pattern-plugin-memory-sync/1` ALPN.
//!
//! Mirrors [`crate::plugin::host_handler::spawn`] shape: each session spawns its
//! own handler at open time with a [`MemorySyncApiContext`] bundle carrying the
//! session's memory store + cross-block observer. Plugin processes that dial
//! the memory-sync ALPN reach the right session via
//! [`SessionRoutingProtocolHandler`] (registered per-ALPN in pattern_server).
//!
//! ## Lifecycle
//!
//! Plugin opens a bidi stream via `MemorySyncProtocol::Sync(SyncRequest)`. The
//! handler:
//! 1. Parses the [`SyncRequest`] (Filter or Addrs, with optional resume-from VVs)
//! 2. Enumerates initial watched-addrs set, emits `BlockAvailable` for each
//!    with a full loro snapshot via [`SnapshotPayload::Inline`]
//! 3. Spawns a long-running task that bridges two streams:
//!    - inbound `WireMemoryEdit` from plugin (deltas, subscribe, unsubscribe, done)
//!    - the cross-block [`MemoryObserver`] broadcast — filtered by watched set
//!      and by origin (we skip events we ourselves published, to avoid echo)
//! 4. On clean close (Done from plugin or empty rx), emits its own Done before
//!    dropping tx so the plugin can distinguish coherent-close from network-drop.
//!
//! ## Persistence-on-import (option a, landed)
//!
//! When a plugin pushes `WireMemoryEdit::Delta`, the handler imports the bytes
//! into the local loro doc (via `StructuredDocument::apply_updates`). Per loro
//! semantics this does NOT fire `subscribe_local_update`, so the existing
//! per-block crossbeam persistence path does NOT trigger from the import
//! itself.
//!
//! To bridge that: the handler then calls `MemoryStore::push_external_commit`,
//! which resolves the (scope, label) to a block_id, lazy-spawns the per-block
//! subscriber, and pushes a `CommitEvent` on the subscriber's crossbeam
//! channel. The existing worker picks it up and runs disk render + FTS5 +
//! embed exactly as it would for a local edit. Plugin-pushed deltas are
//! durable.

use std::sync::Arc;

use irpc::{Client, WithChannels};
use pattern_core::AgentId;
use pattern_core::observer::{MemoryEvent, MemoryObserver, OriginTag};
use pattern_core::plugin::protocol::{MemorySyncMessage, MemorySyncProtocol};
use pattern_core::traits::memory_store::MemoryStore;
use pattern_core::traits::plugin::wire::{
    BlockAddr, DeltaPayload, SnapshotPayload, SyncRequest, WireMemoryEdit, WireMemoryEvent,
};
use pattern_core::types::memory_types::{BlockFilter, Scope};
use smol_str::SmolStr;
use irpc::channel::mpsc as irpc_mpsc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

/// Bundle of runtime registries a per-session memory-sync handler needs.
/// Cloned cheaply via Arc internals; each handler holds its own copy.
#[derive(Clone)]
pub struct MemorySyncApiContext {
    /// This session's memory store. Block lookups + imports route through this.
    pub memory_store: Arc<dyn MemoryStore>,
    /// Cross-block observer broadcast for forwarding local edits to plugin.
    /// Cloned at construction from `memory_store.observer()` (if Some); held
    /// here so the per-session task can subscribe without re-walking the trait
    /// chain every time.
    pub observer: MemoryObserver,
    /// Session agent_id — used as default scope context if SyncRequest lacks one.
    pub session_agent_id: AgentId,
    /// Default scope for this session.
    pub default_scope: Scope,
}

impl std::fmt::Debug for MemorySyncApiContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySyncApiContext")
            .field("session_agent_id", &self.session_agent_id)
            .field("default_scope", &self.default_scope)
            .finish_non_exhaustive()
    }
}

/// Spawn a per-session memory-sync handler actor. Returns a Client whose
/// `as_local()` can be passed to `MemorySyncProtocol::remote_handler` for Router
/// accept.
///
/// The provided [`MemorySyncApiContext`] is moved into the actor task and used
/// for dispatching incoming `Sync` requests.
pub fn spawn(ctx: MemorySyncApiContext) -> Client<MemorySyncProtocol> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run(rx, ctx));
    Client::local(tx)
}

async fn run(mut rx: mpsc::Receiver<MemorySyncMessage>, ctx: MemorySyncApiContext) {
    while let Some(msg) = rx.recv().await {
        handle(msg, &ctx).await;
    }
}

/// Per-session-message origin tag. Each Sync session gets a fresh
/// `connection_id` so that two instances of the same plugin (same plugin_id)
/// don't filter out each other's events.
fn mint_session_origin(plugin_id: &str) -> OriginTag {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    OriginTag {
        plugin_id: SmolStr::from(plugin_id),
        connection_id: SmolStr::from(format!("{n}")),
    }
}

async fn handle(msg: MemorySyncMessage, ctx: &MemorySyncApiContext) {
    use MemorySyncMessage::*;
    match msg {
        Sync(req) => {
            let WithChannels { tx, rx, inner, .. } = req;
            let ctx_clone = ctx.clone();
            // Spawn the long-running session task so we don't block the
            // handler's main rx loop. Each session task owns its own watched-
            // addrs set, its own observer subscription, and its own origin tag.
            tokio::spawn(session_task(inner, tx, rx, ctx_clone));
        }
    }
}

/// Long-running per-session task. Drives the bidi stream:
/// - outbound: BlockAvailable (initial), then Delta + MetadataChanged + BlockGone
///   forwarded from observer broadcast
/// - inbound: WireMemoryEdit (Delta / Subscribe / Unsubscribe / Done)
async fn session_task(
    req: SyncRequest,
    tx: irpc_mpsc::Sender<WireMemoryEvent>,
    mut rx: irpc_mpsc::Receiver<WireMemoryEdit>,
    ctx: MemorySyncApiContext,
) {
    // For now the origin uses a placeholder plugin_id; the real value should
    // come from the plugin's authenticated identity (pubkey → plugin_id lookup).
    // The route-table-side lookup is the right source of truth; threading it
    // through to here is a follow-up when we wire ALPN registration in stage 6.
    let self_origin = mint_session_origin("plugin");

    // 1) Determine initial watched-addrs set from the request.
    let initial_addrs: Vec<BlockAddr> = match &req {
        SyncRequest::Addrs { addrs, .. } => addrs.clone(),
        SyncRequest::Filter { filter, .. } => match enumerate_filter(&ctx, filter) {
            Ok(addrs) => addrs,
            Err(e) => {
                tracing::warn!(error = %e, "memory_sync: filter enumeration failed");
                let _ = tx
                    .send(WireMemoryEvent::Done {
                        reason: format!("filter enumeration failed: {e}").into(),
                    })
                    .await;
                return;
            }
        },
        _ => {
            tracing::warn!("memory_sync: unknown SyncRequest variant");
            return;
        }
    };

    // 2) Emit BlockAvailable for each initial addr.
    //    TODO consider: for resume-from-VV (known list in the request), emit
    //    Delta-since-VV instead of full snapshot. For v1 we always emit full
    //    snapshot; resume support is a stage-4 followup.
    let mut watched: std::collections::HashSet<BlockAddr> = Default::default();
    for addr in &initial_addrs {
        match emit_block_available(&ctx, &tx, addr, None).await {
            Ok(()) => {
                watched.insert(addr.clone());
            }
            Err(e) => {
                tracing::warn!(addr = ?addr, error = %e, "memory_sync: BlockAvailable emit failed");
            }
        }
    }

    // 3) Subscribe to the observer broadcast for cross-block change forwarding.
    let mut observer_rx = ctx.observer.subscribe();

    // 4) Main bidi loop: select on observer events (out) + plugin edits (in).
    loop {
        tokio::select! {
            // Observer event: maybe forward to plugin.
            ev = observer_rx.recv() => {
                match ev {
                    Ok(event) => {
                        if let Err(e) = forward_observer_event(&tx, &watched, &self_origin, event).await {
                            tracing::warn!(error = %e, "memory_sync: observer forward failed; closing session");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // Observer fell behind. Plugin can self-heal by
                        // re-Syncing with its current VV. For v1 we just log
                        // and continue; a future improvement would send a
                        // "please re-sync" signal on the wire.
                        tracing::warn!(skipped, "memory_sync: observer lagged; plugin may have stale view of skipped events");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Broadcast sender dropped — store is shutting down.
                        let _ = tx
                            .send(WireMemoryEvent::Done { reason: "store closed".into() })
                            .await;
                        return;
                    }
                }
            }
            // Plugin edit: ingest.
            edit_result = rx.recv() => {
                let edit = match edit_result {
                    Ok(opt) => opt,
                    Err(e) => {
                        tracing::warn!(error = %e, "memory_sync: plugin rx error; closing session");
                        return;
                    }
                };
                match edit {
                    Some(WireMemoryEdit::Delta { addr, payload }) => {
                        if let Err(e) = ingest_delta(&ctx, &self_origin, &addr, payload).await {
                            tracing::warn!(addr = ?addr, error = %e, "memory_sync: delta ingest failed");
                        }
                    }
                    Some(WireMemoryEdit::Subscribe { addrs, .. }) => {
                        for addr in addrs {
                            if watched.insert(addr.clone()) {
                                if let Err(e) = emit_block_available(&ctx, &tx, &addr, None).await {
                                    tracing::warn!(addr = ?addr, error = %e, "memory_sync: BlockAvailable emit failed on Subscribe");
                                }
                            }
                        }
                    }
                    Some(WireMemoryEdit::Unsubscribe { addrs }) => {
                        for addr in addrs {
                            if watched.remove(&addr) {
                                let _ = tx.send(WireMemoryEvent::BlockGone {
                                    addr,
                                    reason: pattern_core::traits::plugin::wire::BlockGoneReason::OutOfScope,
                                }).await;
                            }
                        }
                    }
                    Some(WireMemoryEdit::Done { reason }) => {
                        tracing::debug!(reason = %reason, "memory_sync: plugin signalled clean close");
                        let _ = tx
                            .send(WireMemoryEvent::Done { reason: "plugin closed".into() })
                            .await;
                        return;
                    }
                    Some(_) => {
                        // Forward-compat: unknown WireMemoryEdit variant. Log + continue.
                        tracing::warn!("memory_sync: received unknown WireMemoryEdit variant");
                    }
                    None => {
                        // Plugin's rx sender dropped (transport-drop or unclean close).
                        // Don't bother sending Done — the channel's gone.
                        return;
                    }
                }
            }
        }
    }

    // Loop exited (only on observer-forward error in current shape). Send Done
    // before dropping tx so plugin can distinguish graceful close.
    let _ = tx
        .send(WireMemoryEvent::Done { reason: "session ended".into() })
        .await;
}

/// Enumerate addresses matching a `BlockFilter` from the memory store.
fn enumerate_filter(
    ctx: &MemorySyncApiContext,
    filter: &BlockFilter,
) -> Result<Vec<BlockAddr>, pattern_core::error::MemoryError> {
    let metas = ctx.memory_store.list_blocks(filter.clone())?;
    Ok(metas
        .into_iter()
        .filter_map(|m| {
            let scope = Scope::from_db_key(&m.agent_id)?;
            Some(BlockAddr { scope, label: m.label.into() })
        })
        .collect())
}

/// Fetch a block's StructuredDocument, export a full snapshot, and emit
/// BlockAvailable to the plugin. Returns Err on any step that fails.
async fn emit_block_available(
    ctx: &MemorySyncApiContext,
    tx: &irpc_mpsc::Sender<WireMemoryEvent>,
    addr: &BlockAddr,
    origin: Option<OriginTag>,
) -> Result<(), String> {
    let doc_opt = ctx
        .memory_store
        .get_block(&addr.scope, &addr.label)
        .map_err(|e| format!("get_block: {e}"))?;
    let doc = doc_opt.ok_or_else(|| format!("block not found: {:?} / {}", addr.scope, addr.label))?;
    let snapshot_bytes = doc
        .export_snapshot()
        .map_err(|e| format!("export_snapshot: {e}"))?;
    let metadata = doc.metadata().clone();
    let event = WireMemoryEvent::BlockAvailable {
        addr: addr.clone(),
        metadata,
        snapshot: SnapshotPayload::Inline { bytes: snapshot_bytes },
    };
    // Note: BlockAvailable in WireMemoryEvent doesn't carry origin (it's an
    // initial-state delivery, not a propagated edit). origin arg kept for
    // signature symmetry with delta forwarding; future-use if needed.
    let _ = origin;
    tx.send(event)
        .await
        .map_err(|_| "plugin tx dropped".to_string())
}

/// Forward an observer broadcast event to the plugin if it matches the watched
/// set and didn't originate from this session.
async fn forward_observer_event(
    tx: &irpc_mpsc::Sender<WireMemoryEvent>,
    watched: &std::collections::HashSet<BlockAddr>,
    self_origin: &OriginTag,
    event: MemoryEvent,
) -> Result<(), String> {
    let (addr, origin_opt) = match &event {
        MemoryEvent::Delta { addr, origin, .. }
        | MemoryEvent::BlockAvailable { addr, origin, .. }
        | MemoryEvent::MetadataChanged { addr, origin, .. }
        | MemoryEvent::BlockGone { addr, origin, .. } => (addr.clone(), origin.clone()),
        _ => return Ok(()),
    };

    // Filter 1: addr must be in our watched set.
    if !watched.contains(&addr) {
        return Ok(());
    }
    // Filter 2: skip events we ourselves published (echo suppression).
    if let Some(o) = &origin_opt {
        if o == self_origin {
            return Ok(());
        }
    }

    let wire_event = match event {
        MemoryEvent::Delta { update_bytes, .. } => WireMemoryEvent::Delta {
            addr,
            payload: DeltaPayload::Inline { bytes: update_bytes },
        },
        MemoryEvent::MetadataChanged { metadata, .. } => {
            WireMemoryEvent::MetadataChanged { addr, metadata }
        }
        MemoryEvent::BlockGone { reason, .. } => {
            let wire_reason = match reason {
                pattern_core::observer::BlockGoneReason::Deleted => {
                    pattern_core::traits::plugin::wire::BlockGoneReason::Deleted
                }
                pattern_core::observer::BlockGoneReason::OutOfScope => {
                    pattern_core::traits::plugin::wire::BlockGoneReason::OutOfScope
                }
                _ => pattern_core::traits::plugin::wire::BlockGoneReason::Deleted,
            };
            WireMemoryEvent::BlockGone { addr, reason: wire_reason }
        }
        MemoryEvent::BlockAvailable { metadata, snapshot, .. } => WireMemoryEvent::BlockAvailable {
            addr,
            metadata,
            snapshot: SnapshotPayload::Inline { bytes: snapshot },
        },
        _ => return Ok(()),
    };

    tx.send(wire_event)
        .await
        .map_err(|_| "plugin tx dropped".to_string())
}

/// Ingest a plugin-pushed Delta: import the loro update into the local doc,
/// then republish on the observer broadcast (with origin = self) so other
/// session-handlers watching the same addr can forward to their plugins.
///
/// Persistence: loro's subscribe_local_update does NOT fire on import, so
/// after `apply_updates` we explicitly call `push_external_commit` which
/// pushes a CommitEvent on the per-block crossbeam — the existing worker
/// handles disk + FTS5 + embed exactly as for a local edit.
async fn ingest_delta(
    ctx: &MemorySyncApiContext,
    self_origin: &OriginTag,
    addr: &BlockAddr,
    payload: DeltaPayload,
) -> Result<(), String> {
    let bytes = match payload {
        DeltaPayload::Inline { bytes } => bytes,
        DeltaPayload::Chunked { .. } => {
            return Err("DeltaPayload::Chunked: v1 emit-and-receive Inline only".into());
        }
        _ => return Err("unknown DeltaPayload variant".into()),
    };

    // Look up the StructuredDocument and apply the update.
    let doc_opt = ctx
        .memory_store
        .get_block(&addr.scope, &addr.label)
        .map_err(|e| format!("get_block on ingest: {e}"))?;
    let doc = doc_opt.ok_or_else(|| {
        format!("ingest_delta: block not found: {:?} / {}", addr.scope, addr.label)
    })?;
    doc.apply_updates(&bytes)
        .map_err(|e| format!("apply_updates: {e}"))?;

    // Drive persistence path: push CommitEvent on per-block crossbeam so the
    // existing worker (disk render + FTS5 + embed) runs exactly as it would
    // for a local edit. Bypasses loro's subscribe_local_update which doesn't
    // fire on imports.
    if let Err(e) = ctx
        .memory_store
        .push_external_commit(&addr.scope, &addr.label, bytes.clone())
    {
        tracing::warn!(addr = ?addr, error = %e, "memory_sync: push_external_commit failed; in-memory delta applied but disk persistence skipped");
    }

    // Republish on observer broadcast with origin=self_origin so other
    // observers (other plugin sessions watching same addr) see it and we don't
    // echo back to ourselves (filter handled at receive side).
    ctx.observer.publish(MemoryEvent::Delta {
        addr: addr.clone(),
        update_bytes: bytes,
        origin: Some(self_origin.clone()),
    });

    tracing::debug!(addr = ?addr, "memory_sync: plugin delta ingested + persisted + broadcast");

    Ok(())
}
