//! Parser for `.pattern-plugin/marketplace.kdl` — multi-plugin repo manifests.
//!
//! A marketplace.kdl at a repo root declares which subdirs are plugins, letting
//! one repo ship multiple plugins (e.g. pattern's first-party bundle: discord,
//! bsky-push, learning-opportunities-via-OOP) installable as a unit.
//!
//! Schema (v1):
//! ```kdl
//! plugin "discord" path="plugins/discord"
//! plugin "bsky-push" path="plugins/bsky-push"
//! ```
//!
//! Each entry's `path` is relative to the repo root + points at a subdir
//! containing manifest.kdl. Install processes each entry by cd-ing in,
//! parsing manifest.kdl, running cargo build (or validating prebuilt), copying
//! binary + standard layout + extras to cache, running --pattern-plugin-init.
//!
//! v1 behaviour: install ALL entries (no per-plugin granularity). v1.5 will add
//! URL-fragment selection (`<url>#<plugin-id>`).

use std::path::{Path, PathBuf};

use pattern_core::plugin::ManifestError;

/// One plugin entry in a marketplace.kdl.
#[derive(Debug, Clone)]
pub struct MarketplaceEntry {
    /// Plugin id (matches the `name` declared in the subdir's manifest.kdl).
    pub plugin_id: String,
    /// Subdir path relative to the marketplace.kdl location (= repo root).
    pub path: PathBuf,
}

/// A parsed marketplace.kdl file.
#[derive(Debug, Clone, Default)]
pub struct Marketplace {
    pub plugins: Vec<MarketplaceEntry>,
}

/// Parse marketplace.kdl from a file path.
pub fn from_kdl_file(path: &Path) -> Result<Marketplace, ManifestError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let doc: kdl::KdlDocument = raw.parse().map_err(|e: kdl::KdlError| ManifestError::Kdl {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    from_kdl_doc(&doc, path)
}

/// Parse from an already-parsed KDL document.
pub fn from_kdl_doc(doc: &kdl::KdlDocument, path: &Path) -> Result<Marketplace, ManifestError> {
    let mut mp = Marketplace::default();

    for node in doc.nodes() {
        if node.name().value() != "plugin" {
            tracing::debug!(node = %node.name().value(), "unknown marketplace node — skipping");
            continue;
        }

        // First positional arg is the plugin id.
        let plugin_id = node
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .and_then(|e| e.value().as_string())
            .map(String::from)
            .ok_or_else(|| ManifestError::MissingField {
                field: "plugin id (positional string arg)",
                path: path.to_path_buf(),
            })?;

        // Named `path="..."` entry.
        let path_str = node
            .entries()
            .iter()
            .find(|e| e.name().map(|n| n.value()) == Some("path"))
            .and_then(|e| e.value().as_string())
            .ok_or_else(|| ManifestError::MissingField {
                field: "path",
                path: path.to_path_buf(),
            })?;

        mp.plugins.push(MarketplaceEntry {
            plugin_id,
            path: PathBuf::from(path_str),
        });
    }

    Ok(mp)
}

/// Find a marketplace.kdl at the conventional location relative to a repo root.
/// Returns `Some(path)` if `<repo>/.pattern-plugin/marketplace.kdl` exists.
pub fn discover(repo_root: &Path) -> Option<PathBuf> {
    let p = repo_root.join(".pattern-plugin").join("marketplace.kdl");
    p.exists().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_marketplace() {
        let raw = r#"
plugin "discord" path="plugins/discord"
plugin "bsky-push" path="plugins/bsky-push"
        "#;
        let doc: kdl::KdlDocument = raw.parse().expect("parse kdl");
        let mp = from_kdl_doc(&doc, Path::new("<test>")).expect("parse marketplace");
        assert_eq!(mp.plugins.len(), 2);
        assert_eq!(mp.plugins[0].plugin_id, "discord");
        assert_eq!(mp.plugins[0].path, PathBuf::from("plugins/discord"));
        assert_eq!(mp.plugins[1].plugin_id, "bsky-push");
    }

    #[test]
    fn missing_path_errors() {
        let raw = r#"plugin "discord""#;
        let doc: kdl::KdlDocument = raw.parse().expect("parse kdl");
        let err = from_kdl_doc(&doc, Path::new("<test>")).unwrap_err();
        assert!(matches!(err, ManifestError::MissingField { field: "path", .. }));
    }
}
