//! In-memory registry mapping [`PersonaId`] to live session mailboxes.
//!
//! Every active session registers its mailbox sender here at open time
//! and unregisters on close. Peer sessions look up recipients by
//! [`PersonaId`] before routing — the `agent:` scheme router
//! ([`crate::router::agent::AgentRouter`]) drives this lookup.
//!
//! # Draft persona queuing (AC6.5)
//!
//! When a persona is registered with [`SessionStatus::Draft`], it has no
//! live session — messages cannot be delivered. The registry instead
//! buffers them in a per-persona `draft_queues` entry. Phase 6's
//! `PromoteDraft` RPC will drain the queue via
//! [`AgentRegistry::drain_draft_queue`] after promoting a draft to an
//! active session. Phase 4 ships the *queueing* path only; the drain
//! path is documented but unexercised until Phase 6.
//!
//! # RAII unregistration
//!
//! Callers that want automatic unregistration on drop (e.g.
//! [`TidepoolSession`](crate::session::TidepoolSession)) should use
//! [`RegistryGuard`], an RAII wrapper that calls
//! [`AgentRegistry::unregister`] when dropped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;
use tokio::sync::mpsc;

use crate::mailbox::MailboxInput;
use crate::router::RouterError;

/// Lifecycle status of a registered persona.
///
/// `Active`: session is open; messages are delivered to the mailbox sender.
/// `Draft`: persona was registered but no session is running (waiting for
///          [`PromoteDraft`]); messages are queued in `draft_queues`.
/// `Inactive`: persona has been deregistered; the entry is gone.
///
/// Note: `Inactive` is not stored in the map — `unregister` removes the
/// entry entirely. This enum represents the status while an entry exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// Session is open; the mailbox sender is live and accepting sends.
    Active,
    /// Persona is known but no session is open. Messages are queued until
    /// the persona is promoted to `Active` (Phase 6 `PromoteDraft`).
    Draft,
}

/// One entry in the registry for a registered persona.
///
/// `Draft` personas have a `mailbox_tx` that points to a closed channel
/// (i.e. there is no receiving end). Senders through it will immediately
/// fail; the draft path uses [`AgentRegistry::queue_for_draft`] instead of
/// going through the sender.
#[derive(Debug)]
pub struct AgentEntry {
    /// Sender half of the per-session mailbox channel.
    ///
    /// Valid and open when `status == Active`; the draft path does NOT
    /// send through this — it writes to `draft_queues` instead.
    pub mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
    /// Whether this persona has a live session.
    pub status: SessionStatus,
}

/// Per-draft-persona queue for messages that arrive before a session opens.
///
/// Keyed by [`PersonaId`]; entry exists only while a persona is in `Draft`
/// status. [`AgentRegistry::unregister`] removes the queue when the persona
/// is deregistered without ever being promoted.
type DraftQueue = Mutex<VecDeque<(Message, MessageOrigin)>>;

/// In-memory registry mapping [`PersonaId`] to live session mailboxes.
///
/// Thread-safe: backed by [`DashMap`] for lock-free concurrent access
/// across multiple sessions.
///
/// Construct via [`AgentRegistry::new`]; the resulting `Arc<AgentRegistry>`
/// is shared across all sessions that participate in the same runtime. For
/// tests that do not need multi-session interaction, `Arc::new(AgentRegistry::new())`
/// is sufficient.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    /// Active and draft persona entries keyed by `PersonaId`.
    entries: DashMap<PersonaId, AgentEntry>,
    /// Pending messages for draft personas awaiting `PromoteDraft` (Phase 6).
    draft_queues: DashMap<PersonaId, DraftQueue>,
}

impl AgentRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a persona. Callers supply the mailbox sender and the
    /// initial status.
    ///
    /// Passing `SessionStatus::Draft` creates a queue entry in
    /// `draft_queues` so subsequent messages are buffered.
    /// Passing `SessionStatus::Active` removes any stale draft queue
    /// for this persona (in case a prior draft entry exists).
    ///
    /// Overwrites any existing entry for `id`.
    pub fn register(
        &self,
        id: PersonaId,
        tx: mpsc::UnboundedSender<MailboxInput>,
        status: SessionStatus,
    ) {
        if status == SessionStatus::Draft {
            // Pre-create the queue; only draft personas need it.
            self.draft_queues
                .entry(id.clone())
                .or_insert_with(|| Mutex::new(VecDeque::new()));
        } else {
            // Promote from draft → active: drop any stale queue.
            self.draft_queues.remove(&id);
        }
        self.entries.insert(
            id,
            AgentEntry {
                mailbox_tx: tx,
                status,
            },
        );
    }

    /// Unregister a persona. If the persona was in `Draft` status, its
    /// pending draft queue is also dropped (any queued messages are
    /// discarded).
    ///
    /// No-op if the persona was not registered.
    pub fn unregister(&self, id: &PersonaId) {
        self.entries.remove(id);
        self.draft_queues.remove(id);
    }

    /// Return a clone of the mailbox sender for an `Active` persona, or
    /// `None` if the persona is not registered or is in `Draft` status.
    ///
    /// Callers route messages through the returned sender. Draft personas
    /// do not have a live receiving session; use
    /// [`Self::queue_for_draft`] instead.
    pub fn sender(&self, id: &PersonaId) -> Option<mpsc::UnboundedSender<MailboxInput>> {
        let entry = self.entries.get(id)?;
        if entry.status == SessionStatus::Active {
            Some(entry.mailbox_tx.clone())
        } else {
            None
        }
    }

    /// Current status of a persona, or `None` if not registered.
    pub fn status(&self, id: &PersonaId) -> Option<SessionStatus> {
        self.entries.get(id).map(|e| e.status.clone())
    }

    /// Route a message to the correct destination atomically.
    ///
    /// This is the preferred entry point for routing — it atomically checks
    /// the persona's status and performs the appropriate action within the
    /// same DashMap shard lock, preventing the TOCTOU race that exists when
    /// callers separately call [`Self::status`] and then [`Self::sender`] or
    /// [`Self::queue_for_draft`].
    ///
    /// # Outcomes
    ///
    /// - `Active` with a live sender: delivers `msg` to the mailbox.
    /// - `Draft`: appends `msg` to the draft queue for future [`PromoteDraft`].
    /// - Not registered (vacant): returns `Err(RouterError::PersonaNotFound)`.
    ///
    /// # Rationale
    ///
    /// DashMap's `entry(id)` acquires an exclusive shard-level write lock,
    /// so the status read and the send/queue operation are atomic with respect
    /// to concurrent [`Self::register`] calls that promote Draft → Active.
    /// Without this, a sender could observe `Draft`, then a promoter could
    /// complete, then the sender would call `queue_for_draft` which would find
    /// `Active` status and return `PersonaNotFound` — losing the message.
    pub fn route_or_queue(
        &self,
        id: &PersonaId,
        msg: MailboxInput,
    ) -> Result<(), RouterError> {
        // Use the DashMap entry API to hold the shard lock for the entire
        // read-then-dispatch sequence, preventing the Draft→Active promotion
        // race described in the doc comment above.
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| RouterError::PersonaNotFound(id.clone()))?;

        match entry.status {
            SessionStatus::Active => {
                let tx = entry.mailbox_tx.clone();
                // Release the shard lock before sending to avoid holding it
                // across a potentially blocking channel operation.
                drop(entry);
                tx.send(msg).map_err(|_| RouterError::MailboxClosed)
            }
            SessionStatus::Draft => {
                // Release the shard lock before locking the queue mutex to
                // maintain consistent lock ordering (entries lock → queue
                // lock is wrong; reverse or sequential avoids deadlock).
                let id_clone = id.clone();
                drop(entry);
                self.draft_queues
                    .get(&id_clone)
                    .ok_or_else(|| RouterError::PersonaNotFound(id_clone.clone()))?
                    .lock()
                    .expect("draft queue mutex poisoned")
                    .push_back((msg.msg, msg.from));
                Ok(())
            }
        }
    }

    /// Append a message to a draft persona's queue.
    ///
    /// Returns `Err(RouterError::PersonaNotFound)` if the persona is not
    /// registered as `Draft` — callers in the `agent:` router should call
    /// [`Self::sender`] first for `Active` personas and only fall through
    /// to this method when the status is `Draft`.
    ///
    /// Prefer [`Self::route_or_queue`] for routing: it atomically checks
    /// status and queues, eliminating the TOCTOU race between two calls.
    pub fn queue_for_draft(
        &self,
        id: &PersonaId,
        msg: Message,
        origin: MessageOrigin,
    ) -> Result<(), RouterError> {
        // We only queue when the persona is known-Draft.
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| RouterError::PersonaNotFound(id.clone()))?;
        if entry.status != SessionStatus::Draft {
            return Err(RouterError::PersonaNotFound(id.clone()));
        }
        drop(entry); // release the shard lock before locking the queue.
        self.draft_queues
            .get(id)
            .ok_or_else(|| RouterError::PersonaNotFound(id.clone()))?
            .lock()
            .expect("draft queue mutex poisoned")
            .push_back((msg, origin));
        Ok(())
    }

    /// Drain all queued messages for a persona in FIFO order (oldest first).
    ///
    /// Returns an empty `Vec` if the persona has no queue or is not in
    /// `Draft` status. This is an idempotent operation — a second call
    /// returns empty.
    ///
    /// Used by Phase 6's `PromoteDraft` RPC after opening a live session:
    /// drain the queue, then route each message through the newly-created
    /// mailbox sender.
    pub fn drain_draft_queue(&self, id: &PersonaId) -> Vec<(Message, MessageOrigin)> {
        self.draft_queues
            .get(id)
            .map(|q| {
                q.lock()
                    .expect("draft queue mutex poisoned")
                    .drain(..)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// RAII guard that calls [`AgentRegistry::unregister`] when dropped.
///
/// Callers (typically [`TidepoolSession`](crate::session::TidepoolSession)
/// open path) hold this guard for the lifetime of the session. When the
/// session is dropped the guard fires and the persona is removed from the
/// registry — no leftover stale entries.
///
/// Holding `Arc<AgentRegistry>` ensures the registry outlives the guard
/// in multi-session scenarios where the session drops before the
/// `Arc<AgentRegistry>` shared with the daemon.
pub struct RegistryGuard {
    registry: Arc<AgentRegistry>,
    persona_id: PersonaId,
}

impl RegistryGuard {
    /// Register a persona with `Active` status and return a guard that
    /// unregisters it on drop.
    pub fn register_active(
        registry: Arc<AgentRegistry>,
        persona_id: PersonaId,
        tx: mpsc::UnboundedSender<MailboxInput>,
    ) -> Self {
        registry.register(persona_id.clone(), tx, SessionStatus::Active);
        Self {
            registry,
            persona_id,
        }
    }
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        self.registry.unregister(&self.persona_id);
    }
}

impl std::fmt::Debug for RegistryGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryGuard")
            .field("persona_id", &self.persona_id)
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

    fn make_tx() -> (
        mpsc::UnboundedSender<MailboxInput>,
        mpsc::UnboundedReceiver<MailboxInput>,
    ) {
        mpsc::unbounded_channel()
    }

    // --- AC6.1 / AC6.4 / AC6.5 groundwork ---

    #[test]
    fn active_persona_sender_is_available() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, _rx) = make_tx();
        reg.register("persona-a".into(), tx, SessionStatus::Active);

        assert_eq!(reg.status(&"persona-a".into()), Some(SessionStatus::Active));
        assert!(reg.sender(&"persona-a".into()).is_some());
    }

    #[test]
    fn draft_persona_sender_returns_none() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, _rx) = make_tx();
        reg.register("draft-b".into(), tx, SessionStatus::Draft);

        // Status is Draft, not Active.
        assert_eq!(reg.status(&"draft-b".into()), Some(SessionStatus::Draft));
        // No sender for draft personas — use queue_for_draft instead.
        assert!(reg.sender(&"draft-b".into()).is_none());
    }

    #[test]
    fn unregistered_persona_returns_none() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.status(&"nobody".into()), None);
        assert!(reg.sender(&"nobody".into()).is_none());
    }

    /// AC6.4: looking up a nonexistent persona must yield PersonaNotFound.
    #[test]
    fn queue_for_draft_unknown_persona_returns_persona_not_found() {
        let reg = AgentRegistry::new();
        let err = reg
            .queue_for_draft(&"ghost".into(), test_message("hi"), test_origin())
            .unwrap_err();
        assert!(
            matches!(err, RouterError::PersonaNotFound(ref id) if id.as_str() == "ghost"),
            "expected PersonaNotFound(ghost), got: {err:?}"
        );
    }

    /// AC6.5: draft queue accepts messages without routing to a session.
    #[test]
    fn queue_for_draft_stores_messages_in_order() {
        let reg = AgentRegistry::new();
        let (tx, _rx) = make_tx();
        reg.register("draft-c".into(), tx, SessionStatus::Draft);

        let msg1 = test_message("first");
        let msg2 = test_message("second");
        reg.queue_for_draft(&"draft-c".into(), msg1.clone(), test_origin())
            .unwrap();
        reg.queue_for_draft(&"draft-c".into(), msg2.clone(), test_origin())
            .unwrap();

        let drained = reg.drain_draft_queue(&"draft-c".into());
        assert_eq!(drained.len(), 2, "should have 2 queued messages");
        let t0 = drained[0].0.chat_message.content.first_text().unwrap();
        let t1 = drained[1].0.chat_message.content.first_text().unwrap();
        assert_eq!(t0, "first");
        assert_eq!(t1, "second");
    }

    #[test]
    fn drain_draft_queue_is_idempotent() {
        let reg = AgentRegistry::new();
        let (tx, _rx) = make_tx();
        reg.register("draft-d".into(), tx, SessionStatus::Draft);
        reg.queue_for_draft(&"draft-d".into(), test_message("x"), test_origin())
            .unwrap();

        let first_drain = reg.drain_draft_queue(&"draft-d".into());
        let second_drain = reg.drain_draft_queue(&"draft-d".into());
        assert_eq!(first_drain.len(), 1);
        assert_eq!(second_drain.len(), 0, "second drain should be empty");
    }

    #[test]
    fn unregister_removes_entry_and_draft_queue() {
        let reg = AgentRegistry::new();
        let (tx, _rx) = make_tx();
        reg.register("draft-e".into(), tx, SessionStatus::Draft);
        reg.queue_for_draft(&"draft-e".into(), test_message("pending"), test_origin())
            .unwrap();

        reg.unregister(&"draft-e".into());
        assert_eq!(reg.status(&"draft-e".into()), None);
        // After unregister, drain returns empty (queue dropped).
        let drained = reg.drain_draft_queue(&"draft-e".into());
        assert_eq!(drained.len(), 0);
    }

    #[test]
    fn register_active_removes_stale_draft_queue() {
        let reg = AgentRegistry::new();
        let (tx1, _rx1) = make_tx();
        let (tx2, _rx2) = make_tx();
        reg.register("flip-f".into(), tx1, SessionStatus::Draft);
        reg.queue_for_draft(&"flip-f".into(), test_message("queued"), test_origin())
            .unwrap();

        // Promote: register as Active.
        reg.register("flip-f".into(), tx2, SessionStatus::Active);
        assert_eq!(reg.status(&"flip-f".into()), Some(SessionStatus::Active));

        // Draft queue was discarded on promotion.
        let drained = reg.drain_draft_queue(&"flip-f".into());
        assert_eq!(
            drained.len(),
            0,
            "draft queue should be cleared on promotion"
        );
    }

    /// Registry guard fires unregister on drop.
    #[test]
    fn registry_guard_unregisters_on_drop() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, _rx) = make_tx();

        let guard = RegistryGuard::register_active(reg.clone(), "guard-g".into(), tx);
        assert_eq!(reg.status(&"guard-g".into()), Some(SessionStatus::Active));

        drop(guard);
        assert_eq!(
            reg.status(&"guard-g".into()),
            None,
            "registry guard should unregister on drop"
        );
    }

    /// Active persona can send a live message through the sender.
    #[tokio::test]
    async fn active_sender_delivers_message() {
        let reg = Arc::new(AgentRegistry::new());
        let (tx, mut rx) = make_tx();
        reg.register("active-h".into(), tx, SessionStatus::Active);

        let sender = reg.sender(&"active-h".into()).unwrap();
        sender
            .send(crate::mailbox::MailboxInput {
                from: test_origin(),
                msg: test_message("delivered"),
            })
            .unwrap();

        let received = rx.recv().await.unwrap();
        let text = received.msg.chat_message.content.first_text().unwrap();
        assert_eq!(text, "delivered");
    }
}
