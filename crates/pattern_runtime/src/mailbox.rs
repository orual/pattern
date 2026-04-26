//! Per-session mailbox: queues inbound activations into the agent's
//! turn loop.
//!
//! v3-multi-agent Phase 4 introduces *mailboxes* — every active session
//! owns one [`Mailbox`] that buffers inbound activations and feeds the
//! [`MailboxTask`] (T3) which calls `drive_step` whenever the session
//! is idle.
//!
//! All inbound activations — direct messages, task assignments, and
//! wake events fired by registered conditions — share a single carrier:
//! a [`pattern_core::Message`] paired with the dispatcher's
//! [`MessageOrigin`]. Wake-triggered activations distinguish themselves
//! by setting the origin's `author` to
//! [`pattern_core::SystemReason::TaskTimeout`] /
//! [`Interval`](pattern_core::SystemReason::Interval) /
//! [`BlockChanged`](pattern_core::SystemReason::BlockChanged) etc., and
//! by attaching the same structured payload (block refs, elapsed spans)
//! to the variant. There is no separate "wake reason" axis on the
//! turn input — `Author::System { reason }` already answers the
//! "why is this turn happening" question.
//!
//! Task assignments are messages with the assigned task pinned into
//! the message's `block_refs`; the snapshot composer sees the
//! `BlockRef` automatically without a separate dispatch path.
//!
//! T2 lands the *data carriers*: the [`MailboxInput`] struct, the
//! [`Mailbox`] itself, and the busy-flag pair on
//! [`SessionContext`](crate::session::SessionContext). T3 wires the
//! [`MailboxTask`] that drains the inbox and calls `drive_step`.

use std::sync::Arc;

use pattern_core::types::ids::PersonaId;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;
use tokio::sync::{Mutex, mpsc};

/// A single activation enqueued into a session's mailbox.
///
/// The carrier is uniformly `(Message, MessageOrigin)`. The origin's
/// `author` field discriminates direct sends (`Agent` / `Partner` /
/// `Human`) from system-emitted wakes (`System { reason: TaskTimeout
/// { .. } }`, etc.). Task assignments are conveyed by populating the
/// message's `block_refs` — the snapshot composer reads them without
/// any mailbox-level branching.
#[derive(Debug, Clone)]
pub struct MailboxInput {
    /// Sender attribution: who/what is activating the agent. Wake
    /// sources synthesise an `Author::System { reason: ... }` origin
    /// carrying the structured payload (block ref + elapsed span)
    /// directly on the variant.
    pub from: MessageOrigin,
    /// The message body to deliver as a turn input. Task-assignment
    /// activations populate `msg.block_refs` so the snapshot composer
    /// pins the assigned task into the recipient's working memory
    /// for that turn.
    pub msg: Message,
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
/// [`MailboxTask`] (T3) owns the receiver guard for as long as the
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
    pub async fn lock_rx(
        &self,
    ) -> tokio::sync::MutexGuard<'_, mpsc::UnboundedReceiver<MailboxInput>> {
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

        tx_a.send(MailboxInput {
            from: test_origin(),
            msg: test_message("from-a"),
        })
        .unwrap();
        tx_b.send(MailboxInput {
            from: test_origin(),
            msg: test_message("from-b"),
        })
        .unwrap();

        let mut rx = mbx.lock_rx().await;
        let first = rx.recv().await.expect("first input");
        let second = rx.recv().await.expect("second input");
        let t1 = first.msg.chat_message.content.first_text().unwrap();
        let t2 = second.msg.chat_message.content.first_text().unwrap();
        assert_eq!((t1, t2), ("from-a", "from-b"));
    }

    #[tokio::test]
    async fn persona_id_is_preserved() {
        let (mbx, _tx) = Mailbox::new(PersonaId::from("anchor"));
        assert_eq!(mbx.persona_id().as_str(), "anchor");
    }
}
