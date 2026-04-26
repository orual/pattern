//! Per-doc sync subscribers and supervisor.
//!
//! Each loaded document has an associated sync subscriber — an OS thread
//! that receives commit events via a crossbeam channel, debounces them, and
//! emits the canonical file + updates FTS5 indexes + queues re-embedding.
//!
//! ## Two-doc model
//!
//! Each subscriber manages two LoroDoc instances:
//!
//! - **memory_doc**: the LoroDoc the agent writes to (lives in MemoryCache).
//! - **disk_doc**: a forked LoroDoc that mirrors the on-disk state.
//!
//! Changes flow bidirectionally via Loro's native update propagation:
//!
//! - memory_doc → disk_doc: `subscribe_local_update` on memory_doc pushes raw
//!   update bytes to the worker, which imports them into disk_doc and renders
//!   disk_doc to the canonical file on disk.
//!
//! - disk_doc → memory_doc: the watcher detects an external file edit, parses
//!   it, applies it to disk_doc (generating Loro operations), then exports
//!   disk_doc's update bytes and imports them into memory_doc. CRDT merge
//!   preserves both the agent's and the human's concurrent edits.
//!
//! The supervisor is an async tokio task that watches heartbeats from each
//! subscriber and restarts workers that fail or become unresponsive.

pub mod event;
pub mod notifier;
pub mod supervisor;
pub mod task;
pub mod worker;

pub use event::{CommitEvent, Heartbeat, ReembedRequest};
pub use notifier::{BlockChangeCallback, BlockChangeNotifier, Subscription};

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::SystemTime;

use loro::LoroDoc;
use tokio_util::sync::CancellationToken;

/// Handle to a running per-doc sync subscriber OS thread.
///
/// Stored in the [`MemoryCache`](crate::cache::MemoryCache) subscriber
/// registry. Dropping the handle does NOT automatically cancel the worker —
/// call [`cancel`](CancellationToken::cancel) and then
/// [`join`](JoinHandle::join) explicitly via [`MemoryCache::drop_doc`].
#[derive(Debug)]
pub struct SubscriberHandle {
    /// Signal to request graceful shutdown of the worker thread.
    pub cancel: CancellationToken,
    /// Join handle for the worker OS thread.
    pub thread: JoinHandle<()>,
    /// Sender side of the commit event channel, used to push events from
    /// `subscribe_local_update` callbacks into the worker.
    pub event_tx: crossbeam_channel::Sender<CommitEvent>,
    /// The loro subscription guard — dropping this unsubscribes the callback.
    /// Must outlive the worker thread.
    pub _subscription: loro::Subscription,
    /// The disk_doc that mirrors the on-disk state. Shared with the worker
    /// (via Arc in WorkerConfig) for external edit application.
    pub disk_doc: Arc<LoroDoc>,
    /// Tracks the mtime of the last file we wrote ourselves, for self-echo
    /// suppression in the watcher. Updated by the worker after each
    /// successful atomic_write.
    pub last_written_mtime: Arc<Mutex<Option<SystemTime>>>,
    /// When true, the `subscribe_local_update` callback skips `try_send` and
    /// the worker enters its pause loop. Set by `pause_subscribers`, cleared
    /// by the worker on resume.
    pub paused: Arc<AtomicBool>,
    /// Worker sets the inner bool to true and notifies when it has finished
    /// flushing and is fully parked.
    pub pause_complete: Arc<(Mutex<bool>, Condvar)>,
    /// `resume_subscribers` sets the inner bool to true and notifies to wake
    /// the parked worker.
    pub resume_signal: Arc<(Mutex<bool>, Condvar)>,
}
