// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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

pub mod bridge;
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

use tokio_util::sync::CancellationToken;

use crate::loro_sync::SyncedDoc;
use crate::subscriber::bridge::BlockSchemaBridge;

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
    /// Join handle for the worker OS thread. `None` when the cache has no
    /// storage config (mount_path/reembed_tx/heartbeat_tx unset) — in that
    /// case the loro subscription still fires observer.publish for cross-
    /// block fanout, but there's no per-block disk/FTS/embed worker.
    pub thread: Option<JoinHandle<()>>,
    /// Sender side of the commit event channel, used to push events from
    /// `subscribe_local_update` callbacks into the worker. `None` in the
    /// observer-only mode (no storage config).
    pub event_tx: Option<crossbeam_channel::Sender<CommitEvent>>,
    /// The loro subscription guard — dropping this unsubscribes the callback.
    /// Always present: the loro subscription is the always-on path that
    /// drives observer.publish for cross-block fanout, regardless of whether
    /// storage is configured.
    pub _subscription: loro::Subscription,
    /// When true, the `subscribe_local_update` callback skips `try_send` and
    /// (when a worker is present) the worker enters its pause loop. Observer
    /// publish ALSO honors paused so all cross-block fanout pauses in lockstep.
    pub paused: Arc<AtomicBool>,
    /// Worker sets the inner bool to true and notifies when it has finished
    /// flushing and is fully parked. Unused in observer-only mode.
    pub pause_complete: Arc<(Mutex<bool>, Condvar)>,
    /// `resume_subscribers` sets the inner bool to true and notifies to wake
    /// the parked worker. Unused in observer-only mode.
    pub resume_signal: Arc<(Mutex<bool>, Condvar)>,
    /// The `SyncedDoc<BlockSchemaBridge>` that owns the two-doc CRDT machinery.
    /// `None` in observer-only mode (no storage config) — there's no disk file
    /// to render to, so no SyncedDoc.
    pub synced_doc: Option<Arc<SyncedDoc<BlockSchemaBridge>>>,
}
