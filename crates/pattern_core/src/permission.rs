//! Per-runtime permission broker.
//!
//! Brokers are constructed one-per-`TidepoolSession` (Phase 1) so each
//! runtime has independent pending-request queues and approve-for-scope
//! caches. There is no global singleton — that path was retired in
//! v3-multi-agent Phase 1 Task 5.
//!
//! Approval flow:
//!
//! 1. A handler calls [`PermissionBroker::request`] with the
//!    immediate-dispatcher [`crate::types::origin::MessageOrigin`] —
//!    not the activating turn's origin. During normal model-driven
//!    flow the runtime publishes `Author::Agent(self)`, which never
//!    triggers the bypass.
//! 2. If the origin's
//!    [`crate::types::origin::MessageOrigin::bypasses_permission_gate`]
//!    predicate fires (i.e. the *immediate dispatcher* is a Partner —
//!    only possible from explicit direct-execution paths, not from
//!    autonomous agent activity), return a synthesized grant.
//! 3. Otherwise the broker checks its scope cache for a prior
//!    `ApproveForScope` or unexpired `ApproveForDuration` grant matching
//!    `(agent_id, scope)` — if present, returns without broadcasting.
//! 4. Cache miss broadcasts a [`PermissionRequest`] and awaits a
//!    decision via a oneshot channel. Timeouts return `None` (denial)
//!    and clean up pending state — no leaks.
//!
//! All durations on the wire are [`jiff::Span`]; cache expiry uses
//! [`jiff::Timestamp`]. The host-side `timeout` parameter on
//! [`PermissionBroker::request`] stays as `std::time::Duration` because
//! it is consumed by `tokio::time::timeout` directly.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast, oneshot};
use uuid::Uuid;

use crate::types::origin::MessageOrigin;

/// A scope predicate identifying *what* the agent is asking permission for.
///
/// Scopes are compared exactly: two `ToolExecution { tool: "shell", args_digest: Some("abc") }`
/// requests with identical fields hit the same cache entry; differing
/// `args_digest`s do not. Approve-for-scope grants therefore generalise
/// only as far as the scope's structure allows — callers choose the
/// granularity by what they pass.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PermissionScope {
    MemoryEdit {
        key: String,
    },
    MemoryBatch {
        prefix: String,
    },
    ToolExecution {
        tool: String,
        args_digest: Option<String>,
    },
    DataSourceAction {
        source_id: String,
        action: String,
    },
}

/// A granted permission. Returned from [`PermissionBroker::request`]
/// when a request is approved (either via direct decision, scope-cache
/// hit, or partner bypass).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PermissionGrant {
    /// Unique grant id. For partner-bypass grants this is freshly
    /// minted and never appears in a `PermissionRequest`.
    pub id: String,
    /// The scope the grant covers.
    pub scope: PermissionScope,
    /// When the grant expires. `None` for `ApproveOnce`,
    /// `ApproveForScope`, and partner-bypass grants. `Some(_)` for
    /// `ApproveForDuration` grants — `now < expires_at` is required for
    /// the cached grant to short-circuit a future request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<jiff::Timestamp>,
    /// Audit metadata. Currently used for partner-bypass attribution
    /// (`{"source": "partner_bypass"}`); future fields can layer on
    /// additional context without touching the grant's required shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl PermissionGrant {
    /// Construct a synthesized grant for a Partner-driven turn that
    /// short-circuited the broker. Carries no expiry and a
    /// `{"source": "partner_bypass"}` metadata marker for audit logs.
    pub fn synthesized_partner(scope: PermissionScope) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            scope,
            expires_at: None,
            metadata: Some(serde_json::json!({"source": "partner_bypass"})),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PermissionRequest {
    pub id: String,
    pub agent_id: crate::AgentId,
    pub tool_name: String,
    pub scope: PermissionScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Possible decisions in response to a [`PermissionRequest`].
///
/// `ApproveForDuration` carries a [`jiff::Span`] so the wire format is
/// the same human-readable representation used elsewhere in Pattern;
/// the broker translates this into an absolute [`jiff::Timestamp`] on
/// the resulting grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PermissionDecisionKind {
    Deny,
    ApproveOnce,
    ApproveForDuration(jiff::Span),
    ApproveForScope,
}

/// Cache key for approve-for-scope and approve-for-duration grants.
type ScopeKey = (crate::AgentId, PermissionScope);

/// Source of "now" for the broker. Production uses
/// [`jiff::Timestamp::now`]; tests inject a deterministic clock so
/// duration-based caches can be exercised without sleeping.
type NowFn = Arc<dyn Fn() -> jiff::Timestamp + Send + Sync>;

#[derive(Clone)]
pub struct PermissionBroker {
    tx: broadcast::Sender<PermissionRequest>,
    pending: Arc<RwLock<HashMap<String, oneshot::Sender<PermissionDecisionKind>>>>,
    pending_info: Arc<RwLock<HashMap<String, PermissionRequest>>>,
    /// Cache of `(agent_id, scope)` → grant for `ApproveForScope` and
    /// `ApproveForDuration` decisions. Subsequent matching requests
    /// short-circuit on a cache hit (with expiry check for duration
    /// grants).
    scope_cache: Arc<RwLock<HashMap<ScopeKey, PermissionGrant>>>,
    /// Injected clock — production: `jiff::Timestamp::now`. Tests inject
    /// a deterministic clock for duration-cache assertions.
    now_fn: NowFn,
}

impl PermissionBroker {
    /// Construct a fresh per-runtime broker. Callers wire one
    /// `Arc<PermissionBroker>` per `TidepoolSession` into the session's
    /// `SessionContext` so each runtime has independent pending queues
    /// and approval caches.
    pub fn new() -> Self {
        Self::with_clock(Arc::new(jiff::Timestamp::now))
    }

    /// Construct a broker with an injected clock. Tests use this to
    /// drive duration-based cache expiry deterministically.
    pub fn with_clock(now_fn: NowFn) -> Self {
        let (tx, _rx) = broadcast::channel(64);
        Self {
            tx,
            pending: Arc::new(RwLock::new(HashMap::new())),
            pending_info: Arc::new(RwLock::new(HashMap::new())),
            scope_cache: Arc::new(RwLock::new(HashMap::new())),
            now_fn,
        }
    }

    /// Subscribe to broadcast `PermissionRequest`s. Each subscriber
    /// receives its own copy of every request that this broker
    /// publishes.
    pub fn subscribe(&self) -> broadcast::Receiver<PermissionRequest> {
        self.tx.subscribe()
    }

    /// Request a permission grant.
    ///
    /// Resolution order:
    /// 1. **Partner bypass**: if `origin.bypasses_permission_gate()` returns
    ///    true (i.e. the Partner is driving the turn), return a
    ///    [`PermissionGrant::synthesized_partner`] without broadcasting.
    /// 2. **Scope cache**: if a prior `ApproveForScope` or unexpired
    ///    `ApproveForDuration` grant matches `(agent_id, scope)`, return
    ///    a clone without broadcasting.
    /// 3. **Broadcast + await**: publish a [`PermissionRequest`] and
    ///    block on a oneshot decision until `timeout` elapses.
    /// 4. **Timeout**: clean up pending entries and return `None`
    ///    (denial). No leaks.
    #[allow(clippy::too_many_arguments)]
    pub async fn request(
        &self,
        agent_id: crate::AgentId,
        tool_name: String,
        scope: PermissionScope,
        origin: &MessageOrigin,
        reason: Option<String>,
        metadata: Option<serde_json::Value>,
        timeout: std::time::Duration,
    ) -> Option<PermissionGrant> {
        // (1) Partner bypass — short-circuit before broadcasting.
        if origin.bypasses_permission_gate() {
            tracing::debug!(
                "permission.request partner-bypass tool={} scope={:?}",
                tool_name,
                scope
            );
            return Some(PermissionGrant::synthesized_partner(scope));
        }

        // (2) Scope-cache lookup. Hit returns immediately; expired
        //     duration grants are pruned and fall through to broadcast.
        {
            let cache_key = (agent_id.clone(), scope.clone());
            let mut cache = self.scope_cache.write().await;
            if let Some(grant) = cache.get(&cache_key) {
                let still_valid = match grant.expires_at {
                    None => true,
                    Some(exp) => (self.now_fn)() < exp,
                };
                if still_valid {
                    tracing::debug!(
                        "permission.request scope-cache hit tool={} scope={:?}",
                        tool_name,
                        scope
                    );
                    return Some(grant.clone());
                }
                // Expired — drop it and fall through to broadcast.
                cache.remove(&cache_key);
            }
        }

        // (3) Broadcast + await.
        tracing::debug!("permission.request tool={} scope={:?}", tool_name, scope);
        let id = Uuid::new_v4().to_string();
        let (tx_decision, rx_decision) = oneshot::channel();
        {
            let mut p = self.pending.write().await;
            p.insert(id.clone(), tx_decision);
        }
        let req = PermissionRequest {
            id: id.clone(),
            agent_id: agent_id.clone(),
            tool_name: tool_name.clone(),
            scope: scope.clone(),
            reason,
            metadata,
        };
        {
            let mut pi = self.pending_info.write().await;
            pi.insert(id.clone(), req.clone());
        }
        let _ = self.tx.send(req);

        match tokio::time::timeout(timeout, rx_decision).await {
            Ok(Ok(decision)) => self.materialise_grant(id, agent_id, scope, decision).await,
            _ => {
                // (4) Timeout / channel closed — clean up pending state
                //     so the maps don't leak entries on every aborted
                //     request.
                tracing::warn!(
                    "permission.request timeout or channel closed: tool={} scope={:?}",
                    tool_name,
                    scope
                );
                self.pending.write().await.remove(&id);
                self.pending_info.write().await.remove(&id);
                None
            }
        }
    }

    /// Translate a decision into a `PermissionGrant`, populating the
    /// scope cache for `ApproveForScope` / `ApproveForDuration`.
    async fn materialise_grant(
        &self,
        id: String,
        agent_id: crate::AgentId,
        scope: PermissionScope,
        decision: PermissionDecisionKind,
    ) -> Option<PermissionGrant> {
        match decision {
            PermissionDecisionKind::Deny => None,
            PermissionDecisionKind::ApproveOnce => Some(PermissionGrant {
                id,
                scope,
                expires_at: None,
                metadata: None,
            }),
            PermissionDecisionKind::ApproveForScope => {
                let grant = PermissionGrant {
                    id,
                    scope: scope.clone(),
                    expires_at: None,
                    metadata: None,
                };
                self.scope_cache
                    .write()
                    .await
                    .insert((agent_id, scope), grant.clone());
                Some(grant)
            }
            PermissionDecisionKind::ApproveForDuration(span) => {
                let now = (self.now_fn)();
                let expires_at = now.checked_add(span).ok();
                let grant = PermissionGrant {
                    id,
                    scope: scope.clone(),
                    expires_at,
                    metadata: None,
                };
                if expires_at.is_some() {
                    self.scope_cache
                        .write()
                        .await
                        .insert((agent_id, scope), grant.clone());
                }
                Some(grant)
            }
        }
    }

    /// Resolve a pending request with a decision. Returns `true` if a
    /// pending entry was found and the decision was delivered.
    pub async fn resolve(&self, request_id: &str, decision: PermissionDecisionKind) -> bool {
        let tx_opt = { self.pending.write().await.remove(request_id) };
        {
            let mut pi = self.pending_info.write().await;
            pi.remove(request_id);
        }
        if let Some(tx) = tx_opt {
            tracing::debug!(
                "permission.resolve id={} decision={:?}",
                request_id,
                decision
            );
            let _ = tx.send(decision);
            true
        } else {
            false
        }
    }

    pub async fn list_pending(&self) -> Vec<PermissionRequest> {
        let pi = self.pending_info.read().await;
        pi.values().cloned().collect()
    }

    /// Number of pending requests awaiting a decision. Test-only.
    #[doc(hidden)]
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }
}

impl Default for PermissionBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PermissionBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionBroker")
            // Internal channels carry tokio types whose `Debug` impls
            // dump `<...>` placeholders; surface the public-facing
            // shape only.
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::new_id;
    use crate::types::origin::{AgentAuthor, Author, Human, Partner, Sphere, SystemReason};
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    fn agent() -> crate::AgentId {
        crate::AgentId::from("test-agent")
    }

    fn shell_scope() -> PermissionScope {
        PermissionScope::ToolExecution {
            tool: "shell".into(),
            args_digest: Some("digest-1".into()),
        }
    }

    fn human_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Human(Human {
                user_id: new_id(),
                display_name: None,
            }),
            Sphere::Private,
        )
    }

    fn partner_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Partner(Partner { user_id: new_id() }),
            Sphere::Private,
        )
    }

    fn agent_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::Agent(AgentAuthor {
                agent_id: crate::AgentId::from("sibling"),
            }),
            Sphere::Internal,
        )
    }

    fn system_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::System {
                reason: SystemReason::Wakeup,
            },
            Sphere::System,
        )
    }

    /// Drive the broker through one approve-once cycle by spawning a
    /// scripted responder that reads the broadcast and resolves the
    /// matching id.
    async fn drive_approve_once(broker: Arc<PermissionBroker>) -> tokio::task::JoinHandle<()> {
        let mut rx = broker.subscribe();
        tokio::spawn(async move {
            if let Ok(req) = rx.recv().await {
                broker
                    .resolve(&req.id, PermissionDecisionKind::ApproveOnce)
                    .await;
            }
        })
    }

    #[tokio::test]
    async fn partner_origin_short_circuits_without_broadcast() {
        let broker = Arc::new(PermissionBroker::new());
        let mut rx = broker.subscribe();
        let origin = partner_origin();

        let grant = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(50),
            )
            .await
            .expect("partner bypass returns Some");
        let metadata = grant
            .metadata
            .expect("partner-bypass grants carry metadata");
        assert_eq!(metadata["source"], "partner_bypass");
        // No subscriber should have observed a broadcast.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), rx.recv())
                .await
                .is_err(),
            "partner bypass must not broadcast a request"
        );
    }

    #[tokio::test]
    async fn non_partner_origins_broadcast_normally() {
        for origin in [human_origin(), agent_origin(), system_origin()] {
            let broker = Arc::new(PermissionBroker::new());
            let _responder = drive_approve_once(broker.clone()).await;
            let grant = broker
                .request(
                    agent(),
                    "shell".into(),
                    shell_scope(),
                    &origin,
                    None,
                    None,
                    Duration::from_millis(200),
                )
                .await
                .expect("approval should arrive");
            assert!(
                grant.metadata.is_none(),
                "non-partner grants must not carry partner-bypass metadata, got {:?}",
                grant.metadata
            );
        }
    }

    #[tokio::test]
    async fn approve_for_scope_caches_subsequent_requests() {
        let broker = Arc::new(PermissionBroker::new());
        let origin = human_origin();

        // First request: scripted responder approves for scope. Subscribe
        // before spawning so the responder cannot miss the broadcast.
        let mut responder_rx = broker.subscribe();
        let broker_clone = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = responder_rx.recv().await {
                broker_clone
                    .resolve(&req.id, PermissionDecisionKind::ApproveForScope)
                    .await;
            }
        });

        let first = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(200),
            )
            .await
            .expect("approve-for-scope returns Some");
        assert!(first.expires_at.is_none());
        responder.await.unwrap();

        // Second request with the same scope: no broadcast — must be
        // satisfied from the cache. We assert no request lands in the
        // pending queue during a short window.
        let mut rx = broker.subscribe();
        let second = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(50),
            )
            .await
            .expect("scope cache hit returns Some");
        assert_eq!(second.scope, first.scope);
        // No broadcast on the second request.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), rx.recv())
                .await
                .is_err(),
            "scope-cache hit must not re-broadcast"
        );
    }

    #[tokio::test]
    async fn approve_for_duration_expires_via_injected_clock() {
        // Inject a clock backed by an atomic; advance it between calls.
        let now_micros = Arc::new(AtomicI64::new(1_700_000_000_000_000));
        let clock = {
            let now_micros = now_micros.clone();
            Arc::new(move || {
                let micros = now_micros.load(Ordering::SeqCst);
                jiff::Timestamp::from_microsecond(micros).expect("valid timestamp")
            }) as NowFn
        };
        let broker = Arc::new(PermissionBroker::with_clock(clock));
        let origin = human_origin();

        // Subscribe synchronously so the responder cannot miss the broadcast.
        let mut responder_rx = broker.subscribe();
        let broker_clone = broker.clone();
        let responder = tokio::spawn(async move {
            if let Ok(req) = responder_rx.recv().await {
                broker_clone
                    .resolve(
                        &req.id,
                        PermissionDecisionKind::ApproveForDuration(jiff::Span::new().seconds(60)),
                    )
                    .await;
            }
        });

        let first = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(200),
            )
            .await
            .expect("first approval");
        assert!(
            first.expires_at.is_some(),
            "duration grant carries expires_at"
        );
        responder.await.unwrap();

        // Advance clock by 30s — still within the 60s window. Cache hit.
        now_micros.fetch_add(30 * 1_000_000, Ordering::SeqCst);
        let mut rx = broker.subscribe();
        let cached = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(50),
            )
            .await
            .expect("within-window cache hit");
        assert_eq!(cached.scope, first.scope);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), rx.recv())
                .await
                .is_err(),
            "within-window must not broadcast"
        );

        // Advance clock past the 60s window. Cache should be considered
        // expired and a new broadcast should fire.
        now_micros.fetch_add(45 * 1_000_000, Ordering::SeqCst);
        // Subscribe synchronously to avoid the spawn-vs-broadcast race.
        let mut post_rx = broker.subscribe();
        let broker_clone = broker.clone();
        let post_responder = tokio::spawn(async move {
            if let Ok(req) = post_rx.recv().await {
                broker_clone
                    .resolve(&req.id, PermissionDecisionKind::Deny)
                    .await;
            }
        });
        let denied = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(200),
            )
            .await;
        assert!(denied.is_none(), "post-expiry must re-gate, got {denied:?}");
        post_responder.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_path_cleans_up_pending_state() {
        let broker = Arc::new(PermissionBroker::new());
        let origin = human_origin();

        // No subscriber drains broadcasts and no resolver fires — request
        // should time out.
        let result = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(20),
            )
            .await;
        assert!(result.is_none(), "timeout returns None, got {result:?}");
        assert_eq!(
            broker.pending_count().await,
            0,
            "pending map must be empty after timeout"
        );
        assert_eq!(
            broker.list_pending().await.len(),
            0,
            "pending_info map must be empty after timeout"
        );
    }

    #[tokio::test]
    async fn two_brokers_have_independent_state() {
        // AC2.9: per-runtime brokers do not share pending queues or
        // scope caches.
        let a = Arc::new(PermissionBroker::new());
        let b = Arc::new(PermissionBroker::new());
        let origin = human_origin();

        // Approve for scope on broker A. Subscribe synchronously to avoid
        // the spawn-vs-broadcast race.
        let mut a_rx = a.subscribe();
        let a_clone = a.clone();
        let _responder = tokio::spawn(async move {
            if let Ok(req) = a_rx.recv().await {
                a_clone
                    .resolve(&req.id, PermissionDecisionKind::ApproveForScope)
                    .await;
            }
        });
        a.request(
            agent(),
            "shell".into(),
            shell_scope(),
            &origin,
            None,
            None,
            Duration::from_millis(200),
        )
        .await
        .expect("A approves");

        // Broker B must NOT see the grant — no responder on B, request
        // should time out (denial).
        let denied = b
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(30),
            )
            .await;
        assert!(
            denied.is_none(),
            "broker B must not inherit broker A's scope cache"
        );
    }

    #[tokio::test]
    async fn approve_once_does_not_populate_cache() {
        // ApproveOnce explicitly does not generalise — a second matching
        // request must re-gate.
        let broker = Arc::new(PermissionBroker::new());
        let origin = human_origin();

        let _responder = drive_approve_once(broker.clone()).await;
        let first = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(200),
            )
            .await
            .expect("first approval");
        assert_eq!(first.scope, shell_scope());

        // Second request with no responder — must time out, not hit the cache.
        let second = broker
            .request(
                agent(),
                "shell".into(),
                shell_scope(),
                &origin,
                None,
                None,
                Duration::from_millis(30),
            )
            .await;
        assert!(
            second.is_none(),
            "approve-once must not populate scope cache"
        );
    }
}
