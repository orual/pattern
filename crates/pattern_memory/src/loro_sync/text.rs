// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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

use std::sync::Mutex as StdMutex;

/// Cached mapping from line numbers to unicode character offsets.
///
/// Built lazily from the LoroText content. Invalidated on any edit;
/// rebuilt on next access. `line_starts[i]` is the unicode char offset
/// of the start of line `i` (0-indexed).
#[derive(Debug, Clone)]
pub struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// Build a line index from a string, counting unicode scalar positions.
    pub fn build(text: &str) -> Self {
        let mut starts = vec![0usize];
        let mut char_offset = 0usize;
        for ch in text.chars() {
            char_offset += 1;
            if ch == '\n' {
                starts.push(char_offset);
            }
        }
        Self {
            line_starts: starts,
        }
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Unicode char offset of the start of `line` (1-indexed).
    /// Returns None if line is out of range.
    pub fn line_start(&self, line: usize) -> Option<usize> {
        if line == 0 || line > self.line_starts.len() {
            None
        } else {
            Some(self.line_starts[line - 1])
        }
    }

    /// Unicode char offset of the end of `line` (1-indexed).
    /// End is the position just past the newline (or the end of text for the last line).
    pub fn line_end(&self, line: usize, total_chars: usize) -> Option<usize> {
        if line == 0 || line > self.line_starts.len() {
            None
        } else if line < self.line_starts.len() {
            // Next line starts at this offset; the newline char is at offset - 1
            Some(self.line_starts[line])
        } else {
            // Last line: end is total length
            Some(total_chars)
        }
    }
}

/// Public file-oriented wrapper around `SyncedDoc<TextBridge>`.
///
/// Keeping this as a newtype (not a `pub type` alias) lets us add
/// file-specific methods without leaking the `SyncedDoc` generic into
/// `FileHandler` signatures. Phase 2's `FileManager` consumes this.
pub struct LoroSyncedFile {
    inner: SyncedDoc<TextBridge>,
    /// Lazily-computed line index. `None` means needs rebuild.
    line_index: Arc<StdMutex<Option<LineIndex>>>,
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
        let doc = LoroDoc::new();
        let inner = SyncedDoc::open_with_subscription(
            SyncedDocConfig {
                path,
                doc,
                bridge,
                event_channel_bound: 256,
                conflict_policy: ConflictPolicy::RejectAndNotify,
            },
            router,
        )?;
        Ok(Self {
            inner,
            line_index: Arc::new(StdMutex::new(None)),
        })
    }

    /// Open with a private per-file watcher (standalone / test usage).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, LoroSyncError> {
        let path: PathBuf = path.into();
        if !path.exists() {
            return Err(LoroSyncError::NotFound(path));
        }
        let bridge = Arc::new(TextBridge::from_path(&path));
        let doc = LoroDoc::new();
        let inner = SyncedDoc::open_standalone(SyncedDocConfig {
            path,
            doc,
            bridge,
            event_channel_bound: 256,
            conflict_policy: ConflictPolicy::RejectAndNotify,
        })?;
        Ok(Self {
            inner,
            line_index: Arc::new(StdMutex::new(None)),
        })
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
    /// Direct reference to the underlying `LoroDoc` for CRDT-native edits.
    pub fn doc(&self) -> &LoroDoc {
        self.inner.doc()
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

    /// Write UTF-8 content to the file. Applies content as a CRDT update
    /// (Myers-diff via bridge) then atomic-writes to disk synchronously.
    /// Uses the agent-write path (write_bytes), not the watcher path
    /// (apply_external_bytes), so bookkeeping stays correct.
    pub fn write(&self, content: &str) -> Result<(), LoroSyncError> {
        self.invalidate_line_index();
        self.inner.write_bytes(content.as_bytes())?;
        Ok(())
    }

    /// Subscribe to external change notifications.
    pub fn subscribe_external_changes(&self) -> Receiver<ExternalChangeEvent> {
        self.inner.subscribe_external_changes()
    }

    /// Subscribe to disk-write notifications. Each successful render +
    /// `atomic_write` (whether triggered by a local CRDT update, a sync
    /// write, or an external edit reconciled into disk_doc) fires one
    /// `WriteNotification` per live subscriber. Used by callers (and
    /// tests) that need to await the async ingest pipeline rather than
    /// poll the file.
    pub fn subscribe_writes(&self) -> Receiver<crate::loro_sync::synced_doc::WriteNotification> {
        self.inner.subscribe_writes()
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

    /// Force-apply content as the authoritative state. Clears any
    /// pending-conflict flag and atomic-writes `content` to disk. Used by
    /// the conflict-resolution path (`FileManager::force_write`).
    pub fn force_apply_external_bytes(&self, content: &[u8]) -> Result<(), LoroSyncError> {
        self.invalidate_line_index();
        self.inner.force_apply_external_bytes(content)
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

    // ---- Line-level edit operations ----------------------------------------

    /// Ensure the line index is built and return a clone.
    fn ensure_line_index(&self) -> LineIndex {
        let mut guard = self.line_index.lock().unwrap();
        if let Some(ref idx) = *guard {
            return idx.clone();
        }
        let text = self.inner.doc().get_text("content").to_string();
        let idx = LineIndex::build(&text);
        *guard = Some(idx.clone());
        idx
    }

    /// Invalidate the cached line index (call after any edit).
    fn invalidate_line_index(&self) {
        *self.line_index.lock().unwrap() = None;
    }

    /// Insert content after line `after_line` (1-indexed).
    /// Line 0 inserts at the very beginning of the file.
    /// The content string may contain newlines.
    pub fn insert_lines(&self, after_line: usize, content: &str) -> Result<(), LoroSyncError> {
        let idx = self.ensure_line_index();
        let text = self.inner.doc().get_text("content");
        let total_chars = text.len_unicode();

        let insert_pos = if after_line == 0 {
            0
        } else if after_line >= idx.line_count() {
            // After the last line: append at end
            total_chars
        } else {
            // Insert at the start of the next line (= end of line after_line)
            idx.line_end(after_line, total_chars).ok_or_else(|| {
                LoroSyncError::Other(format!(
                    "line {after_line} out of range (file has {} lines)",
                    idx.line_count()
                ))
            })?
        };

        // Wrap the inserted content with a separating newline so the
        // surrounding lines stay distinct after the insert.
        let to_insert = if after_line == 0 && total_chars > 0 {
            // Inserting at top of non-empty file: trailing newline
            // separates the inserted block from line 1.
            format!("{content}\n")
        } else if after_line >= idx.line_count() && total_chars > 0 {
            // Appending past the last line: leading newline starts a
            // fresh line after whatever the file ended with.
            format!("\n{content}")
        } else {
            // Mid-file insert at the start of `after_line + 1`: append
            // a trailing newline so the inserted content gets its own
            // line and doesn't merge with the next existing line.
            format!("{content}\n")
        };

        text.insert(insert_pos, &to_insert)
            .map_err(|e| LoroSyncError::Other(format!("insert failed: {e}")))?;
        self.inner.doc().commit();
        self.invalidate_line_index();
        // Single-doc + explicit-flush: caller methods on LoroSyncedFile
        // synchronously persist to disk after the CRDT op completes.
        self.inner.write_local()?;
        Ok(())
    }

    /// Replace lines `from`..`to` (1-indexed, inclusive) with new content.
    /// The replacement may have a different number of lines.
    pub fn replace_lines(
        &self,
        from: usize,
        to: usize,
        content: &str,
    ) -> Result<(), LoroSyncError> {
        if from < 1 || from > to {
            return Err(LoroSyncError::Other(format!(
                "invalid line range {from}..{to}"
            )));
        }
        let idx = self.ensure_line_index();
        if from > idx.line_count() {
            return Err(LoroSyncError::Other(format!(
                "line {from} out of range (file has {} lines)",
                idx.line_count()
            )));
        }
        let text = self.inner.doc().get_text("content");
        let total_chars = text.len_unicode();

        let start_pos = idx
            .line_start(from)
            .ok_or_else(|| LoroSyncError::Other(format!("line {from} out of range")))?;
        let end_pos = idx
            .line_end(to.min(idx.line_count()), total_chars)
            .ok_or_else(|| LoroSyncError::Other(format!("line {to} out of range")))?;
        let delete_len = end_pos - start_pos;

        // Mirror `insert_lines`: the deleted span typically ended with a
        // newline (line N's terminator). Re-add one after the
        // replacement so the line that follows stays separate, unless
        // the replacement already ends with a newline, or we're
        // replacing through the last line of the file (no trailing
        // newline existed in the deleted span).
        let replaced_through_last = to >= idx.line_count();
        let replacement = if replaced_through_last || content.ends_with('\n') {
            content.to_string()
        } else {
            format!("{content}\n")
        };

        text.splice(start_pos, delete_len, &replacement)
            .map_err(|e| LoroSyncError::Other(format!("splice failed: {e}")))?;
        self.inner.doc().commit();
        self.invalidate_line_index();
        self.inner.write_local()?;
        Ok(())
    }

    /// Delete lines `from`..`to` (1-indexed, inclusive).
    pub fn delete_lines(&self, from: usize, to: usize) -> Result<(), LoroSyncError> {
        if from < 1 || from > to {
            return Err(LoroSyncError::Other(format!(
                "invalid line range {from}..{to}"
            )));
        }
        let idx = self.ensure_line_index();
        if from > idx.line_count() {
            return Err(LoroSyncError::Other(format!(
                "line {from} out of range (file has {} lines)",
                idx.line_count()
            )));
        }
        let text = self.inner.doc().get_text("content");
        let total_chars = text.len_unicode();

        let start_pos = idx
            .line_start(from)
            .ok_or_else(|| LoroSyncError::Other(format!("line {from} out of range")))?;
        let end_pos = idx
            .line_end(to.min(idx.line_count()), total_chars)
            .ok_or_else(|| LoroSyncError::Other(format!("line {to} out of range")))?;
        let delete_len = end_pos - start_pos;

        text.splice(start_pos, delete_len, "")
            .map_err(|e| LoroSyncError::Other(format!("splice failed: {e}")))?;
        self.inner.doc().commit();
        self.invalidate_line_index();
        self.inner.write_local()?;
        Ok(())
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
