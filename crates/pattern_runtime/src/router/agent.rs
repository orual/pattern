//! Agent-scheme router: resolves `agent:<persona-id>` recipients via the
//! [`AgentRegistry`](crate::agent_registry::AgentRegistry).
//!
//! Routing table:
//!
//! | Registry status | Outcome |
//! |---|---|
//! | `Active` | Deliver to the live mailbox sender. |
//! | `Draft` | Queue the message for future `PromoteDraft` (Phase 6); return `Ok(())`. |
//! | Not registered | Return [`RouterError::PersonaNotFound`]. |
//!
//! The scheme string is `"agent"`, so the [`RouterRegistry`] routes
//! `"agent:pattern-entropy"` here with `target == "pattern-entropy"` (the
//! registry strips the scheme prefix before calling `Router::route`).

use std::sync::Arc;

use async_trait::async_trait;
use pattern_core::fronting::{parse_direct_address, strip_direct_address};
use pattern_core::types::ids::PersonaId;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;

use crate::agent_registry::AgentRegistry;
#[cfg(test)]
use crate::agent_registry::SessionStatus;
use crate::fronting_dispatch::{FrontingState, dispatch_to_mailboxes};
use crate::mailbox::MailboxInput;
use crate::router::{Router, RouterError};

/// Routes `agent:<persona-id>` messages to the correct live mailbox.
///
/// Backed by an [`Arc<AgentRegistry>`] shared across all sessions in the
/// same runtime. The router is stateless after construction — all
/// mutable state lives in the registry.
///
/// # Routing behaviour
///
/// `AgentRouter` is the single entry point for `agent:` scheme deliveries
/// per the v3-multi-agent Phase 5 design. Behaviour by `target`:
///
/// - `target == "<persona-id>"` (non-empty, not `"auto"`): direct delivery
///   to the named persona via [`AgentRegistry::route_or_queue`]. Honours the
///   Phase 4 cycle-3 atomicity guarantees (no silent message loss across
///   concurrent Draft→Active promotions).
/// - `target` starts with `@` (e.g. `"@alice"` or `"@alice please…"`):
///   parse the leading direct-address marker and route direct to that
///   persona. The `@…` prefix is stripped from the body before delivery.
/// - `target == ""` or `target == "auto"`: dispatch through the
///   [`FrontingState`] resolver (Phase 5 T4) — fronting rules, fallback,
///   fan-out, default-persona. Requires [`Self::with_fronting`]; otherwise
///   returns [`RouterError::PersonaNotFound`] with id `"<unspecified>"`.
pub struct AgentRouter {
    registry: Arc<AgentRegistry>,
    /// Optional fronting-aware dispatch state. When set, `route()` with
    /// an empty or `"auto"` target consults the resolver. When None,
    /// such targets fail with `PersonaNotFound("<unspecified>")` so
    /// callers see a clear "fronting not wired" signal rather than
    /// silently dropping the message.
    fronting: Option<FrontingState>,
}

impl AgentRouter {
    /// Create a new agent router backed by `registry`. Fronting-aware
    /// dispatch is disabled by default; wire it via
    /// [`Self::with_fronting`].
    pub fn new(registry: Arc<AgentRegistry>) -> Self {
        Self {
            registry,
            fronting: None,
        }
    }

    /// Builder-style: enable fronting-aware dispatch. Production
    /// callers (the daemon's `get_or_open_session`) wire this so
    /// unqualified `agent:` / `agent:auto` recipients route through
    /// the resolver.
    #[must_use]
    pub fn with_fronting(mut self, fronting: FrontingState) -> Self {
        self.fronting = Some(fronting);
        self
    }

    /// Expose the underlying registry (e.g. for session registration).
    pub fn registry(&self) -> &Arc<AgentRegistry> {
        &self.registry
    }
}

#[async_trait]
impl Router for AgentRouter {
    fn scheme(&self) -> &str {
        "agent"
    }

    /// Route `body` to the persona identified by `target`.
    ///
    /// `target` is the part of the recipient *after* the `"agent:"` prefix
    /// (the registry strips it). The full recipient `"agent:pattern-entropy"`
    /// arrives here as `target == "pattern-entropy"`.
    ///
    /// Routing is atomic: the status check and the send/queue are performed
    /// under the same DashMap shard lock via
    /// [`AgentRegistry::route_or_queue`], preventing the TOCTOU race where
    /// a concurrent Draft→Active promotion could cause a message to be lost
    /// (status observed as Draft, promotion completes, then queue_for_draft
    /// sees Active and returns PersonaNotFound).
    async fn route(
        &self,
        sender: &MessageOrigin,
        target: &str,
        body: &Message,
    ) -> Result<(), RouterError> {
        // (1) Empty or "auto" target → fronting-aware dispatch.
        if target.is_empty() || target == "auto" {
            return match &self.fronting {
                Some(state) => {
                    // dispatch_to_mailboxes returns DispatchOutcome on success.
                    // Router::route must return Result<(), RouterError>; we
                    // surface SystemDefault at trace level here so SDK callers
                    // (whose route() return value is `()`) leave a breadcrumb
                    // when their message hit the no-fronting-configured path
                    // — the daemon's SendMessage handler emits a Display::Note
                    // for human-visible signal, but SDK callers don't have
                    // that surface available.
                    let outcome = dispatch_to_mailboxes(&self.registry, state, sender, body).await?;
                    if matches!(
                        outcome,
                        crate::fronting_dispatch::DispatchOutcome::SystemDefault
                    ) {
                        tracing::warn!(
                            target = "pattern_runtime::router::agent",
                            from = ?sender.author,
                            "agent router: fronting resolved to SystemDefault \
                             (no fronting configured + no Active personas); \
                             message acked but not delivered"
                        );
                    }
                    Ok(())
                }
                None => {
                    tracing::debug!("agent router: empty/auto target with no fronting state wired");
                    Err(RouterError::PersonaNotFound(PersonaId::from(
                        "<unspecified>",
                    )))
                }
            };
        }

        // (2) `@persona-name` direct addressing in the BODY (recipient
        // string is just `agent:`, body says `@alice please…`). We've
        // already handled the empty-target case above, so this branch
        // fires when callers used `agent:@alice` (legacy / convenience
        // form) — strip the `@` from the target and route direct.
        //
        // When the @-prefix is parsed FROM the body (target was a plain
        // persona id but body opens with `@persona-id`), we also strip
        // the prefix from the body before delivery so the recipient
        // doesn't see the routing marker. This matches the plan's
        // "@persona-name parsing" section: "Snip the prefix off the
        // message body before delivery."
        let mut delivery_body = body.clone();
        let direct_id: PersonaId = if let Some(stripped) = target.strip_prefix('@') {
            PersonaId::from(stripped)
        } else if let Some(parsed) =
            parse_direct_address(body.chat_message.content.first_text().unwrap_or(""))
        {
            // The recipient string is a literal persona id but the
            // body opens with `@persona-id`. Honour the body's
            // directive and override target.
            let resolved = if parsed.as_str() == target {
                PersonaId::from(target)
            } else {
                parsed
            };
            // Strip the leading `@<persona-id>[:[ ]]` token from the
            // body's first text part so the recipient sees a clean
            // message. We rebuild the ChatMessage with the cleaned
            // text and copy the rest of the Message verbatim.
            if let Some(text) = body.chat_message.content.first_text() {
                let cleaned = strip_direct_address(text);
                delivery_body.chat_message = genai::chat::ChatMessage::new(
                    body.chat_message.role.clone(),
                    cleaned,
                );
            }
            resolved
        } else {
            PersonaId::from(target)
        };

        let input = MailboxInput {
            from: sender.clone(),
            msg: delivery_body,
        };

        // route_or_queue atomically checks status and delivers/queues.
        // Returns PersonaNotFound if the persona is not registered.
        match self.registry.route_or_queue(&direct_id, input) {
            Ok(()) => {
                tracing::trace!(
                    persona_id = %direct_id,
                    "agent router: message dispatched (active deliver or draft queue)"
                );
                Ok(())
            }
            Err(RouterError::PersonaNotFound(_)) => {
                tracing::debug!(
                    persona_id = %direct_id,
                    "agent router: persona not found"
                );
                Err(RouterError::PersonaNotFound(direct_id))
            }
            Err(e) => Err(e),
        }
    }
}

impl std::fmt::Debug for AgentRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRouter").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::InMemoryConstellationRegistry;
    use jiff::Timestamp;
    use pattern_core::constellation::ConstellationRegistry;
    use pattern_core::fronting::{FrontingSet, MessagePattern, RoutingRule, RoutingTable};
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
    use smol_str::SmolStr;
    use std::sync::RwLock;
    use tokio::sync::mpsc;

    fn test_message(body: &str) -> Message {
        Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::User,
                body.to_string(),
            ),
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

    fn test_sender() -> MessageOrigin {
        MessageOrigin::new(
            Author::System {
                reason: SystemReason::Timer,
            },
            Sphere::System,
        )
    }

    /// AC6.1: active persona receives the message on its mailbox.
    #[tokio::test]
    async fn active_persona_delivers_message() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        reg.register("active-persona".into(), tx, SessionStatus::Active);

        let router = AgentRouter::new(reg);
        let msg = test_message("hello");
        router
            .route(&test_sender(), "active-persona", &msg)
            .await
            .unwrap();

        let received = rx.recv().await.expect("should receive message");
        let text = received.msg.chat_message.content.first_text().unwrap();
        assert_eq!(text, "hello");
    }

    /// AC6.4: nonexistent persona returns PersonaNotFound.
    #[tokio::test]
    async fn nonexistent_persona_returns_not_found() {
        let reg = Arc::new(AgentRegistry::new());
        let router = AgentRouter::new(reg);
        let err = router
            .route(&test_sender(), "ghost-persona", &test_message("oops"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, RouterError::PersonaNotFound(ref id) if id.as_str() == "ghost-persona"),
            "expected PersonaNotFound, got: {err:?}"
        );
    }

    /// AC6.5: draft persona queues message but returns Ok (no error).
    #[tokio::test]
    async fn draft_persona_queues_message_ok() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, _rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("draft-persona".into(), tx, SessionStatus::Draft);

        let router = AgentRouter::new(Arc::clone(&reg));
        let msg = test_message("queued");
        router
            .route(&test_sender(), "draft-persona", &msg)
            .await
            .unwrap();

        // Message is in the draft queue, not delivered to any session.
        let queued = reg.drain_draft_queue(&"draft-persona".into());
        assert_eq!(queued.len(), 1);
        let text = queued[0].0.chat_message.content.first_text().unwrap();
        assert_eq!(text, "queued");
    }

    /// AC6.5: draft persona queue returns Ok without triggering drive_step
    /// (no live session → mailbox channel is unused).
    #[tokio::test]
    async fn draft_persona_mailbox_is_not_triggered() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, mut rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("draft-b".into(), tx, SessionStatus::Draft);

        let router = AgentRouter::new(Arc::clone(&reg));
        router
            .route(&test_sender(), "draft-b", &test_message("x"))
            .await
            .unwrap();

        // Channel must be empty — nothing was sent through it.
        assert!(
            rx.try_recv().is_err(),
            "draft mailbox must not receive sends"
        );
    }

    /// MailboxClosed when the receiver has been dropped.
    #[tokio::test]
    async fn closed_mailbox_returns_mailbox_closed() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("closing-c".into(), tx, SessionStatus::Active);
        drop(rx); // close the receiver end.

        let router = AgentRouter::new(reg);
        let err = router
            .route(&test_sender(), "closing-c", &test_message("drop"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, RouterError::MailboxClosed),
            "expected MailboxClosed, got: {err:?}"
        );
    }

    /// AC6.6: 10 concurrent sends to an active persona; all arrive in order
    /// (FIFO per single-producer; interleaving not guaranteed across producers
    /// but no message loss).
    #[tokio::test]
    async fn concurrent_sends_all_delivered_no_loss() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, mut rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("burst-d".into(), tx, SessionStatus::Active);

        let router = Arc::new(AgentRouter::new(Arc::clone(&reg)));

        // 3 concurrent senders, 10 messages total.
        let handles: Vec<_> = (0..10u32)
            .map(|i| {
                let r = Arc::clone(&router);
                let sender = test_sender();
                let msg = test_message(&format!("msg-{i}"));
                tokio::spawn(async move {
                    r.route(&sender, "burst-d", &msg).await.unwrap();
                })
            })
            .collect();

        for h in handles {
            h.await.unwrap();
        }

        // Drain and count — no message loss.
        let mut count = 0;
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 10, "all 10 messages must be delivered");
    }

    // ── Fronting-aware dispatch tests ─────────────────────────────────────────

    /// Empty target with fronting configured routes through the resolver
    /// (fallback path) and delivers to the fallback persona.
    #[tokio::test]
    async fn empty_target_with_fronting_routes_via_resolver() {
        let reg = Arc::new(AgentRegistry::new());
        let (alice_tx, mut alice_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("alice".into(), alice_tx, SessionStatus::Active);

        let fronting_set = FrontingSet::from_parts(
            Vec::new(),
            Some(SmolStr::from("alice")),
            RoutingTable::default(),
        );
        let constellation: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), constellation);

        let router = AgentRouter::new(Arc::clone(&reg)).with_fronting(state);
        // Empty target → fronting resolver → fallback = alice.
        router
            .route(&test_sender(), "", &test_message("hi"))
            .await
            .unwrap();

        let received = alice_rx.recv().await.expect("alice must receive message");
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "hi"
        );
    }

    /// `"auto"` target with fronting configured routes through the resolver
    /// (same as empty target — both are the fronting dispatch trigger).
    #[tokio::test]
    async fn auto_target_with_fronting_routes_via_resolver() {
        let reg = Arc::new(AgentRegistry::new());
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("bob".into(), bob_tx, SessionStatus::Active);

        let fronting_set = FrontingSet::from_parts(
            Vec::new(),
            Some(SmolStr::from("bob")),
            RoutingTable::default(),
        );
        let constellation: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), constellation);

        let router = AgentRouter::new(Arc::clone(&reg)).with_fronting(state);
        // "auto" target → fronting resolver → fallback = bob.
        router
            .route(&test_sender(), "auto", &test_message("ping"))
            .await
            .unwrap();

        let received = bob_rx.recv().await.expect("bob must receive message");
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "ping"
        );
    }

    /// `@alice` target strips the `@` prefix and delivers direct to alice,
    /// bypassing the fronting resolver entirely.
    #[tokio::test]
    async fn target_with_at_prefix_strips_and_delivers_direct() {
        let reg = Arc::new(AgentRegistry::new());
        let (alice_tx, mut alice_rx) = mpsc::unbounded_channel::<MailboxInput>();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("alice".into(), alice_tx, SessionStatus::Active);
        reg.register("bob".into(), bob_tx, SessionStatus::Active);

        // Fronting fallback = bob, but @alice should bypass it.
        let fronting_set = FrontingSet::from_parts(
            Vec::new(),
            Some(SmolStr::from("bob")),
            RoutingTable::default(),
        );
        let constellation: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), constellation);

        let router = AgentRouter::new(Arc::clone(&reg)).with_fronting(state);
        // "@alice" target → strip '@' → deliver direct to alice.
        router
            .route(&test_sender(), "@alice", &test_message("direct"))
            .await
            .unwrap();

        let received = alice_rx
            .recv()
            .await
            .expect("alice must receive direct message");
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "direct",
            "alice should receive the direct-addressed message"
        );
        // Bob (the fronting fallback) must NOT receive it.
        assert!(
            bob_rx.try_recv().is_err(),
            "bob (fallback) must not receive a message directly addressed to alice"
        );
    }

    /// When the message body opens with `@bob …`, the body-override logic
    /// in the router honours the in-body direct address even when the
    /// target string itself is a persona name (without the `@` prefix).
    ///
    /// This tests the "body says @bob, target says alice" override path at
    /// agent.rs:135-148 which picks `bob` when body has `@bob` and target
    /// does not match bob.
    #[tokio::test]
    async fn at_prefix_in_body_overrides_target() {
        let reg = Arc::new(AgentRegistry::new());
        let (alice_tx, mut alice_rx) = mpsc::unbounded_channel::<MailboxInput>();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("alice".into(), alice_tx, SessionStatus::Active);
        reg.register("bob".into(), bob_tx, SessionStatus::Active);

        let router = AgentRouter::new(Arc::clone(&reg));
        // target = "alice", but body starts with "@bob" — override to bob.
        let msg = test_message("@bob please help");
        router.route(&test_sender(), "alice", &msg).await.unwrap();

        let received = bob_rx
            .recv()
            .await
            .expect("bob should receive body-directed message");
        // The `@bob` prefix is stripped before delivery — the recipient
        // sees a clean message body.
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "please help",
            "bob should receive the body with the leading `@bob` stripped"
        );
        assert!(
            alice_rx.try_recv().is_err(),
            "alice (target string) must not receive when body overrides to bob"
        );
    }

    /// Empty target with NO fronting configured returns
    /// `PersonaNotFound("<unspecified>")` — not a panic, not a silent drop.
    #[tokio::test]
    async fn empty_target_no_fronting_returns_unspecified() {
        let reg = Arc::new(AgentRegistry::new());
        // No personas registered, no fronting wired.
        let router = AgentRouter::new(reg);
        let err = router
            .route(&test_sender(), "", &test_message("lost"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                RouterError::PersonaNotFound(id) if id.as_str() == "<unspecified>"
            ),
            "expected PersonaNotFound(<unspecified>), got: {err:?}"
        );
    }

    /// Routing rule (Prefix) matches body → routes to the rule's target,
    /// not the fallback.
    #[tokio::test]
    async fn routing_rule_prefix_match_routes_to_rule_target() {
        let reg = Arc::new(AgentRegistry::new());
        let (math_tx, mut math_rx) = mpsc::unbounded_channel::<MailboxInput>();
        let (chat_tx, mut chat_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register("math".into(), math_tx, SessionStatus::Active);
        reg.register("chat".into(), chat_tx, SessionStatus::Active);

        let rules = vec![RoutingRule::new(
            "math-rule".to_string(),
            MessagePattern::Prefix("!math".to_string()),
            SmolStr::from("math"),
            10,
        )];
        let table = RoutingTable::try_from_rules(rules).unwrap();
        let fronting_set = FrontingSet::from_parts(Vec::new(), Some(SmolStr::from("chat")), table);
        let constellation: Arc<dyn ConstellationRegistry> =
            Arc::new(InMemoryConstellationRegistry::new());
        let state = FrontingState::new(Arc::new(RwLock::new(fronting_set)), constellation);

        let router = AgentRouter::new(Arc::clone(&reg)).with_fronting(state);
        // Empty target + body starting with "!math" → rule match → math.
        router
            .route(&test_sender(), "", &test_message("!math 2+2"))
            .await
            .unwrap();

        let received = math_rx
            .recv()
            .await
            .expect("math must receive the rule-matched message");
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap_or(""),
            "!math 2+2"
        );
        // Chat (fallback) must not receive anything.
        assert!(
            chat_rx.try_recv().is_err(),
            "chat fallback must not receive rule-matched message"
        );
    }
}
