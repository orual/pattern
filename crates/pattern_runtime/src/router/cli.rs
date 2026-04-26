//! CLI router: routes messages to a CLI consumer via an unbounded channel.
//!
//! The caller (CLI binary, test harness, daemon actor) creates a
//! `CliRouter`, registers it with the `RouterRegistry` (from `super`), and
//! holds the receiver side to consume agent-to-human output.
//!
//! Phase 5 foundation: all `cli:*` recipients go to the single registered
//! sink. Mapping to multiple CLI consumers (by target id) is future
//! scope; the target is currently passed through verbatim on
//! [`CliRouterEvent::target`] so consumers can dispatch on it.
//!
//! Phase 4 (v3-multi-agent): the channel item is [`CliRouterEvent`] —
//! a typed envelope carrying the [`MessageOrigin`] of the dispatcher
//! plus the target string and the message body. The daemon's actor
//! consumes these events and fans them out to subscribed TUI clients
//! as `WireTurnEvent::MessageSent` so the recipient can render with
//! sender attribution. Pre-Phase-4 callers that only needed the
//! `Message` should access `event.body`.
//!
//! Registering a `CliRouter` with
//! `RouterRegistry::with_default_scheme("cli")` makes it absorb
//! fallback routing for malformed or unknown-scheme recipients —
//! typically what you want for an interactive session where every
//! stray message should still reach the operator.

use async_trait::async_trait;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;
use tokio::sync::mpsc;

use super::{Router, RouterError};

/// Envelope sent on a [`CliRouter`]'s channel.
///
/// Carries enough provenance for the consumer (typically the daemon
/// actor) to construct an attribution-tagged event for the TUI.
#[derive(Debug, Clone)]
pub struct CliRouterEvent {
    /// Origin of the dispatcher — used for sender attribution at the
    /// consumer.
    pub sender: MessageOrigin,
    /// Target portion of the recipient as received by the router.
    /// For `cli:*` routes this is the part after `cli:`; for default-
    /// scheme fallback this is the full original recipient string.
    pub target: String,
    /// The message body being routed.
    pub body: Message,
}

/// Routes messages to a CLI consumer via a `tokio::sync::mpsc` channel.
pub struct CliRouter {
    sink: mpsc::UnboundedSender<CliRouterEvent>,
}

impl CliRouter {
    /// Create a new CLI router and its paired receiver.
    ///
    /// The caller holds the receiver to consume routed events
    /// (agent → human output, with sender attribution).
    pub fn new() -> (Self, mpsc::UnboundedReceiver<CliRouterEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { sink: tx }, rx)
    }
}

#[async_trait]
impl Router for CliRouter {
    fn scheme(&self) -> &str {
        "cli"
    }

    async fn route(
        &self,
        sender: &MessageOrigin,
        target: &str,
        body: &Message,
    ) -> Result<(), RouterError> {
        let event = CliRouterEvent {
            sender: sender.clone(),
            target: target.to_string(),
            body: body.clone(),
        };
        self.sink
            .send(event)
            .map_err(|e| RouterError::RouteFailed(format!("cli sink closed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};

    fn test_message(text: &str) -> Message {
        Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::User,
                text.to_string(),
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

    #[tokio::test]
    async fn cli_router_delivers_message_with_sender() {
        let (router, mut rx) = CliRouter::new();
        let msg = test_message("hello from agent");
        let sender = test_sender();
        router.route(&sender, "user", &msg).await.unwrap();

        let received = rx.recv().await.unwrap();
        // Sender attribution preserved.
        assert!(matches!(
            received.sender.author,
            Author::System {
                reason: SystemReason::Timer
            }
        ));
        // Target preserved (post-scheme-strip).
        assert_eq!(received.target, "user");
        // Body content preserved.
        let text = received
            .body
            .chat_message
            .content
            .first_text()
            .expect("message should have text content");
        assert_eq!(text, "hello from agent");
    }

    #[tokio::test]
    async fn cli_router_scheme_is_cli() {
        let (router, _rx) = CliRouter::new();
        assert_eq!(router.scheme(), "cli");
    }

    #[tokio::test]
    async fn cli_router_errors_when_receiver_dropped() {
        let (router, rx) = CliRouter::new();
        drop(rx);
        let msg = test_message("orphaned");
        let err = router
            .route(&test_sender(), "user", &msg)
            .await
            .unwrap_err();
        assert!(
            matches!(err, RouterError::RouteFailed(_)),
            "expected RouteFailed, got: {err:?}"
        );
    }
}
