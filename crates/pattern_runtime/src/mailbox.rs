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

use pattern_core::types::ids::{AgentId, PersonaId};
use pattern_core::types::message::Message;
use pattern_core::types::origin::MessageOrigin;
use pattern_core::types::turn::TurnInput;
use tokio::sync::{Mutex, mpsc};

use crate::agent_loop::{EvalDispatcher, drive_step};
use crate::memory::TurnHistory;
use crate::session::SessionContext;

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
    /// Number of messages enqueued but not yet consumed by the drain loop.
    /// Checked by `drive_step` between continuation turns to allow partner
    /// messages to interrupt long tool-call chains.
    pending: std::sync::atomic::AtomicUsize,
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
            pending: std::sync::atomic::AtomicUsize::new(0),
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

    /// Enqueue an input and bump the pending counter.
    /// Callers that bypass this (using the raw sender) must call
    /// [`note_enqueued`] themselves.
    pub fn send_input(
        &self,
        input: MailboxInput,
    ) -> Result<(), mpsc::error::SendError<MailboxInput>> {
        self.tx.send(input)?;
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Decrement the pending counter after consuming a message from
    /// the receiver. Called by the mailbox drain loop.
    pub fn note_consumed(&self) {
        self.pending
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Bump the pending counter (for callers that send via a raw sender
    /// clone rather than [`send_input`]).
    pub fn note_enqueued(&self) {
        self.pending
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Check whether there are pending messages waiting to be drained.
    /// Used by `drive_step` to break continuation loops when the partner
    /// sends a message mid-turn.
    pub fn has_pending(&self) -> bool {
        self.pending.load(std::sync::atomic::Ordering::SeqCst) > 0
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

/// Build a [`TurnInput`] from a single inbound mailbox activation.
///
/// The carrier is uniformly `(MessageOrigin, Message)` (see
/// [`MailboxInput`]). The synthesised TurnInput uses the activating
/// origin verbatim — wake events declare themselves via
/// [`pattern_core::SystemReason`] variants on `from.author` so the
/// agent can branch on activation cause.
fn build_turn_input(input: MailboxInput, ctx: &SessionContext) -> TurnInput {
    use pattern_core::types::ids::new_snowflake_id;
    let _ = ctx;
    let id = new_snowflake_id();
    TurnInput {
        turn_id: id.clone(),
        batch_id: pattern_core::types::ids::BatchId::from(id),
        origin: input.from,
        messages: vec![input.msg],
    }
}

/// Spawn the per-session mailbox-drain task on the supplied
/// [`tokio::task::JoinSet`].
///
/// The task pulls activations from the session's mailbox and calls
/// [`drive_step`] when the session is idle. It exits when:
///
/// 1. The session's [`crate::timeout::CancelState`] fires — explicit
///    shutdown signal.
/// 2. The mailbox's last sender is dropped (channel closed) — natural
///    termination when the session and all peer registry entries are
///    gone.
/// 3. The `JoinSet` is dropped — the JoinSet's `Drop` aborts every
///    task it tracks. This is the cleanup path when
///    [`crate::session::TidepoolSession`] itself is dropped.
///
/// The task watches `is_in_turn` + `turn_done` to deliver inbound
/// activations only between turns; activations that arrive while the
/// session is busy queue in the mailbox and drain on the next idle
/// edge (FIFO).
pub fn spawn_mailbox_task(
    tasks: &mut tokio::task::JoinSet<()>,
    ctx: Arc<SessionContext>,
    turn_history: Arc<std::sync::Mutex<TurnHistory>>,
    dispatcher: Arc<dyn EvalDispatcher>,
    preamble: Arc<str>,
    cache_profile: pattern_provider::compose::CacheProfile,
) {
    tasks.spawn(mailbox_task_body(
        ctx,
        turn_history,
        dispatcher,
        preamble,
        cache_profile,
    ));
}

async fn mailbox_task_body(
    ctx: Arc<SessionContext>,
    turn_history: Arc<std::sync::Mutex<TurnHistory>>,
    dispatcher: Arc<dyn EvalDispatcher>,
    preamble: Arc<str>,
    cache_profile: pattern_provider::compose::CacheProfile,
) {
    use std::sync::atomic::Ordering;

    let mailbox = ctx.mailbox().clone();
    let cancel = ctx.cancel_state();
    let agent_id = AgentId::from(ctx.agent_id());
    let _ = agent_id; // reserved for future structured-logging fields.

    loop {
        // Phase 1: park while busy. Re-arm `notified()` BEFORE
        // re-checking the busy flag so `notify_waiters()` calls that
        // happen between the load and the await don't get lost.
        loop {
            if cancel.is_cancelled() {
                return;
            }
            if !ctx.is_in_turn().load(Ordering::SeqCst) {
                break;
            }
            let notified = ctx.turn_done().notified();
            tokio::pin!(notified);
            // Re-check after arming: turn may have ended in the gap.
            if !ctx.is_in_turn().load(Ordering::SeqCst) {
                break;
            }
            let cancel_wait = cancel.wait_for_cancel();
            tokio::pin!(cancel_wait);
            tokio::select! {
                _ = notified.as_mut() => continue,
                _ = cancel_wait => return,
            }
        }

        // Phase 2: receive next input, racing against cancel.
        let input = {
            let mut rx = mailbox.lock_rx().await;
            let cancel_wait = cancel.wait_for_cancel();
            tokio::pin!(cancel_wait);
            tokio::select! {
                msg = rx.recv() => msg,
                _ = cancel_wait => return,
            }
        };
        let Some(input) = input else {
            // All senders dropped — channel closed. Natural termination.
            return;
        };
        mailbox.note_consumed();

        // Phase 3: dispatch. drive_step manages its own busy flag via
        // BusyFlagGuard; we don't set is_in_turn ourselves here.
        //
        // Run inside a child task so a panic in drive_step (or the eval
        // dispatcher) is caught by the JoinHandle rather than propagating
        // out and killing this drain loop. The BusyFlagGuard inside
        // drive_step clears is_in_turn on panic via Drop, so subsequent
        // activations are not permanently blocked.
        let turn_input = build_turn_input(input, &ctx);
        let ctx_c = ctx.clone();
        let hist_c = turn_history.clone();
        let cp = cache_profile.clone();
        let disp = dispatcher.clone();
        let pre = preamble.clone();
        let step_handle = tokio::spawn(async move {
            drive_step(turn_input, ctx_c, hist_c, cp, disp.as_ref(), &pre, None).await
        });
        match step_handle.await {
            Ok(Ok(_reply)) => {}
            Ok(Err(err)) => {
                tracing::warn!(
                    error = ?err,
                    "mailbox-triggered drive_step failed; drain loop continues"
                );
            }
            Err(join_err) if join_err.is_panic() => {
                tracing::error!(
                    "mailbox-triggered drive_step panicked; \
                     BusyFlagGuard cleared is_in_turn; drain loop continues"
                );
            }
            Err(join_err) => {
                tracing::warn!(
                    error = ?join_err,
                    "mailbox-triggered drive_step task cancelled; drain loop continues"
                );
            }
        }
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

    /// Drive the spawn/drain loop end-to-end: send a MailboxInput,
    /// observe drive_step run via a counting dispatcher, then trip
    /// cancel_state and assert the task exits.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_and_cancel_drives_one_turn_then_exits() {
        use crate::agent_loop::EvalDispatcher;
        use crate::testing::{InMemoryMemoryStore, MockProviderClient};
        use async_trait::async_trait;
        use pattern_core::traits::MemoryStore;
        use pattern_core::types::provider::{ToolCall, ToolOutcome};
        use pattern_core::types::snapshot::PersonaSnapshot;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Counting dispatcher — never called for a pure-text turn but
        // available so the EvalDispatcher type is satisfied.
        #[derive(Default)]
        struct CountDispatcher(AtomicUsize);
        #[async_trait]
        impl EvalDispatcher for CountDispatcher {
            async fn dispatch(&self, _: ToolCall, _: &str) -> ToolOutcome {
                self.0.fetch_add(1, Ordering::SeqCst);
                ToolOutcome::Error("unused".into())
            }
        }

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let provider: Arc<dyn pattern_core::ProviderClient> =
            Arc::new(MockProviderClient::with_turns(vec![
                MockProviderClient::text_turn("ack"),
            ]));
        let db = crate::testing::test_db().await;
        // Seed the FK row drive_step's persistence path needs.
        let agent_row = pattern_db::models::Agent {
            id: "agent-mbx".to_string(),
            name: "Test".to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "test".to_string(),
            config: pattern_db::Json(serde_json::json!({})),
            enabled_tools: pattern_db::Json(vec![]),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(&db.get().unwrap(), &agent_row).unwrap();

        let persona = PersonaSnapshot::new("agent-mbx", "Mailbox Test");
        let ctx = Arc::new(SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        ));

        let dispatcher: Arc<dyn EvalDispatcher> = Arc::new(CountDispatcher::default());
        let preamble: Arc<str> = Arc::from("");
        let turn_history = Arc::new(std::sync::Mutex::new(crate::memory::TurnHistory::empty()));

        let mut tasks = tokio::task::JoinSet::new();
        spawn_mailbox_task(
            &mut tasks,
            ctx.clone(),
            turn_history.clone(),
            dispatcher,
            preamble,
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        );

        // Hand the mailbox a single Message activation.
        let sender = ctx.mailbox().sender();
        let body = test_message("hello mailbox");
        sender
            .send(MailboxInput {
                from: MessageOrigin::new(
                    Author::Agent(pattern_core::types::origin::AgentAuthor {
                        agent_id: "agent-peer".into(),
                    }),
                    Sphere::Internal,
                ),
                msg: body,
            })
            .unwrap();

        // Wait for drive_step to run + complete by polling turn_history.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let len = turn_history.lock().unwrap().active_len();
            if len > 0 {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("mailbox-driven turn did not record into history within 5s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // Trip cancel — task should exit promptly.
        ctx.cancel_state().request_cancel();

        // Wait for the task to finish.
        tokio::time::timeout(std::time::Duration::from_secs(2), tasks.join_next())
            .await
            .expect("mailbox task did not exit within 2s of cancel")
            .expect("JoinSet had no task")
            .expect("task panicked");
    }

    /// Claim (c): the mailbox drain loop (`spawn_mailbox_task`) must survive
    /// a panicking turn and continue processing subsequent inputs.
    ///
    /// Uses `spawn_mailbox_task` (not bare `drive_step`) so the test exercises
    /// the production code path. A `MaybePanickingDispatcher` panics on the
    /// first dispatch call (first input triggers a tool_use) and succeeds on
    /// subsequent calls.
    ///
    /// Assertion: after the first input causes a panic, the second input is
    /// processed and recorded in `TurnHistory`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_loop_survives_panicking_turn() {
        use crate::agent_loop::EvalDispatcher;
        use crate::testing::{InMemoryMemoryStore, MockProviderClient};
        use async_trait::async_trait;
        use pattern_core::traits::MemoryStore;
        use pattern_core::types::provider::{ToolCall, ToolOutcome};
        use pattern_core::types::snapshot::PersonaSnapshot;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Panics on the first dispatch, succeeds (no-op) on subsequent calls.
        // This lets the first turn panic (triggering BusyFlagGuard cleanup)
        // while the second turn succeeds via the MockProvider text response.
        struct MaybePanickingDispatcher(AtomicUsize);
        #[async_trait]
        impl EvalDispatcher for MaybePanickingDispatcher {
            async fn dispatch(&self, _: ToolCall, _: &str) -> ToolOutcome {
                let prev = self.0.fetch_add(1, Ordering::SeqCst);
                if prev == 0 {
                    panic!("deliberate first-dispatch panic in drain_loop test");
                }
                ToolOutcome::Error("subsequent dispatch (should not happen in this test)".into())
            }
        }

        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let db = crate::testing::test_db().await;
        let agent_row = pattern_db::models::Agent {
            id: "mbx-drain-survive-agent".to_string(),
            name: "Drain Survive Test".to_string(),
            description: None,
            model_provider: "test".to_string(),
            model_name: "test-model".to_string(),
            system_prompt: "test".to_string(),
            config: pattern_db::Json(serde_json::json!({})),
            enabled_tools: pattern_db::Json(vec![]),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(&db.get().unwrap(), &agent_row).unwrap();

        // First provider response: tool_use (triggers the panicking dispatcher).
        // Second provider response: plain text (processed by the second input).
        let provider: Arc<dyn pattern_core::ProviderClient> =
            Arc::new(MockProviderClient::with_turns(vec![
                MockProviderClient::tool_use_turn(
                    "toolu_panic",
                    "code",
                    serde_json::json!({"code": "pure ()"}),
                ),
                MockProviderClient::text_turn("second turn ack"),
            ]));

        let persona = PersonaSnapshot::new("mbx-drain-survive-agent", "Drain Survive Test");
        let ctx = Arc::new(SessionContext::from_persona(
            &persona,
            store,
            provider,
            db,
            tokio::runtime::Handle::current(),
        ));

        let dispatcher: Arc<dyn EvalDispatcher> =
            Arc::new(MaybePanickingDispatcher(AtomicUsize::new(0)));
        let preamble: Arc<str> = Arc::from("");
        let turn_history = Arc::new(std::sync::Mutex::new(crate::memory::TurnHistory::empty()));

        let mut tasks = tokio::task::JoinSet::new();
        spawn_mailbox_task(
            &mut tasks,
            ctx.clone(),
            turn_history.clone(),
            dispatcher,
            preamble,
            pattern_provider::compose::CacheProfile::default_anthropic_subscriber(),
        );

        let sender = ctx.mailbox().sender();

        // Input 1: triggers the panicking turn (tool_use response → dispatcher panics).
        sender
            .send(MailboxInput {
                from: MessageOrigin::new(
                    Author::System {
                        reason: SystemReason::Timer,
                    },
                    Sphere::System,
                ),
                msg: test_message("first: triggers panic"),
            })
            .unwrap();

        // Wait for the panic to be processed and is_in_turn to clear.
        // The drain loop must survive and become idle again.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            use std::sync::atomic::Ordering;
            if !ctx.is_in_turn().load(Ordering::SeqCst) {
                // Drain loop is idle; ready for second input.
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "is_in_turn did not clear within 5s after panicking turn; \
                     drain loop may be stuck"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let history_before = turn_history.lock().unwrap().active_len();

        // Input 2: a plain message that should be processed by the surviving drain loop.
        sender
            .send(MailboxInput {
                from: MessageOrigin::new(
                    Author::System {
                        reason: SystemReason::Timer,
                    },
                    Sphere::System,
                ),
                msg: test_message("second: after panic"),
            })
            .unwrap();

        // Wait for the second turn to be recorded.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let len = turn_history.lock().unwrap().active_len();
            if len > history_before {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "drain loop did not process second input within 5s after panic; \
                     drain loop may have died from the first panic"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        ctx.cancel_state().request_cancel();
        tokio::time::timeout(std::time::Duration::from_secs(2), tasks.join_next())
            .await
            .expect("mailbox task did not exit within 2s of cancel")
            .expect("JoinSet had no task")
            .expect("drain task itself panicked (unexpected)");
    }
}
