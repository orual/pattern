// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Plugin-side MemorySync client.
//!
//! Opens a bidi stream against the daemon's `pattern-plugin-memory-sync/1`
//! ALPN, maintains a local cache of `StructuredDocument`s materialised from
//! `BlockAvailable` snapshots, applies incoming `Delta`s, and exposes a push
//! API for the plugin-side `MemoryStore` impl to send local edits upstream.
//!
//! ## Lifecycle
//!
//! 1. Plugin calls [`MemorySyncClient::open`] with a [`SyncRequest`] (Filter
//!    or Addrs) once at startup or first MemoryStore use.
//! 2. The constructor dials the daemon over the memory-sync ALPN, opens the
//!    bidi stream, spawns a receive task that updates the local cache, and
//!    returns a handle.
//! 3. Plugin reads via [`MemorySyncClient::get_block`] (cache lookup).
//! 4. Plugin pushes local edits via [`MemorySyncClient::push_delta`].
//! 5. Drop sends [`WireMemoryEdit::Done`] before tearing down the stream so
//!    the daemon can distinguish graceful close from network-drop.
//!
//! ## What's wired in stage 7a (this commit)
//!
//! - Open the stream + spawn receive task.
//! - Handle `BlockAvailable` (materialise via `StructuredDocument::from_snapshot_with_metadata`).
//! - Handle `Delta` (apply via `apply_updates` on the cached doc).
//! - Handle `MetadataChanged` (mutate metadata on the cached doc).
//! - Handle `BlockGone` (remove from cache).
//! - Handle `Done` (mark stream closed; future ops return error).
//! - Push API for sending `WireMemoryEdit::Delta` upstream.
//!
//! ## Not wired yet (stage 7b)
//!
//! - Plugin-side `MemoryStore` impl that uses the cache + push API.
//! - subscribe_local_update bridge: when the agent edits the local doc, the
//!   subscription should auto-push Delta upstream. Stage 7a leaves this manual
//!   (plugin calls `push_delta` explicitly); stage 7b wires the auto-push.
//! - `PluginHandle::open_memory_sync` convenience method that uses the
//!   plugin's existing endpoint + daemon addr. Stage 7a takes endpoint and
//!   daemon_addr as constructor args.
//! - `Subscribe` / `Unsubscribe` API for live-mutating watched set.
//! - End-to-end test exercising host emit -> plugin sees, plugin emit -> host
//!   imports + persists.

use std::sync::Arc;

use dashmap::DashMap;
use iroh::{Endpoint, EndpointAddr};
use irpc::Client;
use irpc::channel::mpsc;
use pattern_core::memory::StructuredDocument;
use pattern_core::plugin::protocol::{MemorySyncProtocol, PLUGIN_MEMORY_SYNC_ALPN};
use pattern_core::traits::plugin::wire::{
    BlockAddr, DeltaPayload, SnapshotPayload, SyncRequest, WireMemoryEdit, WireMemoryEvent,
};
use smol_str::SmolStr;
use tokio::task::JoinHandle;

/// Per-cache-entry bundle: the materialised doc plus the loro
/// `subscribe_local_update` subscription that auto-pushes plugin-local edits
/// upstream as `WireMemoryEdit::Delta`. The Subscription field must be kept
/// alive for the doc's lifetime in the cache; dropping it unsubscribes loro.
struct CachedBlock {
    doc: Arc<StructuredDocument>,
    _sub: loro::Subscription,
}

/// Errors opening a MemorySync stream or operating against an open one.
#[derive(Debug, thiserror::Error)]
pub enum MemorySyncError {
    #[error("opening MemorySync stream failed: {message}")]
    Open { message: SmolStr },
    #[error("snapshot decode failed for {addr:?}: {message}")]
    SnapshotDecode {
        addr: BlockAddr,
        message: SmolStr,
    },
    #[error("delta apply failed for {addr:?}: {message}")]
    DeltaApply {
        addr: BlockAddr,
        message: SmolStr,
    },
    #[error("push failed: stream closed")]
    StreamClosed,
    #[error("unsupported payload variant (Chunked not implemented v1)")]
    UnsupportedChunked,
}

/// Plugin-side MemorySync client. Owns the outbound tx, the local cache, and
/// the receive task.
pub struct MemorySyncClient {
    /// Outbound channel: plugin -> daemon (WireMemoryEdit).
    tx: mpsc::Sender<WireMemoryEdit>,
    /// Local materialised cache keyed by addr. Each entry bundles the doc
    /// with the loro subscription that drives auto-push of local edits.
    cache: Arc<DashMap<BlockAddr, CachedBlock>>,
    /// One-shot notification channels for addrs being awaited on first arrival
    /// (e.g. PluginMemoryStore::create_block registers a waiter before sending
    /// the host MemoryCreateBlock RPC; receive_loop drains and fires the
    /// sender when BlockAvailable arrives for that addr).
    pending_block_waiters: Arc<DashMap<BlockAddr, crossbeam_channel::Sender<()>>>,
    /// Receive task driving the cache update loop. Joined on Drop.
    _recv_task: JoinHandle<()>,
}

impl MemorySyncClient {
    /// Open a MemorySync stream against the daemon over the given endpoint.
    /// The `request` selects initial watched-addrs set (Filter or Addrs +
    /// optional resume VVs). Receive task starts processing events immediately;
    /// callers can poll [`Self::get_block`] once the initial BlockAvailable
    /// snapshots have been delivered (use [`Self::has_block`] / wait pattern).
    pub async fn open(
        plugin_endpoint: Endpoint,
        daemon_endpoint_addr: EndpointAddr,
        request: SyncRequest,
    ) -> Result<Self, MemorySyncError> {
        let client = irpc_iroh::client::<MemorySyncProtocol>(
            plugin_endpoint,
            daemon_endpoint_addr,
            PLUGIN_MEMORY_SYNC_ALPN,
        );
        Self::open_with_client(client, request).await
    }

    /// In-process / pre-constructed-client variant. Accepts a
    /// [`Client<MemorySyncProtocol>`] directly (e.g. from
    /// `memory_sync_handler::spawn`'s `Client::local(tx)`) and skips the iroh
    /// dial. Used by integration tests that drive both sides of the protocol
    /// in-process via tokio channels, but also usable in any context where the
    /// client is constructed by some other means than dialing.
    pub async fn open_with_client(
        client: Client<MemorySyncProtocol>,
        request: SyncRequest,
    ) -> Result<Self, MemorySyncError> {
        // bidi_streaming(msg, update_cap, response_cap): we send WireMemoryEdit
        // as Updates, receive WireMemoryEvent as Responses.
        let (tx, rx) = client
            .bidi_streaming(request, 64, 64)
            .await
            .map_err(|e| MemorySyncError::Open {
                message: format!("bidi_streaming: {e}").into(),
            })?;

        let cache: Arc<DashMap<BlockAddr, CachedBlock>> = Arc::new(DashMap::new());
        let pending_block_waiters: Arc<DashMap<BlockAddr, crossbeam_channel::Sender<()>>> = Arc::new(DashMap::new());
        let cache_clone = Arc::clone(&cache);
        let waiters_clone = Arc::clone(&pending_block_waiters);
        let tx_for_recv = tx.clone();
        let recv_task = tokio::spawn(receive_loop(rx, cache_clone, waiters_clone, tx_for_recv));

        Ok(Self {
            tx,
            cache,
            pending_block_waiters,
            _recv_task: recv_task,
        })
    }

    /// Read a block from the local cache. Returns None if the addr hasn't
    /// been materialised yet (e.g. BlockAvailable hasn't arrived, or addr is
    /// outside the watched set).
    pub fn get_block(&self, addr: &BlockAddr) -> Option<Arc<StructuredDocument>> {
        self.cache.get(addr).map(|e| Arc::clone(&e.value().doc))
    }

    /// Register a one-shot waiter for the first BlockAvailable arrival of a
    /// given addr. Returns a `Receiver` that the caller can block on
    /// (typically with a timeout) until the doc has been materialised into
    /// the local cache. Used by `PluginMemoryStore::create_block` to avoid
    /// the alternative of polling `get_block` in a sleep loop.
    ///
    /// If the addr is already in the cache when this is called, the caller
    /// should `get_block` first and skip waiter registration. The receive
    /// loop only fires the waiter on cache insertion, not on "addr is
    /// already present".
    pub fn register_block_arrival_waiter(
        &self,
        addr: BlockAddr,
    ) -> crossbeam_channel::Receiver<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.pending_block_waiters.insert(addr, tx);
        rx
    }

    /// Check whether a block has been materialised yet.
    pub fn has_block(&self, addr: &BlockAddr) -> bool {
        self.cache.contains_key(addr)
    }

    /// Push a local edit to the daemon. The caller (typically the plugin-side
    /// MemoryStore impl) provides the loro update bytes from the local doc
    /// (e.g. via `doc.inner().subscribe_local_update` callback or explicit
    /// `export(ExportMode::updates_since)` after a write).
    pub async fn push_delta(
        &self,
        addr: BlockAddr,
        update_bytes: Vec<u8>,
    ) -> Result<(), MemorySyncError> {
        let edit = WireMemoryEdit::Delta {
            addr,
            payload: DeltaPayload::Inline { bytes: update_bytes },
        };
        self.tx
            .send(edit)
            .await
            .map_err(|_| MemorySyncError::StreamClosed)
    }

    /// Subscribe to additional addrs mid-session. The daemon will respond with
    /// `BlockAvailable` for each newly-added addr that resolves to a real block.
    pub async fn subscribe(&self, addrs: Vec<BlockAddr>) -> Result<(), MemorySyncError> {
        let edit = WireMemoryEdit::Subscribe { addrs, known: vec![] };
        self.tx
            .send(edit)
            .await
            .map_err(|_| MemorySyncError::StreamClosed)
    }

    /// Drop addrs from the watched set. Daemon responds with `BlockGone`
    /// (OutOfScope) for each, and the local cache is cleaned up accordingly.
    pub async fn unsubscribe(&self, addrs: Vec<BlockAddr>) -> Result<(), MemorySyncError> {
        let edit = WireMemoryEdit::Unsubscribe { addrs };
        self.tx
            .send(edit)
            .await
            .map_err(|_| MemorySyncError::StreamClosed)
    }
}

impl Drop for MemorySyncClient {
    fn drop(&mut self) {
        // Best-effort graceful close. Send Done synchronously via try_send
        // (we're in Drop, can't .await). If the channel is full or closed,
        // nothing to do; the daemon will see the rx-drop as a network close.
        let _ = self.tx.try_send(WireMemoryEdit::Done {
            reason: "plugin dropping client".into(),
        });
        // Receive task is aborted via JoinHandle drop (cancels the task).
    }
}

/// Receive loop: consume WireMemoryEvent from the daemon, update the local
/// cache. Runs until the stream closes (Done from daemon, rx drop, or our own
/// tx-side drop tearing down the connection).
async fn receive_loop(
    mut rx: mpsc::Receiver<WireMemoryEvent>,
    cache: Arc<DashMap<BlockAddr, CachedBlock>>,
    waiters: Arc<DashMap<BlockAddr, crossbeam_channel::Sender<()>>>,
    tx_for_subs: mpsc::Sender<WireMemoryEdit>,
) {
    loop {
        match rx.recv().await {
            Ok(Some(event)) => {
                if let Err(e) = handle_event(event, &cache, &waiters, &tx_for_subs) {
                    tracing::warn!(error = %e, "memory_sync_client: event handling failed");
                }
            }
            Ok(None) => {
                tracing::debug!("memory_sync_client: stream closed (None)");
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "memory_sync_client: receive error; closing");
                return;
            }
        }
    }
}

fn handle_event(
    event: WireMemoryEvent,
    cache: &DashMap<BlockAddr, CachedBlock>,
    waiters: &DashMap<BlockAddr, crossbeam_channel::Sender<()>>,
    tx_for_subs: &mpsc::Sender<WireMemoryEdit>,
) -> Result<(), MemorySyncError> {
    match event {
        WireMemoryEvent::BlockAvailable {
            addr,
            metadata,
            snapshot,
        } => {
            let bytes = match snapshot {
                SnapshotPayload::Inline { bytes } => bytes,
                SnapshotPayload::Chunked { .. } => return Err(MemorySyncError::UnsupportedChunked),
                _ => return Err(MemorySyncError::UnsupportedChunked),
            };
            // Materialise the doc from the snapshot bytes + metadata.
            // Note: BlockMetadata carries its own schema; from_snapshot_with_metadata
            // takes (snapshot, metadata, accessor_agent_id).
            let doc = StructuredDocument::from_snapshot_with_metadata(
                &bytes,
                metadata,
                None,
            )
            .map_err(|e| MemorySyncError::SnapshotDecode {
                addr: addr.clone(),
                message: format!("{e}").into(),
            })?;
            let doc_arc = Arc::new(doc);
            let sub = subscribe_auto_push(&doc_arc, addr.clone(), tx_for_subs.clone());
            cache.insert(addr.clone(), CachedBlock { doc: doc_arc, _sub: sub });
            // Fire any registered first-arrival waiter for this addr (e.g.
            // PluginMemoryStore::create_block awaiting the daemon's snapshot
            // after MemoryCreateBlock returned the addr).
            if let Some((_, waiter)) = waiters.remove(&addr) {
                let _ = waiter.send(());
            }
        }
        WireMemoryEvent::Delta { addr, payload } => {
            let bytes = match payload {
                DeltaPayload::Inline { bytes } => bytes,
                DeltaPayload::Chunked { .. } => return Err(MemorySyncError::UnsupportedChunked),
                _ => return Err(MemorySyncError::UnsupportedChunked),
            };
            let Some(entry) = cache.get(&addr) else {
                tracing::warn!(addr = ?addr, "memory_sync_client: Delta for unknown addr; skipping (daemon may have missed Subscribe ack)");
                return Ok(());
            };
            entry.doc.apply_updates(&bytes).map_err(|e| {
                MemorySyncError::DeltaApply {
                    addr: addr.clone(),
                    message: format!("{e}").into(),
                }
            })?;
        }
        WireMemoryEvent::MetadataChanged { addr, metadata } => {
            // Swap in a new StructuredDocument carrying the updated metadata
            // while preserving the underlying loro state. `LoroDoc::clone` is
            // a reference clone (loro is internally Arc'd) so this is cheap
            // and the existing CRDT state stays consistent across the swap.
            let old_doc = match cache.get(&addr).map(|e| Arc::clone(&e.value().doc)) {
                Some(d) => d,
                None => {
                    tracing::warn!(addr = ?addr, "memory_sync_client: MetadataChanged for unknown addr; skipping");
                    return Ok(());
                }
            };
            let loro_doc_clone = old_doc.inner().clone();
            let schema = old_doc.schema().clone();
            let new_doc = StructuredDocument::from_doc_with_metadata(
                loro_doc_clone,
                metadata,
                schema,
            )
            .map_err(|e| MemorySyncError::SnapshotDecode {
                addr: addr.clone(),
                message: format!("from_doc_with_metadata on MetadataChanged: {e}").into(),
            })?;
            let new_doc_arc = Arc::new(new_doc);
            let sub = subscribe_auto_push(&new_doc_arc, addr.clone(), tx_for_subs.clone());
            cache.insert(addr, CachedBlock { doc: new_doc_arc, _sub: sub });
        }
        WireMemoryEvent::BlockGone { addr, reason } => {
            cache.remove(&addr);
            tracing::debug!(addr = ?addr, reason = ?reason, "memory_sync_client: block removed from cache");
        }
        WireMemoryEvent::Done { reason } => {
            tracing::debug!(reason = %reason, "memory_sync_client: daemon signalled clean close");
            // Receive loop will exit on next rx.recv() returning Ok(None).
        }
        _ => {
            tracing::warn!("memory_sync_client: received unknown WireMemoryEvent variant");
        }
    }
    Ok(())
}

/// Subscribe to loro local-update events on a freshly-materialised doc. The
/// callback fires on plugin-local edits (NOT on imports — loro's
/// subscribe_local_update is intentionally local-only), pushing the resulting
/// update bytes upstream as `WireMemoryEdit::Delta`. This is the plugin half
/// of the echo-suppressed sync loop: daemon-side edits arrive as `Delta`
/// events handled by `handle_event` (which calls apply_updates, NOT firing
/// this subscription), while plugin-side edits flow through here back to the
/// daemon (which has its own echo filter via OriginTag matching).
fn subscribe_auto_push(
    doc: &Arc<StructuredDocument>,
    addr: BlockAddr,
    tx: mpsc::Sender<WireMemoryEdit>,
) -> loro::Subscription {
    doc.inner().subscribe_local_update(Box::new(move |update_bytes| {
        let edit = WireMemoryEdit::Delta {
            addr: addr.clone(),
            payload: DeltaPayload::Inline { bytes: update_bytes.clone() },
        };
        // subscribe_local_update fires synchronously from inside loro's commit
        // path. We're in a tokio task context (the plugin called the
        // MemoryStore mutation method that triggered the commit), so we can
        // spawn the async send. Fire-and-forget: if the stream is closed we
        // log on next call; the daemon will see the connection-drop on its end.
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            if let Err(_) = tx_clone.send(edit).await {
                tracing::warn!("memory_sync_client: auto-push Delta failed; stream closed");
            }
        });
        true // keep subscription active
    }))
}
