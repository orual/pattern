//! Memory event broadcast — cross-block observer fanout for sync clients.
//!
//! Sibling to per-block subscribe_local_update → CommitEvent crossbeam channels
//! (which exclusively drive persistence). This broadcast is for OBSERVERS
//! that need cross-block visibility into raw loro update bytes + origin tags,
//! without entangling them in the per-block persistence flow.
//!
//! ## Drop semantics
//!
//! `tokio::sync::broadcast` is a bounded ring buffer. A receiver that lags more
//! than `capacity` events behind gets `Err(RecvError::Lagged(skipped))` and its
//! position fast-forwards to the oldest still-buffered event. This is
//! **acceptable for observers** because they can self-heal (re-sync with their
//! current version vector) on lag; it is **not** acceptable for persistence,
//! which is why persistence stays on the per-block crossbeam channels.
//!
//! ## Provenance
//!
//! Each event carries an optional [`OriginTag`]. `None` means "local agent
//! edit" (or any host-originated change); `Some` identifies a plugin instance
//! that pushed the change in. Observers filter out their own pushes by
//! checking origin, preventing echo storms.

use smol_str::SmolStr;
use tokio::sync::broadcast;

use crate::types::memory_types::{BlockAddr, BlockMetadata};

/// Identifier for the source of a memory event.
///
/// `plugin_id` alone is insufficient: a single plugin can have multiple
/// instances active simultaneously (separate sessions, separate processes
/// connecting to the same daemon). `connection_id` disambiguates per-session
/// so that sibling instances of the same plugin don't filter out each other's
/// changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OriginTag {
    pub plugin_id: SmolStr,
    pub connection_id: SmolStr,
}

/// A memory-system event published on the [`MemoryObserver`] broadcast.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MemoryEvent {
    /// A loro doc changed (locally edited, or an external delta was imported).
    /// `update_bytes` is the raw loro update payload — the same bytes that
    /// `LoroDoc::subscribe_local_update` produced for local edits, or that
    /// arrived via wire for plugin-pushed deltas.
    Delta {
        addr: BlockAddr,
        update_bytes: Vec<u8>,
        /// `None` = local / host-originated. `Some` = plugin-pushed.
        origin: Option<OriginTag>,
    },
    /// A new block was created (after persist + cache insert). Carries the
    /// initial snapshot bytes so observers can seed their local cache without
    /// a separate fetch.
    BlockAvailable {
        addr: BlockAddr,
        metadata: BlockMetadata,
        snapshot: Vec<u8>,
        origin: Option<OriginTag>,
    },
    /// A block's metadata changed (pinned / type / schema / description /
    /// char_limit). Metadata lives outside the loro CRDT, so Delta events
    /// don't carry it; this is the distinct signal.
    MetadataChanged {
        addr: BlockAddr,
        metadata: BlockMetadata,
        origin: Option<OriginTag>,
    },
    /// A block was deleted, or removed from an observer's filter scope.
    BlockGone {
        addr: BlockAddr,
        reason: BlockGoneReason,
        origin: Option<OriginTag>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockGoneReason {
    /// Block was deleted from the store.
    Deleted,
    /// Block no longer matches the observer's filter scope (for filter-shape
    /// subscriptions where the watched set is policy-defined rather than
    /// explicit-addr).
    OutOfScope,
}

/// Fanout primitive owned by concrete [`crate::traits::memory_store::MemoryStore`]
/// implementations that support cross-block observation. Cheaply cloneable;
/// internally an [`Arc`]'d broadcast Sender.
#[derive(Clone)]
pub struct MemoryObserver {
    tx: broadcast::Sender<MemoryEvent>,
}

impl MemoryObserver {
    /// Construct with default capacity (1024 events).
    pub fn new() -> Self {
        Self::with_capacity(1024)
    }

    /// Construct with a custom ring-buffer capacity. Receivers more than
    /// `capacity` events behind the latest publish get
    /// `Err(RecvError::Lagged(skipped))` and fast-forward.
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event. Returns the count of active receivers that
    /// received it (may be zero — a broadcast with no live receivers is a
    /// no-op, not an error).
    pub fn publish(&self, event: MemoryEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    /// Subscribe a fresh receiver. Each `subscribe()` call returns a new
    /// receiver positioned at the most-recent event.
    pub fn subscribe(&self) -> broadcast::Receiver<MemoryEvent> {
        self.tx.subscribe()
    }

    /// Current number of active receivers. For tests + observability.
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for MemoryObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MemoryObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryObserver")
            .field("receiver_count", &self.tx.receiver_count())
            .finish_non_exhaustive()
    }
}
