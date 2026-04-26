//! Per-session mailbox: queues inbound activations into the agent's
//! turn loop.
//!
//! v3-multi-agent Phase 4 introduces *mailboxes* — every active session
//! owns one [`Mailbox`] that buffers three kinds of activations:
//!
//! 1. **Direct messages** sent by another agent or the partner via
//!    `Pattern.Message.Send`/`Reply`/`Notify`.
//! 2. **Task assignments** delegated by another agent. Carry an
//!    extra [`BlockRef`] that gets pinned into the recipient's
//!    snapshot selection so the assigned task shows up in their
//!    composed context.
//! 3. **Wake events** triggered by registered conditions (timers,
//!    block-changed subscribers, task-dependency resolvers; see
//!    [`pattern_core::wake::WakeReason`]).
//!
//! T2 lands the *data carriers*: the [`MailboxInput`] enum, the
//! [`Mailbox`] struct holding a tokio mpsc, and the
//! [`SessionContext`](crate::session::SessionContext) busy-flag pair
//! ([`is_in_turn`](crate::session::SessionContext::is_in_turn) +
//! [`turn_done`](crate::session::SessionContext::turn_done)) that the
//! `MailboxTask` (T3) waits on.
//!
//! The `MailboxTask` itself — the tokio task that drains the inbox and
//! calls `drive_step` when the session is idle — lands in T3.

use std::sync::Arc;

use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;
use pattern_core::wake::WakeReason;
use tokio::sync::{Mutex, mpsc};

/// A single activation enqueued into a session's mailbox.
///
/// Three shapes today; `#[non_exhaustive]` so future transports
/// (RPC, plugin-injected events) can grow the enum without breaking
/// match arms.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum MailboxInput {
    /// A direct message from another agent or the partner.
    Message {
        /// The body to deliver as a turn input.
        msg: Message,
        /// Sender origin — used by handlers + TUI for attribution.
        from: MessageOrigin,
    },
    /// A task assignment from another agent.
    ///
    /// The recipient's [`MailboxTask`] (T3) appends `task` to the
    /// message's `block_refs` so the snapshot composer pins the task
    /// into the agent's working memory for that turn.
    TaskAssigned {
        /// The task block being assigned.
        task: BlockRef,
        /// Persona id of the assigner — printed in observability logs
        /// and surfaced to the agent's prompt pipeline.
        from: PersonaId,
        /// The accompanying message body. Typically a short
        /// instruction or context note from the assigner.
        msg: Message,
    },
    /// A registered wake condition fired.
    Wake {
        /// Why this wake was triggered. Round-trips to the agent's
        /// Haskell program via `TurnInput::wake` (added in T6).
        reason: WakeReason,
    },
}

/// Per-session inbox.
///
/// Holds the receiving half of an unbounded tokio mpsc channel under a
/// tokio mutex (T3's drain task awaits across `recv`, so a `std`
/// mutex would deadlock the runtime). Senders are produced via
/// [`Mailbox::sender`] and freely cloned — the [`AgentRegistry`] (T4)
/// hands them out to other sessions wanting to deliver a message to
/// this agent.
///
/// The mailbox itself does not drive any turn loop — the
/// [`MailboxTask`] in T3 owns the receiver guard for as long as the
/// session lives. This struct just bundles the send + receive halves
/// with the persona id that owns it for clearer observability.
pub struct Mailbox {
    tx: mpsc::UnboundedSender<MailboxInput>,
    rx: Mutex<mpsc::UnboundedReceiver<MailboxInput>>,
    persona_id: PersonaId,
}

impl Mailbox {
    /// Construct a fresh mailbox for `persona_id`.
    ///
    /// Returns the boxed mailbox alongside an extra sender clone for
    /// the registry to hand out — callers wanting more sender clones
    /// later use [`Self::sender`].
    pub fn new(persona_id: PersonaId) -> (Arc<Self>, mpsc::UnboundedSender<MailboxInput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mbx = Arc::new(Self {
            tx: tx.clone(),
            rx: Mutex::new(rx),
            persona_id,
        });
        (mbx, tx)
    }

    /// Clone of the sender half — hand out to peers that want to send
    /// activations to this session.
    pub fn sender(&self) -> mpsc::UnboundedSender<MailboxInput> {
        self.tx.clone()
    }

    /// The persona this mailbox belongs to. Used for observability
    /// logs and the [`AgentRegistry`] (T4) lookup table.
    pub fn persona_id(&self) -> &PersonaId {
        &self.persona_id
    }

    /// Acquire the receiver guard. The [`MailboxTask`] (T3) holds this
    /// for the lifetime of its loop; tests can use it to assert that a
    /// specific input was delivered.
    pub async fn lock_rx(&self) -> tokio::sync::MutexGuard<'_, mpsc::UnboundedReceiver<MailboxInput>> {
        self.rx.lock().await
    }
}

impl std::fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox")
            .field("persona_id", &self.persona_id)
            .field("rx_locked", &"<tokio::Mutex>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};

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

    fn test_origin() -> MessageOrigin {
        MessageOrigin::new(
            Author::System {
                reason: SystemReason::Timer,
            },
            Sphere::System,
        )
    }

    #[tokio::test]
    async fn sender_clones_deliver_into_same_inbox() {
        let (mbx, tx_a) = Mailbox::new(PersonaId::from("agent-a"));
        let tx_b = mbx.sender();

        tx_a.send(MailboxInput::Message {
            msg: test_message("from-a"),
            from: test_origin(),
        })
        .unwrap();
        tx_b.send(MailboxInput::Message {
            msg: test_message("from-b"),
            from: test_origin(),
        })
        .unwrap();

        let mut rx = mbx.lock_rx().await;
        let first = rx.recv().await.expect("first input");
        let second = rx.recv().await.expect("second input");
        match (first, second) {
            (
                MailboxInput::Message { msg: m1, .. },
                MailboxInput::Message { msg: m2, .. },
            ) => {
                let t1 = m1.chat_message.content.first_text().unwrap();
                let t2 = m2.chat_message.content.first_text().unwrap();
                assert_eq!((t1, t2), ("from-a", "from-b"));
            }
            other => panic!("expected two Message variants, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn persona_id_is_preserved() {
        let (mbx, _tx) = Mailbox::new(PersonaId::from("anchor"));
        assert_eq!(mbx.persona_id().as_str(), "anchor");
    }
}
