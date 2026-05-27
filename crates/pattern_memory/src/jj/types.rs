// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Output types for the jj CLI adapter.
//!
//! All structs are deserialized from `json(self) ++ "\n"` template output.
//! Fields are intentionally minimal — we only request what Pattern needs.
//! Structs are tolerant of extra fields jj might add in future versions
//! (`deny_unknown_fields = false`, which is the serde default).

use serde::Deserialize;

/// A single log entry from `jj log -T 'json(self) ++ "\n"'`.
///
/// `jj log` outputs one JSON object per commit. We capture only the fields
/// Pattern uses for VCS history navigation: identity (change_id, commit_id),
/// the commit message, and parent commit IDs.
///
/// The `parents` field contains the commit IDs of the immediate parent(s).
/// A commit with `parents.len() >= 2` is a merge commit (created via
/// `jj new <rev1> <rev2> ...`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct JjLogEntry {
    /// The jj change ID (a content-stable identifier across rewrites).
    pub change_id: String,
    /// The git-compatible commit hash.
    pub commit_id: String,
    /// The commit description (message). May contain a trailing newline.
    pub description: String,
    /// Parent commit IDs. A root commit has zero parents; a merge commit
    /// has two or more.
    #[serde(default)]
    pub parents: Vec<String>,
}

/// The target commit information embedded in a workspace listing.
///
/// `jj workspace list -T 'json(self) ++ "\n"'` outputs a `target` field
/// that is a full commit object. We capture only its commit_id.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct JjWorkspaceTarget {
    /// The commit hash this workspace is currently pointing at.
    pub commit_id: String,
}

/// A workspace entry from `jj workspace list -T 'json(self) ++ "\n"'`.
///
/// Each workspace has a name and a target commit. Pattern uses this to
/// enumerate workspaces when managing multi-workspace Standalone/Sidecar layouts.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct JjWorkspace {
    /// The workspace name (e.g. `"default"`).
    pub name: String,
    /// The commit this workspace's working copy is based on.
    pub target: JjWorkspaceTarget,
}

/// A bookmark entry from `jj bookmark list -T 'json(self) ++ "\n"'`.
///
/// jj 0.40 outputs `target` as an array of commit ID strings (a bookmark can
/// point at multiple targets when in a conflicted state).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct JjBookmark {
    /// The bookmark name.
    pub name: String,
    /// One or more commit IDs this bookmark resolves to. Conflicted bookmarks
    /// have more than one entry; normal bookmarks have exactly one.
    pub target: Vec<String>,
}
