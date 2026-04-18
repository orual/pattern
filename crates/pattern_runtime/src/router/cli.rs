//! CLI router: routes messages to a CLI consumer via an unbounded channel.
//!
//! The caller (CLI binary, test harness) creates a `CliRouter`, registers
//! it with the [`super::RouterRegistry`], and holds the receiver side to
//! consume agent-to-human output.
//!
//! Phase 5 foundation: all `cli:*` recipients go to the single registered
//! sink. Mapping to multiple CLI consumers (by target id) is future
//! scope; the target is ignored for now.
//!
//! Registering a `CliRouter` with
//! [`super::RouterRegistry::with_default_scheme("cli")`] makes it absorb
//! fallback routing for malformed or unknown-scheme recipients —
//! typically what you want for an interactive session where every
//! stray message should still reach the operator.

use async_trait::async_trait;
use pattern_core::types::message::Message;
use tokio::sync::mpsc;

use super::{Router, RouterError};

/// Routes messages to a CLI consumer via a `tokio::sync::mpsc` channel.
pub struct CliRouter {
    sink: mpsc::UnboundedSender<Message>,
}

impl CliRouter {
    /// Create a new CLI router and its paired receiver.
    ///
    /// The caller holds the receiver to consume routed messages
    /// (agent -> human output).
    pub fn new() -> (Self, mpsc::UnboundedReceiver<Message>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { sink: tx }, rx)
    }
}

#[async_trait]
impl Router for CliRouter {
    fn scheme(&self) -> &str {
        "cli"
    }

    async fn route(&self, _target: &str, body: &Message) -> Result<(), RouterError> {
        // Target is ignored for Phase 5 foundation — all `cli:*`
        // recipients (and any fallback-routed strings when this router
        // is the registry's default) go to the single registered sink.
        self.sink
            .send(body.clone())
            .map_err(|e| RouterError::RouteFailed(format!("cli sink closed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use pattern_core::types::ids::{new_id, AgentId, BatchId, MessageId};

    fn test_message(text: &str) -> Message {
        Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::User,
                text.to_string(),
            ),
            id: MessageId::from(new_id().to_string()),
            owner_id: AgentId::from("test-agent"),
            created_at: Timestamp::now(),
            batch: BatchId::from(new_id().to_string()),
            response_meta: None,
            block_refs: vec![],
        }
    }

    #[tokio::test]
    async fn cli_router_delivers_message() {
        let (router, mut rx) = CliRouter::new();
        let msg = test_message("hello from agent");
        router.route("cli:user", &msg).await.unwrap();

        let received = rx.recv().await.unwrap();
        // Verify the message content was preserved.
        let text = received.chat_message.content.first_text()
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
        let err = router.route("cli:user", &msg).await.unwrap_err();
        assert!(
            matches!(err, RouterError::RouteFailed(_)),
            "expected RouteFailed, got: {err:?}"
        );
    }
}
