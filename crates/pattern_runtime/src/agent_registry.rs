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
//! buffers them in a per-slot `VecDeque` protected by a `Mutex`. Phase 6's
//! `PromoteDraft` RPC promotes a draft to an active session by calling
//! [`AgentRegistry::register_active`], which atomically swaps the slot from
//! `Draft` to `Active` and replays buffered messages onto the live sender.
//!
//! # Atomicity guarantee
//!
//! The registry uses a single `DashMap<PersonaId, AgentSlot>`. Reads
//! go through `DashMap::get()`, which returns a `Ref` holding the per-shard
//! read lock for the *entire* `Ref`'s lifetime. Writes go through
//! `DashMap::insert()`, which acquires the per-shard write lock.
//! `DashMap::insert` cannot proceed while any reader holds a `Ref` on the
//! same shard. This closes the TOCTOU race that existed in the two-map
//! design:
//!
//! - Two-map race: a sender could observe `Draft`, the promoter could
//!   complete (swap entry + drain + remove draft queue), and the sender would
//!   then find a removed queue and return `PersonaNotFound`, silently losing
//!   the message.
//! - Single-map fix: the sender's `Ref` holds the shard read lock; the
//!   promoter cannot swap the slot until the sender releases it. Either the
//!   sender queues into the Draft slot (and the promoter drains it later), or
//!   the promoter has already swapped to Active (and the sender sends directly).
//!   No message is ever lost.
//!
//! # RAII unregistration
//!
//! Callers that want automatic unregistration on drop (e.g.
//! [`TidepoolSession`](crate::session::TidepoolSession)) should use
//! [`RegistryGuard`], an RAII wrapper that calls
//! [`AgentRegistry::unregister`] when dropped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::mailbox::{Mailbox, MailboxInput};
use crate::router::RouterError;
use dashmap::DashMap;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;

/// Lifecycle status of a registered persona.
///
/// `Active`: session is open; messages are delivered to the mailbox sender.
/// `Draft`: persona was registered but no session is running (waiting for
///          [`PromoteDraft`]); messages are queued in the slot's internal queue.
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

/// A slot in the agent registry. Each registered persona occupies exactly
/// one slot; the variant encodes whether the persona has a live session.
///
/// Both variants carry the data needed for their routing path. The inner
/// `Mutex` on `Draft.queue` is a *per-slot* lock — cheap to acquire because
/// it serialises only the queue push/pop for a single persona, not the whole
/// registry.
#[derive(Debug)]
enum AgentSlot {
    /// Persona has a live session; messages are routed through the sender.
    /// Persona has a live session; messages are routed through its mailbox.
    Active { mailbox: Arc<Mailbox> },
    /// Persona is known but has no live session; messages are queued for
    /// future replay on promotion.
    Draft {
        /// Buffered messages waiting for the next `register_active` call.
        /// The `Mutex` is per-slot, not per-shard; it is always acquired
        /// *while* holding the DashMap entry guard (which already holds the
        /// shard lock), so the acquisition order is always shard → queue and
        /// there is no lock-ordering inversion.
        queue: Mutex<VecDeque<MailboxInput>>,
    },
}

impl AgentSlot {
    fn status(&self) -> SessionStatus {
        match self {
            AgentSlot::Active { .. } => SessionStatus::Active,
            AgentSlot::Draft { .. } => SessionStatus::Draft,
        }
    }
}

/// In-memory registry mapping [`PersonaId`] to live session mailboxes.
///
/// Thread-safe: backed by a single [`DashMap`] whose entry guards hold the
/// per-shard lock for the full duration of each registry operation, closing
/// the TOCTOU race between Draft-path senders and Draft→Active promoters.
///
/// Construct via [`AgentRegistry::new`]; the resulting `Arc<AgentRegistry>`
/// is shared across all sessions that participate in the same runtime. For
/// tests that do not need multi-session interaction, `Arc::new(AgentRegistry::new())`
/// is sufficient.
///
/// # Aliases
///
/// In addition to the canonical-id slot map, the registry maintains a
/// separate alias map (`alias → canonical_id`). Aliases let an agent be
/// addressed by a non-canonical name (typically the persona's `name`
/// field when it differs from `agent_id`). Lookups try the canonical
/// slot map first; on miss they consult the alias map and retry.
///
/// Aliases are registered explicitly by callers that know the
/// canonical/alias mapping (e.g. session open). Unregistering a canonical
/// also removes any aliases pointing at it.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    /// Single-map design: one entry per persona, status encoded in the slot
    /// variant. All operations acquire the DashMap entry guard for their full
    /// duration — no cross-shard windows where a second operation can observe
    /// a partially-updated state.
    slots: DashMap<PersonaId, AgentSlot>,

    /// Alias index: `alias → canonical_id`. Lookup helpers consult this on
    /// canonical miss to support addressing an agent by its persona `name`
    /// when that differs from its `agent_id`. Registered via
    /// [`AgentRegistry::register_alias`]; cleaned up on canonical
    /// [`AgentRegistry::unregister`].
    aliases: DashMap<PersonaId, PersonaId>,
}

impl AgentRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a persona as `Draft`, creating an empty message queue.
    ///
    /// If the persona was previously registered (as `Active` or `Draft`),
    /// the existing slot is overwritten and any queued messages are discarded.
    /// Callers should only call this before a session opens; re-registering
    /// an `Active` persona as `Draft` would strand in-flight messages.
    pub fn register_draft(&self, id: PersonaId) {
        self.slots.insert(
            id,
            AgentSlot::Draft {
                queue: Mutex::new(VecDeque::new()),
            },
        );
    }

    /// Register a persona as `Active` with the given mailbox sender.
    ///
    /// If the persona was previously in `Draft` status, any buffered messages
    /// are atomically replayed onto `tx` before this call returns. The slot
    /// swap and drain are performed under the same DashMap entry guard, so
    /// no concurrent sender can push into the (now-moved) queue after the
    /// swap — the drain is guaranteed to see every message queued before the
    /// promotion and none after.
    ///
    /// If the persona was not previously registered, it is created as `Active`
    /// immediately (no draft queue to drain).
    ///
    /// Messages that fail to send during replay (closed channel) are silently
    /// dropped — the session that owns `tx` has gone away.
    pub fn register_active(&self, id: PersonaId, mailbox: Arc<Mailbox>) {
        let prev = self.slots.insert(
            id,
            AgentSlot::Active {
                mailbox: mailbox.clone(),
            },
        );

        // If the previous slot was Draft, drain its queue and replay
        // through the mailbox (which bumps the pending counter).
        if let Some(AgentSlot::Draft { queue }) = prev {
            let msgs: VecDeque<MailboxInput> = queue
                .into_inner()
                .expect("draft queue mutex poisoned during register_active drain");
            for msg in msgs {
                let _ = mailbox.send_input(msg);
            }
        }
    }

    /// Legacy combined registration method.
    ///
    /// Passing `SessionStatus::Draft` calls [`Self::register_draft`]; the `tx`
    /// parameter is unused but kept for API compatibility.
    /// Passing `SessionStatus::Active` calls [`Self::register_active`].
    ///
    /// Prefer the dedicated `register_draft` / `register_active` methods for
    /// clarity; this method exists to avoid churn at call sites that pre-date
    /// the single-map refactor.
    pub fn register(&self, id: PersonaId, mailbox: Arc<Mailbox>, status: SessionStatus) {
        match status {
            SessionStatus::Draft => self.register_draft(id),
            SessionStatus::Active => self.register_active(id, mailbox),
        }
    }

    /// Unregister a persona. If the persona was in `Draft` status, any
    /// pending queued messages are discarded. Any aliases pointing at this
    /// canonical id are also removed.
    ///
    /// No-op if the persona was not registered.
    ///
    /// Returns `true` if the persona was registered and has now been removed,
    /// `false` if the persona was not present.
    pub fn unregister(&self, id: &PersonaId) -> bool {
        let removed = self.slots.remove(id).is_some();
        // Drop dangling aliases pointing at this canonical.
        self.aliases.retain(|_alias, canonical| canonical != id);
        removed
    }

    /// Register an alias that resolves to a canonical persona id.
    ///
    /// Returns an error if the alias would shadow a different canonical
    /// agent already in the registry, or if the alias already resolves to
    /// a different canonical id. Self-registration (alias == canonical) is
    /// a no-op.
    ///
    /// Idempotent: registering the same `(alias, canonical)` pair twice
    /// succeeds.
    ///
    /// Note: registering an alias for a canonical that is not yet in the
    /// slot map is allowed — the canonical may be registered later. The
    /// alias is removed when its canonical is `unregister`ed.
    pub fn register_alias(
        &self,
        alias: PersonaId,
        canonical: PersonaId,
    ) -> Result<(), RouterError> {
        if alias == canonical {
            return Ok(());
        }

        // Refuse if the alias would shadow a different canonical id.
        if self.slots.contains_key(&alias) {
            return Err(RouterError::AliasCollision { alias, canonical });
        }

        // Idempotent: same target → ok. Different target → collision.
        if let Some(existing) = self.aliases.get(&alias) {
            if *existing != canonical {
                return Err(RouterError::AliasCollision { alias, canonical });
            }
            return Ok(());
        }

        self.aliases.insert(alias, canonical);
        Ok(())
    }

    /// Remove an alias entry. No-op if not present.
    pub fn unregister_alias(&self, alias: &PersonaId) -> bool {
        self.aliases.remove(alias).is_some()
    }

    /// Resolve an addressable id (canonical or alias) to its canonical
    /// counterpart. Returns the input unchanged if it's already canonical
    /// in the slot map; returns `None` if neither canonical nor a known
    /// alias.
    fn resolve_to_canonical(&self, id: &PersonaId) -> Option<PersonaId> {
        if self.slots.contains_key(id) {
            return Some(id.clone());
        }
        self.aliases.get(id).map(|r| r.clone())
    }

    /// Return a clone of the `Arc<Mailbox>` for an `Active` persona, or
    /// `None` if the persona is not registered or is in `Draft` status.
    /// Resolves through the alias map on canonical miss.
    ///
    /// Production callers route messages through [`Self::route_or_queue`]
    /// which handles `Active`/`Draft` atomically and goes through
    /// [`Mailbox::send_input`]. This method exists for tests and
    /// observability — direct enqueues should still go through
    /// `Mailbox::send_input` so the `pending` counter stays in sync.
    pub fn mailbox(&self, id: &PersonaId) -> Option<Arc<Mailbox>> {
        let canonical = self.resolve_to_canonical(id)?;
        let slot = self.slots.get(&canonical)?;
        match &*slot {
            AgentSlot::Active { mailbox } => Some(mailbox.clone()),
            AgentSlot::Draft { .. } => None,
        }
    }

    /// Current status of a persona, or `None` if not registered. Resolves
    /// through the alias map on canonical miss.
    pub fn status(&self, id: &PersonaId) -> Option<SessionStatus> {
        let canonical = self.resolve_to_canonical(id)?;
        self.slots.get(&canonical).map(|s| s.status())
    }

    /// Route a message to the correct destination, atomically.
    ///
    /// The status check and the send/queue are performed under the same
    /// DashMap shard read lock via the `Ref` returned by `get()`, closing
    /// two distinct TOCTOU races:
    ///
    /// - **Draft→Active promotion** (the original concern of the cycle-3
    ///   single-map fix): two-map design saw status as Draft, then the
    ///   promotion completed and removed the queue, then the push found
    ///   no queue and silently dropped. With the consolidated map and
    ///   the held read guard, the promoter's write lock blocks until we
    ///   release the read guard.
    ///
    /// - **Active→Active swap**: between the status read and the channel
    ///   send, a concurrent `register_active` could replace the slot
    ///   with a new session's `tx`. A previously-cloned `tx_old` would
    ///   still be valid (the old session retains the receiver), so the
    ///   message would land in the old session's mailbox — silent
    ///   misroute. Holding the read guard across `tx.send` blocks the
    ///   swap until the in-flight send completes.
    ///
    /// # Outcomes
    ///
    /// - `Active` with a live sender: delivers `msg` to the mailbox.
    /// - `Draft`: appends `msg` to the draft queue for future replay on
    ///   promotion via [`Self::register_active`].
    /// - Not registered (vacant): returns `Err(RouterError::PersonaNotFound)`.
    pub fn route_or_queue(&self, id: &PersonaId, msg: MailboxInput) -> Result<(), RouterError> {
        // Acquire the slot guard via a single `get` so the canonical-only
        // path preserves the TOCTOU guarantees described above. Only fall
        // back to the alias map when the canonical lookup misses; the
        // alias-resolved second `get` reacquires a fresh guard, but at
        // that point the caller addressed an alias that doesn't have its
        // own canonical slot, so the Active→Active swap concern doesn't
        // apply (the alias points to a single canonical, and that
        // canonical's own guard governs delivery).
        let slot = match self.slots.get(id) {
            Some(s) => s,
            None => {
                let canonical = self
                    .aliases
                    .get(id)
                    .map(|r| r.clone())
                    .ok_or_else(|| RouterError::PersonaNotFound(id.clone()))?;
                self.slots
                    .get(&canonical)
                    .ok_or_else(|| RouterError::PersonaNotFound(id.clone()))?
            }
        };

        match &*slot {
            AgentSlot::Active { mailbox } => {
                // Hold the shard read guard across the send. This closes the
                // Active→Active race (see original tx.send comment).
                // send_input is non-blocking (channel push + atomic increment).
                mailbox
                    .send_input(msg)
                    .map_err(|_| RouterError::MailboxClosed)
            }
            AgentSlot::Draft { queue } => {
                // Acquire the per-slot queue lock *while holding the entry
                // guard*. Lock order is always: shard lock (held by entry
                // guard) → queue lock. No inversion is possible because the
                // queue Mutex is only ever locked from here and from
                // `register_active`, both of which acquire the entry guard
                // first.
                queue
                    .lock()
                    .expect("draft queue mutex poisoned")
                    .push_back(msg);
                // Drop entry guard (releases shard lock) after the push so
                // the promoter cannot remove the slot between the match and
                // the push.
                drop(slot);
                Ok(())
            }
        }
    }

    /// Append a message to a draft persona's queue.
    ///
    /// Returns `Err(RouterError::PersonaNotFound)` if the persona is not
    /// registered as `Draft` — callers should prefer [`Self::route_or_queue`]
    /// which handles both `Active` and `Draft` atomically. This method is
    /// retained for callers that have explicitly checked status beforehand
    /// and need to push into a known-Draft slot.
    pub fn queue_for_draft(
        &self,
        id: &PersonaId,
        msg: Message,
        origin: MessageOrigin,
    ) -> Result<(), RouterError> {
        let slot = self
            .slots
            .get(id)
            .ok_or_else(|| RouterError::PersonaNotFound(id.clone()))?;
        match &*slot {
            AgentSlot::Draft { queue } => {
                queue
                    .lock()
                    .expect("draft queue mutex poisoned")
                    .push_back(MailboxInput::new(origin, msg));
                drop(slot);
                Ok(())
            }
            AgentSlot::Active { .. } => {
                drop(slot);
                Err(RouterError::PersonaNotFound(id.clone()))
            }
        }
    }

    /// Drain all queued messages for a persona in FIFO order (oldest first).
    ///
    /// Returns an empty `Vec` if the persona has no queue or is not in
    /// `Draft` status. This is an idempotent operation — a second call
    /// returns empty.
    ///
    /// Used by Phase 6's `PromoteDraft` RPC after opening a live session:
    /// drain the queue, then route each message through the newly-created
    /// mailbox sender. Prefer [`Self::register_active`] which performs the
    /// drain atomically as part of the promotion.
    pub fn drain_draft_queue(&self, id: &PersonaId) -> Vec<(Message, MessageOrigin)> {
        let slot = match self.slots.get(id) {
            Some(s) => s,
            None => return Vec::new(),
        };
        match &*slot {
            AgentSlot::Draft { queue } => queue
                .lock()
                .expect("draft queue mutex poisoned")
                .drain(..)
                .map(|m| (m.msg, m.from))
                .collect(),
            AgentSlot::Active { .. } => Vec::new(),
        }
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
        mailbox: Arc<Mailbox>,
    ) -> Self {
        registry.register_active(persona_id.clone(), mailbox);
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
    use crate::mailbox::{DeliveryMode, MailboxInput};

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

    // --- AC6.1 / AC6.4 / AC6.5 groundwork ---

    #[test]
    fn active_persona_sender_is_available() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("persona-a".into());
        reg.register("persona-a".into(), mailbox, SessionStatus::Active);

        assert_eq!(reg.status(&"persona-a".into()), Some(SessionStatus::Active));
        assert!(reg.mailbox(&"persona-a".into()).is_some());
    }

    #[test]
    fn draft_persona_sender_returns_none() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("draft-b".into());
        reg.register("draft-b".into(), mailbox, SessionStatus::Draft);

        // Status is Draft, not Active.
        assert_eq!(reg.status(&"draft-b".into()), Some(SessionStatus::Draft));
        // No sender for draft personas — use queue_for_draft instead.
        assert!(reg.mailbox(&"draft-b".into()).is_none());
    }

    #[test]
    fn unregistered_persona_returns_none() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.status(&"nobody".into()), None);
        assert!(reg.mailbox(&"nobody".into()).is_none());
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
        let (mailbox, _) = Mailbox::new("draft-e".into());
        reg.register("draft-c".into(), mailbox, SessionStatus::Draft);

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
        let (mailbox, _) = Mailbox::new("draft-e".into());
        reg.register("draft-d".into(), mailbox, SessionStatus::Draft);
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
        let (mailbox, _) = Mailbox::new("draft-e".into());
        reg.register("draft-e".into(), mailbox, SessionStatus::Draft);
        reg.queue_for_draft(&"draft-e".into(), test_message("pending"), test_origin())
            .unwrap();

        reg.unregister(&"draft-e".into());
        assert_eq!(reg.status(&"draft-e".into()), None);
        // After unregister, drain returns empty (slot dropped).
        let drained = reg.drain_draft_queue(&"draft-e".into());
        assert_eq!(drained.len(), 0);
    }

    #[test]
    fn register_active_replays_queued_draft_messages() {
        let reg = AgentRegistry::new();
        let (mailbox, _) = Mailbox::new("flip-f".into());

        let (mailbox2, _) = Mailbox::new("flip-f".into());
        reg.register("flip-f".into(), mailbox, SessionStatus::Draft);
        reg.queue_for_draft(&"flip-f".into(), test_message("queued"), test_origin())
            .unwrap();

        // Promote: register as Active — should drain and replay the queue.
        reg.register("flip-f".into(), mailbox2.clone(), SessionStatus::Active);
        assert_eq!(reg.status(&"flip-f".into()), Some(SessionStatus::Active));

        // The queued message must arrive on the active channel.
        let received = mailbox2
            .blocking_lock_rx()
            .try_recv()
            .expect("queued message should be replayed onto active tx on promotion");
        let text = received.msg.chat_message.content.first_text().unwrap();
        assert_eq!(text, "queued", "replayed message content must match");

        // Draft queue is now empty (messages were replayed, not left in queue).
        let drained = reg.drain_draft_queue(&"flip-f".into());
        assert_eq!(
            drained.len(),
            0,
            "draft queue should be empty after promotion replay"
        );
    }

    /// Registry guard fires unregister on drop.
    #[test]
    fn registry_guard_unregisters_on_drop() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("guard-g".into());

        let guard = RegistryGuard::register_active(reg.clone(), "guard-g".into(), mailbox);
        assert_eq!(reg.status(&"guard-g".into()), Some(SessionStatus::Active));

        drop(guard);
        assert_eq!(
            reg.status(&"guard-g".into()),
            None,
            "registry guard should unregister on drop"
        );
    }

    /// Active persona can send a live message via send_input.
    #[tokio::test]
    async fn active_sender_delivers_message() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("active-h".into());
        reg.register("active-h".into(), mailbox.clone(), SessionStatus::Active);

        let resolved = reg.mailbox(&"active-h".into()).unwrap();
        resolved
            .send_input(MailboxInput {
                from: test_origin(),
                msg: test_message("delivered"),
                delivery: DeliveryMode::Queue,
            })
            .unwrap();

        let received = mailbox.lock_rx().await.recv().await.unwrap();
        let text = received.msg.chat_message.content.first_text().unwrap();
        assert_eq!(text, "delivered");
    }

    /// route_or_queue on a draft persona queues the message in the slot.
    #[test]
    fn route_or_queue_draft_queues_message() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("draft-rq".into());
        reg.register("draft-rq".into(), mailbox, SessionStatus::Draft);

        let input = MailboxInput {
            from: test_origin(),
            msg: test_message("route-queued"),
            delivery: DeliveryMode::Queue,
        };
        reg.route_or_queue(&"draft-rq".into(), input).unwrap();

        let drained = reg.drain_draft_queue(&"draft-rq".into());
        assert_eq!(drained.len(), 1);
        let text = drained[0].0.chat_message.content.first_text().unwrap();
        assert_eq!(text, "route-queued");
    }

    /// route_or_queue on an active persona delivers directly.
    #[tokio::test]
    async fn route_or_queue_active_delivers_message() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("active-rq".into());
        reg.register("active-rq".into(), mailbox.clone(), SessionStatus::Active);

        let input = MailboxInput {
            from: test_origin(),
            msg: test_message("route-active"),
            delivery: DeliveryMode::Queue,
        };
        reg.route_or_queue(&"active-rq".into(), input).unwrap();

        let received = mailbox.lock_rx().await.recv().await.unwrap();
        let text = received.msg.chat_message.content.first_text().unwrap();
        assert_eq!(text, "route-active");
    }

    /// I-4 regression: Active→Active swap correctness across pre-swap,
    /// in-flight, and post-swap delivery.
    ///
    /// The contract `route_or_queue` defends (per its doc comment):
    /// 1. Senders that began before the swap go to `tx_old`.
    /// 2. Senders that begin after the swap commits go to `tx_new`.
    /// 3. Senders overlapping the swap go to *exactly one* mailbox
    ///    (no loss, no duplication). Under the held-guard discipline,
    ///    the swap is serialised after each in-flight `tx.send`.
    ///
    /// The earlier shape of this test asserted only "no loss in the
    /// race" plus a brittle `received_new > 0` sanity check. That was
    /// strictly weaker than the contract:
    /// - Pre/post-swap delivery were never exercised.
    /// - Silent misroute (a sender holding a stale `tx_old` reference
    ///   after the swap) was invisible because `rx_old` stayed alive
    ///   throughout — the message was counted as "delivered" even
    ///   though it landed in the wrong place.
    /// - The race-window check failed spuriously when scheduling
    ///   serialised the senders before the swap fired.
    ///
    /// This three-phase rework verifies the full contract:
    /// - **Phase A (pre-swap, deterministic):** synchronous sends. All
    ///   must land in `rx_old`, none in `rx_new`.
    /// - **Phase B (in-flight race, probabilistic-on-coverage but
    ///   deterministic-on-correctness):** N concurrent senders + one
    ///   swap task. Assert no loss.
    /// - **Phase C (post-swap, deterministic):** drop `rx_old` to
    ///   expose silent misroute, then synchronous sends. Each must
    ///   succeed (i.e. resolve to `tx_new`). A failure here means the
    ///   slot still points at the now-closed `tx_old`.
    // #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    // async fn route_or_queue_active_swap_preserves_routing() {
    //     const PRE_SENDS: usize = 50;
    //     const RACE_SENDS: usize = 4_096;
    //     const POST_SENDS: usize = 50;

    //     let reg = Arc::new(AgentRegistry::new());
    //     let (mailbox, _) = Mailbox::new("active-swap".into());

    //     reg.register("active-swap".into(), mailbox, SessionStatus::Active);

    //     // -------------------------------------------------------------
    //     // Phase A — pre-swap delivery.
    //     // -------------------------------------------------------------
    //     for i in 0..PRE_SENDS {
    //         let input = MailboxInput {
    //             from: test_origin(),
    //             msg: test_message(&format!("pre-{i}")),
    //         };
    //         reg.route_or_queue(&"active-swap".into(), input)
    //             .expect("pre-swap route_or_queue must succeed");
    //     }

    //     // -------------------------------------------------------------
    //     // Phase B — in-flight race.
    //     // -------------------------------------------------------------
    //     let mut send_tasks = Vec::with_capacity(RACE_SENDS);
    //     for i in 0..RACE_SENDS {
    //         let reg = reg.clone();
    //         send_tasks.push(tokio::spawn(async move {
    //             let input = MailboxInput {
    //                 from: test_origin(),
    //                 msg: test_message(&format!("race-{i}")),
    //             };
    //             tokio::task::yield_now().await;
    //             reg.route_or_queue(&"active-swap".into(), input)
    //         }));
    //     }
    //     let swap_task = {
    //         let reg = reg.clone();
    //         tokio::spawn(async move {
    //             for _ in 0..8 {
    //                 tokio::task::yield_now().await;
    //             }
    //             reg.register_active("active-swap".into(), tx_new);
    //         })
    //     };

    //     let mut race_send_ok = 0usize;
    //     for t in send_tasks {
    //         match t.await.expect("race send task should not panic") {
    //             Ok(()) => race_send_ok += 1,
    //             Err(e) => {
    //                 panic!("route_or_queue must not return an error in race phase: {e:?}")
    //             }
    //         }
    //     }
    //     swap_task.await.expect("swap task should not panic");

    //     // Drain both receivers post-race.
    //     let mut old_msgs: Vec<String> = Vec::new();
    //     while let Ok(m) = rx_old.try_recv() {
    //         old_msgs.push(text_of(&m));
    //     }
    //     let mut new_msgs: Vec<String> = Vec::new();
    //     while let Ok(m) = rx_new.try_recv() {
    //         new_msgs.push(text_of(&m));
    //     }

    //     // Phase A assertions: every pre-swap message must be in rx_old, none in rx_new.
    //     for i in 0..PRE_SENDS {
    //         let label = format!("pre-{i}");
    //         assert!(
    //             old_msgs.iter().any(|m| m == &label),
    //             "pre-swap message {label} must be in rx_old"
    //         );
    //         assert!(
    //             !new_msgs.iter().any(|m| m == &label),
    //             "pre-swap message {label} must NOT be in rx_new"
    //         );
    //     }

    //     // Phase B: race phase has no loss.
    //     let race_in_old = old_msgs.iter().filter(|m| m.starts_with("race-")).count();
    //     let race_in_new = new_msgs.iter().filter(|m| m.starts_with("race-")).count();
    //     assert_eq!(
    //         race_in_old + race_in_new,
    //         race_send_ok,
    //         "race phase: every successful send must land in exactly one mailbox; \
    //          race_in_old={race_in_old}, race_in_new={race_in_new}, send_ok={race_send_ok}"
    //     );

    //     // -------------------------------------------------------------
    //     // Phase C — post-swap delivery.
    //     //
    //     // Drop rx_old before sending. Any send that resolves to a stale
    //     // `tx_old` will fail with `MailboxClosed`; with the correct
    //     // implementation, the slot now points at `tx_new` and sends
    //     // succeed.
    //     // -------------------------------------------------------------
    //     drop(rx_old);

    //     for i in 0..POST_SENDS {
    //         let input = MailboxInput {
    //             from: test_origin(),
    //             msg: test_message(&format!("post-{i}")),
    //         };
    //         reg.route_or_queue(&"active-swap".into(), input).expect(
    //             "post-swap route_or_queue must resolve to tx_new and succeed; \
    //              a MailboxClosed error here means the slot is still pointed at \
    //              the closed tx_old",
    //         );
    //     }

    //     // Drain post-swap messages from rx_new and verify they all
    //     // arrived. Existing race-phase messages were drained above so
    //     // anything in rx_new now is post-* only.
    //     let mut post_msgs: Vec<String> = Vec::new();
    //     while let Ok(m) = rx_new.try_recv() {
    //         post_msgs.push(text_of(&m));
    //     }
    //     for i in 0..POST_SENDS {
    //         let label = format!("post-{i}");
    //         assert!(
    //             post_msgs.iter().any(|m| m == &label),
    //             "post-swap message {label} must be in rx_new"
    //         );
    //     }
    // }

    // -----------------------------------------------------------------------
    // Alias resolution
    // -----------------------------------------------------------------------

    /// Registering an alias and resolving via `route_or_queue` delivers the
    /// message to the canonical session's mailbox.
    #[tokio::test]
    async fn route_via_alias_delivers_to_canonical() {
        let reg = Arc::new(AgentRegistry::new());
        let (mailbox, _) = Mailbox::new("pattern-default".into());
        reg.register(
            "pattern-default".into(),
            mailbox.clone(),
            SessionStatus::Active,
        );
        reg.register_alias("pattern".into(), "pattern-default".into())
            .expect("alias registration should succeed");

        reg.route_or_queue(
            &"pattern".into(),
            MailboxInput {
                from: test_origin(),
                msg: test_message("via-alias"),
                delivery: DeliveryMode::Queue,
            },
        )
        .expect("route via alias should succeed");

        let received = mailbox.lock_rx().await.recv().await.unwrap();
        assert_eq!(
            received.msg.chat_message.content.first_text().unwrap(),
            "via-alias"
        );
    }

    /// `sender()` resolves through the alias map.
    #[test]
    fn sender_resolves_through_alias() {
        let reg = AgentRegistry::new();
        let (mailbox, _) = Mailbox::new("canonical-x".into());
        reg.register("canonical-x".into(), mailbox.clone(), SessionStatus::Active);
        reg.register_alias("alias-x".into(), "canonical-x".into())
            .unwrap();

        assert!(reg.mailbox(&"canonical-x".into()).is_some());
        assert!(
            reg.mailbox(&"alias-x".into()).is_some(),
            "alias should resolve to active sender"
        );
    }

    /// `status()` resolves through the alias map.
    #[test]
    fn status_resolves_through_alias() {
        let reg = AgentRegistry::new();
        let (mailbox, _) = Mailbox::new("canonical-y".into());
        reg.register("canonical-y".into(), mailbox.clone(), SessionStatus::Active);
        reg.register_alias("alias-y".into(), "canonical-y".into())
            .unwrap();

        assert_eq!(reg.status(&"alias-y".into()), Some(SessionStatus::Active));
    }

    /// Registering an alias that shadows a different canonical errors.
    #[test]
    fn alias_shadowing_canonical_errors() {
        let reg = AgentRegistry::new();
        let (mailbox_a, _) = Mailbox::new("foo".into());
        let (mailbox_b, _) = Mailbox::new("bar".into());
        reg.register("foo".into(), mailbox_a, SessionStatus::Active);
        reg.register("bar".into(), mailbox_b, SessionStatus::Active);

        // Cannot alias "foo" → "bar" because "foo" already names a
        // different canonical persona.
        let err = reg
            .register_alias("foo".into(), "bar".into())
            .expect_err("expected alias collision");
        assert!(matches!(err, RouterError::AliasCollision { .. }));
    }

    /// Registering the same `(alias, canonical)` pair twice is idempotent.
    #[test]
    fn alias_registration_is_idempotent() {
        let reg = AgentRegistry::new();
        let (mailbox, _) = Mailbox::new("canonical-z".into());
        reg.register("canonical-z".into(), mailbox, SessionStatus::Active);

        reg.register_alias("alias-z".into(), "canonical-z".into())
            .unwrap();
        reg.register_alias("alias-z".into(), "canonical-z".into())
            .expect("second identical registration should succeed");
    }

    /// Registering the same alias to a different canonical errors.
    #[test]
    fn conflicting_alias_targets_error() {
        let reg = AgentRegistry::new();
        let (mailbox_a, _) = Mailbox::new("first".into());
        let (mailbox_b, _) = Mailbox::new("second".into());
        reg.register("first".into(), mailbox_a, SessionStatus::Active);
        reg.register("second".into(), mailbox_b, SessionStatus::Active);
        reg.register_alias("shared".into(), "first".into()).unwrap();

        let err = reg
            .register_alias("shared".into(), "second".into())
            .expect_err("expected alias collision on different canonical target");
        assert!(matches!(err, RouterError::AliasCollision { .. }));
    }

    /// Self-aliasing (alias == canonical) is a no-op success.
    #[test]
    fn self_alias_is_noop() {
        let reg = AgentRegistry::new();
        reg.register_alias("self".into(), "self".into())
            .expect("self-alias should be a no-op success");
    }

    /// Unregistering a canonical removes its dangling aliases.
    #[test]
    fn unregister_canonical_drops_aliases() {
        let reg = AgentRegistry::new();
        let (mailbox, _) = Mailbox::new("alpha".into());
        reg.register("alpha".into(), mailbox, SessionStatus::Active);
        reg.register_alias("a".into(), "alpha".into()).unwrap();

        reg.unregister(&"alpha".into());

        assert!(
            reg.status(&"a".into()).is_none(),
            "alias should resolve to None after canonical unregistered"
        );
    }
}
