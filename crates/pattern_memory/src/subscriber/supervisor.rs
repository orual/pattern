//! Subscriber supervisor — async tokio task that watches heartbeats from
//! per-doc sync workers and restarts failed or unresponsive ones.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;

use crate::subscriber::SubscriberHandle;
use crate::subscriber::event::Heartbeat;

/// Default heartbeat timeout — if a worker hasn't sent a heartbeat within
/// this duration, it is considered failed and will be restarted.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the supervisor checks for heartbeat timeouts.
const TICK_INTERVAL: Duration = Duration::from_secs(5);

/// Shared state between the supervisor and the cache.
#[derive(Debug)]
pub(crate) struct SupervisorState {
    /// Last heartbeat time per block_id.
    pub last_heartbeats: DashMap<String, Instant>,
}

impl SupervisorState {
    pub(crate) fn new() -> Self {
        Self {
            last_heartbeats: DashMap::new(),
        }
    }
}

/// Run the supervisor loop. Should be spawned as a tokio task.
///
/// The supervisor:
/// 1. Drains heartbeats from the crossbeam channel (non-blocking).
/// 2. Every [`TICK_INTERVAL`], checks all known workers for heartbeat timeout.
/// 3. Workers that have timed out are cancelled, joined, and restarted via
///    `respawn_fn`.
///
/// `respawn_fn` is called with the `block_id` of any worker that timed out.
/// It is the caller's responsibility to re-spawn the subscriber; the supervisor
/// only cancels and joins the failed handle. If the respawn itself fails, the
/// function should log the error — the supervisor continues running.
pub(crate) async fn run_supervisor(
    heartbeat_rx: crossbeam_channel::Receiver<Heartbeat>,
    subscribers: Arc<DashMap<String, SubscriberHandle>>,
    cancel: CancellationToken,
    state: Arc<SupervisorState>,
    respawn_fn: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let mut tick = tokio::time::interval(TICK_INTERVAL);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("subscriber supervisor shutting down");
                break;
            }
            _ = tick.tick() => {
                // Drain heartbeats non-blockingly.
                while let Ok(hb) = heartbeat_rx.try_recv() {
                    state.last_heartbeats.insert(hb.block_id, hb.at);
                }

                // Check for timeouts.
                let now = Instant::now();
                let mut timed_out = Vec::new();
                for entry in state.last_heartbeats.iter() {
                    if now.duration_since(*entry.value()) > HEARTBEAT_TIMEOUT {
                        timed_out.push(entry.key().clone());
                    }
                }

                for block_id in &timed_out {
                    tracing::error!(
                        block_id = %block_id,
                        "subscriber heartbeat timeout; cancelling worker"
                    );
                    metrics::counter!("memory.sync_worker.restart",
                        "block_id" => block_id.clone()
                    ).increment(1);

                    // Cancel and join the failed worker.
                    if let Some((_, handle)) = subscribers.remove(block_id) {
                        handle.cancel.cancel();
                        let bid = block_id.clone();
                        tokio::task::spawn_blocking(move || {
                            if let Err(e) = handle.thread.join() {
                                tracing::warn!(
                                    block_id = %bid,
                                    "subscriber thread panicked during supervisor restart: {e:?}"
                                );
                            }
                        }).await.ok();
                    }

                    // Remove stale heartbeat entry.
                    state.last_heartbeats.remove(block_id);

                    // Re-spawn the subscriber. The respawn_fn is provided by
                    // MemoryCache and knows how to reconstruct the subscriber
                    // for this block_id. Errors are absorbed here — the
                    // supervisor must not crash if a single respawn fails.
                    let bid = block_id.clone();
                    let respawn = Arc::clone(&respawn_fn);
                    tokio::task::spawn_blocking(move || {
                        respawn(&bid);
                    }).await.ok();
                }

                // Update active worker gauge.
                metrics::gauge!("memory.sync_worker.active")
                    .set(subscribers.len() as f64);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn supervisor_tracks_heartbeats() {
        let (hb_tx, hb_rx) = crossbeam_channel::bounded(64);
        let subscribers: Arc<DashMap<String, SubscriberHandle>> = Arc::new(DashMap::new());
        let cancel = CancellationToken::new();
        let state = Arc::new(SupervisorState::new());

        // Send a heartbeat.
        hb_tx
            .send(Heartbeat {
                block_id: "block_1".to_string(),
                at: Instant::now(),
            })
            .unwrap();

        let state_clone = state.clone();
        let cancel_clone = cancel.clone();
        let subs_clone = subscribers.clone();

        let noop_respawn: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_block_id: &str| {});

        let handle = tokio::spawn(async move {
            run_supervisor(hb_rx, subs_clone, cancel_clone, state_clone, noop_respawn).await;
        });

        // Give the supervisor a tick to process the heartbeat.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Cancel and wait for shutdown.
        cancel.cancel();
        handle.await.unwrap();

        // The heartbeat should have been recorded.
        assert!(state.last_heartbeats.contains_key("block_1"));
    }
}
