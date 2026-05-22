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
    while let Ok(req) = rx.recv() {
        match req {
            Request::CreateBlock { scope, create, reply } => {
                let args = MemoryCreateBlockArgs { scope, create };
                let result = match runtime.block_on(host.rpc(args)) {
                    Ok(inner) => inner,
                    Err(e) => Err(MemoryError::Other(format!(
                        "plugin host MemoryCreateBlock transport: {e}"
                    ))),
                };
                let _ = reply.send(result);
            }
            Request::DeleteBlock { addr, reply } => {
                let result = match runtime.block_on(host.rpc(MemoryDeleteBlockRequest(addr))) {
                    Ok(inner) => inner,
                    Err(e) => Err(MemoryError::Other(format!(
                        "plugin host MemoryDeleteBlock transport: {e}"
                    ))),
                };
                let _ = reply.send(result);
            }
            Request::ListBlocks { filter, reply } => {
                let result = match runtime.block_on(host.rpc(filter)) {
                    Ok(inner) => inner,
                    Err(e) => Err(MemoryError::Other(format!(
                        "plugin host MemoryListBlocks transport: {e}"
                    ))),
                };
                let _ = reply.send(result);
            }
        }
    }
    tracing::debug!("plugin-memory-store-worker exiting");
}

impl MemoryStore for PluginMemoryStore {
    fn create_block(
        &self,
        scope: &Scope,
        create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        let (reply, recv) = cb::bounded(1);
        self.req_tx
            .send(Request::CreateBlock { scope: scope.clone(), create, reply })
            .map_err(|_| MemoryError::Other("plugin memory store worker gone".into()))?;
        let addr = recv.recv()
            .map_err(|_| MemoryError::Other("plugin memory store reply dropped".into()))??;
        // After host accepts create, MemorySync emits BlockAvailable for the
        // new addr. Poll briefly. 7b.2b: replace with an explicit notify on
        // cache insertion (notify per-addr from receive_loop, awaited here).
        for _ in 0..100 {
            if let Some(doc) = self.sync.get_block(&addr) {
                return Ok((*doc).clone());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(MemoryError::Other(format!(
            "create_block: BlockAvailable for {addr:?} did not arrive within 5s"
        )))
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
        &self, _scope: &Scope, _create: BlockCreate,
    ) -> MemoryResult<StructuredDocument> {
        Err(MemoryError::Other("plugin memory store: create_or_replace_block not wired (7b.2b)".into()))
    }
    fn commit_write(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> { Ok(()) }
    fn get_rendered_content(&self, _scope: &Scope, _label: &str) -> MemoryResult<Option<String>> {
        Err(MemoryError::Other("plugin memory store: get_rendered_content not wired (7b.2b)".into()))
    }
    fn persist_block(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> {
        Err(MemoryError::Other("plugin memory store: persist_block not wired (7b.2b)".into()))
    }
    fn mark_dirty(&self, _scope: &Scope, _label: &str) -> MemoryResult<()> { Ok(()) }
    fn insert_archival(&self, _scope: &Scope, _content: &str, _metadata: Option<serde_json::Value>) -> MemoryResult<String> {
        Err(MemoryError::Other("plugin memory store: insert_archival not wired (7b.2b)".into()))
    }
    fn search_archival(&self, _scope: &Scope, _query: &str, _limit: usize) -> MemoryResult<Vec<ArchivalEntry>> {
        Err(MemoryError::Other("plugin memory store: search_archival not wired (7b.2b)".into()))
    }
    fn delete_archival(&self, _id: &str) -> MemoryResult<()> {
        Err(MemoryError::Other("plugin memory store: delete_archival not wired (7b.2b)".into()))
    }
    fn search(&self, _q: &str, _o: SearchOptions, _s: MemorySearchScope) -> MemoryResult<Vec<MemorySearchResult>> {
        Err(MemoryError::Other("plugin memory store: search not wired (7b.2b)".into()))
    }
    fn list_shared_blocks(&self, _scope: &Scope) -> MemoryResult<Vec<SharedBlockInfo>> {
        Err(MemoryError::Other("plugin memory store: list_shared_blocks needs new host RPC".into()))
    }
    fn get_shared_block(&self, _r: &Scope, _o: &Scope, _l: &str) -> MemoryResult<Option<StructuredDocument>> {
        Err(MemoryError::Other("plugin memory store: get_shared_block not wired (7b.2b)".into()))
    }
    fn update_block_metadata(&self, _s: &Scope, _l: &str, _p: BlockMetadataPatch) -> MemoryResult<()> {
        Err(MemoryError::Other("plugin memory store: update_block_metadata not wired (7b.2b)".into()))
    }
    fn undo_redo(&self, _s: &Scope, _l: &str, _op: UndoRedoOp) -> MemoryResult<bool> {
        Err(MemoryError::Other("plugin memory store: undo_redo not wired (7b.2b)".into()))
    }
    fn history_depth(&self, _s: &Scope, _l: &str) -> MemoryResult<UndoRedoDepth> {
        Err(MemoryError::Other("plugin memory store: history_depth needs new host RPC".into()))
    }
}
