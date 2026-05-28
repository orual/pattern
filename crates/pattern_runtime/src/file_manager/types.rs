// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Shared types for the file manager subsystem.

use std::path::PathBuf;

/// Wire-safe metadata for a single directory entry.
///
/// `FileInfo` is serialized to JSON and returned as the `FileInfo = Text` wire
/// type in `Pattern.File.ListDir` responses. The JSON shape is documented in
/// `effect_decl()` as `{path:Path, size:Int, mtime:Timestamp, is_dir:Bool}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileInfo {
    /// Absolute or relative path to the entry.
    pub path: PathBuf,

    /// File size in bytes. Zero for directories.
    pub size: u64,

    /// Modification time. Serialized as ISO 8601 via jiff's serde impl.
    pub mtime: jiff::Timestamp,

    /// `true` if this entry is a directory.
    pub is_dir: bool,
}
