//! Hook bridge: sync eval-thread → async hook bus dispatch.
//!
//! Same pattern as `PermissionBridge`: a tokio task drains an unbounded
//! channel of hook requests. The sync eval thread sends via tokio mpsc
//! (safe from non-tokio threads) and optionally blocks on a sync_channel
//! for the response.

use std::sync::Arc;

use pattern_core::hooks::bus::HookBus;
use pattern_core::hooks::event::{HookEvent, HookResponse, HookSemantics};
use pattern_core::hooks::gate::{GateDecision, GateRequest, GateResponse};

/// Request from the eval thread to the hook bridge task.
struct HookBridgeRequest {
    event: HookEvent,
    /// For notification events: None (fire-and-forget).
    /// For blocking events: Some(reply channel) to send the response back.
    reply: Option<std::sync::mpsc::SyncSender<HookResponse>>,
}

/// Bridge between sync handler code and the async hook bus.
///
/// Send half of a tokio mpsc channel. The bridge task drains it and
/// dispatches to the `HookBus`. Safe to call from non-tokio threads.
#[derive(Clone, Debug)]
pub struct HookBridge {
    tx: tokio::sync::mpsc::UnboundedSender<HookBridgeRequest>,
}

impl HookBridge {
    /// Spawn the bridge task. Runs until all senders are dropped.
    pub fn spawn(bus: Arc<HookBus>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HookBridgeRequest>();
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                match req.event.semantics {
                    HookSemantics::Notification => {
                        bus.emit(req.event);
                        // No reply needed for notifications.
                    }
                    HookSemantics::Blocking => {
                        let response = bus.emit_blocking(req.event).await;
                        if let Some(reply) = req.reply {
                            let _ = reply.send(response);
                        }
                    }
                    _ => {
                        // Future semantics variants: treat as notification.
                        bus.emit(req.event);
                    }
                }
            }
        });
        Self { tx }
    }

    /// Emit a notification event (fire-and-forget). Non-blocking.
    /// Safe to call from a sync thread.
    pub fn emit(&self, event: HookEvent) {
        let request = HookBridgeRequest {
            event,
            reply: None,
        };
        let _ = self.tx.send(request);
    }

    /// Emit a blocking event and wait for the response.
    /// Blocks the calling thread until the hook bus resolves.
    /// Safe to call from a plain OS thread (no tokio context needed).
    pub fn emit_blocking_sync(&self, event: HookEvent) -> HookResponse {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        let request = HookBridgeRequest {
            event,
            reply: Some(reply_tx),
        };
        if self.tx.send(request).is_err() {
            // Bridge task terminated — treat as Continue.
            return HookResponse::Continue;
        }
        // Block until the bridge task sends the response.
        reply_rx.recv().unwrap_or(HookResponse::Continue)
    }
}
