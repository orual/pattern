//! `TextBridge` + `LoroSyncedFile` — opaque-text CRDT file sync.
//!
//! `TextBridge` stores file content as a single `LoroText` under root key
//! `"content"`. `LoroSyncedFile` is a newtype wrapper that presents a
//! file-oriented API without leaking the `SyncedDoc<TextBridge>` generic.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::Receiver;
use loro::LoroDoc;
use smol_str::SmolStr;

use crate::loro_sync::{
    BridgeError, ConflictPolicy, ExternalChangeEvent, LoroDocBridge, LoroSyncError,
    PathFanoutRouter, SyncedDoc, SyncedDocConfig,
};

/// Opaque-text bridge: file content as a single `LoroText` at root key
/// `"content"`. Render returns the text bytes verbatim under the configured
/// extension. Apply-external calls `text.update(content_str)` (Loro's
/// Myers-diff text update).
pub struct TextBridge {
    extension: SmolStr,
}

impl TextBridge {
    /// Construct with a statically-known extension (zero alloc):
    /// `TextBridge::new(SmolStr::new_static("md"))`.
    pub fn new(extension: SmolStr) -> Self {
        Self { extension }
    }

    /// Derive the extension from a path. Falls back to `"txt"` if no extension.
    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(SmolStr::from)
            .unwrap_or_else(|| SmolStr::new_static("txt"));
        Self { extension: ext }
    }
}

impl LoroDocBridge for TextBridge {
    fn render(&self, disk_doc: &LoroDoc) -> Result<(SmolStr, Vec<u8>), BridgeError> {
        let text = disk_doc.get_text("content").to_string();
        Ok((self.extension.clone(), text.into_bytes()))
    }

    fn apply_external(
        &self,
        disk_doc: &LoroDoc,
        content: &[u8],
        path: &Path,
    ) -> Result<(), BridgeError> {
        let s = std::str::from_utf8(content).map_err(|e| BridgeError::Utf8 {
            path: path.to_owned(),
            source: e,
        })?;
        disk_doc
            .get_text("content")
            .update_by_line(s, Default::default())
            .map_err(|e| BridgeError::Loro(format!("text.update failed: {e}")))?;
        Ok(())
    }
}

/// Public file-oriented wrapper around `SyncedDoc<TextBridge>`.
///
/// Keeping this as a newtype (not a `pub type` alias) lets us add
/// file-specific methods without leaking the `SyncedDoc` generic into
/// `FileHandler` signatures. Phase 2's `FileManager` consumes this.
pub struct LoroSyncedFile {
    inner: SyncedDoc<TextBridge>,
}

impl LoroSyncedFile {
    /// Open against a pooled `DirWatcher<PathFanoutRouter>` (production path).
    /// Phase 2's FileManager owns the router.
    pub fn open_with_router(
        path: impl Into<PathBuf>,
        router: &PathFanoutRouter,
    ) -> Result<Self, LoroSyncError> {
        let path: PathBuf = path.into();
        if !path.exists() {
            return Err(LoroSyncError::NotFound(path));
        }
        let bridge = Arc::new(TextBridge::from_path(&path));
        let memory_doc = Arc::new(LoroDoc::new());
        let inner = SyncedDoc::open_with_subscription(
            SyncedDocConfig {
                path,
                memory_doc,
                bridge,
                event_channel_bound: 256,
                // FileManager (Phase 2): surface conflicts rather than silently
                // merging stale-base external writes.
                conflict_policy: ConflictPolicy::RejectAndNotify,
            },
            router,
        )?;
        Ok(Self { inner })
    }

    /// Open with a private per-file watcher (standalone / test usage).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, LoroSyncError> {
        let path: PathBuf = path.into();
        if !path.exists() {
            return Err(LoroSyncError::NotFound(path));
        }
        let bridge = Arc::new(TextBridge::from_path(&path));
        let memory_doc = Arc::new(LoroDoc::new());
        let inner = SyncedDoc::open_standalone(SyncedDocConfig {
            path,
            memory_doc,
            bridge,
            event_channel_bound: 256,
            // Standalone open (tests + one-off usage): surface conflicts.
            // Tests that need AutoMerge semantics open SyncedDoc directly.
            conflict_policy: ConflictPolicy::RejectAndNotify,
        })?;
        Ok(Self { inner })
    }

    /// Direct reference to the underlying `LoroDoc` for CRDT-native edits.
    ///
    /// Agents that need to make incremental edits (e.g., `text.insert(...)`,
    /// `apply_delta(...)`) use this to write directly into the CRDT without
    /// going through the byte-level `write()` API. The `SyncedDoc`'s
    /// local-update subscription on `memory_doc` picks up any commits made
    /// here and propagates them to disk automatically.
    ///
    /// Phase 2's `FileHandler` uses this for incremental edit support.
    pub fn memory_doc(&self) -> &Arc<LoroDoc> {
        self.inner.memory_doc()
    }

    /// Read the current file content as a UTF-8 string.
    pub fn read(&self) -> Result<String, LoroSyncError> {
        let bytes = self.inner.read()?;
        String::from_utf8(bytes).map_err(|e| {
            LoroSyncError::Bridge(BridgeError::Utf8 {
                path: self.inner.path().to_owned(),
                source: e.utf8_error(),
            })
        })
    }

    /// Write UTF-8 content to the file.
    pub fn write(&self, content: &str) -> Result<(), LoroSyncError> {
        self.inner.write(content.as_bytes())
    }

    /// Subscribe to external change notifications.
    pub fn subscribe_external_changes(&self) -> Receiver<ExternalChangeEvent> {
        self.inner.subscribe_external_changes()
    }

    /// Path to the file on disk.
    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Force-apply raw bytes as if they were an external edit, bypassing
    /// the watcher's stale-base conflict check. Used by `File.ForceWrite`
    /// to overwrite the disk version with the agent's content.
    pub fn apply_external_bytes(&self, content: &[u8]) -> Result<(), LoroSyncError> {
        self.inner.apply_external_bytes(content)
    }

    /// Discard uncommitted memory_doc edits, replace with current disk content.
    ///
    /// Recovery path from `FileConflict` when the agent decides to take
    /// the disk version. After reload, `has_unsaved_edits()` returns
    /// `false` and `read()` returns the disk content.
    pub fn reload(&self) -> Result<String, LoroSyncError> {
        let disk_bytes = self.inner.reload()?;
        String::from_utf8(disk_bytes).map_err(|e| {
            LoroSyncError::Bridge(BridgeError::Utf8 {
                path: self.inner.path().to_owned(),
                source: e.utf8_error(),
            })
        })
    }

    /// Returns `true` if `memory_doc` has edits beyond the last successful
    /// local save (i.e., the agent has pending writes not yet rendered to disk).
    pub fn has_unsaved_edits(&self) -> bool {
        self.inner.has_unsaved_edits()
    }

    /// Close the file and stop the watcher. Optional — drop also cleans up.
    pub fn close(self) {
        self.inner.close()
    }

    /// Force `has_unsaved_edits()` to return `true` by clearing the saved
    /// frontier. Test-only — deterministically sets up the conflict path
    /// without relying on timing between the ingest thread and watcher debounce.
    ///
    /// Available under `#[cfg(test)]` (unit tests) and when the `test-support`
    /// feature is enabled (integration tests in `tests/`).
    /// Never call this in production code.
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_saved_frontier_for_test(&self) {
        self.inner.clear_saved_frontier_for_test();
    }
}
