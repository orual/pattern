//! Probe test: heavy message-loss stress for the consolidated single-map AgentRegistry.
//!
//! This is the 64×500×200 heavy variant that empirically detected ~1 silent
//! loss per 6M sends with the cycle-2 two-map design. After the single-map
//! consolidation this probe must report ZERO loss across all runs.
//!
//! The probe counts both `Ok` results from senders AND messages actually
//! received from `active_rx`, then asserts `received == sent_ok`. This
//! catches the "silent loss" window that the old PersonaNotFound-only assertion
//! missed.
//!
//! Parameters: 64 senders × 500 msgs/sender × 200 iterations = 6.4M sends.
//! Promoter: yields 5 times before promoting to maximise scheduling interleaving.
//!
//! Run with:
//!   cargo nextest run -p pattern-runtime --test probe_consolidation -- --nocapture

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pattern_core::types::ids::PersonaId;
use pattern_runtime::agent_registry::{AgentRegistry, SessionStatus};
use pattern_runtime::mailbox::MailboxInput;
use tokio::sync::mpsc;

const N_SENDERS: usize = 64;
const MSGS_PER_SENDER: usize = 500;
const N_ITERATIONS: usize = 200;

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

/// Heavy probe: 64 senders × 500 msgs × 200 iterations with a 5-yield promoter.
///
/// Asserts both:
/// 1. No `PersonaNotFound` errors (original TOCTOU check).
/// 2. `delivered_count == ok_count` — no silent message loss.
///
/// The second assertion is the critical addition: the old cycle-2 race caused
/// ~1 loss per 6M sends that was invisible to CI because the test only counted
/// PersonaNotFound vs Ok. With the single-map design, zero loss is the invariant.
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn consolidation_probe_zero_loss_heavy() {
    let mut total_ok: usize = 0;
    let mut total_delivered: usize = 0;
    let mut total_not_found: usize = 0;
    let mut iters_with_loss: usize = 0;
    let mut max_loss_per_iter: usize = 0;

    for iter in 0..N_ITERATIONS {
        let agent_id: PersonaId = "promo-agent".into();
        let reg = Arc::new(AgentRegistry::new());

        // Register as Draft. The tx is unused by the draft path (single-map
        // design: Draft slots hold a queue, not the tx). We still need a tx
        // arg to keep the legacy register() signature happy.
        let (draft_tx, _draft_rx) = mpsc::unbounded_channel::<MailboxInput>();
        reg.register(agent_id.clone(), draft_tx, SessionStatus::Draft);

        let (active_tx, mut active_rx) = mpsc::unbounded_channel::<MailboxInput>();

        // Promoter: yield 5 times to maximise scheduling interleaving.
        let reg_for_promoter = reg.clone();
        let agent_id_for_promoter = agent_id.clone();
        let active_tx_for_promoter = active_tx.clone();
        let promoter = tokio::spawn(async move {
            for _ in 0..5 {
                tokio::task::yield_now().await;
            }
            reg_for_promoter.register(
                agent_id_for_promoter,
                active_tx_for_promoter,
                SessionStatus::Active,
            );
        });

        let ok_iter = Arc::new(AtomicUsize::new(0));
        let nf_iter = Arc::new(AtomicUsize::new(0));
        let mut sender_handles = Vec::with_capacity(N_SENDERS);
        for _ in 0..N_SENDERS {
            let reg_clone = reg.clone();
            let id_clone = agent_id.clone();
            let oc = ok_iter.clone();
            let nfc = nf_iter.clone();
            let handle = tokio::spawn(async move {
                for _ in 0..MSGS_PER_SENDER {
                    match reg_clone.route_or_queue(&id_clone, dummy_input()) {
                        Ok(()) => {
                            oc.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(pattern_runtime::router::RouterError::PersonaNotFound(_)) => {
                            nfc.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(pattern_runtime::router::RouterError::MailboxClosed) => {
                            // Active channel closed — not a TOCTOU bug, count as ok
                            // for the PersonaNotFound check. Messages that return
                            // MailboxClosed are NOT expected to appear in active_rx,
                            // so we do NOT count them in ok_iter.
                            //
                            // (In practice MailboxClosed should not fire here because
                            // we hold active_tx alive until after the senders complete.)
                        }
                        Err(e) => {
                            panic!("unexpected error: {e:?}");
                        }
                    }
                }
            });
            sender_handles.push(handle);
        }

        for h in sender_handles {
            h.await.expect("sender panicked");
        }
        promoter.await.expect("promoter panicked");

        // Drop our clone of active_tx and the registry so that all senders that
        // hold active_tx (from the slot after promotion) are dropped when the
        // registry is dropped. This closes the channel so active_rx.recv()
        // returns None.
        drop(active_tx);
        drop(reg);

        // Drain active_rx to count delivered messages. Use async recv() so we
        // block until the channel is closed (all senders dropped).
        let mut delivered_iter = 0usize;
        while active_rx.recv().await.is_some() {
            delivered_iter += 1;
        }

        let ok_this_iter = ok_iter.load(Ordering::Relaxed);
        let nf_this_iter = nf_iter.load(Ordering::Relaxed);
        total_ok += ok_this_iter;
        total_delivered += delivered_iter;
        total_not_found += nf_this_iter;

        if delivered_iter < ok_this_iter {
            iters_with_loss += 1;
            let loss = ok_this_iter - delivered_iter;
            if loss > max_loss_per_iter {
                max_loss_per_iter = loss;
            }
            eprintln!(
                "iter {}: ok={} delivered={} loss={} not_found={}",
                iter, ok_this_iter, delivered_iter, loss, nf_this_iter
            );
        }
    }

    eprintln!(
        "TOTALS: ok={} delivered={} loss={} not_found={} iters_with_loss={}/{} max_loss_per_iter={}",
        total_ok,
        total_delivered,
        total_ok.saturating_sub(total_delivered),
        total_not_found,
        iters_with_loss,
        N_ITERATIONS,
        max_loss_per_iter,
    );

    assert_eq!(
        total_not_found, 0,
        "TOCTOU race: {} PersonaNotFound errors (should be 0 with single-map fix)",
        total_not_found,
    );
    assert_eq!(
        total_delivered,
        total_ok,
        "silent message loss: ok={} delivered={} loss={}",
        total_ok,
        total_delivered,
        total_ok.saturating_sub(total_delivered),
    );
}
