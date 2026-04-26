//! Concurrent promotion race test for [`AgentRegistry`].
//!
//! Verifies that the TOCTOU race between `AgentRouter::route` and a
//! concurrent Draft→Active promotion via `AgentRegistry::register` does
//! not cause spurious `PersonaNotFound` errors or silent message loss.
//!
//! # What the TOCTOU race was
//!
//! The two-map design (cycle-1/cycle-2) had a race where:
//! 1. Sender observes `Draft` status via `route_or_queue`.
//! 2. Promoter fires: inserts Active entry, drains draft queue, removes queue.
//! 3. Sender tries to push to draft queue → queue gone → `PersonaNotFound`.
//!
//! The cycle-2 reorder fix narrowed but did not close the race (~1 silent
//! loss per 6M sends remained). The cycle-3 single-map consolidation closes
//! it completely: the sender's `entry()` guard holds the shard lock for the
//! full operation, so the promoter cannot swap the slot until the sender
//! releases it. Either the sender queues into the Draft slot (and the promoter
//! drains it) or the promoter has already swapped to Active (and the sender
//! sends directly). No message is ever lost.
//!
//! # Test scenario
//!
//! 500 iterations × 32 senders × 200 messages, with `yield_now()` in
//! the promoter to maximise scheduling interleaving. The test must pass
//! consistently with the fix and consistently fail without it (verified by
//! temporarily reverting `register` to the old ordering).
//!
//! Two assertions:
//! 1. `PersonaNotFound` count is zero — the classic TOCTOU check.
//! 2. `delivered_count == ok_count` — messages that returned Ok(()) must
//!    actually arrive at `active_rx`. This is the "silent loss" check that
//!    the cycle-2 probe identified as the real gap: ~1 loss per 6M sends
//!    was invisible to CI because the test only counted PersonaNotFound.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pattern_core::types::ids::PersonaId;
use pattern_runtime::agent_registry::{AgentRegistry, SessionStatus};
use pattern_runtime::mailbox::MailboxInput;
use tokio::sync::mpsc;

// Stress parameters — scaled to reliably expose the race window.
const N_SENDERS: usize = 32;
const MSGS_PER_SENDER: usize = 200;
const N_ITERATIONS: usize = 500;

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
            chat_message: genai::chat::ChatMessage::new(genai::chat::ChatRole::User, "ping"),
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

/// Verify that `route_or_queue` never loses messages during concurrent
/// Draft→Active promotion.
///
/// Two invariants checked:
/// 1. `PersonaNotFound` count == 0 (no TOCTOU error).
/// 2. `delivered_count == ok_count` (no silent message loss).
///
/// The second assertion catches the cycle-2 residual race where a sender
/// could observe Draft, the promoter could complete (drain + remove queue),
/// and the sender's push would be silently dropped. With the single-map
/// consolidation both assertions must hold strictly.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn route_or_queue_never_returns_persona_not_found_during_promotion() {
    let not_found_total = Arc::new(AtomicUsize::new(0));
    let ok_total = Arc::new(AtomicUsize::new(0));
    let mut delivered_total: usize = 0;

    for _ in 0..N_ITERATIONS {
        let agent_id: PersonaId = "promo-agent".into();
        let reg = Arc::new(AgentRegistry::new());

        // Draft registration — tx is unused by the single-map draft slot.
        let (draft_tx, _draft_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register(agent_id.clone(), draft_tx, SessionStatus::Draft);

        // Active channel for after promotion.
        let (active_tx, mut active_rx) = mpsc::unbounded_channel::<MailboxInput>();

        // Promoter: yield once to let senders observe Draft status.
        let reg_for_promoter = reg.clone();
        let agent_id_for_promoter = agent_id.clone();
        let active_tx_for_promoter = active_tx.clone();
        let promoter = tokio::spawn(async move {
            tokio::task::yield_now().await;
            reg_for_promoter.register(
                agent_id_for_promoter,
                active_tx_for_promoter,
                SessionStatus::Active,
            );
        });

        let not_found_iter = Arc::new(AtomicUsize::new(0));
        let ok_iter = Arc::new(AtomicUsize::new(0));

        let mut sender_handles = Vec::with_capacity(N_SENDERS);
        for _ in 0..N_SENDERS {
            let reg_clone = reg.clone();
            let id_clone = agent_id.clone();
            let nf = not_found_iter.clone();
            let oc = ok_iter.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..MSGS_PER_SENDER {
                    match reg_clone.route_or_queue(&id_clone, dummy_input()) {
                        Ok(()) => {
                            oc.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(pattern_runtime::router::RouterError::PersonaNotFound(_)) => {
                            nf.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(pattern_runtime::router::RouterError::MailboxClosed) => {
                            // Active channel closed — not the TOCTOU bug.
                            // Do NOT count these in ok_iter: the message was
                            // not delivered and we should not expect it in active_rx.
                        }
                        Err(e) => {
                            panic!("unexpected error from route_or_queue: {e:?}");
                        }
                    }
                }
            });
            sender_handles.push(handle);
        }

        for h in sender_handles {
            h.await.expect("sender task panicked");
        }
        promoter.await.expect("promoter task panicked");

        // Drop active_tx and registry so the channel closes and recv() returns None.
        let ok_this_iter = ok_iter.load(Ordering::Relaxed);
        drop(active_tx);
        drop(reg);

        // Count messages actually delivered to active_rx.
        let mut delivered_iter = 0usize;
        while active_rx.recv().await.is_some() {
            delivered_iter += 1;
        }

        not_found_total.fetch_add(not_found_iter.load(Ordering::Relaxed), Ordering::Relaxed);
        ok_total.fetch_add(ok_this_iter, Ordering::Relaxed);
        delivered_total += delivered_iter;
    }

    let not_found = not_found_total.load(Ordering::Relaxed);
    let ok_count = ok_total.load(Ordering::Relaxed);
    let total = N_ITERATIONS * N_SENDERS * MSGS_PER_SENDER;

    // Assertion 1: no TOCTOU PersonaNotFound errors.
    assert_eq!(
        not_found, 0,
        "TOCTOU race: {not_found} PersonaNotFound errors across {N_ITERATIONS} iterations \
         (ok={ok_count} of {total})"
    );

    // Assertion 2: every Ok(()) result must have arrived at active_rx.
    // No tolerance — with the single-map consolidation zero loss is the invariant.
    // If this assertion fails, the consolidation has a bug; do not add tolerance.
    assert_eq!(
        delivered_total, ok_count,
        "silent message loss: ok={ok_count} delivered={delivered_total} \
         loss={}",
        ok_count.saturating_sub(delivered_total),
    );
}
