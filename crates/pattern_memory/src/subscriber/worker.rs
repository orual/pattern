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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::Receiver;
use loro::LoroDoc;
use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::BlockSchema;
use pattern_db::ConstellationDb;
use tokio_util::sync::CancellationToken;

use crate::fs::kdl::TopShape;
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
    /// Base path for canonical file output.
    pub mount_path: Arc<PathBuf>,
    /// The disk_doc (forked from memory_doc). All rendering is done from
    /// this doc, which is kept in sync via Loro update byte imports.
    pub disk_doc: Arc<LoroDoc>,
    /// The StructuredDocument (memory_doc), used only for FTS preview
    /// rendering (which needs the human-readable representation).
    pub doc: StructuredDocument,
    /// Shared mtime tracker for self-echo suppression. Updated after each
    /// successful atomic_write so the watcher can skip re-importing files
    /// we wrote ourselves.
    pub last_written_mtime: Arc<Mutex<Option<SystemTime>>>,
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
        mount_path,
        disk_doc,
        doc,
        last_written_mtime,
    } = config;

    let mut last_emitted_hash: Option<[u8; 32]> = None;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Block waiting for an event or send a heartbeat on timeout.
        let first_event: Option<CommitEvent>;
        crossbeam_channel::select! {
            recv(rx) -> msg => {
                match msg {
                    Ok(ev) => { first_event = Some(ev); }
                    Err(_) => break, // Sender dropped — unload in progress.
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

        // Import the first event's bytes into disk_doc.
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

        // Render canonical content from disk_doc.
        let (ext, canonical_bytes) = match render_canonical_from_disk_doc(&disk_doc, &schema) {
            Ok(pair) => pair,
            Err(e) => {
                metrics::counter!("memory.subscriber.render_failed").increment(1);
                tracing::error!(
                    block_id = %block_id, error = %e,
                    "canonical render failed; skipping emission cycle"
                );
                continue;
            }
        };
        let new_hash: [u8; 32] = blake3::hash(&canonical_bytes).into();

        // Hash-based echo suppression: skip if the content hasn't changed.
        if Some(new_hash) == last_emitted_hash {
            let _ = heartbeat_tx.try_send(Heartbeat {
                block_id: block_id.clone(),
                at: Instant::now(),
            });
            continue;
        }

        // Emit canonical file.
        let file_path = mount_path.join(format!("{}.{}", block_id, ext));
        if let Err(e) = crate::fs::atomic_write(&file_path, &canonical_bytes) {
            metrics::counter!("memory.subscriber.fs_write_failed").increment(1);
            tracing::error!(path = ?file_path, error = %e, "atomic_write failed");
            continue;
        }

        // Record the mtime of the file we just wrote for self-echo suppression.
        if let Ok(metadata) = std::fs::metadata(&file_path)
            && let Ok(mtime) = metadata.modified()
            && let Ok(mut guard) = last_written_mtime.lock()
        {
            *guard = Some(mtime);
        }

        // The FTS5 preview column stores the human-readable render regardless
        // of the on-disk format. Use doc.render() which produces the LLM-context
        // representation (not the raw canonical bytes for KDL/JSONL).
        let preview = doc.render();

        // Update FTS5 row via the content_preview column (triggers handle FTS).
        match db.get() {
            Ok(conn) => {
                let preview_str = if preview.is_empty() {
                    None
                } else {
                    Some(preview.as_str())
                };
                if let Err(e) =
                    pattern_db::queries::update_block_preview(&conn, &block_id, preview_str)
                {
                    metrics::counter!("memory.subscriber.fts_update_failed").increment(1);
                    tracing::error!(
                        block_id = %block_id, error = %e, "FTS5 update failed"
                    );
                }
            }
            Err(e) => {
                metrics::counter!("memory.subscriber.pool_exhausted").increment(1);
                tracing::error!(error = %e, "DB pool get failed");
            }
        }

        // Queue a re-embed request unconditionally on hash change.
        // This is acceptable overhead: the re-embed consumer silently drops
        // requests when no embedding provider is configured, and the clone
        // cost of canonical_bytes is negligible for typical block sizes.
        let _ = reembed_tx.send(ReembedRequest {
            block_id: block_id.clone(),
            canonical_bytes: canonical_bytes.clone(),
            content_hash: new_hash,
        });

        last_emitted_hash = Some(new_hash);

        let _ = heartbeat_tx.try_send(Heartbeat {
            block_id: block_id.clone(),
            at: Instant::now(),
        });
    }
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
        let disk_doc = Arc::new(doc.inner().fork());
        let last_written_mtime = Arc::new(Mutex::new(None));

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
                disk_doc,
                doc,
                last_written_mtime,
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
        let disk_doc = Arc::new(doc.inner().fork());
        let last_written_mtime = Arc::new(Mutex::new(None));

        let cancel_clone = cancel.clone();
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "test_block".to_string(),
                schema: BlockSchema::text(),
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: Arc::new(dir.path().to_path_buf()),
                disk_doc,
                doc,
                last_written_mtime,
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
        let disk_doc = Arc::new(doc.inner().fork());
        let last_written_mtime = Arc::new(Mutex::new(None));

        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "test_block".to_string(),
                schema: BlockSchema::text(),
                rx,
                cancel,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: Arc::new(dir.path().to_path_buf()),
                disk_doc,
                doc,
                last_written_mtime,
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
        let disk_doc = Arc::new(doc.inner().fork());
        let last_written_mtime = Arc::new(Mutex::new(None));

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
                schema: BlockSchema::text(),
                rx,
                cancel: cancel_clone,
                db: db.clone(),
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: mount_clone,
                disk_doc,
                doc,
                last_written_mtime,
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
        let disk_doc = Arc::new(doc.inner().fork());
        let last_written_mtime = Arc::new(Mutex::new(None));

        // Capture the update bytes.
        let update_bytes = {
            let vv_before = doc.inner().oplog_vv();
            doc.set_text("Same content", true).unwrap();
            doc.inner()
                .export(loro::ExportMode::updates(&vv_before))
                .unwrap()
        };

        let cancel_clone = cancel.clone();
        let handle = std::thread::spawn(move || {
            run_subscriber(WorkerConfig {
                block_id: "echo_block".to_string(),
                schema: BlockSchema::text(),
                rx,
                cancel: cancel_clone,
                db,
                reembed_tx,
                heartbeat_tx: hb_tx,
                mount_path: Arc::new(dir.path().to_path_buf()),
                disk_doc,
                doc,
                last_written_mtime,
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
}
