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
