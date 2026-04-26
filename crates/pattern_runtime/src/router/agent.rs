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
use pattern_core::fronting::parse_direct_address;
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
                Some(state) => dispatch_to_mailboxes(&self.registry, state, sender, body).await,
                None => {
                    tracing::debug!(
                        "agent router: empty/auto target with no fronting state wired"
                    );
                    Err(RouterError::PersonaNotFound(PersonaId::from("<unspecified>")))
                }
            };
        }

        // (2) `@persona-name` direct addressing in the BODY (recipient
        // string is just `agent:`, body says `@alice please…`). We've
        // already handled the empty-target case above, so this branch
        // fires when callers used `agent:@alice` (legacy / convenience
        // form) — strip the `@` from the target and route direct.
        let direct_id: PersonaId =
            if let Some(stripped) = target.strip_prefix('@') {
                PersonaId::from(stripped)
            } else if let Some(parsed) =
                parse_direct_address(body.chat_message.content.first_text().unwrap_or(""))
            {
                // The recipient string is a literal persona id but the
                // body opens with `@persona-id`. Honour the body's
                // directive and override target. This keeps the `@`
                // semantics consistent across SDK call shapes.
                if parsed.as_str() == target {
                    PersonaId::from(target)
                } else {
                    parsed
                }
            } else {
                PersonaId::from(target)
            };

        let input = MailboxInput {
            from: sender.clone(),
            msg: body.clone(),
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
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};
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
}
