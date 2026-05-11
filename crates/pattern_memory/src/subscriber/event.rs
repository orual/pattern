//! Event types for the per-doc sync subscriber pipeline.

use std::time::Instant;

/// A commit event pushed from a `subscribe_local_update` callback into the
/// subscriber worker's crossbeam channel. Carries the raw Loro update bytes
/// so that the worker can import them into the disk_doc without re-exporting
/// from the memory_doc.
#[derive(Debug, Clone)]
pub struct CommitEvent {
    /// Block ID of the document that was committed.
    pub block_id: String,
    /// Raw Loro update bytes from `subscribe_local_update`. Applied to
    /// `disk_doc` to keep it in sync with `memory_doc`.
    pub update_bytes: Vec<u8>,
}

/// Heartbeat sent by a subscriber worker to prove liveness. The supervisor
/// watches for heartbeat lapses exceeding its timeout (30s).
#[derive(Debug, Clone)]
pub struct Heartbeat {
    /// Block ID of the document this worker manages.
    pub block_id: String,
    /// When the heartbeat was sent.
    pub at: Instant,
}

/// Request to re-embed a document's content. Sent from the sync subscriber
/// (OS thread) to the async re-embed queue (tokio task) via
/// `tokio::sync::mpsc::UnboundedSender`.
#[derive(Debug, Clone)]
pub struct ReembedRequest {
    /// Block ID of the document to re-embed (or archival entry ID for ArchivalEntry).
    pub block_id: String,
    /// Content type — distinguishes MemoryBlock writes from ArchivalEntry inserts
    /// (and future Message / FilePassage paths). Routes to the correct vector
    /// index row at re-embed time.
    pub content_type: pattern_db::vector::ContentType,
    /// Canonical bytes of the rendered content.
    pub canonical_bytes: Vec<u8>,
    /// blake3 hash of `canonical_bytes`.
    pub content_hash: [u8; 32],
}
