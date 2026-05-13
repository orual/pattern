//! V1 stub handler for the `pattern-plugin-host/1` ALPN (Phase 6 Task 5b).
//!
//! Plugin processes that dial this ALPN reach this actor for host-side
//! callbacks (HostSendMessage, task ops, skill invoke) and db-poking memory
//! ops. v1: every variant returns Unimplemented; real dispatch into the
//! runtime plugin registry lands when OOP plugins exist (task 5c+).

use irpc::{Client, WithChannels};
use pattern_core::traits::plugin::wire::*;
use tokio::sync::mpsc;

use pattern_core::plugin::protocol::{PluginHostMessage, PluginHostProtocol};

/// Spawn the host-handler actor. Returns a Client whose `as_local()` can
/// be passed to `PluginHostProtocol::remote_handler` for Router accept.
pub fn spawn() -> Client<PluginHostProtocol> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run(rx));
    Client::local(tx)
}

async fn run(mut rx: mpsc::Receiver<PluginHostMessage>) {
    while let Some(msg) = rx.recv().await {
        handle(msg).await;
    }
}

fn pe(m: &str) -> WirePluginError {
    WirePluginError::Unimplemented { method: m.into() }
}

fn me(m: &str) -> WireMemoryError {
    WireMemoryError::Unimplemented { method: m.into() }
}

async fn handle(msg: PluginHostMessage) {
    use PluginHostMessage::*;
    match msg {
        HostSendMessage(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostSendMessage"))).await; }
        HostTaskCreate(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskCreate"))).await; }
        HostTaskTransition(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskTransition"))).await; }
        HostTaskLink(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskLink"))).await; }
        HostTaskQuery(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Vec::new()).await; }
        HostSkillInvoke(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostSkillInvoke"))).await; }
        MemoryCreateBlock(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryCreateBlock"))).await; }
        MemoryDeleteBlock(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryDeleteBlock"))).await; }
        MemorySearch(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Vec::new()).await; }
        MemoryListBlocks(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Vec::new()).await; }
        MemoryPersist(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryPersist"))).await; }
        MemoryUpdateMetadata(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryUpdateMetadata"))).await; }
        MemoryUndoRedo(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryUndoRedo"))).await; }
        MemoryGetSharedBlock(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryGetSharedBlock"))).await; }
        MemoryInsertArchival(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryInsertArchival"))).await; }
        MemorySearchArchival(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Vec::new()).await; }
        MemoryDeleteArchival(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryDeleteArchival"))).await; }
    }
}
