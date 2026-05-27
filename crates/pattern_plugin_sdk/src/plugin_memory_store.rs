// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Plugin-side MemoryStore via channel-+-worker bridge.
//!
//! Trait is sync; host RPC client is async. Bridge: sync trait method ->
//! crossbeam_channel send to dispatcher thread -> handle.block_on(client.rpc())
//! -> reply via per-call crossbeam_channel::bounded(1). Local cache reads
//! (get_block / get_block_metadata) skip the worker.

use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel as cb;
use irpc::Client;
use pattern_core::error::MemoryError;
use pattern_core::memory::StructuredDocument;
use pattern_core::plugin::protocol::{
    MemoryCreateBlockArgs, MemoryDeleteBlockRequest, PluginHostProtocol,
};
use pattern_core::traits::MemoryStore;
use pattern_core::traits::plugin::wire::BlockAddr;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    ArchivalEntry, BlockFilter, BlockMetadata, BlockMetadataPatch, MemoryResult,
    MemorySearchResult, MemorySearchScope, Scope, SearchOptions, SharedBlockInfo, UndoRedoDepth,
    UndoRedoOp,
};

use crate::memory_sync_client::MemorySyncClient;

enum Request {
    CreateBlock {
        scope: Scope,
        create: BlockCreate,
        reply: cb::Sender<Result<BlockAddr, MemoryError>>,
    },
    DeleteBlock {
        addr: BlockAddr,
        reply: cb::Sender<Result<(), MemoryError>>,
    },
    ListBlocks {
        filter: BlockFilter,
        reply: cb::Sender<Result<Vec<BlockMetadata>, MemoryError>>,
    },
    PersistBlock {
        addr: BlockAddr,
        reply: cb::Sender<Result<(), MemoryError>>,
    },
    DeleteArchival {
        id: smol_str::SmolStr,
        reply: cb::Sender<Result<(), MemoryError>>,
    },
    UndoRedo {
        addr: BlockAddr,
        op: UndoRedoOp,
        reply: cb::Sender<Result<bool, MemoryError>>,
    },
    UpdateBlockMetadata {
        addr: BlockAddr,
        patch: BlockMetadataPatch,
        reply: cb::Sender<Result<(), MemoryError>>,
    },
    CreateOrReplaceBlock {
        scope: Scope,
        create: BlockCreate,
        reply: cb::Sender<Result<BlockAddr, MemoryError>>,
    },
    Search {
        query: pattern_core::traits::plugin::wire::WireSearchQuery,
        reply: cb::Sender<Result<Vec<pattern_core::traits::plugin::wire::WireSearchResult>, MemoryError>>,
    },
    SearchArchival {
        query: pattern_core::traits::plugin::wire::WireSearchQuery,
        reply: cb::Sender<Result<Vec<ArchivalEntry>, MemoryError>>,
    },
    GetSharedBlock {
        requester: Scope,
        owner: Scope,
        label: smol_str::SmolStr,
        reply: cb::Sender<Result<Option<BlockAddr>, MemoryError>>,
    },
    ListSharedBlocks {
        scope: Scope,
        reply: cb::Sender<Result<Vec<SharedBlockInfo>, MemoryError>>,
    },
    HistoryDepth {
        addr: BlockAddr,
        reply: cb::Sender<Result<UndoRedoDepth, MemoryError>>,
    },
    InsertArchival {
        scope: Scope,
        content: String,
        metadata: Option<serde_json::Value>,
        reply: cb::Sender<Result<smol_str::SmolStr, MemoryError>>,
    },
}

#[derive(Clone)]
pub struct PluginMemoryStore {
    sync: Arc<MemorySyncClient>,
    req_tx: cb::Sender<Request>,
    _worker: Arc<WorkerHandle>,
}

struct WorkerHandle {
    join: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        if let Some(j) = self.join.lock().ok().and_then(|mut g| g.take()) {
            let _ = j.join();
        }
    }
}

impl std::fmt::Debug for PluginMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginMemoryStore").finish_non_exhaustive()
    }
}

impl PluginMemoryStore {
    pub fn new(
        sync: Arc<MemorySyncClient>,
        host: Client<PluginHostProtocol>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let (req_tx, req_rx) = cb::unbounded::<Request>();
        let join = std::thread::Builder::new()
            .name("plugin-memory-store-worker".into())
            .spawn(move || worker_loop(req_rx, host, runtime))
            .expect("spawn plugin-memory-store-worker thread");
        Self {
            sync,
            req_tx,
            _worker: Arc::new(WorkerHandle {
                join: std::sync::Mutex::new(Some(join)),
            }),
        }
    }
}

fn worker_loop(
    rx: cb::Receiver<Request>,
    host: Client<PluginHostProtocol>,
    runtime: tokio::runtime::Handle,
) {
    // Dispatcher: pulls requests off the crossbeam channel and spawns each
    // onto the tokio runtime as an independent task. Lets concurrent
    // MemoryStore calls run in parallel against the host instead of
    // serializing through this single worker thread.
    while let Ok(req) = rx.recv() {
        let host_clone = host.clone();
        runtime.spawn(handle_request(req, host_clone));
    }
    tracing::debug!("plugin-memory-store-worker exiting");
}

async fn handle_request(req: Request, host: Client<PluginHostProtocol>) {
    match req {
        Request::CreateBlock { scope, create, reply } => {
            let args = MemoryCreateBlockArgs { scope, create };
            let result = match host.rpc(args).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryCreateBlock transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::DeleteBlock { addr, reply } => {
            let result = match host.rpc(MemoryDeleteBlockRequest(addr)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryDeleteBlock transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::ListBlocks { filter, reply } => {
            let result = match host.rpc(filter).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryListBlocks transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::PersistBlock { addr, reply } => {
            use pattern_core::plugin::protocol::MemoryPersistRequest;
            let result = match host.rpc(MemoryPersistRequest(addr)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryPersist transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::DeleteArchival { id, reply } => {
            use pattern_core::plugin::protocol::MemoryDeleteArchivalRequest;
            let result = match host.rpc(MemoryDeleteArchivalRequest(id)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryDeleteArchival transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::UndoRedo { addr, op, reply } => {
            use pattern_core::plugin::protocol::{MemoryUndoRedoArgs, MemoryUndoRedoRequest};
            let args = MemoryUndoRedoArgs { addr, op };
            let result = match host.rpc(MemoryUndoRedoRequest(args)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryUndoRedo transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::UpdateBlockMetadata { addr, patch, reply } => {
            use pattern_core::plugin::protocol::{MemoryUpdateMetadataArgs, MemoryUpdateMetadataRequest};
            let args = MemoryUpdateMetadataArgs { addr, patch };
            let result = match host.rpc(MemoryUpdateMetadataRequest(args)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryUpdateMetadata transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::CreateOrReplaceBlock { scope, create, reply } => {
            use pattern_core::plugin::protocol::{MemoryCreateBlockArgs, MemoryCreateOrReplaceBlockRequest};
            let args = MemoryCreateBlockArgs { scope, create };
            let result = match host.rpc(MemoryCreateOrReplaceBlockRequest(args)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryCreateOrReplaceBlock transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::Search { query, reply } => {
            use pattern_core::plugin::protocol::MemorySearchRequest;
            let result = match host.rpc(MemorySearchRequest(query)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemorySearch transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::SearchArchival { query, reply } => {
            use pattern_core::plugin::protocol::MemorySearchArchivalRequest;
            let result = match host.rpc(MemorySearchArchivalRequest(query)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemorySearchArchival transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::GetSharedBlock { requester, owner, label, reply } => {
            use pattern_core::plugin::protocol::{MemoryGetSharedBlockArgs, MemoryGetSharedBlockRequest};
            let args = MemoryGetSharedBlockArgs { requester, owner, label };
            let result = match host.rpc(MemoryGetSharedBlockRequest(args)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryGetSharedBlock transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::ListSharedBlocks { scope, reply } => {
            use pattern_core::plugin::protocol::MemoryListSharedBlocksRequest;
            let result = match host.rpc(MemoryListSharedBlocksRequest(scope)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryListSharedBlocks transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::HistoryDepth { addr, reply } => {
            use pattern_core::plugin::protocol::MemoryHistoryDepthRequest;
            let result = match host.rpc(MemoryHistoryDepthRequest(addr)).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryHistoryDepth transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
        Request::InsertArchival { scope, content, metadata, reply } => {
            use pattern_core::plugin::protocol::MemoryInsertArchivalArgs;
            let args = MemoryInsertArchivalArgs { scope, content, metadata };
            let result = match host.rpc(args).await {
                Ok(inner) => inner,
                Err(e) => Err(MemoryError::Other(format!(
                    "plugin host MemoryInsertArchival transport: {e}"
                ))),
            };
            let _ = reply.send(result);
        }
    }
}

impl MemoryStore for PluginMemoryStore {
    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        // Dispatch the host RPC + register a waiter for BlockAvailable in the
        // same step. Worker handles the host call; receive_loop fires the
        // waiter once the daemon's MemorySync stream delivers the snapshot.
        // No polling: blocks on a bounded(1) receiver with a sane timeout.
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::CreateBlock { scope: scope.clone(), create, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        let addr = recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))??;
        // If the block was already in the cache (rare but possible — the
        // BlockAvailable could have arrived between the RPC return and now),
        // skip waiter registration.
        if let Some(doc) = self.sync.get_block(&addr) {
            return Ok((*doc).clone());
        }
        let arrival = self.sync.register_block_arrival_waiter(addr.clone());
        // Re-check after registering: cache insert could have raced past us.
        if let Some(doc) = self.sync.get_block(&addr) {
            return Ok((*doc).clone());
        }
        match arrival.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(()) => match self.sync.get_block(&addr) {
                Some(doc) => Ok((*doc).clone()),
                None => Err(MemoryError::Other(format!(
                    "create_block: arrival waiter fired but cache lookup for {addr:?} returned None"
                ))),
            },
            Err(_) => Err(MemoryError::Other(format!(
                "create_block: BlockAvailable for {addr:?} did not arrive within 30s"
            ))),
        }
    }

    fn get_block(&self, scope: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        Ok(self.sync.get_block(&addr).map(|d| (*d).clone()))
    }

    fn get_block_metadata(
        &self,
        scope: &Scope,
        label: &str,
    ) -> MemoryResult<Option<BlockMetadata>> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        Ok(self.sync.get_block(&addr).map(|d| d.metadata().clone()))
    }

    fn list_blocks(&self, filter: BlockFilter) -> MemoryResult<Vec<BlockMetadata>> {
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::ListBlocks { filter, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }

    fn delete_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::DeleteBlock { addr, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }

    // 7b.2b stubs: same channel+worker pattern, more Request variants.
    fn create_or_replace_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        // Same notify-waiter shape as create_block: dispatch RPC, get addr,
        // wait for BlockAvailable via MemorySyncClient::register_block_arrival_waiter.
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::CreateOrReplaceBlock { scope: scope.clone(), create, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        let addr = recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))??;
        if let Some(doc) = self.sync.get_block(&addr) {
            return Ok((*doc).clone());
        }
        let arrival = self.sync.register_block_arrival_waiter(addr.clone());
        if let Some(doc) = self.sync.get_block(&addr) {
            return Ok((*doc).clone());
        }
        match arrival.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(()) => match self.sync.get_block(&addr) {
                Some(doc) => Ok((*doc).clone()),
                None => Err(MemoryError::Other(format!(
                    "create_or_replace_block: arrival waiter fired but cache lookup for {addr:?} returned None"
                ))),
            },
            Err(_) => Err(MemoryError::Other(format!(
                "create_or_replace_block: BlockAvailable for {addr:?} did not arrive within 30s"
            ))),
        }
    }
    fn commit_write(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> { Ok(()) }
    fn get_rendered_content(&self, scope: &Scope, label: &str) -> MemoryResult<Option<String>> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        match self.sync.get_block(&addr) {
            Some(doc) => Ok(Some(doc.render())),
            None => Ok(None),
        }
    }
    fn persist_block(&self, scope: &Scope, label: &str) -> MemoryResult<()> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::PersistBlock { addr, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
    fn mark_dirty(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> { Ok(()) }
    fn insert_archival(
        &self,
        scope: &Scope,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> MemoryResult<String> {
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::InsertArchival {
                scope: scope.clone(),
                content: content.to_string(),
                metadata,
                reply,
            })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        let id = recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))??;
        Ok(id.to_string())
    }
    fn search_archival(&self, scope: &Scope, query: &str, limit: usize) -> MemoryResult<Vec<ArchivalEntry>> {
        use pattern_core::traits::plugin::wire::WireSearchQuery;
        let wire_query = WireSearchQuery {
            query: query.to_string(),
            scope: Some(MemorySearchScope::Scope(scope.clone())),
            limit: limit as u32,
        };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::SearchArchival { query: wire_query, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
    fn delete_archival(&self, id: &str) -> MemoryResult<()> {
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::DeleteArchival { id: id.into(), reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
    fn search(&self, query: &str, options: SearchOptions, scope: MemorySearchScope) -> MemoryResult<Vec<MemorySearchResult>> {
        use pattern_core::traits::plugin::wire::WireSearchQuery;
        use pattern_core::types::memory_types::{SearchContentType, SearchHit};
        let wire_query = WireSearchQuery {
            query: query.to_string(),
            scope: Some(scope),
            limit: options.limit as u32,
        };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::Search { query: wire_query, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        let wire_results = recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))??;
        Ok(wire_results.into_iter().map(|w| MemorySearchResult {
            hit: SearchHit::Block { scope: w.addr.scope, label: w.addr.label },
            content_type: SearchContentType::Blocks,
            content: Some(w.snippet),
            score: w.score,
        }).collect())
    }
    fn list_shared_blocks(&self, scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::ListSharedBlocks { scope: scope.clone(), reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
    fn get_shared_block(&self, requester: &Scope, owner: &Scope, label: &str) -> MemoryResult<Option<StructuredDocument>> {
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::GetSharedBlock { requester: requester.clone(), owner: owner.clone(), label: label.into(), reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        let maybe_addr = recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))??;
        // Host returned the addr if the requester has read access. Look up in
        // local cache. If the plugin hasn't subscribed to this addr the lookup
        // returns None - plugin can subscribe explicitly via MemorySyncClient.
        Ok(maybe_addr.and_then(|addr| self.sync.get_block(&addr).map(|d| (*d).clone())))
    }
    fn update_block_metadata(&self, scope: &Scope, label: &str, patch: BlockMetadataPatch) -> MemoryResult<()> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::UpdateBlockMetadata { addr, patch, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
    fn undo_redo(&self, scope: &Scope, label: &str, op: UndoRedoOp) -> MemoryResult<bool> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::UndoRedo { addr, op, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
    fn history_depth(&self, scope: &Scope, label: &str) -> MemoryResult<UndoRedoDepth> {
        let addr = BlockAddr { scope: scope.clone(), label: label.into() };
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::HistoryDepth { addr, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))?
    }
}
