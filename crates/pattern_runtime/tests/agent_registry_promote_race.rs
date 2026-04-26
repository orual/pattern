//! Concurrent promotion race test for [`AgentRegistry`].
//!
//! Verifies that the TOCTOU race between `AgentRouter::route` and a
//! concurrent Draft→Active promotion via `AgentRegistry::register` does
//! not cause spurious `PersonaNotFound` errors that silently drop messages.
//!
//! # What the TOCTOU race is
//!
//! Without the `route_or_queue` atomic helper, a two-step sequence was:
//!
//! 1. Check status → Draft
//! 2. [promotion fires: removes draft queue, updates entry to Active]
//! 3. Call `queue_for_draft` → no queue exists → `PersonaNotFound`
//!
//! With `route_or_queue`, the status check and the send/queue happen under
//! the same DashMap shard lock, so the promotion cannot interleave between
//! steps 1 and 3.
//!
//! # Test scenario
//!
//! 1. N=8 sender tasks each send 100 messages to "promo-agent".
//! 2. One promoter task registers the persona as `Draft`, waits briefly,
//!    then promotes it to `Active`.
//! 3. `route_or_queue` must not return `PersonaNotFound` for any message
//!    sent after registration — every call must either succeed (draft queue
//!    or active deliver) or fail with `MailboxClosed`.
//!
//! Note: messages queued to draft BEFORE promotion are discarded by the
//! promotion itself (that is intentional design — Phase 6's PromoteDraft
//! RPC drains the queue after opening a live session). What we verify here
//! is that the promotion window does not produce spurious `PersonaNotFound`
//! errors.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pattern_core::types::ids::PersonaId;
use pattern_runtime::agent_registry::{AgentRegistry, SessionStatus};
use pattern_runtime::mailbox::MailboxInput;
use tokio::sync::mpsc;

// Total messages we attempt (senders × messages_per_sender).
const N_SENDERS: usize = 8;
const MSGS_PER_SENDER: usize = 100;

/// Build a minimal `MailboxInput` for test purposes.
fn dummy_input() -> MailboxInput {
    use jiff::Timestamp;
    use pattern_core::types::ids::{AgentId, BatchId, MessageId, new_id, new_snowflake_id};
    use pattern_core::types::message::Message;
    use pattern_core::types::origin::{Author, MessageOrigin, Sphere, SystemReason};

    MailboxInput {
        from: MessageOrigin::new(
            Author::System {
                reason: SystemReason::Timer,
            },
            Sphere::System,
        ),
        msg: Message {
            chat_message: genai::chat::ChatMessage::new(
                genai::chat::ChatRole::User,
                "ping",
            ),
            id: MessageId::from(new_id().to_string()),
            position: new_snowflake_id(),
            owner_id: AgentId::from("sender"),
            created_at: Timestamp::now(),
            batch: BatchId::from(new_snowflake_id()),
            response_meta: None,
            block_refs: vec![],
            attachments: vec![],
        },
    }
}

/// Verify that `route_or_queue` never returns `PersonaNotFound` for a
/// persona that is registered (as Draft or Active) at the time of the call,
/// even when a concurrent Draft→Active promotion races with the senders.
///
/// The TOCTOU window exists between:
/// - `route_or_queue` observing `Draft` status, and
/// - the promotion removing the draft queue and updating the entry to `Active`.
///
/// With the atomic `route_or_queue` fix, both the status read and the
/// send/queue action hold the same DashMap shard lock, so the promoter
/// cannot interleave between them. Every call must return `Ok` (draft queue
/// or active channel) — never `Err(PersonaNotFound)`.
///
/// This test uses `tokio::test(flavor = "multi_thread", worker_threads = 4)`
/// so that sender tasks and the promoter genuinely run in parallel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn route_or_queue_never_returns_persona_not_found_during_promotion() {
    let agent_id: PersonaId = "promo-agent".into();
    let reg = Arc::new(AgentRegistry::new());

    // Draft channel: the test only needs a tx to satisfy register(); the
    // draft path uses draft_queues, not this channel.
    let (draft_tx, _draft_rx) = mpsc::unbounded_channel::<MailboxInput>();

    // Register the persona as Draft so senders see it as registered.
    reg.register(agent_id.clone(), draft_tx, SessionStatus::Draft);

    // Build the active mailbox channel for after promotion.
    let (active_tx, mut active_rx) = mpsc::unbounded_channel::<MailboxInput>();

    // Promoter task: wait briefly, then promote to Active.
    // The sleep ensures some senders start running while the persona is still
    // Draft, maximising the chance of hitting the TOCTOU window.
    let reg_for_promoter = reg.clone();
    let agent_id_for_promoter = agent_id.clone();
    let active_tx_for_promoter = active_tx.clone();
    let promoter = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        reg_for_promoter.register(
            agent_id_for_promoter,
            active_tx_for_promoter,
            SessionStatus::Active,
        );
    });

    // Track PersonaNotFound errors (the specific error the TOCTOU race causes).
    let not_found_errors = Arc::new(AtomicUsize::new(0));
    let success_count = Arc::new(AtomicUsize::new(0));

    // N sender tasks, each sending MSGS_PER_SENDER messages.
    let mut sender_handles = Vec::with_capacity(N_SENDERS);
    for _ in 0..N_SENDERS {
        let reg_clone = reg.clone();
        let id_clone = agent_id.clone();
        let not_found_clone = not_found_errors.clone();
        let success_clone = success_count.clone();
        let handle = tokio::spawn(async move {
            for _ in 0..MSGS_PER_SENDER {
                match reg_clone.route_or_queue(&id_clone, dummy_input()) {
                    Ok(()) => {
                        success_clone.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(pattern_runtime::router::RouterError::PersonaNotFound(_)) => {
                        // This is the TOCTOU bug: observed Draft, draft queue
                        // was gone, returned PersonaNotFound.
                        not_found_clone.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(pattern_runtime::router::RouterError::MailboxClosed) => {
                        // Acceptable: the active channel was closed. Counted
                        // as success-adjacent (message was accepted but
                        // channel closed) — this is not the TOCTOU race.
                        success_clone.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        panic!("unexpected error from route_or_queue: {e:?}");
                    }
                }
            }
        });
        sender_handles.push(handle);
    }

    // Wait for all senders, then the promoter.
    for h in sender_handles {
        h.await.expect("sender task panicked");
    }
    promoter.await.expect("promoter task panicked");

    drop(active_tx);

    // Drain any messages that made it to the active mailbox (not strictly
    // needed for the assertion but aids debugging).
    let mut active_count = 0usize;
    while active_rx.try_recv().is_ok() {
        active_count += 1;
    }

    let not_found = not_found_errors.load(Ordering::Relaxed);
    let succeeded = success_count.load(Ordering::Relaxed);

    assert_eq!(
        not_found,
        0,
        "TOCTOU race: {not_found} PersonaNotFound errors during promotion \
         (succeeded={succeeded}, active={active_count})"
    );

    // Sanity: every attempt must have been counted.
    assert_eq!(
        succeeded,
        N_SENDERS * MSGS_PER_SENDER,
        "expected all {total} route_or_queue calls to return Ok, got {succeeded}",
        total = N_SENDERS * MSGS_PER_SENDER,
    );
}
