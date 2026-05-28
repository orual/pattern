// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! CRDT-backed file sync primitives.
//!
//! Shared by the block subscriber (Subcomponent B, Tasks 6-8) and the
//! `FileHandler`'s `FileManager` coordinator (Phase 2).
//!
//! # Architecture
//!
//! Two orthogonal primitives:
//!
//! - **`DirWatcher`** — one `notify_debouncer_full::Debouncer` per root
//!   directory, one ingest thread that drains debounced events and calls
//!   `R::handle(events)`. Router trait is intentionally tiny (one method)
//!   so routing logic is injected rather than inherited.
//!
//! - **`SyncedDoc<B>`** — one `LoroDoc memory_doc` (caller-supplied) + one
//!   `LoroDoc disk_doc` (owned) + mtime/blake3 echo suppression + a
//!   subscription to external-change events for its file. Two constructors:
//!   `open_with_subscription` (receives events from an externally-owned
//!   `DirWatcher<PathFanoutRouter>`) and `open_standalone` (spawns its own
//!   single-file `DirWatcher<PathFanoutRouter>`, for tests and one-off usage).

pub mod bridge;
pub mod dir_watcher;
pub mod error;
pub mod router;
pub mod routers;
pub mod synced_doc;
pub mod text;

#[cfg(test)]
mod tests;

pub use bridge::{BridgeError, LoroDocBridge};
pub use dir_watcher::{DirWatcher, DirWatcherConfig};
pub use error::{LoroSyncError, SyncedDocError};
pub use router::EventRouter;
pub use routers::{PathFanoutRouter, PathFanoutSubscription};
pub use synced_doc::{
    ConflictPolicy, ExternalChangeEvent, SyncedDoc, SyncedDocConfig, SyncedDocConfigBuilder,
    WriteNotification,
};
pub use text::{LineIndex, LoroSyncedFile, TextBridge};
