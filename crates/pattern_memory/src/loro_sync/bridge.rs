// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Bridge trait for schema-specific CRDT document adapters.
//!
//! A bridge is a stateless adapter between a LoroDoc and a concrete on-disk
//! format. `TextBridge` handles opaque text files; `BlockSchemaBridge` (Task 6)
//! handles typed memory-block schemas.

use std::path::Path;

use loro::LoroDoc;
use smol_str::SmolStr;

/// Pluggable schema/format adapter for a `SyncedDoc`.
///
/// One bridge per concrete representation: `TextBridge` for opaque file
/// content, `BlockSchemaBridge` for memory-block schemas. Bridges are
/// stateless adapters — schema configuration lives on `Self`; per-doc
/// state lives on the SyncedDoc.
pub trait LoroDocBridge: Send + Sync + 'static {
    /// Render `disk_doc` to the canonical on-disk bytes. Returns
    /// `(file_extension_without_dot, bytes)`. The extension is `SmolStr`
    /// so bridges can use `SmolStr::new_static("md")` with zero allocation
    /// for compile-time-known constants.
    fn render(&self, disk_doc: &LoroDoc) -> Result<(SmolStr, Vec<u8>), BridgeError>;

    /// Apply external file `content` to `disk_doc` as Loro operations.
    /// `path` is diagnostic context only. Caller (SyncedDoc) handles
    /// exporting disk_doc's new ops and importing into memory_doc.
    fn apply_external(
        &self,
        disk_doc: &LoroDoc,
        content: &[u8],
        path: &Path,
    ) -> Result<(), BridgeError>;
}

/// Errors produced by bridge operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BridgeError {
    /// The file contained bytes that are not valid UTF-8.
    #[error("invalid utf-8 from file {path}: {source}")]
    Utf8 {
        path: std::path::PathBuf,
        source: std::str::Utf8Error,
    },
    /// A format-specific parse failed (KDL, JSONL, etc.).
    #[error("parse failed for {path}: {message}")]
    Parse {
        path: std::path::PathBuf,
        message: String,
    },
    /// A loro operation failed (e.g. `text.update`).
    #[error("loro operation failed: {0}")]
    Loro(String),
    /// Rendering to the canonical bytes failed.
    #[error("render failed: {0}")]
    Render(String),
}
