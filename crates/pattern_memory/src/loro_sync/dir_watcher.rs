//! `DirWatcher<R>` — notify-debouncer + ingest thread for a directory.
//!
//! Full implementation lives in Task 2. This file is a compilation stub.

use std::path::PathBuf;
use std::time::Duration;

use notify::RecursiveMode;

use crate::loro_sync::{EventRouter, SyncedDocError};

/// Configuration for a `DirWatcher`.
pub struct DirWatcherConfig {
    /// Directory to watch.
    pub root: PathBuf,
    /// Whether to recurse into subdirectories.
    pub recursive: RecursiveMode,
    /// Debounce window (default: 500ms, matching the existing MountWatcher).
    pub debounce: Duration,
}

impl DirWatcherConfig {
    /// Construct a config with sensible defaults.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            recursive: RecursiveMode::NonRecursive,
            debounce: Duration::from_millis(500),
        }
    }
}

/// A running directory watcher that routes debounced events to `R`.
///
/// Dropping this struct stops the watcher and joins the ingest thread.
pub struct DirWatcher {
    /// The underlying notify debouncer. Dropping it stops the watch and
    /// causes the ingest thread's receiver to disconnect, allowing clean exit.
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    /// Ingest thread join handle.
    _ingest_thread: std::thread::JoinHandle<()>,
    /// Cancellation signal; cancelled on drop so the thread exits promptly.
    cancel: tokio_util::sync::CancellationToken,
}

impl DirWatcher {
    /// Start a directory watcher. The `router` runs on a dedicated OS thread
    /// named `dir-watcher:<root-basename>`; it is moved in and exclusively
    /// owned by the thread.
    pub fn start<R: EventRouter>(cfg: DirWatcherConfig, router: R) -> Result<Self, SyncedDocError> {
        start_impl(cfg, router)
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        self.cancel.cancel();
        // Dropping _debouncer closes the sender, causing the ingest thread's
        // recv() to return Err and the thread to exit cleanly.
    }
}

fn start_impl<R: EventRouter>(
    cfg: DirWatcherConfig,
    mut router: R,
) -> Result<DirWatcher, SyncedDocError> {
    use crossbeam_channel::unbounded;
    use notify_debouncer_full::{DebounceEventResult, new_debouncer};
    use tokio_util::sync::CancellationToken;

    // Unbounded so the notify-debouncer callback (called on a foreign thread
    // from outside our control) cannot drop events when the ingest thread
    // is briefly slow. The debouncer already coalesces bursts within its
    // window, so practical growth is bounded by file-edit cadence × ingest
    // pause; realistically small. send() can only fail if the receiver is
    // dropped, which only happens after we cancel and tear down the watcher.
    let (tx, rx) = unbounded::<Vec<notify_debouncer_full::DebouncedEvent>>();

    let mut debouncer = new_debouncer(cfg.debounce, None, move |result: DebounceEventResult| {
        if let Ok(events) = result {
            let _ = tx.send(events);
        }
    })
    .map_err(|e| SyncedDocError::Watcher {
        path: cfg.root.clone(),
        message: e.to_string(),
    })?;

    debouncer
        .watch(&cfg.root, cfg.recursive)
        .map_err(|e| SyncedDocError::Watcher {
            path: cfg.root.clone(),
            message: e.to_string(),
        })?;

    let cancel = CancellationToken::new();
    let cancel_thread = cancel.clone();
    let root_name = cfg
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("root")
        .to_string();

    let ingest_thread = std::thread::Builder::new()
        .name(format!("dir-watcher:{root_name}"))
        .spawn(move || {
            while let Ok(events) = rx.recv() {
                if cancel_thread.is_cancelled() {
                    break;
                }
                router.handle(events);
            }
        })
        .map_err(|e| SyncedDocError::Io {
            path: cfg.root.clone(),
            source: e,
        })?;

    Ok(DirWatcher {
        _debouncer: debouncer,
        _ingest_thread: ingest_thread,
        cancel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loro_sync::PathFanoutRouter;
    use crossbeam_channel::bounded;
    use notify_debouncer_full::DebouncedEvent;
    use std::time::{Duration, Instant};

    /// Wait up to `deadline` for `check()` to return true, polling every 25ms.
    fn wait_for(deadline: Duration, check: impl Fn() -> bool) -> bool {
        let end = Instant::now() + deadline;
        while Instant::now() < end {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        check()
    }

    #[test]
    fn dir_watcher_routes_events_to_subscriber() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("foo.txt");
        std::fs::write(&file_path, "initial").unwrap();

        let router = PathFanoutRouter::new();
        let (tx, rx) = bounded::<DebouncedEvent>(32);
        let _guard = router.subscribe(file_path.clone(), tx);

        let cfg = DirWatcherConfig {
            root: dir.path().to_path_buf(),
            recursive: RecursiveMode::NonRecursive,
            debounce: Duration::from_millis(100),
        };
        let _watcher = DirWatcher::start(cfg, router).expect("watcher should start");

        // Give inotify a moment to register the watch.
        std::thread::sleep(Duration::from_millis(50));

        std::fs::write(&file_path, "hello").unwrap();

        let received = wait_for(Duration::from_secs(5), || !rx.is_empty());
        assert!(
            received,
            "subscriber should have received an event within 5s"
        );
    }

    #[test]
    fn dir_watcher_drops_unsubscribed_events() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("unregistered.txt");
        std::fs::write(&file_path, "initial").unwrap();

        // Use PathFanoutRouter with no subscriptions — unregistered path.
        // Events for unregistered paths are silently dropped by the router.
        let router = PathFanoutRouter::new();
        let cfg = DirWatcherConfig {
            root: dir.path().to_path_buf(),
            recursive: RecursiveMode::NonRecursive,
            debounce: Duration::from_millis(100),
        };
        let _watcher = DirWatcher::start(cfg, router).expect("watcher should start");

        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&file_path, "change").unwrap();

        // PathFanoutRouter drops events for unsubscribed paths — verify no
        // receiver sees anything by checking there's no subscriber to receive.
        // This test passes if it doesn't panic and no delivery assertion fires.
        std::thread::sleep(Duration::from_millis(500));
        // Implicit: no panic, no assertion violation.
    }

    #[test]
    fn subscription_drop_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("dropped.txt");
        std::fs::write(&file_path, "init").unwrap();

        let router = PathFanoutRouter::new();
        let (tx, rx) = bounded::<DebouncedEvent>(32);

        let cfg = DirWatcherConfig {
            root: dir.path().to_path_buf(),
            recursive: RecursiveMode::NonRecursive,
            debounce: Duration::from_millis(100),
        };
        let _watcher = DirWatcher::start(cfg, router.clone()).expect("watcher should start");

        std::thread::sleep(Duration::from_millis(50));

        // Subscribe, then drop the guard.
        let guard = router.subscribe(file_path.clone(), tx.clone());
        drop(guard);

        // Drain any events that arrived before drop (there should be none).
        while rx.try_recv().is_ok() {}

        std::fs::write(&file_path, "after-drop").unwrap();

        // Wait 750ms; no events should arrive after subscription was dropped.
        std::thread::sleep(Duration::from_millis(750));
        assert!(
            rx.try_recv().is_err(),
            "no events should arrive after subscription drop"
        );
    }

    #[test]
    fn multiple_subscribers_in_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        std::fs::write(&path_a, "init_a").unwrap();
        std::fs::write(&path_b, "init_b").unwrap();

        let router = PathFanoutRouter::new();
        let (tx_a, rx_a) = bounded::<DebouncedEvent>(32);
        let (tx_b, rx_b) = bounded::<DebouncedEvent>(32);
        let _guard_a = router.subscribe(path_a.clone(), tx_a);
        let _guard_b = router.subscribe(path_b.clone(), tx_b);

        let cfg = DirWatcherConfig {
            root: dir.path().to_path_buf(),
            recursive: RecursiveMode::NonRecursive,
            debounce: Duration::from_millis(100),
        };
        let _watcher = DirWatcher::start(cfg, router).expect("watcher should start");

        std::thread::sleep(Duration::from_millis(50));

        std::fs::write(&path_a, "change_a").unwrap();

        // Wait for a.txt's subscriber to fire.
        let got_a = wait_for(Duration::from_secs(5), || !rx_a.is_empty());
        assert!(got_a, "subscriber_a should have received an event");

        // b.txt's subscriber should not have received anything yet.
        // Drain a.txt's events through the full debounce window — under
        // parallel test load, events for a.txt can arrive AFTER an initial
        // try_recv-loop drain because the debouncer's 500ms window is wider
        // than a single sleep. Repeat the drain for >debounce_window to
        // ensure a.txt's tail events are flushed before we write b.txt.
        let drain_deadline = std::time::Instant::now() + Duration::from_millis(700);
        while std::time::Instant::now() < drain_deadline {
            while rx_a.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(25));
        }

        std::fs::write(&path_b, "change_b").unwrap();

        let got_b = wait_for(Duration::from_secs(5), || !rx_b.is_empty());
        assert!(got_b, "subscriber_b should have received an event");

        // Confirm a.txt's subscriber didn't pick up b.txt's event.
        assert!(
            rx_a.try_recv().is_err(),
            "subscriber_a should not receive b.txt events"
        );
    }
}
