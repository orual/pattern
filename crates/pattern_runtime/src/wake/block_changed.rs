//! Evaluator for [`super::WakeCondition::BlockChanged`].
//!
//! Subscribes a callback to
//! [`pattern_memory::subscriber::BlockChangeNotifier`]; the
//! subscriber worker fires the callback after each successful render
//! (real content change, not a self-echo). The callback pushes a
//! `MailboxInput::Wake { reason: BlockChanged { block } }` onto the
//! agent's mailbox.
//!
//! The "evaluator task" here is a tokio task that holds the
//! [`pattern_memory::subscriber::Subscription`] guard and parks
//! forever on `pending()`. Aborting the task drops the guard, which
//! unsubscribes the callback. This mirrors how the timer-based
//! evaluators (`rust_primitives::spawn_*`) keep their lifetime tied
//! to a `JoinHandle<()>` that the registry stores.

use std::sync::Arc;

use pattern_core::types::block_ref::BlockRef;
use pattern_core::types::origin::SystemReason;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::mailbox::MailboxInput;
use crate::wake::registry::wake_mailbox_input;

/// Spawn the evaluator task for a [`super::WakeCondition::BlockChanged`].
///
/// `block` identifies what to watch — the subscription is keyed by
/// `block.block_id` (matches what
/// [`pattern_memory::subscriber::worker::WorkerConfig::block_id`]
/// passes to the notifier on fire).
///
/// `tokio_handle` is required because this function may be called from
/// the eval-worker OS thread, which has no ambient tokio runtime.
pub(super) fn spawn_block_changed(
    block: BlockRef,
    notifier: pattern_memory::subscriber::BlockChangeNotifier,
    mailbox_tx: mpsc::UnboundedSender<MailboxInput>,
    tokio_handle: &Handle,
) -> JoinHandle<()> {
    let block_for_callback = block.clone();
    let mailbox_tx_inner = mailbox_tx.clone();
    let callback: pattern_memory::subscriber::BlockChangeCallback = Arc::new(move |bref| {
        let body = format!("wake: block changed — {}", bref.label);
        let input = wake_mailbox_input(
            SystemReason::BlockChanged {
                block: block_for_callback.clone(),
            },
            &body,
        );
        // Send failure means the mailbox is gone — nothing to do.
        let _ = mailbox_tx_inner.send(input);
    });

    // Subscribe synchronously BEFORE spawning so the callback is
    // registered as soon as `spawn_block_changed` returns. If we
    // moved the subscribe call into the async block, callers couldn't
    // assume the subscription is live until the task body had a
    // chance to run on the executor — racy under heavy load and
    // unintuitive for tests.
    let subscription = notifier.subscribe(&block.block_id, callback);

    tokio_handle.spawn(async move {
        // Hold the subscription guard for the task's lifetime. Drop
        // on abort unsubscribes the callback from the notifier.
        let _subscription = subscription;
        // Park forever; the registry aborts this task on
        // `unregister` or registry drop.
        std::future::pending::<()>().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pattern_core::types::origin::Author;
    use std::time::Duration;

    fn br(label: &str, block_id: &str) -> BlockRef {
        BlockRef::new(label, block_id)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn block_change_fires_wake() {
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let block = br("notes", "block-notes");

        let handle = spawn_block_changed(
            block.clone(),
            notifier.clone(),
            tx,
            &tokio::runtime::Handle::current(),
        );

        // Yield so the task subscribes before we fire.
        tokio::task::yield_now().await;
        // Fire the notifier as the worker would after a render.
        notifier.fire(&block.block_id, &block);

        let input = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("wake should fire within 1s")
            .expect("mailbox channel open");
        match input.from.author {
            Author::System {
                reason: SystemReason::BlockChanged { block: b },
            } => assert_eq!(b.block_id, "block-notes"),
            other => panic!("expected BlockChanged, got {other:?}"),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn abort_unsubscribes_callback() {
        let notifier = pattern_memory::subscriber::BlockChangeNotifier::new();
        let (tx, mut rx) = mpsc::unbounded_channel();
        // Keep an extra sender alive so the channel doesn't close
        // when the abort drops the callback's clone — we want to
        // distinguish "subscription gone, no further fires" from
        // "channel closed, recv returns None".
        let _keepalive = tx.clone();
        let block = br("notes", "block-notes");

        let handle = spawn_block_changed(
            block.clone(),
            notifier.clone(),
            tx,
            &tokio::runtime::Handle::current(),
        );
        tokio::task::yield_now().await;
        assert_eq!(notifier.subscriber_count(&block.block_id), 1);

        handle.abort();
        // Give the abort a moment to land; the subscription guard
        // drops as part of task cleanup.
        for _ in 0..20 {
            if notifier.subscriber_count(&block.block_id) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            notifier.subscriber_count(&block.block_id),
            0,
            "abort should drop the Subscription guard"
        );

        // No further fires should reach the mailbox.
        notifier.fire(&block.block_id, &block);
        let result = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
        assert!(
            result.is_err(),
            "no wake should fire after abort; got {result:?}"
        );
    }
}
