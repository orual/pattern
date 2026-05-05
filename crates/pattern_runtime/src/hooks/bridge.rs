//! HookBridge: sync eval thread → async HookBus dispatch.
//!
//! The eval worker (no tokio runtime) sends hook events through the bridge.
//! The bridge task (spawned on the tokio runtime) dispatches to the bus.

use std::sync::Arc;

use pattern_core::hooks::HookBus;
use pattern_core::hooks::event::{HookEvent, HookResponse, HookSemantics};

/// Internal request from sync thread → bridge task.
struct HookBridgeRequest {
    event: HookEvent,
    reply: Option<std::sync::mpsc::SyncSender<HookResponse>>,
}

/// Bridge between sync eval threads and the async HookBus.
#[derive(Debug, Clone)]
pub struct HookBridge {
    tx: tokio::sync::mpsc::UnboundedSender<HookBridgeRequest>,
}

impl HookBridge {
    /// Spawn the bridge task. Uses the provided handle, or falls back to
    /// the current runtime, or becomes inert if neither is available.
    pub fn spawn(bus: Arc<HookBus>) -> Self {
        Self::spawn_with_handle(bus, tokio::runtime::Handle::try_current().ok())
    }

    /// Spawn with an explicit tokio handle.
    pub fn spawn_on(bus: Arc<HookBus>, handle: tokio::runtime::Handle) -> Self {
        Self::spawn_with_handle(bus, Some(handle))
    }

    fn spawn_with_handle(bus: Arc<HookBus>, handle: Option<tokio::runtime::Handle>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HookBridgeRequest>();

        if let Some(handle) = handle {
            handle.spawn(async move {
                while let Some(req) = rx.recv().await {
                    match req.event.semantics {
                        HookSemantics::Notification => {
                            bus.emit(req.event);
                        }
                        HookSemantics::Blocking => {
                            let response = bus.emit_blocking(req.event).await;
                            if let Some(reply) = req.reply {
                                let _ = reply.send(response);
                            }
                        }
                        _ => {
                            bus.emit(req.event);
                        }
                    }
                }
            });
        } else {
            // No runtime — bridge is inert. Events sent to tx will
            // accumulate in the channel but never be drained.
            // This happens in test contexts that create SessionContext
            // without a tokio runtime (e.g. wake evaluator tests).
            tracing::debug!("HookBridge::spawn called without tokio runtime; bridge is inert");
        }

        Self { tx }
    }

    /// Emit a notification event (fire-and-forget). Non-blocking.
    pub fn emit(&self, event: HookEvent) {
        let _ = self.tx.send(HookBridgeRequest {
            event,
            reply: None,
        });
    }

    /// Emit a blocking event and wait for the response.
    /// Blocks the calling thread until the hook bus resolves.
    pub fn emit_blocking_sync(&self, event: HookEvent) -> HookResponse {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        let request = HookBridgeRequest {
            event,
            reply: Some(reply_tx),
        };
        if self.tx.send(request).is_err() {
            return HookResponse::Continue;
        }
        // Wait with a timeout so a stalled bridge doesn't block the eval
        // thread indefinitely.
        match reply_rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(response) => response,
            Err(_) => HookResponse::Continue,
        }
    }
}

impl std::fmt::Debug for HookBridgeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookBridgeRequest")
            .field("tag", &self.event.tag)
            .field("has_reply", &self.reply.is_some())
            .finish()
    }
}
