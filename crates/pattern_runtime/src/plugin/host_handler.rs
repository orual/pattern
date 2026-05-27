// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use pattern_core::error::MemoryError;
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

fn me(m: &str) -> MemoryError {
    MemoryError::Other(format!("{m}: not yet implemented"))
}

async fn handle(msg: PluginHostMessage, ctx: &HostApiContext) {
    // A.2c.2+: per-variant dispatch wired incrementally. Variants still returning
    // Err(Unimplemented) are queued for follow-up commits.
    use PluginHostMessage::*;
    match msg {
        HostSendMessage(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::types::ids::PersonaId;
            use pattern_core::types::origin::{Author, MessageOrigin};
            use pattern_core::wire::ui::Recipient;
            let pam = inner;
            let target_id = match &pam.recipient {
                Recipient::Direct(id) => PersonaId::from(id.as_str()),
                Recipient::Address(p) => PersonaId::from(p.as_str()),
                Recipient::Auto => { let _ = tx.send(Err(WirePluginError::Other { message: "HostSendMessage: Recipient::Auto needs fronting resolution; not yet wired via plugin host_handler".into() })).await; return; }
            };
            // Construct MessageOrigin server-side from the plugin's self-reported
            // fields. Plugin cannot encode Partner/Human/Agent/System authorship
            // via this wire — the type doesn't expose those variants.
            let mut origin = MessageOrigin::new(
                Author::Plugin { plugin_id: pam.plugin_id.clone(), partner_authority: pam.partner_authority },
                pam.sphere,
            );
            if let Some(hint) = pam.transport_hint { origin = origin.with_transport_hint(hint); }
            let chat_message = genai::chat::ChatMessage {
                role: genai::chat::ChatRole::User,
                content: pattern_core::types::provider::MessageContent::from_parts(pam.parts.clone()),
                options: None,
            };
            let message = pattern_core::types::message::Message {
                chat_message,
                id: pattern_core::types::ids::MessageId::from(pattern_core::types::ids::new_id().to_string()),
                position: pattern_core::types::ids::new_snowflake_id(),
                owner_id: pattern_core::types::ids::AgentId::from(target_id.as_str()),
                created_at: jiff::Timestamp::now(),
                batch: pam.batch_id.clone(),
                response_meta: None,
                block_refs: vec![],
                attachments: vec![],
            };
            let mailbox_input = crate::mailbox::MailboxInput::new(origin, message);
            let result = match ctx.agent_registry.route_or_queue(&target_id, mailbox_input) {
                Ok(()) => Ok(()),
                Err(e) => Err(WirePluginError::Other { message: format!("{e}").into() }),
            };
            if tx.send(result).await.is_err() { tracing::warn!(method = "HostSendMessage", "plugin host_handler: reply receiver dropped before send"); }
        }


        MemoryCreateBlock(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let args = inner;
            let label = args.create.label.clone().into();
            let result = ctx.memory_store.create_block(&args.scope, args.create)
                .map(|_doc| BlockAddr { scope: args.scope.clone(), label });
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryCreateBlock", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemoryDeleteBlock(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            // Wrap shape: irpc generates MemoryDeleteBlockRequest as a tuple struct wrapping BlockAddr; access via inner.0
            let addr = &inner.0;
            let scope = addr.scope.clone();
            let result = ctx.memory_store.delete_block(&scope, &addr.label);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }

        MemorySearch(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            use pattern_core::types::memory_types::{MemorySearchScope, SearchOptions};
            let q = inner.0;
            let scope = q.scope.unwrap_or_else(|| MemorySearchScope::Scope(ctx.default_scope.clone()));
            let opts = SearchOptions::new().limit(q.limit as usize).blocks_only();
            let result = ctx.memory_store.search(&q.query, opts, scope)
                .map(|hits| hits.into_iter().filter_map(|h| {
                    // Only surface block hits over the wire (archival/message hits are
                    // out of scope for plugin block addressing).
                    use pattern_core::types::memory_types::SearchHit;
                    let addr = match h.hit {
                        SearchHit::Block { scope, label } => BlockAddr { scope, label },
                        SearchHit::Archival { .. } | SearchHit::Message { .. } => return None,
                        _ => return None,
                    };
                    Some(WireSearchResult {
                        addr,
                        snippet: h.content.unwrap_or_default(),
                        score: h.score,
                    })
                }).collect::<Vec<_>>());
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemorySearch", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemoryListBlocks(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let result = ctx
                .memory_store
                .list_blocks(inner);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }
        MemoryPersist(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let addr = &inner.0;
            let scope = addr.scope.clone();
            let result = ctx.memory_store.persist_block(&scope, &addr.label);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }

        MemoryUpdateMetadata(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let args = inner.0;
            let scope = args.addr.scope.clone();
            let result = ctx.memory_store.update_block_metadata(&scope, &args.addr.label, args.patch);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }

        MemoryUndoRedo(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let args = inner.0;
            let scope = args.addr.scope.clone();
            let result = ctx.memory_store.undo_redo(&scope, &args.addr.label, args.op);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }

        MemoryGetSharedBlock(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let args = inner.0;
            // Use the plugin-supplied requester scope explicitly. The trait's
            // permission check is keyed on this; silently substituting
            // ctx.default_scope would let a plugin query as a different
            // identity than it specified, which is the wrong shape for a
            // permission gate.
            let result = ctx.memory_store.get_shared_block(&args.requester, &args.owner, &args.label)
                .map(|doc_opt| doc_opt.map(|_doc| BlockAddr { scope: args.owner.clone(), label: args.label.clone() }));
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryGetSharedBlock", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemoryInsertArchival(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let args = inner;
            let result = ctx.memory_store
                .insert_archival(&args.scope, &args.content, args.metadata)
                .map(|id| smol_str::SmolStr::from(id));
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryInsertArchival", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemorySearchArchival(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            use pattern_core::types::memory_types::MemorySearchScope;
            let q = inner.0;
            // Respect plugin-supplied scope when it targets a single scope.
            // Fall back to session default for Constellation/None (archival
            // search is single-scope; multi-scope iteration would need its
            // own protocol shape).
            let scope = match q.scope {
                Some(MemorySearchScope::Scope(s)) => s,
                _ => ctx.default_scope.clone(),
            };
            let result = ctx.memory_store.search_archival(&scope, &q.query, q.limit as usize);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }

        MemoryDeleteArchival(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let result = ctx.memory_store.delete_archival(&inner.0);
            if tx.send(result).await.is_err() { tracing::warn!("plugin host_handler: reply receiver dropped before send (plugin call abandoned mid-flight)"); }
        }

        MemoryCreateOrReplaceBlock(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let args = inner.0;
            let label = args.create.label.clone().into();
            let result = ctx.memory_store.create_or_replace_block(&args.scope, args.create)
                .map(|_doc| BlockAddr { scope: args.scope.clone(), label });
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryCreateOrReplaceBlock", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemoryListConstellationScopes(req) => {
            let WithChannels { tx, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let result = ctx.memory_store.list_constellation_scopes();
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryListConstellationScopes", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemoryListSharedBlocks(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let scope = inner.0;
            let result = ctx.memory_store.list_shared_blocks(&scope);
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryListSharedBlocks", "plugin host_handler: reply receiver dropped before send"); }
        }

        MemoryHistoryDepth(req) => {
            let WithChannels { tx, inner, .. } = req;
            use pattern_core::traits::memory_store::MemoryStore;
            let addr = inner.0;
            let result = ctx.memory_store.history_depth(&addr.scope, addr.label.as_str());
            if tx.send(result).await.is_err() { tracing::warn!(method = "MemoryHistoryDepth", "plugin host_handler: reply receiver dropped before send"); }
        }
    }

}
