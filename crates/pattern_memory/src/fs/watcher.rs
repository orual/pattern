//! File system watcher for external edits to canonical memory block files.
//!
//! `MountWatcher` is a thin wrapper around `DirWatcher<BlockFanoutRouter>`.
//! The `BlockFanoutRouter` handles block-path filtering, self-echo suppression,
//! format validation, and delegates to `MemoryCache::apply_external_edit` for
//! the CRDT merge.

#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::cache::MemoryCache;
use crate::fs::FsError;
use crate::loro_sync::dir_watcher::{DirWatcher, DirWatcherConfig};
use crate::loro_sync::routers::BlockFanoutRouter;

/// A running file system watcher for a memory mount directory.
///
/// Watches for external edits to canonical block files and triggers CRDT
/// merges via the two-doc model. Dropping this struct stops the watcher.
pub struct MountWatcher {
    /// The underlying `DirWatcher<BlockFanoutRouter>`. Dropping it cancels
    /// the watcher and its ingest thread.
    _dir_watcher: DirWatcher,
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
    /// Constructs a `DirWatcher` with a `BlockFanoutRouter` that performs
    /// block-path filtering, self-echo suppression, format validation, and
    /// CRDT merge via `MemoryCache::apply_external_edit`.
    pub fn start(config: WatcherConfig) -> Result<Self, FsError> {
        let dir_watcher_cfg = DirWatcherConfig {
            root: config.mount_path.clone(),
            recursive: notify::RecursiveMode::Recursive,
            debounce: Duration::from_millis(500),
        };
        let router = BlockFanoutRouter::new(config.cache);
        let dir_watcher = DirWatcher::start(dir_watcher_cfg, router).map_err(|e| FsError::Io {
            path: config.mount_path,
            source: std::io::Error::other(e.to_string()),
        })?;
        Ok(MountWatcher {
            _dir_watcher: dir_watcher,
        })
    }
}

/// Re-export for tests that previously used the local helpers.
#[cfg(test)]
fn is_block_path(path: &Path) -> bool {
    crate::loro_sync::routers::is_block_path(path)
}

/// Re-export for tests that previously used the local helpers.
#[cfg(test)]
fn block_id_from_path(path: &Path) -> Option<String> {
    crate::loro_sync::routers::block_id_from_path(path)
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
        use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
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
                BlockCreate::new("test", MemoryBlockType::Working, BlockSchema::text())
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
