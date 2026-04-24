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
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    use super::*;
    use crate::subscriber::event::CommitEvent;

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

    /// Test that the supervisor fires the `memory.sync_worker.restart` metric
    /// when it detects a heartbeat timeout.
    ///
    /// Strategy: inject a stale heartbeat directly into `state.last_heartbeats`
    /// with a timestamp already past `HEARTBEAT_TIMEOUT`. Then build a
    /// single-threaded tokio runtime and run the entire async body via
    /// `Runtime::block_on` inside a `metrics::with_local_recorder` sync closure.
    /// Because `block_on` executes the future on the current thread (the same
    /// thread where the recorder is installed as a thread-local), all metric
    /// emissions from the supervisor task — which runs on that same thread —
    /// are captured by the recorder. `tokio::time::pause()` is called manually
    /// at the start of the body so the clock can be fast-forwarded past
    /// `TICK_INTERVAL` without real sleeps.
    #[test]
    fn supervisor_timeout_fires_restart_metric() {
        use tokio_util::sync::CancellationToken as WorkerCancel;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();

        // Build a current-thread runtime so `block_on` keeps all async execution
        // on this thread, matching the thread-local recorder installed below.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        metrics::with_local_recorder(&recorder, || {
            rt.block_on(async {
                tokio::time::pause();

                let (_hb_tx, hb_rx) = crossbeam_channel::bounded::<Heartbeat>(64);
                let subscribers: Arc<DashMap<String, SubscriberHandle>> = Arc::new(DashMap::new());
                let cancel = CancellationToken::new();
                let state = Arc::new(SupervisorState::new());

                // Inject a stale heartbeat — already past HEARTBEAT_TIMEOUT relative
                // to the paused tokio clock. Using std::time::Instant (which tokio
                // also intercepts when the clock is paused) ensures the supervisor's
                // `Instant::now().duration_since(...)` comparison fires immediately.
                let stale_at = Instant::now()
                    .checked_sub(HEARTBEAT_TIMEOUT + Duration::from_secs(1))
                    .expect("system clock must support past-Instant subtraction");
                state
                    .last_heartbeats
                    .insert("stale-block".to_string(), stale_at);

                // Add a dummy SubscriberHandle so the supervisor can cancel and join it.
                let worker_cancel = WorkerCancel::new();
                let worker_cancel_clone = worker_cancel.clone();
                let dummy_handle = SubscriberHandle {
                    cancel: worker_cancel_clone,
                    thread: std::thread::spawn(move || {
                        while !worker_cancel.is_cancelled() {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }),
                    event_tx: {
                        let (tx, _) = crossbeam_channel::bounded::<CommitEvent>(1);
                        tx
                    },
                    _subscription: {
                        let doc = loro::LoroDoc::new();
                        doc.subscribe_local_update(Box::new(|_| true))
                    },
                    disk_doc: Arc::new(loro::LoroDoc::new()),
                    last_written_mtime: Arc::new(Mutex::new(None)),
                    paused: Arc::new(AtomicBool::new(false)),
                    pause_complete: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
                    resume_signal: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
                };
                subscribers.insert("stale-block".to_string(), dummy_handle);

                let respawn_called = Arc::new(AtomicBool::new(false));
                let respawn_called_clone = respawn_called.clone();
                let respawn_fn: Arc<dyn Fn(&str) + Send + Sync> =
                    Arc::new(move |block_id: &str| {
                        assert_eq!(block_id, "stale-block");
                        respawn_called_clone.store(true, std::sync::atomic::Ordering::Release);
                    });

                let state_clone = state.clone();
                let cancel_clone = cancel.clone();
                let subs_clone = subscribers.clone();

                let handle = tokio::spawn(async move {
                    run_supervisor(hb_rx, subs_clone, cancel_clone, state_clone, respawn_fn).await;
                });

                // Advance the tokio clock past TICK_INTERVAL so the supervisor tick fires.
                tokio::time::advance(TICK_INTERVAL + Duration::from_millis(100)).await;
                // Yield control so the spawned supervisor task can actually run.
                tokio::task::yield_now().await;
                // Give a tiny real sleep for the blocking join inside the supervisor to finish.
                tokio::time::sleep(Duration::from_millis(50)).await;

                cancel.cancel();
                handle.await.unwrap();

                assert!(
                    respawn_called.load(std::sync::atomic::Ordering::Acquire),
                    "supervisor must call respawn_fn for the timed-out block"
                );
                assert!(
                    !state.last_heartbeats.contains_key("stale-block"),
                    "supervisor must remove the timed-out entry from last_heartbeats"
                );
            });
        });

        let snapshot = snapshotter.snapshot().into_vec();
        let restart_entry = snapshot
            .iter()
            .find(|(ck, _, _, _)| ck.key().name() == "memory.sync_worker.restart");

        assert!(
            restart_entry.is_some(),
            "supervisor must emit 'memory.sync_worker.restart' counter on timeout; \
             got snapshot: {snapshot:?}"
        );
        let (_, _, _, value) = restart_entry.unwrap();
        assert_eq!(
            *value,
            DebugValue::Counter(1),
            "restart counter must be 1 after one timeout detection"
        );
    }
}
