// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Plugin scope: where a plugin is pinned/discovered.

/// Where a plugin lives in the precedence hierarchy.
///
/// `Project > Global > Ambient` for resolution. Within `Project`,
/// `private` vs shared is a storage distinction (private is gitignored)
/// not a precedence distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[derive(serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum PluginScope {
    /// Pinned in <project>/.pattern/{shared,private}/plugins.kdl.
    Project { private: bool },
    /// Pinned in ~/.pattern/plugins/registry.kdl.
    Global,
    /// On-disk in ~/.pattern/plugins/<id>/ but not pinned in any registry.
    Ambient,
}
