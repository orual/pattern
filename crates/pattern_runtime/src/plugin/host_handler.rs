//! Per-session host-side dispatcher for the `pattern-plugin-host/1` ALPN.
//!
//! Each `TidepoolSession` spawns its own host-handler at open time with a
//! [`HostApiContext`] bundle carrying the session's runtime registries. Plugin
//! processes that dial the host ALPN reach the right session via
//! [`SessionRoutingProtocolHandler`] looking up `session_id` from the route entry.
//!
//! v2 (post Phase A.2c) bundles per-session state. v3 (A.2c.2+) wires real dispatch
//! per variant — currently the bundle is plumbed but every variant still returns
//! `Unimplemented` until per-variant wiring lands.

use std::sync::Arc;

use irpc::{Client, WithChannels};
use pattern_core::AgentId;
use pattern_core::traits::plugin::wire::*;
use pattern_core::types::memory_types::Scope;
use tokio::sync::mpsc;

use pattern_core::plugin::protocol::{PluginHostMessage, PluginHostProtocol};
use pattern_core::traits::memory_store::MemoryStore;

/// Bundle of runtime registries a per-session host handler needs to dispatch
/// plugin → host callbacks. Cloned cheaply via Arc internals; each handler
/// holds its own copy.
#[derive(Clone)]
pub struct HostApiContext {
    /// This session's memory store. Memory ops route through this.
    pub memory_store: Arc<dyn MemoryStore>,
    /// This session's agent registry. Host-message dispatch routes through this.
    pub agent_registry: Arc<crate::agent_registry::AgentRegistry>,
    /// The session's agent_id — used as origin / target resolution context.
    pub session_agent_id: AgentId,
    /// Default scope for this session — used when wire ops don't specify one.
    pub default_scope: Scope,
    /// Constellation database (for archival ops, persistence).
    pub db: Arc<pattern_db::ConstellationDb>,
}

impl std::fmt::Debug for HostApiContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostApiContext")
            .field("session_agent_id", &self.session_agent_id)
            .field("default_scope", &self.default_scope)
            .finish_non_exhaustive()
    }
}

/// Spawn a per-session host-handler actor. Returns a Client whose `as_local()`
/// can be passed to `PluginHostProtocol::remote_handler` for Router accept.
///
/// The provided [`HostApiContext`] is moved into the actor task and used for
/// dispatch on every incoming `PluginHostMessage`.
pub fn spawn(ctx: HostApiContext) -> Client<PluginHostProtocol> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(run(rx, ctx));
    Client::local(tx)
}

async fn run(mut rx: mpsc::Receiver<PluginHostMessage>, ctx: HostApiContext) {
    while let Some(msg) = rx.recv().await {
        handle(msg, &ctx).await;
    }
}

fn pe(m: &str) -> WirePluginError {
    WirePluginError::Unimplemented { method: m.into() }
}

fn me(m: &str) -> WireMemoryError {
    WireMemoryError::Unimplemented { method: m.into() }
}

async fn handle(msg: PluginHostMessage, _ctx: &HostApiContext) {
    // A.2c.2+: replace Err(Unimplemented) with real dispatch per variant.
    // The _ctx underscore-prefix is intentional — switches to `ctx` as variants are wired.
    use PluginHostMessage::*;
    match msg {
        HostSendMessage(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostSendMessage"))).await; }
        HostTaskCreate(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskCreate"))).await; }
        HostTaskTransition(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskTransition"))).await; }
        HostTaskLink(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskLink"))).await; }
        HostTaskQuery(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostTaskQuery"))).await; }
        HostSkillInvoke(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(pe("HostSkillInvoke"))).await; }
        MemoryCreateBlock(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryCreateBlock"))).await; }
        MemoryDeleteBlock(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryDeleteBlock"))).await; }
        MemorySearch(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemorySearch"))).await; }
        MemoryListBlocks(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryListBlocks"))).await; }
        MemoryPersist(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryPersist"))).await; }
        MemoryUpdateMetadata(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryUpdateMetadata"))).await; }
        MemoryUndoRedo(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryUndoRedo"))).await; }
        MemoryGetSharedBlock(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryGetSharedBlock"))).await; }
        MemoryInsertArchival(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryInsertArchival"))).await; }
        MemorySearchArchival(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemorySearchArchival"))).await; }
        MemoryDeleteArchival(req) => { let WithChannels { tx, .. } = req; let _ = tx.send(Err(me("MemoryDeleteArchival"))).await; }
    }
}
