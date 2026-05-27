// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Scheme-dispatched message router registry.
//!
//! The [`RouterRegistry`] holds `Arc<dyn Router>` keyed by scheme
//! prefix. Message-producing effects (Send / Reply / Notify) parse the
//! recipient string into `scheme:target`, look up the matching router,
//! and dispatch with the resolved target (scheme stripped).
//!
//! # Fallback routing
//!
//! If the recipient is malformed (no `:` separator) or the scheme has
//! no registered router, the registry dispatches to the **default
//! scheme** — typically `"cli"` — with the full original recipient as
//! the target. This lets agents produce output without a structured
//! address and still have it land somewhere visible to a human.
//! Registries configured without a default scheme return an error in
//! these cases.
//!
//! Phase 5 ships with a single scheme handler: [`cli::CliRouter`].
//! Inter-agent (`agent:`), Discord (`discord:`), Bluesky (`bluesky:`)
//! routers are future scope.

pub mod agent;
pub mod cli;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;

// ---------------------------------------------------------------------------
// RouterBridge — sync ↔ async bridge for eval worker → router dispatch
// ---------------------------------------------------------------------------

/// Request sent from the eval worker thread to the async router task.
struct RouterRequest {
    sender: MessageOrigin,
    recipient: String,
    message: Message,
    reply: std::sync::mpsc::SyncSender<Result<(), RouterError>>,
}

/// Bridge between sync eval worker and async router dispatch.
///
/// Holds the send half of a `tokio::sync::mpsc` channel. The eval
/// worker thread sends requests through it; a tokio task reads them
/// and dispatches via the [`RouterRegistry`].
///
/// The `tokio::sync::mpsc::UnboundedSender::send` method is safe to
/// call from non-tokio threads (it does not require a runtime
/// context). The reply path uses `std::sync::mpsc::sync_channel` so
/// the eval worker can block on the result without needing tokio.
#[derive(Clone, Debug)]
pub struct RouterBridge {
    tx: tokio::sync::mpsc::UnboundedSender<RouterRequest>,
}

impl RouterBridge {
    /// Spawn the async router task and return the bridge.
    ///
    /// The task runs on the tokio runtime and processes router requests
    /// until the bridge (and all clones) are dropped, which closes the
    /// channel and terminates the task.
    pub fn spawn(registry: Arc<RouterRegistry>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RouterRequest>();

        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let result = registry
                    .route(&req.sender, &req.recipient, &req.message)
                    .await;
                // Reply channel may be closed if the eval worker timed
                // out or was cancelled — that is not an error.
                let _ = req.reply.send(result);
            }
        });

        Self { tx }
    }

    /// Route a message synchronously. Blocks the calling thread until
    /// the async router task processes the request and sends back the
    /// result.
    ///
    /// Safe to call from a plain OS thread (no tokio runtime context
    /// required).
    ///
    /// `sender` is the [`MessageOrigin`] of the dispatcher — typically
    /// `Author::Agent` for autonomous turns, or `Author::Partner` for
    /// direct partner-driven dispatch. Routers carrying this through to
    /// receivers (TUI, mailbox, transport endpoints) lets downstream
    /// consumers attribute the message correctly.
    pub fn route_sync(
        &self,
        sender: &MessageOrigin,
        recipient: &str,
        message: &Message,
    ) -> Result<(), RouterError> {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        let request = RouterRequest {
            sender: sender.clone(),
            recipient: recipient.to_string(),
            message: message.clone(),
            reply: reply_tx,
        };
        self.tx
            .send(request)
            .map_err(|_| RouterError::RouteFailed("router bridge channel closed".into()))?;
        reply_rx
            .recv()
            .map_err(|_| RouterError::RouteFailed("router bridge reply channel closed".into()))?
    }
}

/// Errors produced by the routing layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RouterError {
    /// Recipient string is malformed and no default scheme is
    /// configured to absorb the fallback.
    #[error(
        "malformed recipient: expected 'scheme:target', got {0:?} \
         (no default scheme configured)"
    )]
    MalformedRecipient(String),

    /// No router registered for the given scheme and no default
    /// scheme is configured to absorb the fallback.
    #[error(
        "no router registered for scheme '{scheme}' (recipient {recipient:?}; \
         no default scheme configured)"
    )]
    NoRouterForScheme {
        /// The scheme that was requested.
        scheme: String,
        /// The full original recipient string.
        recipient: String,
    },

    /// The router attempted delivery but failed.
    #[error("route failed: {0}")]
    RouteFailed(String),

    /// No registered persona with the given id. Returned by the
    /// `agent:` scheme router when the target is unknown or `Inactive`.
    ///
    /// Well-known prefix used by the message handler to convert this to
    /// `EffectError::Handler` with a parseable prefix; see
    /// [`ROUTER_ERROR_PREFIX`].
    #[error("persona not found: {0}")]
    PersonaNotFound(pattern_core::types::ids::PersonaId),

    /// The persona's mailbox channel is closed (the session has ended).
    /// The sending end was obtained from the registry but the receiving
    /// end has since been dropped.
    #[error("persona mailbox is closed")]
    MailboxClosed,

    /// Attempt to register an alias that would shadow a different
    /// canonical persona, or that already resolves to a different
    /// canonical id.
    #[error("alias collision: alias {alias:?} cannot resolve to {canonical:?}")]
    AliasCollision {
        /// The alias being registered.
        alias: pattern_core::types::ids::PersonaId,
        /// The canonical id the alias was supposed to resolve to.
        canonical: pattern_core::types::ids::PersonaId,
    },
}

/// Well-known prefix attached to `EffectError::Handler` messages produced
/// when an `AgentRouter` call fails. Consumers (tests, TUI, CLI) match on
/// this prefix to distinguish routing failures from other handler errors
/// without parsing free-form prose.
///
/// Pattern:
/// ```text
/// RouterError: PersonaNotFound: <persona-id>
/// RouterError: MailboxClosed
/// ```
pub const ROUTER_ERROR_PREFIX: &str = "RouterError: ";

/// A scheme-specific message router.
///
/// Implementations handle one URI scheme (e.g. `cli`, `agent`,
/// `discord`). The router receives just the **target** portion of the
/// recipient — the scheme has been resolved and stripped by the
/// [`RouterRegistry`] before dispatch.
///
/// For example, a recipient `"agent:pattern-entropy"` dispatched to
/// the agent router arrives as `target == "pattern-entropy"`. For
/// fallback routing (malformed recipient, unknown scheme), the default
/// router receives the full original recipient as `target`.
#[async_trait]
pub trait Router: Send + Sync {
    /// The URI scheme this router handles (e.g. `"cli"`).
    fn scheme(&self) -> &str;

    /// Route a message to the given target.
    ///
    /// `sender` is the [`MessageOrigin`] of the dispatcher — used for
    /// attribution at the receiver (TUI rendering, mailbox tagging,
    /// transport-side identity). Implementations should NOT use
    /// `sender` for permission gating; that happens upstream in the
    /// handler before `route` is called.
    ///
    /// `target` is the portion of the recipient AFTER the scheme
    /// prefix was stripped (or the full original recipient when this
    /// is the default-scheme fallback).
    async fn route(
        &self,
        sender: &MessageOrigin,
        target: &str,
        body: &Message,
    ) -> Result<(), RouterError>;
}

/// Registry of scheme-dispatched routers.
///
/// Thread-safe for read access (routers are registered at session open
/// and read during handler dispatch). Uses a plain `HashMap` behind a
/// shared reference — registration happens once at setup, not at
/// runtime.
///
/// # Default scheme
///
/// A registry may carry an optional default scheme (set via
/// [`RouterRegistry::with_default_scheme`]). If set, malformed
/// recipients and unregistered schemes fall back to this router with
/// the full original recipient as target. If not set, those cases
/// produce [`RouterError::MalformedRecipient`] or
/// [`RouterError::NoRouterForScheme`] respectively.
#[derive(Default)]
pub struct RouterRegistry {
    routers: HashMap<String, Arc<dyn Router>>,
    /// Scheme name of the default router (if any). Set via
    /// [`Self::with_default_scheme`]; the router must also be
    /// registered separately via [`Self::register`].
    default_scheme: Option<String>,
}

impl RouterRegistry {
    /// Create an empty registry with no default scheme.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a router for its declared scheme. Overwrites any
    /// previously registered router for the same scheme.
    pub fn register(&mut self, router: Arc<dyn Router>) {
        let scheme = router.scheme().to_string();
        self.routers.insert(scheme, router);
    }

    /// Configure the default scheme used for fallback routing.
    ///
    /// A router for this scheme must also be registered via
    /// [`Self::register`]; the default-scheme setting is just a name
    /// that points at one of the registered entries.
    ///
    /// Typical use: `registry.with_default_scheme("cli")`. Builder
    /// style; returns `self` for chaining.
    #[must_use]
    pub fn with_default_scheme(mut self, scheme: impl Into<String>) -> Self {
        self.default_scheme = Some(scheme.into());
        self
    }

    /// Scheme name of the currently-configured default router, if any.
    pub fn default_scheme(&self) -> Option<&str> {
        self.default_scheme.as_deref()
    }

    /// Route a message to the given recipient.
    ///
    /// Resolution order:
    /// 1. Split `recipient` at the first `:` into `(scheme, target)`.
    /// 2. If a router is registered for `scheme`, dispatch with
    ///    `target` (scheme stripped).
    /// 3. Otherwise, if a default scheme is configured and registered,
    ///    dispatch to it with the full original recipient as `target`.
    /// 4. Otherwise return [`RouterError::NoRouterForScheme`] (or
    ///    [`RouterError::MalformedRecipient`] if step 1 failed and
    ///    there's no default).
    pub async fn route(
        &self,
        sender: &MessageOrigin,
        recipient: &str,
        body: &Message,
    ) -> Result<(), RouterError> {
        if let Some((scheme, target)) = recipient.split_once(':') {
            if let Some(router) = self.routers.get(scheme) {
                return router.route(sender, target, body).await;
            }
            // Scheme not registered — try default.
            if let Some(router) = self.default_router() {
                return router.route(sender, recipient, body).await;
            }
            Err(RouterError::NoRouterForScheme {
                scheme: scheme.into(),
                recipient: recipient.into(),
            })
        } else {
            // Malformed — no scheme separator. Try default.
            if let Some(router) = self.default_router() {
                return router.route(sender, recipient, body).await;
            }
            Err(RouterError::MalformedRecipient(recipient.into()))
        }
    }

    fn default_router(&self) -> Option<&Arc<dyn Router>> {
        self.default_scheme
            .as_deref()
            .and_then(|s| self.routers.get(s))
    }
}

impl std::fmt::Debug for RouterRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterRegistry")
            .field("schemes", &self.routers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::message::Message;
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};

    fn test_sender() -> MessageOrigin {
        MessageOrigin::new(
            Author::System {
                reason: SystemReason::Timer,
            },
            Sphere::System,
        )
    }

    /// Create a minimal test message.
    fn test_message() -> Message {
        Message {
            chat_message: genai::chat::ChatMessage::new(genai::chat::ChatRole::User, "hello"),
            id: MessageId::from(new_id().to_string()),
            position: new_snowflake_id(),
            owner_id: AgentId::from("test-agent"),
            created_at: Timestamp::now(),
            batch: BatchId::from(new_snowflake_id()),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        }
    }

    /// Mock router that records whether it was called.
    struct MockRouter {
        scheme_name: &'static str,
        called: std::sync::Mutex<Vec<String>>,
    }

    impl MockRouter {
        fn new(scheme: &'static str) -> Arc<Self> {
            Arc::new(Self {
                scheme_name: scheme,
                called: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<String> {
            self.called.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Router for MockRouter {
        fn scheme(&self) -> &str {
            self.scheme_name
        }

        async fn route(
            &self,
            _sender: &MessageOrigin,
            target: &str,
            _body: &Message,
        ) -> Result<(), RouterError> {
            self.called.lock().unwrap().push(target.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn route_dispatches_to_correct_scheme_with_target_only() {
        let mock = MockRouter::new("test");
        let mut registry = RouterRegistry::new();
        registry.register(mock.clone());

        let msg = test_message();
        registry
            .route(&test_sender(), "test:target", &msg)
            .await
            .unwrap();

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        // Router receives TARGET only, scheme stripped.
        assert_eq!(calls[0], "target");
    }

    #[tokio::test]
    async fn route_dispatches_multi_segment_target_untouched() {
        // Targets can contain further ':' — we only split on the FIRST
        // one, so "discord:#general:thread-42" → target "#general:thread-42".
        let mock = MockRouter::new("discord");
        let mut registry = RouterRegistry::new();
        registry.register(mock.clone());

        let msg = test_message();
        registry
            .route(&test_sender(), "discord:#general:thread-42", &msg)
            .await
            .unwrap();

        assert_eq!(mock.calls()[0], "#general:thread-42");
    }

    #[tokio::test]
    async fn route_unknown_scheme_without_default_returns_error() {
        let registry = RouterRegistry::new();
        let msg = test_message();
        let err = registry
            .route(&test_sender(), "unknown:target", &msg)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                RouterError::NoRouterForScheme { ref scheme, ref recipient }
                    if scheme == "unknown" && recipient == "unknown:target"
            ),
            "expected NoRouterForScheme, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn route_malformed_recipient_without_default_returns_error() {
        let registry = RouterRegistry::new();
        let msg = test_message();
        let err = registry
            .route(&test_sender(), "no-colon-here", &msg)
            .await
            .unwrap_err();
        assert!(
            matches!(err, RouterError::MalformedRecipient(_)),
            "expected MalformedRecipient, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn route_malformed_with_default_falls_back_to_default_router() {
        let cli = MockRouter::new("cli");
        let mut registry = RouterRegistry::new().with_default_scheme("cli");
        registry.register(cli.clone());

        let msg = test_message();
        registry
            .route(&test_sender(), "just-a-bare-string", &msg)
            .await
            .unwrap();

        // Default router receives the FULL original recipient as target.
        let calls = cli.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "just-a-bare-string");
    }

    #[tokio::test]
    async fn route_unknown_scheme_with_default_falls_back_to_default_router() {
        let cli = MockRouter::new("cli");
        let mut registry = RouterRegistry::new().with_default_scheme("cli");
        registry.register(cli.clone());

        let msg = test_message();
        registry
            .route(&test_sender(), "agent:pattern-entropy", &msg)
            .await
            .unwrap();

        // Default router receives the full "agent:pattern-entropy"
        // string so it can surface what was attempted.
        let calls = cli.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "agent:pattern-entropy");
    }

    #[tokio::test]
    async fn route_known_scheme_does_not_fall_back_even_with_default_configured() {
        let cli = MockRouter::new("cli");
        let agent = MockRouter::new("agent");
        let mut registry = RouterRegistry::new().with_default_scheme("cli");
        registry.register(cli.clone());
        registry.register(agent.clone());

        let msg = test_message();
        registry
            .route(&test_sender(), "agent:pattern-entropy", &msg)
            .await
            .unwrap();

        assert!(
            cli.calls().is_empty(),
            "default must NOT be used when scheme matches"
        );
        assert_eq!(agent.calls().len(), 1);
        assert_eq!(agent.calls()[0], "pattern-entropy");
    }

    #[tokio::test]
    async fn default_scheme_without_registered_router_still_errors() {
        // Configuring a default for a scheme that has no registered
        // router doesn't magic one into existence.
        let registry = RouterRegistry::new().with_default_scheme("cli");
        let msg = test_message();
        let err = registry
            .route(&test_sender(), "bare-string", &msg)
            .await
            .unwrap_err();
        assert!(
            matches!(err, RouterError::MalformedRecipient(_)),
            "expected MalformedRecipient (no fallback router registered), got: {err:?}"
        );
    }

    #[tokio::test]
    async fn register_overwrites_previous() {
        let first = MockRouter::new("test");
        let second = MockRouter::new("test");
        let mut registry = RouterRegistry::new();
        registry.register(first.clone());
        registry.register(second.clone());

        let msg = test_message();
        registry
            .route(&test_sender(), "test:x", &msg)
            .await
            .unwrap();

        assert!(
            first.calls().is_empty(),
            "first router should not be called"
        );
        assert_eq!(second.calls().len(), 1, "second router should be called");
    }
}
