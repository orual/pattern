// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Concrete `EventRouter` implementations.
//!
//! - `PathFanoutRouter`: exact-path → channel fanout, used by `SyncedDoc`
//!   when multiple files share a single `DirWatcher`.
//! - `BlockFanoutRouter`: stem → block_id lookup + `cache.apply_external_edit`,
//!   used by the mount-wide `DirWatcher` for memory-block files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::Sender;
use dashmap::DashMap;
use notify_debouncer_full::DebouncedEvent;

use crate::loro_sync::EventRouter;

/// Exact-path fanout router. Subscribers register `(path, sender)`; for
/// each debounced event, events whose `paths` contain a subscribed path
/// are forwarded to the matching sender. Events on unsubscribed paths are
/// dropped silently.
///
/// Used by `SyncedDoc::open_with_subscription` (Phase 2's FileManager):
/// one `DirWatcher<PathFanoutRouter>` per parent directory, multiple
/// `SyncedDoc` instances subscribing to exact file paths within.
#[derive(Clone, Default)]
pub struct PathFanoutRouter {
    inner: Arc<PathFanoutInner>,
}

#[derive(Default)]
struct PathFanoutInner {
    subscribers: DashMap<PathBuf, Sender<DebouncedEvent>>,
}

impl PathFanoutRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscription for `path`. Returns a guard that removes the
    /// subscription on drop. Sender is the caller's side of a crossbeam channel.
    pub fn subscribe(
        &self,
        path: PathBuf,
        sender: Sender<DebouncedEvent>,
    ) -> PathFanoutSubscription {
        self.inner.subscribers.insert(path.clone(), sender);
        PathFanoutSubscription {
            inner: Arc::clone(&self.inner),
            path,
        }
    }
}

impl EventRouter for PathFanoutRouter {
    fn handle(&mut self, events: Vec<DebouncedEvent>) {
        for debounced in events {
            for path in &debounced.event.paths {
                if let Some(sender) = self.inner.subscribers.get(path) {
                    // Build a per-subscriber event with paths filtered to
                    // only this subscription's path. notify-debouncer can
                    // coalesce multiple close-in-time writes (or a file-
                    // modify + parent-dir-modify pair) into one DebouncedEvent
                    // whose `paths` includes multiple files; if we sent the
                    // unfiltered clone, subscriber A would receive events
                    // whose paths include B's file, which is wrong.
                    let mut tailored = debounced.clone();
                    tailored.event.paths = vec![path.clone()];
                    // try_send: if a subscriber is slow, drop the event
                    // rather than block the whole router.
                    let _ = sender.try_send(tailored);
                }
            }
        }
    }
}

/// RAII guard that removes a path subscription from `PathFanoutRouter` on drop.
pub struct PathFanoutSubscription {
    inner: Arc<PathFanoutInner>,
    path: PathBuf,
}

impl Drop for PathFanoutSubscription {
    fn drop(&mut self) {
        self.inner.subscribers.remove(&self.path);
    }
}

// ---------------------------------------------------------------------------
// BlockFanoutRouter
// ---------------------------------------------------------------------------

/// Check whether a path looks like a block file we manage.
///
/// Accepts `.md`, `.kdl`, `.jsonl` files. Rejects temporary files from
/// `atomic_write` (which have extensions like `.md.tmp`).
pub(crate) fn is_block_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(ext, "md" | "kdl" | "jsonl")
}

/// Extract the block ID from a canonical block file path.
///
/// The worker writes files as `{block_id}.{ext}`. The block ID is the stem
/// (filename without extension). Returns `None` if the path has no stem.
pub(crate) fn block_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Block-level fanout router. For each debounced event, filters to
/// Modify/Create events on block paths (`.md|.kdl|.jsonl`), extracts the
/// block_id from the file stem, performs self-echo suppression via mtime
/// comparison, validates the file format, and delegates to
/// `MemoryCache::apply_external_edit`.
///
/// This is a direct port of the `ingest_loop` function from
/// `fs/watcher.rs`, restructured as an `EventRouter` implementation.
pub struct BlockFanoutRouter {
    cache: Arc<crate::cache::MemoryCache>,
}

impl BlockFanoutRouter {
    pub fn new(cache: Arc<crate::cache::MemoryCache>) -> Self {
        Self { cache }
    }
}

impl EventRouter for BlockFanoutRouter {
    fn handle(&mut self, events: Vec<DebouncedEvent>) {
        for debounced in events {
            use notify::EventKind;
            match debounced.event.kind {
                EventKind::Create(_) | EventKind::Modify(_) => {}
                _ => continue,
            }

            for path in &debounced.event.paths {
                if !is_block_path(path) {
                    continue;
                }

                let block_id = if let Some(id) = self.cache.resolve_block_id_from_path(path) {
                    id
                } else if let Some(id) = block_id_from_path(path) {
                    // Legacy fallback: flat files from before the agent-scoped layout.
                    id
                } else {
                    continue;
                };

                // Self-echo suppression via mtime comparison.
                if let Some(subscriber) = self.cache.subscriber_handle(&block_id) {
                    let file_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
                        Ok(mtime) => mtime,
                        Err(_) => continue,
                    };
                    // Observer-only subscribers have no synced_doc → no echo-
                    // suppression mtime to compare against. Fall through and
                    // process the file normally (which apply_external_edit
                    // will skip since there's no synced_doc anyway).
                    if let Some(synced_doc) = &subscriber.synced_doc
                        && let Some(last_written) = synced_doc.last_written_mtime()
                        && file_mtime == last_written
                    {
                        continue;
                    }
                }

                // Read the file content.
                let content = match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::debug!(path = ?path, error = %e, "failed to read changed file");
                        continue;
                    }
                };

                // Validate the file format before attempting a CRDT import.
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let format_ok = match ext {
                    "md" => true,
                    "kdl" => match String::from_utf8(content.clone()) {
                        Ok(text) => match crate::fs::kdl::parse_kdl(&text) {
                            Ok(_) => true,
                            Err(e) => {
                                metrics::counter!("memory.kdl.parse_failed").increment(1);
                                tracing::warn!(
                                    path = ?path, error = %e,
                                    "invalid KDL from external edit; skipping merge"
                                );
                                false
                            }
                        },
                        Err(e) => {
                            tracing::warn!(
                                path = ?path, error = %e,
                                "KDL file is not valid UTF-8"
                            );
                            false
                        }
                    },
                    "jsonl" => match String::from_utf8(content.clone()) {
                        Ok(text) => match crate::fs::jsonl::jsonl_to_log_entries(&text) {
                            Ok(_) => true,
                            Err(e) => {
                                metrics::counter!("memory.jsonl.parse_failed").increment(1);
                                tracing::warn!(
                                    path = ?path, error = %e,
                                    "invalid JSONL from external edit; skipping merge"
                                );
                                false
                            }
                        },
                        Err(e) => {
                            tracing::warn!(
                                path = ?path, error = %e,
                                "JSONL file is not valid UTF-8"
                            );
                            false
                        }
                    },
                    _ => false,
                };

                if !format_ok {
                    continue;
                }

                self.cache.apply_external_edit(&block_id, &content);
                metrics::counter!("memory.external_edit.merged").increment(1);
            }
        }
    }
}
