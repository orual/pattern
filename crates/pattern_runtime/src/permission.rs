//! Sync-to-async bridge from the eval-worker thread to the
//! [`pattern_core::permission::PermissionBroker`].
//!
//! The eval worker runs Haskell on a plain OS thread (no tokio runtime
//! context — see `agent_loop::eval_worker`). Effect handlers that need
//! to escalate through the broker can't `block_on` the broker's async
//! API: that would either deadlock against the ambient runtime or
//! require spawning a fresh executor. Instead, every session owns a
//! [`PermissionBridge`] — an `mpsc` channel feeding a tokio task that
//! invokes the broker on the runtime — and handlers call
//! [`PermissionBridge::request_sync`] from the eval worker's thread.
//!
//! The shape mirrors [`crate::router::RouterBridge`] for consistency.

use std::sync::Arc;
use std::time::Duration;

use pattern_core::permission::{PermissionBroker, PermissionGrant, PermissionScope};
use pattern_core::types::origin::MessageOrigin;

/// One bridged request from the eval-worker thread to the async broker
/// task.
struct PermissionBridgeRequest {
    agent_id: pattern_core::AgentId,
    tool_name: String,
    scope: PermissionScope,
    origin: MessageOrigin,
    reason: Option<String>,
    metadata: Option<serde_json::Value>,
    timeout: Duration,
    /// Sync reply channel — the eval worker blocks on `recv` here.
    reply: std::sync::mpsc::SyncSender<Option<PermissionGrant>>,
}

/// Bridge between the sync eval worker and the async
/// [`PermissionBroker`].
///
/// Holds the send half of a `tokio::sync::mpsc` channel; a long-lived
/// tokio task drains it and invokes `broker.request(...)`. Replies
/// come back via a `std::sync::mpsc::sync_channel` so the eval worker
/// can block on the result without needing a tokio context.
///
/// `tokio::sync::mpsc::UnboundedSender::send` is documented as safe to
/// call from non-tokio threads; the reply path is plain stdlib.
#[derive(Clone, Debug)]
pub struct PermissionBridge {
    tx: tokio::sync::mpsc::UnboundedSender<PermissionBridgeRequest>,
}

impl PermissionBridge {
    /// Spawn the bridge task and return a handle. The task runs until
    /// the bridge (and all clones) are dropped, closing the channel
    /// and terminating the task.
    pub fn spawn(broker: Arc<PermissionBroker>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PermissionBridgeRequest>();
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let grant = broker
                    .request(
                        req.agent_id,
                        req.tool_name,
                        req.scope,
                        &req.origin,
                        req.reason,
                        req.metadata,
                        req.timeout,
                    )
                    .await;
                // Reply may fail if the eval worker abandoned its receiver
                // (e.g. cancelled or timed out before we replied) — that's
                // not an error, just drop the result.
                let _ = req.reply.send(grant);
            }
        });
        Self { tx }
    }

    /// Request a permission grant synchronously. Blocks the calling
    /// thread until the async broker responds (or the timeout
    /// elapses on the broker side and we receive the resulting `None`).
    ///
    /// Safe to call from a plain OS thread (no tokio runtime context
    /// required). Returns `None` when the bridge channel is closed,
    /// when the broker denies, or when the broker times out.
    #[allow(clippy::too_many_arguments)]
    pub fn request_sync(
        &self,
        agent_id: pattern_core::AgentId,
        tool_name: String,
        scope: PermissionScope,
        origin: &MessageOrigin,
        reason: Option<String>,
        metadata: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Option<PermissionGrant> {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        let request = PermissionBridgeRequest {
            agent_id,
            tool_name,
            scope,
            origin: origin.clone(),
            reason,
            metadata,
            timeout,
            reply: reply_tx,
        };
        if self.tx.send(request).is_err() {
            // Bridge task has terminated — surfaces as denial.
            tracing::warn!("permission bridge channel closed; treating as denial");
            return None;
        }
        // Wait for the reply. The broker enforces its own timeout and
        // returns `None` on expiry; we cap the sync wait slightly
        // longer to absorb queuing + crossover latency. If even that
        // passes, the bridge task has stalled — denial is the safe
        // failure mode.
        let sync_cap = timeout + Duration::from_secs(1);
        match reply_rx.recv_timeout(sync_cap) {
            Ok(grant) => grant,
            Err(_) => {
                tracing::warn!(
                    "permission bridge reply channel timed out after {:?}",
                    sync_cap
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::permission::PermissionDecisionKind;
    use pattern_core::types::ids::new_id;
    use pattern_core::types::origin::{Author, Human, Sphere};

    fn human_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Human(Human {
                user_id: new_id(),
                display_name: None,
            }),
            Sphere::Private,
        )
    }

    #[tokio::test]
    async fn bridge_round_trips_an_approval_to_a_sync_caller() {
        let broker = Arc::new(PermissionBroker::new());

        // Subscribe BEFORE spawning the responder — `subscribe` returns a
        // receiver whose `lag` is reset to the next broadcast.
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                    .await;
            }
        });

        let bridge = PermissionBridge::spawn(broker);
        let scope = PermissionScope::ToolExecution {
            tool: "shell".into(),
            args_digest: Some("d".into()),
        };
        let origin = human_origin();
        let bridge_for_thread = bridge.clone();
        let scope_for_thread = scope.clone();

        // Run request_sync on a non-tokio worker thread — that's the
        // production call shape (eval worker thread). `spawn_blocking`
        // keeps the tokio runtime free to poll the bridge's pump task.
        let grant = tokio::task::spawn_blocking(move || {
            bridge_for_thread.request_sync(
                pattern_core::AgentId::from("agent"),
                "shell".into(),
                scope_for_thread,
                &origin,
                None,
                None,
                Duration::from_millis(500),
            )
        })
        .await
        .expect("blocking task")
        .expect("approval should reach the sync caller");
        assert_eq!(grant.scope, scope);
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn bridge_returns_none_on_broker_denial() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let broker_for_responder = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker_for_responder
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });

        let bridge = PermissionBridge::spawn(broker);
        let scope = PermissionScope::ToolExecution {
            tool: "shell".into(),
            args_digest: None,
        };
        let origin = human_origin();
        let bridge_for_thread = bridge.clone();

        let result = tokio::task::spawn_blocking(move || {
            bridge_for_thread.request_sync(
                pattern_core::AgentId::from("agent"),
                "shell".into(),
                scope,
                &origin,
                None,
                None,
                Duration::from_millis(500),
            )
        })
        .await
        .expect("blocking task");
        assert!(result.is_none(), "denial should surface as None");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn bridge_partner_origin_short_circuits_via_broker_bypass() {
        // Sanity: the bridge passes the origin through to the broker,
        // so partner-origin requests still short-circuit (no responder
        // configured here).
        let broker = Arc::new(PermissionBroker::new());
        let bridge = PermissionBridge::spawn(broker);
        let scope = PermissionScope::ToolExecution {
            tool: "shell".into(),
            args_digest: None,
        };
        let partner = MessageOrigin::new(
            Author::Partner(pattern_core::types::origin::Partner {
                user_id: new_id(),
                display_name: None,
            }),
            Sphere::Private,
        );
        let bridge_for_thread = bridge.clone();
        let grant = tokio::task::spawn_blocking(move || {
            bridge_for_thread.request_sync(
                pattern_core::AgentId::from("agent"),
                "shell".into(),
                scope,
                &partner,
                None,
                None,
                Duration::from_millis(200),
            )
        })
        .await
        .expect("blocking task")
        .expect("partner bypass returns Some without any responder");
        let metadata = grant.metadata.expect("partner bypass marks metadata");
        assert_eq!(metadata["source"], "partner_bypass");
    }
}
