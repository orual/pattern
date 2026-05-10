//! Per-doc sync worker running on an OS thread.
//!
//! The worker loop receives [`CommitEvent`]s via a bounded crossbeam channel,
//! debounces them, and then:
//! 1. Imports the update bytes into the disk_doc.
//! 2. Renders the disk_doc to its canonical format (md/kdl/jsonl).
//! 3. Atomically writes the file to disk.
//! 4. Records the mtime for self-echo suppression.
//! 5. Updates the FTS5 row via `update_block_preview`.
//! 6. Queues a re-embed request if the content hash changed.
//! 7. Sends a heartbeat to the supervisor.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use loro::LoroDoc;
use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::BlockSchema;
use pattern_db::ConstellationDb;
use tokio_util::sync::CancellationToken;

use crate::fs::kdl::TopShape;
use crate::loro_sync::{SyncedDoc, WriteNotification};
use crate::subscriber::bridge::BlockSchemaBridge;
use crate::subscriber::event::{CommitEvent, Heartbeat, ReembedRequest};

/// Derive the file extension and serialized bytes for a document based on its
/// schema. Returns `(extension, canonical_bytes)`.
///
/// - `Text` → `.md` via passthrough markdown serialization.
/// - `Map` / `List` / `Composite` → `.kdl` via KDL serialization of the
///   document's deep value.
/// - `Log` → `.jsonl` via newline-delimited JSON serialization.
///
/// When rendering the disk_doc, pass the LoroDoc directly rather than a
/// StructuredDocument; the disk_doc is not wrapped in one.
///
/// On serialization failure the function returns a human-readable error string
/// and the caller should log and skip the emission cycle rather than panic.
pub(crate) fn render_canonical_from_disk_doc(
    disk_doc: &LoroDoc,
    schema: &BlockSchema,
) -> Result<(&'static str, Vec<u8>), String> {
    match schema {
        BlockSchema::Text { .. } => {
            let text = disk_doc.get_text("content").to_string();
            let bytes = crate::fs::markdown::text_to_markdown(&text).into_bytes();
            Ok(("md", bytes))
        }
        BlockSchema::Map { .. } | BlockSchema::Composite { .. } => {
            // Map and Composite blocks store their content in a top-level
            // LoroMap container ("fields" for Map, sections for Composite).
            // get_deep_value() returns Map { container_name: Map { ... } },
            // which is already a LoroValue::Map suitable for TopShape::Map.
            let deep_value = disk_doc.get_deep_value();
            let kdl_doc = crate::fs::kdl::loro_value_to_kdl(&deep_value, TopShape::Map)
                .map_err(|e| format!("KDL serialization failed: {e}"))?;
            Ok(("kdl", kdl_doc.to_string().into_bytes()))
        }
        BlockSchema::List { .. } => {
            // List blocks store their content in a LoroList container named
            // "items". get_deep_value() returns Map { "items": List([...]) },
            // so we must extract the list value before KDL serialization —
            // TopShape::List expects a LoroValue::List at the root.
            let deep_value = disk_doc.get_deep_value();
            let list_value = if let loro::LoroValue::Map(map) = &deep_value {
                map.get("items")
                    .cloned()
                    .unwrap_or(loro::LoroValue::List(vec![].into()))
            } else {
                loro::LoroValue::List(vec![].into())
            };
            let kdl_doc = crate::fs::kdl::loro_value_to_kdl(&list_value, TopShape::List)
                .map_err(|e| format!("KDL serialization failed: {e}"))?;
            Ok(("kdl", kdl_doc.to_string().into_bytes()))
        }
        BlockSchema::Log { .. } => {
            // Log blocks store entries in a LoroList container named "entries".
            // get_deep_value() returns Map { "entries": List([...]) } where each
            // element may be either:
            //   - LoroValue::Map: written by StructuredDocument::append_log_entry
            //     (json_to_loro converts JSON objects to LoroValue::Map).
            //   - LoroValue::String: written by apply_json_to_loro_doc's external
            //     edit path (serializes entries as JSON strings before storing).
            // Both variants are normalized to serde_json::Value for JSONL output.
            let deep_value = disk_doc.get_deep_value();
            let entries = if let loro::LoroValue::Map(map) = &deep_value {
                if let Some(loro::LoroValue::List(entries)) = map.get("entries") {
                    entries
                        .iter()
                        .filter_map(|v| match v {
                            loro::LoroValue::String(s) => {
                                // External-edit path: stored as serialized JSON string.
                                serde_json::from_str::<serde_json::Value>(s.as_ref()).ok()
                            }
                            other => {
                                // Internal write path: stored as LoroValue (Map, Bool,
                                // I64, Double, etc.) via json_to_loro conversion.
                                crate::fs::kdl::loro_value_to_json(other)
                            }
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            let mut output = String::new();
            for entry in &entries {
                output.push_str(&serde_json::to_string(entry).unwrap_or_default());
                output.push('\n');
            }
            Ok(("jsonl", output.into_bytes()))
        }
        BlockSchema::TaskList { .. } => {
            // TaskList blocks use a LoroMovableList named "items".
            // Build a discriminator map and delegate to the TaskList KDL converter.
            let deep_value = disk_doc.get_deep_value();
            let kdl_doc = crate::fs::kdl::loro_value_to_kdl(&deep_value, TopShape::TaskList)
                .map_err(|e| format!("KDL serialization failed: {e}"))?;
            Ok(("kdl", kdl_doc.to_string().into_bytes()))
        }
        BlockSchema::Skill { .. } => {
            // Skill blocks serialize to YAML-frontmatter + markdown body.
            // The disk_doc stores three root-level containers (populated by
            // the inbound path via `write_skill_to_loro_doc`):
            //   "metadata" — LoroMap with JSON-string-encoded typed fields.
            //   "extras"   — LoroMap with JSON-string-encoded unknown keys.
            //   "body"     — LoroText with the raw markdown body.
            // `get_deep_value()` materializes all live containers into
            // LoroValue snapshots; the loro_bridge helpers project them back.
            let deep_value = disk_doc.get_deep_value();
            let root_map = match &deep_value {
                loro::LoroValue::Map(m) => m,
                _ => {
                    return Err("Skill disk_doc get_deep_value() returned non-map root".to_string());
                }
            };

            // Project metadata fields from the "metadata" sub-map.
            let metadata = crate::fs::markdown_skill::project_metadata_from_loro(root_map)
                .map_err(|e| format!("Skill metadata projection failed: {e}"))?;

            // Project extras (unknown frontmatter keys) from the "extras" sub-map.
            let extras = crate::fs::markdown_skill::project_extras_from_loro(root_map)
                .map_err(|e| format!("Skill extras projection failed: {e}"))?;

            // Body text from the LoroText container. Try "content" first
            // (unified container for all blocks), fall back to "body" for
            // legacy skill blocks.
            let body = match root_map.get("content") {
                Some(loro::LoroValue::String(s)) if !s.is_empty() => s.as_ref().to_string(),
                _ => match root_map.get("body") {
                    Some(loro::LoroValue::String(s)) => s.as_ref().to_string(),
                    _ => String::new(),
                },
            };

            let rendered = crate::fs::markdown_skill::emit(&metadata, &extras, &body)
                .map_err(|e| format!("Skill emit failed: {e}"))?;

            Ok(("md", rendered.into_bytes()))
        }
        // NOTE: `_ =>` covers future non_exhaustive additions beyond the variants
        // currently known. All currently-defined BlockSchema variants must have
        // explicit arms above this catch-all. If a new variant is added to
        // BlockSchema without a corresponding arm here, this branch will silently
        // return an error at runtime rather than failing at compile time. Keep
        // this list current.
        _ => Err(format!(
            "unsupported schema for canonical rendering: {schema:?}"
        )),
    }
}

/// Configuration for a sync subscriber worker.
pub(crate) struct WorkerConfig {
    /// Block ID of the document this worker manages.
    pub block_id: String,
    /// Schema of the block — determines the output format and file extension.
    pub schema: BlockSchema,
    /// Receiver for commit events from `subscribe_local_update` callbacks.
    pub rx: Receiver<CommitEvent>,
    /// Cancellation token — checked each iteration.
    pub cancel: CancellationToken,
    /// Constellation database for FTS5 updates.
    pub db: Arc<ConstellationDb>,
    /// Sender for re-embed requests (async side).
    pub reembed_tx: tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
    /// Sender for heartbeats to the supervisor.
    pub heartbeat_tx: crossbeam_channel::Sender<Heartbeat>,
    /// Base path for canonical file output. Used to construct the block file
    /// path for FTS/reembed side-effects; the actual disk write is via
    /// `synced_doc.write_rendered`.
    pub mount_path: Arc<PathBuf>,
    /// The StructuredDocument (memory_doc), used only for FTS preview
    /// rendering (which needs the human-readable representation) and
    /// pause/resume VV reconciliation.
    pub doc: StructuredDocument,
    /// Shared pause flag — when true, the worker enters its pause loop.
    pub paused: Arc<AtomicBool>,
    /// Worker signals pause completion here (sets bool to true, notifies).
    pub pause_complete: Arc<(Mutex<bool>, Condvar)>,
    /// Worker waits on this for the resume signal from `resume_subscribers`.
    pub resume_signal: Arc<(Mutex<bool>, Condvar)>,
    /// Fan-out for block-change callbacks. Fired after each
    /// successful render (real data change, not a self-echo). Cheap
    /// callback shape — typically a channel send to a tokio task.
    pub block_change_notifier: crate::subscriber::notifier::BlockChangeNotifier,
    /// Owns disk_doc, last_written_mtime/hash (echo suppression), atomic_write,
    /// and last_saved_frontier. The worker imports Loro update bytes into
    /// `synced_doc.doc()`, renders via `render_canonical_from_disk_doc`,
    /// then calls `synced_doc.write_rendered(bytes)` to write and record state.
    /// External edits arrive via `synced_doc.apply_external_bytes`.
    pub synced_doc: Arc<SyncedDoc<BlockSchemaBridge>>,
}

/// Debounce window: accumulate events for this long before acting.
const DEBOUNCE_MS: u64 = 50;

/// Run the sync subscriber worker loop. This function is meant to be called
/// on an OS thread via `std::thread::spawn`.
///
/// The worker exits when:
/// - The cancellation token is cancelled.
/// - The event channel's sender side is dropped.
pub(crate) fn run_subscriber(config: WorkerConfig) {
    let WorkerConfig {
        block_id,
        schema,
        rx,
        cancel,
        db,
        reembed_tx,
        heartbeat_tx,
        mount_path: _mount_path,
        doc,
        paused,
        pause_complete,
        resume_signal,
        block_change_notifier,
        synced_doc,
    } = config;

    // Single-doc world: synced_doc.doc() IS the source of truth. The worker
    // listens to write notifications for FTS5/reembed coalescing; the actual
    // disk persistence happens inside synced_doc.write_local() called by the
    // agent's handler.
    let disk_doc = synced_doc.doc();

    // Subscribe to write notifications from synced_doc so the worker can
    // trigger FTS5 + re-embed after each disk write (both local and external
    // edit paths). The channel is bounded (64); slow consumers drop events
    // rather than blocking the ingest thread.
    let write_rx = synced_doc.subscribe_writes();

    let mut last_emitted_hash: Option<[u8; 32]> = None;

    // Initial render. `disk_doc` is forked from `memory_doc` at SyncedDoc
    // open time, so it carries any content the agent imported BEFORE the
    // subscriber was spawned (e.g. persona-seeded blocks set up via
    // `create_block` + `import_from_json` + `persist_block`). The
    // `subscribe_local_update` callback only fires for FUTURE updates, so
    // without this call the seed content would never be rendered to disk
    // until the agent edits the block.
    //
    // Gated on `disk_doc.oplog_vv()` having entries. An empty disk_doc
    // (no ops applied) has nothing to render — skipping avoids creating
    // empty files on disk for blocks that will receive content via the
    // normal CommitEvent path immediately after spawn (the test fixtures
    // for the worker exercise that path).
    if !disk_doc.oplog_vv().is_empty() {
        render_cycle(
            &block_id,
            &schema,
            disk_doc,
            &doc,
            &synced_doc,
            &db,
            &reembed_tx,
            &heartbeat_tx,
            &mut last_emitted_hash,
        );
    }

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Check if we've been asked to pause (flush-pause-resume for quiesce).
        if paused.load(Ordering::Acquire) {
            handle_pause(
                &block_id,
                &schema,
                &rx,
                disk_doc,
                &doc,
                &synced_doc,
                &db,
                &reembed_tx,
                &heartbeat_tx,
                &paused,
                &pause_complete,
                &resume_signal,
                &cancel,
                &mut last_emitted_hash,
            );
            // Drain write notifications accumulated during pause. These are
            // safe to discard: handle_pause already ran render_cycle (which
            // includes FTS + reembed) for any writes that happened during the
            // flush phase, and the subscriber loop will reconcile further changes
            // on resume via version-vector diff.
            while write_rx.try_recv().is_ok() {}
            // After resume, continue the normal loop.
            continue;
        }

        // Block waiting for a commit event, a write notification, or timeout.
        //
        // `write_rx` wakes this loop when synced_doc.write_rendered fires —
        // i.e., after any disk write, including writes from paths outside the
        // normal CommitEvent → render_cycle flow (e.g., direct calls to
        // write_rendered from a future coordinator). For writes that arrive via
        // CommitEvent, render_cycle has already run FTS + reembed; the
        // write_rx arm re-enters render_cycle, which detects the unchanged hash
        // via `last_emitted_hash` and returns early (no duplicate work).
        let first_event: Option<CommitEvent>;
        crossbeam_channel::select! {
            recv(rx) -> msg => {
                match msg {
                    Ok(ev) => { first_event = Some(ev); }
                    Err(_) => break, // Sender dropped — unload in progress.
                }
            }
            recv(write_rx) -> notification => {
                match notification {
                    Ok(WriteNotification { content_hash }) => {
                        // A disk write occurred outside the CommitEvent path.
                        // If the hash is new, run the full FTS+reembed cycle.
                        // render_cycle checks last_emitted_hash and returns
                        // early if this write was already processed.
                        if Some(content_hash) != last_emitted_hash {
                            render_cycle(
                                &block_id,
                                &schema,
                                disk_doc,
                                &doc,
                                &synced_doc,
                                &db,
                                &reembed_tx,
                                &heartbeat_tx,
                                &mut last_emitted_hash,
                            );
                        }
                        continue;
                    }
                    Err(_) => {
                        // write_rx disconnected — synced_doc dropped. Exit cleanly.
                        break;
                    }
                }
            }
            default(Duration::from_millis(DEBOUNCE_MS)) => {
                // No event within debounce window — send heartbeat and loop.
                let _ = heartbeat_tx.try_send(Heartbeat {
                    block_id: block_id.clone(),
                    at: Instant::now(),
                });
                continue;
            }
        }

        // Drain any further events that arrived within the debounce window.
        // Import all update bytes into disk_doc as they arrive.
        let deadline = Instant::now() + Duration::from_millis(DEBOUNCE_MS);
        let mut got_event = first_event.is_some();

        // Import the first event's bytes into disk_doc (owned by synced_doc).
        if let Some(ref event) = first_event
            && let Err(e) = disk_doc.import(&event.update_bytes)
        {
            tracing::warn!(
                block_id = %block_id, error = %e,
                "failed to import update bytes into disk_doc"
            );
        }

        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(ev) => {
                    got_event = true;
                    if let Err(e) = disk_doc.import(&ev.update_bytes) {
                        tracing::warn!(
                            block_id = %block_id, error = %e,
                            "failed to import update bytes into disk_doc"
                        );
                    }
                }
                Err(_) => break,
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        if !got_event {
            continue;
        }

        // Render canonical bytes, write to disk via synced_doc.write_rendered,
        // update FTS, reconcile schema-specific tables (TaskList → tasks/task_edges),
        // re-embed, heartbeat. Centralised in render_cycle so every event path —
        // normal loop, quiesce pause-flush, and post-resume — runs the same
        // code; no path can silently skip the TaskList reconcile.
        let render_changed = render_cycle(
            &block_id,
            &schema,
            disk_doc,
            &doc,
            &synced_doc,
            &db,
            &reembed_tx,
            &heartbeat_tx,
            &mut last_emitted_hash,
        );

        // Phase 4 T8: fire BlockChanged notifier callbacks AFTER a
        // successful render (real data change, not a self-echo).
        // Callbacks are cheap (typically a channel send to a wake
        // evaluator); they run on this worker thread so they must
        // not block.
        if render_changed {
            let block_ref = pattern_core::types::block_ref::BlockRef::new(
                doc.metadata().label.as_str(),
                &block_id,
            );
            block_change_notifier.fire(&block_id, &block_ref);
        }
    }
}

/// Execute one full render cycle from disk_doc to disk: render canonical bytes,
/// check hash, write via `synced_doc.write_rendered` (which handles atomic_write,
/// echo suppression state, and `last_saved_frontier`), then update FTS,
/// reconcile schema-specific tables (TaskList → tasks/task_edges), re-embed,
/// heartbeat.
///
/// Centralised here so every event path — normal loop, quiesce pause-flush,
/// and post-resume — runs the same code and cannot silently skip the TaskList
/// reconcile. Calling `render_cycle` is the single place that advances
/// persistent state after a content change.
///
/// Returns `true` when the rendered canonical bytes differ from the
/// previously-emitted hash (a real content change), `false` when the
/// hash matches (self-echo / no-op). Callers use the return to gate
/// downstream notifications (Phase 4 T8 BlockChanged fan-out) on
/// real changes only.
#[allow(clippy::too_many_arguments)]
fn render_cycle(
    block_id: &str,
    schema: &BlockSchema,
    disk_doc: &LoroDoc,
    doc: &StructuredDocument,
    synced_doc: &SyncedDoc<BlockSchemaBridge>,
    db: &ConstellationDb,
    reembed_tx: &tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
    heartbeat_tx: &crossbeam_channel::Sender<Heartbeat>,
    last_emitted_hash: &mut Option<[u8; 32]>,
) -> bool {
    let (ext, canonical_bytes) = match render_canonical_from_disk_doc(disk_doc, schema) {
        Ok(pair) => pair,
        Err(e) => {
            metrics::counter!("memory.subscriber.render_failed").increment(1);
            tracing::error!(
                block_id = %block_id, error = %e,
                "canonical render failed during render_cycle"
            );
            return false;
        }
    };
    let new_hash: [u8; 32] = blake3::hash(&canonical_bytes).into();

    if Some(new_hash) == *last_emitted_hash {
        let _ = heartbeat_tx.try_send(Heartbeat {
            block_id: block_id.to_string(),
            at: Instant::now(),
        });
        return false;
    }

    let _ = new_hash;
    if let Err(e) = synced_doc.write_local() {
        metrics::counter!("memory.subscriber.fs_write_failed").increment(1);
        tracing::error!(path = ?synced_doc.path(), error = %e, "write_local failed");
        return false;
    }

    // The extension and path are owned by synced_doc (configured at open time).
    // FTS uses block_id; TaskList reconcile uses disk_doc; no further use of
    // the canonical file path is needed here.
    let _ = ext;

    // Update persistent index tables. The strategy depends on schema:
    //
    // - TaskList blocks: FTS preview update AND task/edge reconcile run
    //   inside a single transaction on one connection — both succeed or
    //   both roll back atomically. Without this, a crash between the two
    //   operations would leave the FTS index out of sync with task rows.
    //
    // - All other schemas: FTS preview update runs standalone (no transaction
    //   needed for a single-statement write).
    let preview = doc.render();
    let preview_str = if preview.is_empty() {
        None
    } else {
        Some(preview.as_str())
    };

    if matches!(schema, BlockSchema::TaskList { .. }) {
        // Single connection, single transaction: FTS + task reconcile are atomic.
        match db.get() {
            Ok(mut conn) => {
                match conn.transaction() {
                    Ok(tx) => {
                        // FTS update inside the transaction.
                        if let Err(e) =
                            pattern_db::queries::update_block_preview(&tx, block_id, preview_str)
                        {
                            metrics::counter!("memory.subscriber.fts_update_failed").increment(1);
                            tracing::error!(
                                block_id = %block_id, error = %e,
                                "FTS5 update failed inside TaskList transaction; rolling back"
                            );
                            // tx drops without commit → implicit rollback.
                        } else if let Err(e) =
                            crate::subscriber::task::reconcile_task_list(&tx, block_id, disk_doc)
                        {
                            metrics::counter!(
                                "memory.sync_worker.reconcile_error",
                                "schema" => "task-list"
                            )
                            .increment(1);
                            tracing::error!(
                                block_id = %block_id, error = %e,
                                "TaskList reconcile failed; transaction rolled back"
                            );
                            // tx drops here without commit → both FTS and reconcile roll back.
                        } else if let Err(e) = tx.commit() {
                            metrics::counter!(
                                "memory.sync_worker.reconcile_error",
                                "schema" => "task-list"
                            )
                            .increment(1);
                            tracing::error!(
                                block_id = %block_id, error = %e,
                                "TaskList transaction commit failed"
                            );
                        }
                    }
                    Err(e) => {
                        metrics::counter!(
                            "memory.sync_worker.reconcile_error",
                            "schema" => "task-list"
                        )
                        .increment(1);
                        tracing::error!(
                            block_id = %block_id, error = %e,
                            "failed to open transaction for TaskList reconcile"
                        );
                    }
                }
            }
            Err(e) => {
                metrics::counter!("memory.subscriber.pool_exhausted").increment(1);
                tracing::error!(error = %e, "DB pool get failed for TaskList reconcile");
            }
        }
    } else {
        // Non-TaskList schemas: standalone FTS update (single statement, no
        // transaction needed).
        match db.get() {
            Ok(conn) => {
                if let Err(e) =
                    pattern_db::queries::update_block_preview(&conn, block_id, preview_str)
                {
                    metrics::counter!("memory.subscriber.fts_update_failed").increment(1);
                    tracing::error!(block_id = %block_id, error = %e, "FTS5 update failed");
                }
            }
            Err(e) => {
                metrics::counter!("memory.subscriber.pool_exhausted").increment(1);
                tracing::error!(error = %e, "DB pool get failed");
            }
        }
    }

    let _ = reembed_tx.send(ReembedRequest {
        block_id: block_id.to_string(),
        canonical_bytes: canonical_bytes.clone(),
        content_hash: new_hash,
    });

    *last_emitted_hash = Some(new_hash);

    let _ = heartbeat_tx.try_send(Heartbeat {
        block_id: block_id.to_string(),
        at: Instant::now(),
    });
    true
}

/// Handle a pause request: flush in-flight work, render, park, then reconcile
/// on resume.
///
/// This implements the flush-pause-resume model for quiesce. Instead of killing
/// the worker (which drops subscriptions and creates a write-loss window), we:
///
/// 1. Drain the channel and import all pending updates into disk_doc.
/// 2. Run one final render cycle so disk is fully up to date.
/// 3. Record version vectors for both memory_doc and disk_doc.
/// 4. Signal pause_complete so the caller knows we're parked.
/// 5. Wait on resume_signal.
/// 6. On resume: reconcile any writes that happened during the pause via
///    version-vector diff, render once, then reset and return to normal loop.
#[allow(clippy::too_many_arguments)]
fn handle_pause(
    block_id: &str,
    schema: &BlockSchema,
    rx: &Receiver<CommitEvent>,
    disk_doc: &LoroDoc,
    doc: &StructuredDocument,
    synced_doc: &SyncedDoc<BlockSchemaBridge>,
    db: &ConstellationDb,
    reembed_tx: &tokio::sync::mpsc::UnboundedSender<ReembedRequest>,
    heartbeat_tx: &crossbeam_channel::Sender<Heartbeat>,
    paused: &AtomicBool,
    pause_complete: &(Mutex<bool>, Condvar),
    resume_signal: &(Mutex<bool>, Condvar),
    cancel: &CancellationToken,
    last_emitted_hash: &mut Option<[u8; 32]>,
) {
    tracing::debug!(block_id = %block_id, "entering pause: flushing in-flight work");

    // Step 1: drain the channel completely, importing all pending updates.
    while let Ok(ev) = rx.try_recv() {
        if let Err(e) = disk_doc.import(&ev.update_bytes) {
            tracing::warn!(
                block_id = %block_id, error = %e,
                "failed to import update bytes into disk_doc during pause flush"
            );
        }
    }

    // Between `paused=true` being set and this point, agent writes may have
    // occurred that were captured in memory_doc (and thus in its oplog VV)
    // but never reached the channel — the subscribe_local_update callback
    // was suppressed during the race window. Sync them into disk_doc now so
    // the VV snapshot below accurately reflects what disk_doc has received.
    // Without this, the resume reconciliation sees these writes already in
    // `pre_pause_memory_vv` and skips them, leaving disk_doc permanently
    // behind.
    let disk_vv_pre_flush = disk_doc.oplog_vv();
    match doc
        .inner()
        .export(loro::ExportMode::updates(&disk_vv_pre_flush))
    {
        Ok(bytes) => {
            if !bytes.is_empty()
                && let Err(e) = disk_doc.import(&bytes)
            {
                tracing::warn!(
                    block_id = %block_id, error = %e,
                    "failed to sync memory_doc to disk_doc during pause flush"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                block_id = %block_id, error = %e,
                "failed to export memory_doc updates during pause flush"
            );
        }
    }

    // Step 2: one final render cycle.
    render_cycle(
        block_id,
        schema,
        disk_doc,
        doc,
        synced_doc,
        db,
        reembed_tx,
        heartbeat_tx,
        last_emitted_hash,
    );

    // Step 3: record version vectors for reconciliation on resume.
    // memory_doc vv: tells us what memory_doc knew at pause time — on resume
    // we export memory_doc's updates since this vv to catch agent writes.
    let pre_pause_memory_vv = doc.inner().oplog_vv();
    // disk_doc vv: tells us what disk_doc knew at pause time — on resume
    // we export disk_doc's updates since this vv to catch external edits
    // that the watcher applied to disk_doc during the pause.
    let pre_pause_disk_vv = disk_doc.oplog_vv();

    // Step 4: signal pause completion.
    {
        let (lock, cvar) = pause_complete;
        let mut complete = lock.lock().unwrap();
        *complete = true;
        cvar.notify_one();
    }

    tracing::debug!(block_id = %block_id, "paused — waiting for resume signal");

    // Step 5: wait for resume (or cancellation).
    {
        let (lock, cvar) = resume_signal;
        let mut resumed = lock.lock().unwrap();
        // Wait with periodic cancel checks so the worker can still exit
        // during a long pause (e.g. if the process is shutting down).
        while !*resumed {
            if cancel.is_cancelled() {
                // Shutting down — reset state and return. The outer loop
                // will break on the cancel check.
                paused.store(false, Ordering::Release);
                return;
            }
            let (guard, _timeout) = cvar
                .wait_timeout(resumed, Duration::from_millis(100))
                .unwrap();
            resumed = guard;
        }
    }

    tracing::debug!(block_id = %block_id, "resumed — reconciling writes from pause window");

    // Step 6: reconcile.
    // 6a: drain the channel completely and discard — these events are stale
    // because the vv reconciliation below covers everything.
    while rx.try_recv().is_ok() {}

    // 6b: memory_doc → disk_doc: export memory_doc's updates since the
    // pre-pause vv and import them into disk_doc. This catches any agent
    // writes that happened while we were parked (the subscribe_local_update
    // callback was suppressed, so those writes never reached the channel).
    match doc
        .inner()
        .export(loro::ExportMode::updates(&pre_pause_memory_vv))
    {
        Ok(bytes) => {
            if !bytes.is_empty()
                && let Err(e) = disk_doc.import(&bytes)
            {
                tracing::warn!(
                    block_id = %block_id, error = %e,
                    "failed to import memory_doc updates into disk_doc on resume"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                block_id = %block_id, error = %e,
                "failed to export memory_doc updates on resume"
            );
        }
    }

    // 6c: disk_doc → memory_doc: export disk_doc's updates since the
    // pre-pause vv and import them into memory_doc. This catches external
    // edits that the watcher applied to disk_doc while we were parked.
    match disk_doc.export(loro::ExportMode::updates(&pre_pause_disk_vv)) {
        Ok(bytes) => {
            if !bytes.is_empty()
                && let Err(e) = doc.inner().import(&bytes)
            {
                tracing::warn!(
                    block_id = %block_id, error = %e,
                    "failed to import disk_doc updates into memory_doc on resume"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                block_id = %block_id, error = %e,
                "failed to export disk_doc updates on resume"
            );
        }
    }

    // 6d: one render cycle to commit the reconciled state to disk.
    render_cycle(
        block_id,
        schema,
        disk_doc,
        doc,
        synced_doc,
        db,
        reembed_tx,
        heartbeat_tx,
        last_emitted_hash,
    );

    // 6e: reset all pause state.
    {
        let (lock, _) = pause_complete;
        let mut complete = lock.lock().unwrap();
        *complete = false;
    }
    {
        let (lock, _) = resume_signal;
        let mut resumed = lock.lock().unwrap();
        *resumed = false;
    }
    paused.store(false, Ordering::Release);

    tracing::debug!(block_id = %block_id, "pause-resume cycle complete, returning to normal loop");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a test agent + block in the given DB and return the block_id.
    fn setup_db_block(db: &ConstellationDb, block_id: &str, agent_id: &str) {
        let conn = db.get().unwrap();
        let agent = pattern_db::models::Agent {
            id: agent_id.to_string(),
            name: "Test Agent".to_string(),
            description: None,
            model_provider: "anthropic".to_string(),
            model_name: "claude".to_string(),
            system_prompt: "test".to_string(),
            config: pattern_db::Json(serde_json::json!({})),
            enabled_tools: pattern_db::Json(vec![]),
            tool_rules: None,
            status: pattern_db::models::AgentStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_agent(&conn, &agent).unwrap();

        let block = pattern_db::models::MemoryBlock {
            id: block_id.to_string(),
            agent_id: agent_id.to_string(),
            label: block_id.to_string(),
            description: "Test block".to_string(),
            block_type: pattern_db::models::MemoryBlockType::Working,
            char_limit: 5000,
            permission: pattern_db::models::MemoryPermission::ReadWrite,
            pinned: false,
            loro_snapshot: vec![],
            content_preview: None,
            metadata: None,
            embedding_model: None,
            is_active: true,
            frontier: None,
            last_seq: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        pattern_db::queries::create_block(&conn, &block).unwrap();
    }

    /// Map a `BlockSchema` to its canonical file extension (used for test file
    /// path construction). Mirrors the `block_schema_extension` helper in
    /// `cache.rs` but scoped to tests here to avoid cross-module visibility.
    fn schema_ext(schema: &BlockSchema) -> &'static str {
        match schema {
            BlockSchema::Text { .. } | BlockSchema::Skill { .. } => "md",
            BlockSchema::Map { .. }
            | BlockSchema::Composite { .. }
            | BlockSchema::List { .. }
            | BlockSchema::TaskList { .. } => "kdl",
            BlockSchema::Log { .. } => "jsonl",
            _ => "dat",
        }
    }

    /// Build an `Arc<SyncedDoc<BlockSchemaBridge>>` for use in worker tests.
    ///
    /// Uses `open_router_owned` so no filesystem watcher is started.
    /// The `memory_doc` is a reference clone of `doc.inner()` — they share the
    /// same underlying Loro state, so writes via `doc` are immediately visible
    /// to the `SyncedDoc` ingest thread.
    ///
    /// The file path is `dir/<block_id>.<ext>` where `ext` is determined from
    /// `schema`. The file need not exist before calling this function.
    fn make_synced_doc(
        block_id: &str,
        schema: &BlockSchema,
        doc: &StructuredDocument,
        dir: &tempfile::TempDir,
    ) -> Arc<SyncedDoc<BlockSchemaBridge>> {
        use crate::loro_sync::{ConflictPolicy, SyncedDocConfig};

        let ext = schema_ext(schema);
        let path = dir.path().join(format!("{block_id}.{ext}"));
        let loro_handle = doc.inner().clone();
        let bridge = Arc::new(BlockSchemaBridge::new(schema.clone()));
        Arc::new(
            SyncedDoc::open_router_owned(SyncedDocConfig {
                path,
                doc: loro_handle,
                bridge,
                event_channel_bound: 64,
                conflict_policy: ConflictPolicy::AutoMerge,
            })
            .expect("open_router_owned must succeed in tests"),
        )
    }

    /// Create default pause state for tests that don't exercise pause/resume.
    #[allow(clippy::type_complexity)]
    fn default_pause_state() -> (
        Arc<AtomicBool>,
        Arc<(Mutex<bool>, std::sync::Condvar)>,
        Arc<(Mutex<bool>, std::sync::Condvar)>,
    ) {
        (
            Arc::new(AtomicBool::new(false)),
            Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            Arc::new((Mutex::new(false), std::sync::Condvar::new())),
        )
    }

    /// Run the subscriber with the given doc/schema, send update_bytes,
    /// wait for processing, and return the path to the emitted file.
    fn run_worker_and_get_file(
        block_id: &str,
        schema: BlockSchema,
        doc: StructuredDocument,
        update_bytes: Vec<u8>,
        db: Arc<ConstellationDb>,
        dir: &tempfile::TempDir,
    ) -> std::path::PathBuf {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let cancel = CancellationToken::new();
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let synced_doc = make_synced_doc(block_id, &schema, &doc, dir);
        let (paused, pause_complete, resume_signal) = default_pause_state();

        let cancel_clone = cancel.clone();
        let mount = Arc::new(dir.path().to_path_buf());
        let mount_clone = mount.clone();
        let block_id_str = block_id.to_string();
        let block_id_str2 = block_id.to_string();
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: block_id_str,
                schema,
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: mount_clone,
                doc,
                synced_doc,
                paused,
                pause_complete,
                resume_signal,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        tx.send(CommitEvent {
            block_id: block_id_str2,
            update_bytes,
        })
        .unwrap();

        // Wait for worker to debounce and write (50ms debounce + processing).
        std::thread::sleep(Duration::from_millis(200));

        cancel.cancel();
        drop(tx);
        handle.join().expect("worker thread should not panic");

        mount.as_ref().clone()
    }

    /// Test that a Map schema block writes a .kdl file with the correct field data.
    ///
    /// Exercises the full subscriber path: StructuredDocument::set_field →
    /// update bytes → CommitEvent → disk_doc import → KDL render → file emit.
    #[test]
    fn worker_emits_kdl_for_map_schema() {
        use pattern_core::types::memory_types::{FieldDef, FieldType};

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "map_block", "agent_map");

        let schema = BlockSchema::Map {
            fields: vec![
                FieldDef {
                    name: "name".to_string(),
                    description: "Name".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: false,
                },
                FieldDef {
                    name: "status".to_string(),
                    description: "Status".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    default: None,
                    read_only: false,
                },
            ],
        };

        let doc = StructuredDocument::new(schema.clone());

        // Write fields using the StructuredDocument API (uses "fields" container).
        let vv_before = doc.inner().oplog_vv();
        doc.set_field("name", serde_json::Value::String("Alice".to_string()), true)
            .unwrap();
        doc.set_field(
            "status",
            serde_json::Value::String("active".to_string()),
            true,
        )
        .unwrap();
        let update_bytes = doc
            .inner()
            .export(loro::ExportMode::updates(&vv_before))
            .unwrap();

        let mount_dir = run_worker_and_get_file("map_block", schema, doc, update_bytes, db, &dir);

        // The worker should emit a .kdl file (not .md).
        let file_path = mount_dir.join("map_block.kdl");
        assert!(
            file_path.exists(),
            "KDL file should be written for Map schema"
        );

        let content = std::fs::read_to_string(&file_path).unwrap();
        // The deep_value has a top-level "fields" key containing the map.
        // The KDL content should mention both the field name and values.
        assert!(
            content.contains("Alice"),
            "KDL file should contain the 'name' field value: {content}"
        );
        assert!(
            content.contains("active"),
            "KDL file should contain the 'status' field value: {content}"
        );
    }

    /// Test that a List schema block writes a .kdl file with the correct items.
    ///
    /// Exercises the full subscriber path: StructuredDocument::push_item →
    /// update bytes → CommitEvent → disk_doc import → KDL render → file emit.
    #[test]
    fn worker_emits_kdl_for_list_schema() {
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "list_block", "agent_list");

        let schema = BlockSchema::List {
            item_schema: None,
            max_items: None,
        };

        let doc = StructuredDocument::new(schema.clone());

        // Write items using the StructuredDocument API (uses "items" container).
        let vv_before = doc.inner().oplog_vv();
        doc.push_item(serde_json::Value::String("first item".to_string()), true)
            .unwrap();
        doc.push_item(serde_json::Value::String("second item".to_string()), true)
            .unwrap();
        let update_bytes = doc
            .inner()
            .export(loro::ExportMode::updates(&vv_before))
            .unwrap();

        let mount_dir = run_worker_and_get_file("list_block", schema, doc, update_bytes, db, &dir);

        // The worker should emit a .kdl file.
        let file_path = mount_dir.join("list_block.kdl");
        assert!(
            file_path.exists(),
            "KDL file should be written for List schema"
        );

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(
            content.contains("first item"),
            "KDL file should contain the first list item: {content}"
        );
        assert!(
            content.contains("second item"),
            "KDL file should contain the second list item: {content}"
        );
    }

    /// Test that a Log schema block writes a .jsonl file with the correct entries.
    ///
    /// Exercises the full subscriber path: StructuredDocument::append_log_entry →
    /// update bytes → CommitEvent → disk_doc import → JSONL render → file emit.
    ///
    /// Critically, this test validates that render_canonical_from_disk_doc reads
    /// the "entries" container (not any other name) from the disk_doc's deep value.
    #[test]
    fn worker_emits_jsonl_for_log_schema() {
        use pattern_core::types::memory_types::{FieldDef, FieldType, LogEntrySchema};

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "log_block", "agent_log");

        let schema = BlockSchema::Log {
            display_limit: 50,
            entry_schema: LogEntrySchema {
                timestamp: true,
                agent_id: false,
                fields: vec![FieldDef {
                    name: "message".to_string(),
                    description: "Log message".to_string(),
                    field_type: FieldType::Text,
                    required: true,
                    default: None,
                    read_only: false,
                }],
            },
        };

        let doc = StructuredDocument::new(schema.clone());

        // Write log entries using the StructuredDocument API (uses "entries" container).
        let vv_before = doc.inner().oplog_vv();
        doc.append_log_entry(
            serde_json::json!({
                "timestamp": "2026-04-19T10:00:00Z",
                "message": "system started"
            }),
            true,
        )
        .unwrap();
        doc.append_log_entry(
            serde_json::json!({
                "timestamp": "2026-04-19T10:01:00Z",
                "message": "task completed"
            }),
            true,
        )
        .unwrap();
        let update_bytes = doc
            .inner()
            .export(loro::ExportMode::updates(&vv_before))
            .unwrap();

        let mount_dir = run_worker_and_get_file("log_block", schema, doc, update_bytes, db, &dir);

        // The worker should emit a .jsonl file (not .md or .kdl).
        let file_path = mount_dir.join("log_block.jsonl");
        assert!(
            file_path.exists(),
            "JSONL file should be written for Log schema"
        );

        let content = std::fs::read_to_string(&file_path).unwrap();
        // Each line should be a valid JSON object.
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "JSONL file should have 2 entries: {content}"
        );

        // Verify the entry content is present.
        assert!(
            content.contains("system started"),
            "JSONL file should contain the first log message: {content}"
        );
        assert!(
            content.contains("task completed"),
            "JSONL file should contain the second log message: {content}"
        );

        // Verify each line is valid JSON.
        for line in &lines {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "each JSONL line should be valid JSON: {line}"
            );
        }
    }

    #[test]
    fn worker_exits_on_cancel() {
        let (_tx, rx) = crossbeam_channel::bounded(8);
        let cancel = CancellationToken::new();
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(8);
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let doc = StructuredDocument::new_text();
        let schema = BlockSchema::text();
        let synced_doc = make_synced_doc("test_block", &schema, &doc, &dir);
        let (paused, pause_complete, resume_signal) = default_pause_state();

        let cancel_clone = cancel.clone();
        let mount_path = Arc::new(dir.path().to_path_buf());
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "test_block".to_string(),
                schema,
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path,
                doc,
                synced_doc,
                paused,
                pause_complete,
                resume_signal,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        // Cancel — should exit promptly.
        cancel.cancel();
        handle.join().expect("worker thread should not panic");
    }

    #[test]
    fn worker_exits_on_sender_drop() {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let cancel = CancellationToken::new();
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(8);
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let doc = StructuredDocument::new_text();
        let schema = BlockSchema::text();
        let synced_doc = make_synced_doc("test_block", &schema, &doc, &dir);
        let (paused, pause_complete, resume_signal) = default_pause_state();
        let mount_path = Arc::new(dir.path().to_path_buf());

        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "test_block".to_string(),
                schema,
                rx,
                cancel,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path,
                doc,
                synced_doc,
                paused,
                pause_complete,
                resume_signal,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        // Drop sender — worker should exit.
        drop(tx);
        handle.join().expect("worker thread should not panic");
    }

    #[test]
    fn worker_emits_file_on_update_bytes() {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let cancel = CancellationToken::new();
        let (reembed_tx, mut reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();

        // Create a test agent and block in DB so FTS update works.
        {
            let conn = db.get().unwrap();
            let agent = pattern_db::models::Agent {
                id: "agent_1".to_string(),
                name: "Test Agent".to_string(),
                description: None,
                model_provider: "anthropic".to_string(),
                model_name: "claude".to_string(),
                system_prompt: "test".to_string(),
                config: pattern_db::Json(serde_json::json!({})),
                enabled_tools: pattern_db::Json(vec![]),
                tool_rules: None,
                status: pattern_db::models::AgentStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_agent(&conn, &agent).unwrap();

            let block = pattern_db::models::MemoryBlock {
                id: "test_block".to_string(),
                agent_id: "agent_1".to_string(),
                label: "test".to_string(),
                description: "Test block".to_string(),
                block_type: pattern_db::models::MemoryBlockType::Working,
                char_limit: 5000,
                permission: pattern_db::models::MemoryPermission::ReadWrite,
                pinned: false,
                loro_snapshot: vec![],
                content_preview: None,
                metadata: None,
                embedding_model: None,
                is_active: true,
                frontier: None,
                last_seq: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_block(&conn, &block).unwrap();
        }

        let doc = StructuredDocument::new_text();
        let schema = BlockSchema::text();
        let synced_doc = make_synced_doc("test_block", &schema, &doc, &dir);
        let (paused, pause_complete, resume_signal) = default_pause_state();

        // Capture the update bytes when we write to the memory_doc.
        let update_bytes = {
            let vv_before = doc.inner().oplog_vv();
            doc.set_text("Hello subscriber!", true).unwrap();
            doc.inner()
                .export(loro::ExportMode::updates(&vv_before))
                .unwrap()
        };

        let cancel_clone = cancel.clone();
        let mount = Arc::new(dir.path().to_path_buf());
        let mount_clone = mount.clone();
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "test_block".to_string(),
                schema,
                rx,
                cancel: cancel_clone,
                db: db.clone(),
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: mount_clone,
                doc,
                synced_doc,
                paused,
                pause_complete,
                resume_signal,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        // Send a commit event with actual update bytes.
        tx.send(CommitEvent {
            block_id: "test_block".to_string(),
            update_bytes,
        })
        .unwrap();

        // Wait for the file to appear (worker debounces 50ms + processing).
        std::thread::sleep(Duration::from_millis(200));

        // Check that the file was written.
        let file_path = mount.join("test_block.md");
        assert!(file_path.exists(), "canonical file should be written");
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello subscriber!");

        // Check that a re-embed request was queued.
        let req = reembed_rx.try_recv();
        assert!(req.is_ok(), "re-embed request should be queued");

        // Shut down.
        cancel.cancel();
        drop(tx);
        handle.join().expect("worker should not panic");
    }

    #[test]
    fn worker_suppresses_echo_on_same_hash() {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let cancel = CancellationToken::new();
        let (reembed_tx, mut reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();

        // Create agent + block.
        {
            let conn = db.get().unwrap();
            let agent = pattern_db::models::Agent {
                id: "agent_1".to_string(),
                name: "Test Agent".to_string(),
                description: None,
                model_provider: "anthropic".to_string(),
                model_name: "claude".to_string(),
                system_prompt: "test".to_string(),
                config: pattern_db::Json(serde_json::json!({})),
                enabled_tools: pattern_db::Json(vec![]),
                tool_rules: None,
                status: pattern_db::models::AgentStatus::Active,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_agent(&conn, &agent).unwrap();

            let block = pattern_db::models::MemoryBlock {
                id: "echo_block".to_string(),
                agent_id: "agent_1".to_string(),
                label: "echo".to_string(),
                description: "Echo test".to_string(),
                block_type: pattern_db::models::MemoryBlockType::Working,
                char_limit: 5000,
                permission: pattern_db::models::MemoryPermission::ReadWrite,
                pinned: false,
                loro_snapshot: vec![],
                content_preview: None,
                metadata: None,
                embedding_model: None,
                is_active: true,
                frontier: None,
                last_seq: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            pattern_db::queries::create_block(&conn, &block).unwrap();
        }

        let doc = StructuredDocument::new_text();
        let schema = BlockSchema::text();
        let synced_doc = make_synced_doc("echo_block", &schema, &doc, &dir);
        let (paused, pause_complete, resume_signal) = default_pause_state();

        // Capture the update bytes.
        let update_bytes = {
            let vv_before = doc.inner().oplog_vv();
            doc.set_text("Same content", true).unwrap();
            doc.inner()
                .export(loro::ExportMode::updates(&vv_before))
                .unwrap()
        };

        let cancel_clone = cancel.clone();
        let mount_path = Arc::new(dir.path().to_path_buf());
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "echo_block".to_string(),
                schema,
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path,
                doc,
                synced_doc,
                paused,
                pause_complete,
                resume_signal,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        // Send the same update bytes twice.
        tx.send(CommitEvent {
            block_id: "echo_block".to_string(),
            update_bytes: update_bytes.clone(),
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));

        tx.send(CommitEvent {
            block_id: "echo_block".to_string(),
            update_bytes,
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));

        // Should have exactly one re-embed request (second was suppressed
        // because disk_doc already has the content, so the hash is identical).
        let first = reembed_rx.try_recv();
        assert!(first.is_ok(), "first re-embed should exist");
        let second = reembed_rx.try_recv();
        assert!(second.is_err(), "second re-embed should be suppressed");

        cancel.cancel();
        drop(tx);
        handle.join().expect("worker should not panic");
    }

    /// Test that the two-doc model correctly propagates updates:
    /// memory_doc write → update bytes → disk_doc import → file render.
    #[test]
    fn two_doc_model_sync() {
        let memory_doc = StructuredDocument::new_text();
        let disk_doc = memory_doc.inner().fork();

        // Write to memory_doc.
        let vv_before = memory_doc.inner().oplog_vv();
        memory_doc.set_text("Agent wrote this", true).unwrap();
        let update_bytes = memory_doc
            .inner()
            .export(loro::ExportMode::updates(&vv_before))
            .unwrap();

        // Import into disk_doc.
        disk_doc.import(&update_bytes).unwrap();

        // disk_doc should now have the same content.
        let disk_text = disk_doc.get_text("content").to_string();
        assert_eq!(disk_text, "Agent wrote this");
    }

    /// Test bidirectional sync: disk_doc change propagates back to memory_doc.
    #[test]
    fn two_doc_model_bidirectional() {
        let memory_doc = StructuredDocument::new_text();
        let disk_doc = memory_doc.inner().fork();

        // Simulate an external edit on disk_doc.
        let disk_text = disk_doc.get_text("content");
        disk_text
            .update("Human edited this", Default::default())
            .unwrap();
        disk_doc.commit();

        // Export disk_doc's updates and import into memory_doc.
        let vv = memory_doc.inner().oplog_vv();
        let update_bytes = disk_doc.export(loro::ExportMode::updates(&vv)).unwrap();
        memory_doc.inner().import(&update_bytes).unwrap();

        // memory_doc should now have the same content.
        assert_eq!(memory_doc.text_content(), "Human edited this");
    }

    /// Test concurrent edits: both memory_doc and disk_doc make changes,
    /// then sync. CRDT merge should preserve both edits.
    #[test]
    fn two_doc_model_concurrent_merge() {
        let memory_doc = StructuredDocument::new_text();

        // Set initial shared content.
        memory_doc.set_text("Initial content", true).unwrap();
        let disk_doc = memory_doc.inner().fork();

        // Memory doc records its version before the concurrent edit.
        let mem_vv_before = memory_doc.inner().oplog_vv();
        let disk_vv_before = disk_doc.oplog_vv();

        // Agent writes to memory_doc (appends at end).
        {
            let text = memory_doc.inner().get_text("content");
            let len = text.len_unicode();
            text.insert(len, " + agent").unwrap();
            memory_doc.inner().commit();
        }

        // Human writes to disk_doc (appends at end).
        {
            let text = disk_doc.get_text("content");
            let len = text.len_unicode();
            text.insert(len, " + human").unwrap();
            disk_doc.commit();
        }

        // Sync memory → disk.
        let mem_updates = memory_doc
            .inner()
            .export(loro::ExportMode::updates(&disk_vv_before))
            .unwrap();
        disk_doc.import(&mem_updates).unwrap();

        // Sync disk → memory.
        let disk_updates = disk_doc
            .export(loro::ExportMode::updates(&mem_vv_before))
            .unwrap();
        memory_doc.inner().import(&disk_updates).unwrap();

        // Both docs should have identical content after CRDT merge.
        let mem_content = memory_doc.text_content();
        let disk_content = disk_doc.get_text("content").to_string();
        assert_eq!(mem_content, disk_content);

        // Both edits should survive — the merged content should contain
        // both " + agent" and " + human" (order may vary per CRDT rules).
        assert!(
            mem_content.contains("agent"),
            "merged content should contain agent's edit: {mem_content}"
        );
        assert!(
            mem_content.contains("human"),
            "merged content should contain human's edit: {mem_content}"
        );
    }

    /// Test that writes during a pause window are reconciled on resume.
    ///
    /// This test properly clones the StructuredDocument before spawning the
    /// worker so both the test and the worker share the same underlying LoroDoc.
    ///
    /// Sequence:
    /// 1. Write "first" to memory_doc, send to worker, wait for file.
    /// 2. Pause the worker.
    /// 3. Write "second" to memory_doc (callback suppressed, no channel event).
    /// 4. Resume the worker.
    /// 5. Verify the emitted file contains "second" (reconciled via vv diff).
    #[test]
    fn pause_resume_reconciles_agent_writes_during_pause() {
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "pr_block", "agent_pr");

        let doc = StructuredDocument::new_text();
        // Clone before spawning — both test and worker share the same Arc<LoroDoc>.
        let doc_clone = doc.clone();
        let schema = BlockSchema::text();
        let synced_doc = make_synced_doc("pr_block", &schema, &doc, &dir);

        let (tx, rx) = crossbeam_channel::bounded(64);
        let cancel = CancellationToken::new();
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let paused = Arc::new(AtomicBool::new(false));
        let pause_complete = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let resume_signal_arc = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let mount = Arc::new(dir.path().to_path_buf());

        // Write "first" and capture update bytes.
        let vv0 = doc.inner().oplog_vv();
        doc.set_text("first", true).unwrap();
        let update1 = doc.inner().export(loro::ExportMode::updates(&vv0)).unwrap();

        let cancel_clone = cancel.clone();
        let mount_clone = Arc::clone(&mount);
        let paused_worker = Arc::clone(&paused);
        let pc_worker = Arc::clone(&pause_complete);
        let rs_worker = Arc::clone(&resume_signal_arc);
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "pr_block".to_string(),
                schema,
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: mount_clone,
                doc: doc_clone,
                synced_doc,
                paused: paused_worker,
                pause_complete: pc_worker,
                resume_signal: rs_worker,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        // Send "first" to the worker.
        tx.send(CommitEvent {
            block_id: "pr_block".to_string(),
            update_bytes: update1,
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let file_path = mount.join("pr_block.md");
        assert!(file_path.exists(), "file should exist after first write");
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "first");

        // Step 2: pause the worker.
        paused.store(true, Ordering::Release);
        {
            let (lock, cvar) = pause_complete.as_ref();
            let mut complete = lock.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !*complete {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "worker did not pause in time");
                let (guard, _) = cvar.wait_timeout(complete, remaining).unwrap();
                complete = guard;
            }
        }

        // Step 3: write "second" to memory_doc while paused. The callback
        // is suppressed, so no CommitEvent reaches the channel. The write
        // only exists in memory_doc's LoroDoc.
        doc.set_text("second", true).unwrap();

        // Step 4: resume.
        {
            let (lock, cvar) = resume_signal_arc.as_ref();
            let mut resumed = lock.lock().unwrap();
            *resumed = true;
            cvar.notify_one();
        }

        // Wait for the worker to reconcile and render.
        std::thread::sleep(Duration::from_millis(300));

        // Step 5: verify the file contains "second".
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(
            content, "second",
            "file should contain 'second' after pause-resume reconciliation"
        );

        // Clean up.
        cancel.cancel();
        drop(tx);
        handle.join().expect("worker should not panic");
    }

    /// Test that external edits to disk_doc during a pause are reconciled
    /// back to memory_doc on resume.
    ///
    /// Sequence:
    /// 1. Write "initial" to memory_doc, send to worker, wait for file.
    /// 2. Pause the worker.
    /// 3. Apply an external edit to disk_doc (simulating a watcher-applied
    ///    human edit).
    /// 4. Resume the worker.
    /// 5. Verify memory_doc contains the external edit.
    #[test]
    fn pause_resume_reconciles_disk_doc_edits() {
        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "ext_block", "agent_ext");

        let doc = StructuredDocument::new_text();
        let doc_clone = doc.clone();
        let schema = BlockSchema::text();
        let synced_doc = make_synced_doc("ext_block", &schema, &doc, &dir);
        // Retain a reference to disk_doc so the test can inject an external edit
        // directly (simulating what the watcher does on a human file edit).
        let disk_doc_test = synced_doc.doc().clone();

        let (tx, rx) = crossbeam_channel::bounded(64);
        let cancel = CancellationToken::new();
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let paused = Arc::new(AtomicBool::new(false));
        let pause_complete = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let resume_signal_arc = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let mount = Arc::new(dir.path().to_path_buf());

        // Write "initial" and capture update bytes.
        let vv0 = doc.inner().oplog_vv();
        doc.set_text("initial", true).unwrap();
        let update1 = doc.inner().export(loro::ExportMode::updates(&vv0)).unwrap();

        let cancel_clone = cancel.clone();
        let mount_clone = Arc::clone(&mount);
        let paused_worker = Arc::clone(&paused);
        let pc_worker = Arc::clone(&pause_complete);
        let rs_worker = Arc::clone(&resume_signal_arc);
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "ext_block".to_string(),
                schema,
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: mount_clone,
                doc: doc_clone,
                synced_doc,
                paused: paused_worker,
                pause_complete: pc_worker,
                resume_signal: rs_worker,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        // Send "initial" to the worker.
        tx.send(CommitEvent {
            block_id: "ext_block".to_string(),
            update_bytes: update1,
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let file_path = mount.join("ext_block.md");
        assert!(file_path.exists(), "file should exist after initial write");
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "initial");

        // Step 2: pause the worker.
        paused.store(true, Ordering::Release);
        {
            let (lock, cvar) = pause_complete.as_ref();
            let mut complete = lock.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !*complete {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "worker did not pause in time");
                let (guard, _) = cvar.wait_timeout(complete, remaining).unwrap();
                complete = guard;
            }
        }

        // Step 3: apply an external edit directly to disk_doc (simulating
        // what the watcher would do when it detects a human file edit).
        {
            let text = disk_doc_test.get_text("content");
            text.update("human edited", Default::default()).unwrap();
            disk_doc_test.commit();
        }

        // Step 4: resume.
        {
            let (lock, cvar) = resume_signal_arc.as_ref();
            let mut resumed = lock.lock().unwrap();
            *resumed = true;
            cvar.notify_one();
        }

        // Wait for the worker to reconcile by polling the on-disk file until
        // it shows the expected content. A fixed sleep of 300 ms was prone to
        // flakes on loaded CI machines because `block_change_notifier.fire`
        // now runs inline in `render_cycle` (adding a small amount of work to
        // the resume path). Polling with a deadline is robust against timing
        // variation.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(content) = std::fs::read_to_string(&file_path)
                && content == "human edited"
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not reconcile disk file to 'human edited' within 5 s"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Step 5: verify memory_doc has the external edit.
        let mem_content = doc.text_content();
        assert_eq!(
            mem_content, "human edited",
            "memory_doc should contain the external edit after pause-resume reconciliation"
        );

        // Verify the file on disk was updated (already confirmed by poll above).
        let file_content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(
            file_content, "human edited",
            "file should contain the external edit after reconciliation"
        );

        // Clean up.
        cancel.cancel();
        drop(tx);
        handle.join().expect("worker should not panic");
    }

    /// Test that `render_canonical_from_disk_doc` with a TaskList schema emits
    /// KDL bytes that parse back via `kdl_to_loro_value(.., TopShape::TaskList)`
    /// into the original disk_doc state (AC Task 9 worker round-trip).
    ///
    /// This exercises the full subscriber path for TaskList:
    /// StructuredDocument::import_from_json → update bytes → CommitEvent →
    /// disk_doc import → TaskList KDL render → file emit → KDL parse →
    /// kdl_to_loro_value → LoroValue equality with original.
    #[test]
    fn worker_emits_kdl_for_task_list_schema() {
        use pattern_core::types::memory_types::TaskStatus;

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "tl_block", "agent_tl");

        let schema = BlockSchema::TaskList {
            default_status: Some(TaskStatus::Pending),
            default_owner: None,
            display_limit: None,
        };

        let doc = StructuredDocument::new(schema.clone());

        // Insert two items via the StructuredDocument API.
        let items_json = serde_json::json!({
            "items": [
                {
                    "id": "item-a",
                    "subject": "First task",
                    "description": "",
                    "status": "pending",
                    "blocks": [],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "item-b",
                    "subject": "Second task",
                    "description": "Has a description",
                    "status": "in-progress",
                    "blocks": [],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-02T00:00:00Z",
                    "updated_at": "2026-01-02T00:00:00Z"
                }
            ]
        });

        let vv_before = doc.inner().oplog_vv();
        doc.import_from_json(&items_json).unwrap();
        doc.commit();
        let update_bytes = doc
            .inner()
            .export(loro::ExportMode::updates(&vv_before))
            .unwrap();

        let mount_dir = run_worker_and_get_file("tl_block", schema, doc, update_bytes, db, &dir);

        // The worker should emit a .kdl file for TaskList schema.
        let file_path = mount_dir.join("tl_block.kdl");
        assert!(
            file_path.exists(),
            "KDL file should be written for TaskList schema"
        );

        let content = std::fs::read_to_string(&file_path).unwrap();

        // Basic content checks.
        assert!(
            content.contains("task-list"),
            "KDL file should contain task-list root node: {content}"
        );
        assert!(
            content.contains("First task"),
            "KDL file should contain first item subject: {content}"
        );
        assert!(
            content.contains("Second task"),
            "KDL file should contain second item subject: {content}"
        );
        assert!(
            content.contains("Has a description"),
            "KDL file should contain non-empty description: {content}"
        );

        // Round-trip: parse the emitted KDL back through kdl_to_loro_value
        // and verify the item ids are preserved (the key AC1.7 guarantee).
        let parsed_kdl =
            crate::fs::kdl::parse_kdl(&content).expect("emitted KDL must be valid KDL");
        let round_tripped =
            crate::fs::kdl::kdl_to_loro_value(&parsed_kdl, crate::fs::kdl::TopShape::TaskList)
                .expect("emitted KDL must parse back to LoroValue via TaskList shape");

        let loro::LoroValue::Map(root) = &round_tripped else {
            panic!("round-tripped value must be a LoroValue::Map");
        };
        let loro::LoroValue::List(items) = root.get("items").expect("items key must exist") else {
            panic!("items must be a LoroValue::List");
        };
        assert_eq!(items.len(), 2, "round-tripped items list must have 2 items");

        // Verify item ids survived the round-trip.
        let ids: Vec<&str> = items
            .iter()
            .filter_map(|item| {
                let loro::LoroValue::Map(m) = item else {
                    return None;
                };
                m.get("id").and_then(|v| match v {
                    loro::LoroValue::String(s) => Some(s.as_str()),
                    _ => None,
                })
            })
            .collect();
        assert!(
            ids.contains(&"item-a"),
            "item-a id must survive round-trip: {ids:?}"
        );
        assert!(
            ids.contains(&"item-b"),
            "item-b id must survive round-trip: {ids:?}"
        );
    }

    /// AC3: CommitEvent → worker dispatch arm → reconcile_task_list pipeline.
    ///
    /// This test verifies that the `BlockSchema::TaskList` dispatch in
    /// `render_cycle` (which is called from the main event loop and from
    /// `handle_pause`) actually fires when a `CommitEvent` arrives. If the
    /// dispatch arm were removed or broken, the previous tests — which call
    /// `reconcile_task_list` directly — would still pass. Only this test would
    /// fail, proving the wiring is live.
    ///
    /// Strategy:
    /// 1. Build a TaskList LoroDoc with 3 items (2 with edges).
    /// 2. Send a `CommitEvent` via the worker channel.
    /// 3. Wait for the worker to process it (debounce + render).
    /// 4. Assert that the `tasks` and `task_edges` SQL tables reflect the doc.
    #[test]
    fn commit_event_drives_task_list_reconcile_in_worker() {
        use pattern_core::types::memory_types::TaskStatus;

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "tl_reconcile_block", "agent_tl_r");

        let schema = BlockSchema::TaskList {
            default_status: Some(TaskStatus::Pending),
            default_owner: None,
            display_limit: None,
        };

        let doc = StructuredDocument::new(schema.clone());

        // Build a TaskList doc with 3 items: item-x (no edges), item-y (1 edge),
        // item-z (1 edge). Total: 3 task rows, 2 edge rows expected.
        let items_json = serde_json::json!({
            "items": [
                {
                    "id": "item-x",
                    "subject": "Item X",
                    "description": "",
                    "status": "pending",
                    "blocks": [],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "item-y",
                    "subject": "Item Y",
                    "description": "",
                    "status": "in-progress",
                    "blocks": [
                        { "block": "other-block", "task_item": "other-item" }
                    ],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "item-z",
                    "subject": "Item Z",
                    "description": "",
                    "status": "blocked",
                    "blocks": [
                        { "block": "another-block" }
                    ],
                    "metadata": {},
                    "comments": [],
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                }
            ]
        });

        let vv_before = doc.inner().oplog_vv();
        doc.import_from_json(&items_json).unwrap();
        doc.commit();
        let update_bytes = doc
            .inner()
            .export(loro::ExportMode::updates(&vv_before))
            .unwrap();

        // Run the full worker pipeline via CommitEvent.
        run_worker_and_get_file(
            "tl_reconcile_block",
            schema,
            doc,
            update_bytes,
            db.clone(),
            &dir,
        );

        // Assert that the SQL tables were reconciled by the worker's dispatch arm.
        let conn = db.get().unwrap();

        let task_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE block_handle = 'tl_reconcile_block'",
                [],
                |r| r.get(0),
            )
            .expect("tasks count query must succeed");
        assert_eq!(
            task_count, 3,
            "worker must reconcile 3 task rows via CommitEvent dispatch; got {task_count}"
        );

        let edge_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_edges WHERE source_block = 'tl_reconcile_block'",
                [],
                |r| r.get(0),
            )
            .expect("task_edges count query must succeed");
        assert_eq!(
            edge_count, 2,
            "worker must reconcile 2 edge rows via CommitEvent dispatch; got {edge_count}"
        );

        // Verify item ids are present.
        let mut item_ids: Vec<String> = conn
            .prepare(
                "SELECT task_item_id FROM tasks WHERE block_handle = 'tl_reconcile_block' \
                 ORDER BY task_item_id",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        item_ids.sort();
        assert_eq!(
            item_ids,
            vec!["item-x", "item-y", "item-z"],
            "all three items must appear in tasks table after CommitEvent"
        );
    }

    /// Critical #4: pause/resume cycle with TaskList block pending runs reconcile.
    ///
    /// Verifies that `render_cycle` — which is now called from `handle_pause` on
    /// both the pause-flush and post-resume paths — also drives TaskList
    /// reconciliation. Before the fix, a write during quiesce would leave
    /// `tasks`/`task_edges` stale until the next CommitEvent.
    ///
    /// Sequence:
    /// 1. Write an initial TaskList (2 items) to the worker; confirm 2 SQL rows.
    /// 2. Pause the worker.
    /// 3. Write a new version (3 items) to memory_doc while paused.
    /// 4. Resume the worker.
    /// 5. Verify `tasks` reflects the 3-item state — proving `render_cycle`
    ///    (called on resume) ran the reconcile.
    #[test]
    fn pause_resume_runs_tasklist_reconcile() {
        use pattern_core::types::memory_types::TaskStatus;

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let dir = tempfile::tempdir().unwrap();
        setup_db_block(&db, "pr_tl_block", "agent_pr_tl");

        let schema = BlockSchema::TaskList {
            default_status: Some(TaskStatus::Pending),
            default_owner: None,
            display_limit: None,
        };

        let doc = StructuredDocument::new(schema.clone());
        let doc_clone = doc.clone();
        let synced_doc = make_synced_doc("pr_tl_block", &schema, &doc, &dir);

        let (tx, rx) = crossbeam_channel::bounded(64);
        let cancel = CancellationToken::new();
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, _hb_rx) = crossbeam_channel::bounded(64);
        let paused = Arc::new(AtomicBool::new(false));
        let pause_complete = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let resume_signal_arc = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let mount = Arc::new(dir.path().to_path_buf());

        // Step 1: write 2 items.
        let v1_json = serde_json::json!({
            "items": [
                {
                    "id": "item-alpha", "subject": "Alpha", "description": "",
                    "status": "pending", "blocks": [], "metadata": {}, "comments": [],
                    "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "item-beta", "subject": "Beta", "description": "",
                    "status": "in-progress", "blocks": [], "metadata": {}, "comments": [],
                    "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
                }
            ]
        });
        let vv0 = doc.inner().oplog_vv();
        doc.import_from_json(&v1_json).unwrap();
        doc.commit();
        let update_v1 = doc.inner().export(loro::ExportMode::updates(&vv0)).unwrap();

        let cancel_clone = cancel.clone();
        let db_clone = Arc::clone(&db);
        let mount_clone = Arc::clone(&mount);
        let paused_worker = Arc::clone(&paused);
        let pc_worker = Arc::clone(&pause_complete);
        let rs_worker = Arc::clone(&resume_signal_arc);

        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "pr_tl_block".to_string(),
                schema,
                rx,
                cancel: cancel_clone,
                db: db_clone,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: mount_clone,
                doc: doc_clone,
                synced_doc,
                paused: paused_worker,
                pause_complete: pc_worker,
                resume_signal: rs_worker,
                block_change_notifier: crate::subscriber::BlockChangeNotifier::new(),
            });
        });

        tx.send(CommitEvent {
            block_id: "pr_tl_block".to_string(),
            update_bytes: update_v1,
        })
        .unwrap();

        // Wait for the worker to process the initial write.
        std::thread::sleep(Duration::from_millis(300));

        // Confirm the initial 2-item reconcile.
        {
            let conn = db.get().unwrap();
            let cnt: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE block_handle = 'pr_tl_block'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                cnt, 2,
                "initial reconcile must produce 2 task rows; got {cnt}"
            );
        }

        // Step 2: pause the worker.
        paused.store(true, Ordering::Release);
        {
            let (lock, cvar) = pause_complete.as_ref();
            let mut complete = lock.lock().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !*complete {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "worker did not pause in time");
                let (guard, _) = cvar.wait_timeout(complete, remaining).unwrap();
                complete = guard;
            }
        }

        // Step 3: add a third item to memory_doc while paused.
        // The callback is suppressed during pause, so no CommitEvent is sent.
        let v2_json = serde_json::json!({
            "items": [
                {
                    "id": "item-alpha", "subject": "Alpha", "description": "",
                    "status": "pending", "blocks": [], "metadata": {}, "comments": [],
                    "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "item-beta", "subject": "Beta", "description": "",
                    "status": "in-progress", "blocks": [], "metadata": {}, "comments": [],
                    "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "item-gamma", "subject": "Gamma", "description": "",
                    "status": "blocked", "blocks": [], "metadata": {}, "comments": [],
                    "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
                }
            ]
        });
        doc.import_from_json(&v2_json).unwrap();
        doc.commit();

        // Step 4: resume the worker.
        {
            let (lock, cvar) = resume_signal_arc.as_ref();
            let mut resumed = lock.lock().unwrap();
            *resumed = true;
            cvar.notify_one();
        }

        // Wait for the resume render_cycle to complete.
        std::thread::sleep(Duration::from_millis(300));

        // Step 5: verify the SQL tables reflect the 3-item state.
        let conn = db.get().unwrap();
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE block_handle = 'pr_tl_block'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            cnt, 3,
            "TaskList reconcile must run during pause/resume render_cycle; \
             expected 3 rows, got {cnt}"
        );

        // Verify item-gamma (the new item added during pause) is present.
        let gamma_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM tasks \
                 WHERE block_handle = 'pr_tl_block' AND task_item_id = 'item-gamma'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            gamma_exists,
            "item-gamma written during pause must be reconciled on resume"
        );

        cancel.cancel();
        drop(tx);
        handle.join().expect("worker should not panic");
    }
}
