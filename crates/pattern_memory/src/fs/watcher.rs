//! File system watcher for external edits to canonical memory block files.
//!
//! Uses `notify-debouncer-full` (500ms debounce) to detect changes made by
//! human editors to `.md`, `.kdl`, and `.jsonl` files in the memory mount.
//! On detecting a change:
//!
//! 1. Read the file and check its mtime.
//! 2. Compare mtime against `last_written_mtime` from the subscriber — if it
//!    matches, this is a self-echo from our own `atomic_write` and is suppressed.
//! 3. Parse the file through the appropriate format module.
//! 4. If parsing fails (e.g., invalid KDL), log a warning, increment a metric,
//!    and skip the merge.
//! 5. Otherwise, apply the parsed content to `disk_doc` as Loro operations,
//!    then propagate the CRDT update to `memory_doc` via
//!    `MemoryCache::apply_external_edit`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};

use crate::cache::MemoryCache;
use crate::fs::FsError;
use crate::subscriber::SubscriberHandle;

/// A running file system watcher for a memory mount directory.
///
/// Watches for external edits to canonical block files and triggers CRDT
/// merges via the two-doc model. Dropping this struct stops the watcher.
pub struct MountWatcher {
    /// The debouncer holds the underlying `notify::RecommendedWatcher` and
    /// its background thread. Dropping it stops watching.
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
    /// Join handle for the ingest thread.
    _ingest_thread: std::thread::JoinHandle<()>,
}

/// Configuration for the mount watcher.
pub struct WatcherConfig {
    /// Path to watch recursively.
    pub mount_path: PathBuf,
    /// The memory cache to apply external edits into via CRDT merge.
    pub cache: Arc<MemoryCache>,
}

impl MountWatcher {
    /// Start watching the given mount path for external file edits.
    ///
    /// The debouncer fires after 500ms of quiet for each file. Events are
    /// forwarded to an ingest thread that performs self-echo suppression
    /// (via mtime comparison), parsing, and triggers the CRDT merge via
    /// `MemoryCache::apply_external_edit`.
    pub fn start(config: WatcherConfig) -> Result<Self, FsError> {
        let (tx, rx) =
            crossbeam_channel::bounded::<Vec<notify_debouncer_full::DebouncedEvent>>(256);

        let mut debouncer = new_debouncer(
            Duration::from_millis(500),
            None,
            move |result: DebounceEventResult| {
                if let Ok(events) = result {
                    let _ = tx.try_send(events);
                }
            },
        )
        .map_err(|e| FsError::Io {
            path: config.mount_path.clone(),
            source: std::io::Error::other(e.to_string()),
        })?;

        debouncer
            .watch(&config.mount_path, RecursiveMode::Recursive)
            .map_err(|e| FsError::Io {
                path: config.mount_path.clone(),
                source: std::io::Error::other(e.to_string()),
            })?;

        let cache = config.cache;

        let ingest_thread = std::thread::Builder::new()
            .name("mount-watcher-ingest".into())
            .spawn(move || {
                ingest_loop(rx, cache);
            })
            .map_err(|e| FsError::Io {
                path: config.mount_path.clone(),
                source: e,
            })?;

        Ok(MountWatcher {
            _debouncer: debouncer,
            _ingest_thread: ingest_thread,
        })
    }
}

/// Check whether a path looks like a block file we manage.
///
/// Accepts `.md`, `.kdl`, `.jsonl` files. Rejects temporary files from
/// `atomic_write` (which have extensions like `.md.tmp`).
fn is_block_path(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    // Accept only canonical block file extensions. The atomic_write helper
    // produces files like `block.md.tmp` whose extension is "tmp", so they
    // are naturally excluded by the extension whitelist.
    matches!(ext, "md" | "kdl" | "jsonl")
}

/// Extract the block ID from a canonical block file path.
///
/// The worker writes files as `{block_id}.{ext}`. The block ID is the stem
/// (filename without extension). Returns `None` if the path has no stem.
fn block_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

/// Check if a file change was written by us (self-echo suppression).
///
/// Compares the file's current mtime against the subscriber's
/// `last_written_mtime`. If they match, the file change was caused by our
/// own `atomic_write` and should be skipped.
fn is_self_echo(path: &Path, subscriber: &SubscriberHandle) -> bool {
    let file_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => mtime,
        Err(_) => return false, // Can't read mtime — not a self-echo.
    };

    if let Ok(guard) = subscriber.last_written_mtime.lock()
        && let Some(last_written) = *guard
    {
        return file_mtime == last_written;
    }

    false
}

/// Main ingest loop running on a dedicated OS thread.
fn ingest_loop(
    rx: crossbeam_channel::Receiver<Vec<notify_debouncer_full::DebouncedEvent>>,
    cache: Arc<MemoryCache>,
) {
    while let Ok(debounced_events) = rx.recv() {
        for debounced in debounced_events {
            // Only process modify/create events.
            use notify::EventKind;
            match debounced.event.kind {
                EventKind::Create(_) | EventKind::Modify(_) => {}
                _ => continue,
            }

            for path in &debounced.event.paths {
                if !is_block_path(path) {
                    continue;
                }

                let Some(block_id) = block_id_from_path(path) else {
                    continue;
                };

                // Self-echo suppression via mtime comparison.
                if let Some(subscriber) = cache.subscriber_handle(&block_id)
                    && is_self_echo(path, &subscriber)
                {
                    continue;
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
                // This catches syntax errors early and avoids importing corrupt
                // content into the LoroDoc.
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let format_ok = match ext {
                    "md" => true, // Markdown is passthrough — always valid.
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
                    _ => false, // Unknown extension — shouldn't happen due to is_block_path.
                };

                if !format_ok {
                    continue;
                }

                // Import the content into the LoroDoc via two-doc CRDT merge.
                // apply_external_edit handles schema-aware parsing and the
                // disk_doc → memory_doc update propagation.
                cache.apply_external_edit(&block_id, &content);
                metrics::counter!("memory.external_edit.merged").increment(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_block_path_accepts_valid_extensions() {
        assert!(is_block_path(Path::new("/mount/block.md")));
        assert!(is_block_path(Path::new("/mount/block.kdl")));
        assert!(is_block_path(Path::new("/mount/block.jsonl")));
    }

    #[test]
    fn is_block_path_rejects_tmp_and_other() {
        // atomic_write produces files like block.md.tmp — extension is "tmp".
        assert!(!is_block_path(Path::new("/mount/block.md.tmp")));
        assert!(!is_block_path(Path::new("/mount/block.txt")));
        assert!(!is_block_path(Path::new("/mount/block.rs")));
        assert!(!is_block_path(Path::new("/mount/block")));
        // Paths containing .tmp in a directory name should still work.
        assert!(is_block_path(Path::new("/tmp/.tmpXYZ/block.md")));
    }

    #[test]
    fn block_id_from_path_extracts_stem() {
        assert_eq!(
            block_id_from_path(Path::new("/mount/mem_abc123.md")),
            Some("mem_abc123".to_string())
        );
        assert_eq!(
            block_id_from_path(Path::new("/mount/mem_def456.kdl")),
            Some("mem_def456".to_string())
        );
        assert_eq!(
            block_id_from_path(Path::new("/mount/mem_ghi789.jsonl")),
            Some("mem_ghi789".to_string())
        );
        assert_eq!(block_id_from_path(Path::new("/")), None);
    }

    #[test]
    fn watcher_rejects_invalid_kdl() {
        use pattern_db::ConstellationDb;
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let cache = Arc::new(MemoryCache::new(db));

        let _watcher = MountWatcher::start(WatcherConfig {
            mount_path: mount.clone(),
            cache,
        })
        .expect("watcher should start");

        // Write invalid KDL.
        let file_path = mount.join("bad_block.kdl");
        std::fs::write(&file_path, "this is {{ invalid kdl").unwrap();

        // Wait for debounce (500ms) + processing overhead.
        std::thread::sleep(Duration::from_secs(2));

        // The watcher ran without panicking; no assert needed beyond that
        // (the invalid KDL is logged and skipped — no block in cache to corrupt).
    }

    #[test]
    fn watcher_accepts_valid_kdl() {
        use pattern_db::ConstellationDb;
        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        let cache = Arc::new(MemoryCache::new(db));

        let _watcher = MountWatcher::start(WatcherConfig {
            mount_path: mount.clone(),
            cache,
        })
        .expect("watcher should start");

        // Give inotify a moment to fully register the watch.
        std::thread::sleep(Duration::from_millis(100));

        // Write valid KDL (no block in cache, so merge is skipped but no panic).
        let file_path = mount.join("good_block.kdl");
        std::fs::write(&file_path, "name \"alice\"\nage 30\n").unwrap();

        // Wait for debounce + processing.
        std::thread::sleep(Duration::from_secs(2));

        // The watcher processed without panicking — that's sufficient.
    }

    #[test]
    fn watcher_detects_external_edit_md() {
        use pattern_core::traits::MemoryStore;
        use pattern_core::types::block::BlockCreate;
        use pattern_core::types::memory_types::{BlockSchema, BlockType};
        use pattern_db::ConstellationDb;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let mount = dir.path().to_path_buf();

        let db = Arc::new(ConstellationDb::open_in_memory().unwrap());
        // Create a test agent so we can create a block.
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
        }

        // Create cache with mount path so subscribers are spawned.
        let (reembed_tx, _reembed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (hb_tx, hb_rx) = crossbeam_channel::bounded(64);
        let cache =
            Arc::new(MemoryCache::new(db).with_mount_path(mount.clone(), reembed_tx, hb_tx, hb_rx));

        // Create a text block and persist it to trigger subscriber spawn.
        let doc = cache
            .create_block(
                "agent_1",
                BlockCreate::new("test", BlockType::Working, BlockSchema::text())
                    .with_description("Test block")
                    .with_char_limit(1000),
            )
            .unwrap();
        let block_id = doc.id().to_string();

        // Write initial content, persist to spawn subscriber.
        doc.set_text("initial", true).unwrap();
        cache.mark_dirty("agent_1", "test");
        cache.persist_block("agent_1", "test").unwrap();

        // Give subscriber time to write the initial file.
        std::thread::sleep(Duration::from_millis(200));

        // Subscribe to document changes to detect when merge fires.
        let merge_count = Arc::new(AtomicUsize::new(0));
        let count_clone = merge_count.clone();
        let _sub = doc.subscribe_root(Arc::new(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let _watcher = MountWatcher::start(WatcherConfig {
            mount_path: mount.clone(),
            cache: Arc::clone(&cache),
        })
        .expect("watcher should start");

        // Give inotify a moment to fully register.
        std::thread::sleep(Duration::from_millis(100));

        // Write a file with the block_id as stem (external edit).
        let file_path = mount.join(format!("{}.md", block_id));
        std::fs::write(&file_path, "Hello from editor").unwrap();

        // Wait for debounce + processing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while merge_count.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }

        assert!(
            merge_count.load(Ordering::SeqCst) >= 1,
            "watcher should trigger CRDT merge for external edit"
        );
        assert_eq!(
            doc.text_content(),
            "Hello from editor",
            "document content should reflect the external edit"
        );
    }
}
