//! Project registry mapping project paths to standalone-mode project IDs.
//!
//! Standalone mode stores Pattern data at `<PATTERN_HOME>/projects/<id>/`,
//! deliberately outside the project repo. The registry at
//! `<PATTERN_HOME>/projects.kdl` records which project paths belong to
//! which project IDs so commands launched from a project path can
//! resolve the right standalone mount without requiring the user to
//! remember and pass the ID every time.
//!
//! # Multi-path mapping
//!
//! One project ID can map to many paths. This supports jj workspaces and
//! persistent forks: a fork's workspace lives at a different path than
//! the primary project root, but should attach to the same Pattern
//! state. Fork creation calls [`ProjectRegistry::add_path`] to record
//! the new path under the existing project ID.
//!
//! # Auto-derived IDs
//!
//! When the user does not supply `--project-id`, `register_project`
//! derives one by slugifying the directory basename (lowercase ASCII
//! alphanumeric + hyphens) and appending `-N` on collision. IDs are
//! readable so users can recognise their project under
//! `<PATTERN_HOME>/projects/`.
//!
//! # File format
//!
//! ```kdl
//! project "my-project" created-at="2026-04-29T12:34:56Z" {
//!     path "/home/orual/projects/my-project"
//!     path "/home/orual/projects/my-project-fork-1"
//! }
//! ```

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode};
use miette::Diagnostic;
use thiserror::Error;

use crate::PatternPaths;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the project registry.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum RegistryError {
    /// The registry file could not be read or written.
    #[error("could not access registry at {path}: {source}")]
    #[diagnostic(code(pattern_memory::projects::io_error))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The registry file exists but contains invalid KDL.
    #[error("could not parse registry at {path}: {message}")]
    #[diagnostic(code(pattern_memory::projects::parse_error))]
    Parse { path: PathBuf, message: String },

    /// A path was already registered under a different project ID.
    #[error(
        "path {path} is already registered under project {existing_id:?}, \
         cannot re-register under {requested_id:?}"
    )]
    #[diagnostic(code(pattern_memory::projects::path_already_registered))]
    PathAlreadyRegistered {
        path: PathBuf,
        existing_id: String,
        requested_id: String,
    },

    /// Attempt to add a path to an unknown project ID.
    #[error("project id {id:?} not found in registry")]
    #[diagnostic(code(pattern_memory::projects::unknown_project))]
    UnknownProject { id: String },
}

// ---------------------------------------------------------------------------
// ProjectEntry + ProjectRegistry
// ---------------------------------------------------------------------------

/// One entry in the project registry: an ID plus the set of paths that
/// resolve to it.
#[derive(Debug, Clone)]
pub struct ProjectEntry {
    pub id: String,
    pub paths: Vec<PathBuf>,
    pub created_at: jiff::Timestamp,
}

/// In-memory representation of `<PATTERN_HOME>/projects.kdl`.
///
/// Construct via [`ProjectRegistry::load`], mutate via
/// [`register_project`](ProjectRegistry::register_project) /
/// [`add_path`](ProjectRegistry::add_path), persist via
/// [`ProjectRegistry::save`].
#[derive(Debug, Clone, Default)]
pub struct ProjectRegistry {
    entries: Vec<ProjectEntry>,
}

impl ProjectRegistry {
    // -----------------------------------------------------------------------
    // Disk I/O
    // -----------------------------------------------------------------------

    /// Path to the registry file under the given [`PatternPaths`].
    /// Lives at `<data_root>/projects.kdl`.
    pub fn registry_path(paths: &PatternPaths) -> PathBuf {
        paths.data_root().join("projects.kdl")
    }

    /// Load the registry from disk. If the file does not exist, returns
    /// an empty registry — a fresh `~/.pattern/` is a valid state.
    pub fn load(paths: &PatternPaths) -> Result<Self, RegistryError> {
        let path = Self::registry_path(paths);
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = std::fs::read_to_string(&path).map_err(|e| RegistryError::Io {
            path: path.clone(),
            source: e,
        })?;
        Self::parse(&source).map_err(|message| RegistryError::Parse { path, message })
    }

    /// Persist the registry atomically. Writes to a sibling tempfile
    /// then renames into place; partial writes never appear on disk.
    pub fn save(&self, paths: &PatternPaths) -> Result<(), RegistryError> {
        let path = Self::registry_path(paths);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io {
                path: parent.to_owned(),
                source: e,
            })?;
        }
        let tmp = path.with_extension(format!(
            "kdl.tmp.{}",
            jiff::Timestamp::now().as_nanosecond()
        ));
        let body = self.emit();
        std::fs::write(&tmp, body).map_err(|e| RegistryError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        std::fs::rename(&tmp, &path).map_err(|e| RegistryError::Io {
            path: path.clone(),
            source: e,
        })?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Lookup
    // -----------------------------------------------------------------------

    /// Resolve a path (typically the cwd or the project root) to the
    /// canonical project ID. Walks upward from `path`: if any ancestor
    /// is registered, returns that project's ID. Returns `None` if
    /// neither `path` nor any ancestor is registered.
    pub fn project_id_for_path(&self, path: &Path) -> Option<&str> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        let mut cur: &Path = &canonical;
        loop {
            for entry in &self.entries {
                if entry.paths.iter().any(|p| p == cur) {
                    return Some(entry.id.as_str());
                }
            }
            match cur.parent() {
                Some(p) if p != cur => cur = p,
                _ => return None,
            }
        }
    }

    /// All paths registered under a given project ID.
    pub fn paths_for_project(&self, id: &str) -> impl Iterator<Item = &Path> {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .into_iter()
            .flat_map(|e| e.paths.iter().map(|p| p.as_path()))
    }

    /// All registered project IDs.
    pub fn project_ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.id.as_str())
    }

    /// Returns `true` if the registry has an entry for the given project ID.
    pub fn contains_id(&self, id: &str) -> bool {
        self.entries.iter().any(|e| e.id == id)
    }

    // -----------------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------------

    /// Register a project at `path`. Returns the project ID — either
    /// `requested_id` if supplied, or a slug derived from the directory
    /// basename otherwise.
    ///
    /// Idempotent: re-registering the same `(path, id)` pair returns
    /// the existing ID. Re-registering an already-registered path under
    /// a different ID errors.
    ///
    /// If `requested_id` names an existing project entry, the path is
    /// added to that entry. If `requested_id` is new (or omitted), a
    /// new entry is created.
    pub fn register_project(
        &mut self,
        path: &Path,
        requested_id: Option<&str>,
    ) -> Result<String, RegistryError> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());

        // Idempotent path: if the path is already registered, validate
        // the requested id matches and return the existing id.
        if let Some(existing) = self.entry_for_exact_path(&canonical) {
            let existing_id = existing.id.clone();
            if let Some(req) = requested_id {
                if req != existing_id {
                    return Err(RegistryError::PathAlreadyRegistered {
                        path: canonical,
                        existing_id,
                        requested_id: req.to_owned(),
                    });
                }
            }
            return Ok(existing_id);
        }

        // Either add to an existing entry by id, or create a new one.
        if let Some(req) = requested_id {
            if let Some(entry) = self.entries.iter_mut().find(|e| e.id == req) {
                entry.paths.push(canonical);
                return Ok(req.to_owned());
            }
            self.entries.push(ProjectEntry {
                id: req.to_owned(),
                paths: vec![canonical],
                created_at: jiff::Timestamp::now(),
            });
            return Ok(req.to_owned());
        }

        // Auto-derive id from the directory basename, with -N suffix on collision.
        let id = self.unique_slug_for(&canonical);
        self.entries.push(ProjectEntry {
            id: id.clone(),
            paths: vec![canonical],
            created_at: jiff::Timestamp::now(),
        });
        Ok(id)
    }

    /// Add a path to an existing project. Idempotent if the same
    /// `(id, path)` pair is already registered. Errors if the path is
    /// already registered under a different ID, or if `id` is unknown.
    pub fn add_path(&mut self, id: &str, path: &Path) -> Result<(), RegistryError> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());

        if let Some(existing) = self.entry_for_exact_path(&canonical) {
            if existing.id == id {
                return Ok(());
            }
            return Err(RegistryError::PathAlreadyRegistered {
                path: canonical,
                existing_id: existing.id.clone(),
                requested_id: id.to_owned(),
            });
        }

        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| RegistryError::UnknownProject { id: id.to_owned() })?;
        entry.paths.push(canonical);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn entry_for_exact_path(&self, path: &Path) -> Option<&ProjectEntry> {
        self.entries
            .iter()
            .find(|e| e.paths.iter().any(|p| p == path))
    }

    fn unique_slug_for(&self, path: &Path) -> String {
        let base = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(slugify)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "project".to_owned());

        if !self.contains_id(&base) {
            return base;
        }
        let mut n: u32 = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !self.contains_id(&candidate) {
                return candidate;
            }
            n = n.saturating_add(1);
        }
    }

    // -----------------------------------------------------------------------
    // KDL parse / emit
    // -----------------------------------------------------------------------

    fn parse(source: &str) -> Result<Self, String> {
        let doc: KdlDocument = source.parse().map_err(|e: kdl::KdlError| e.to_string())?;
        let mut entries = Vec::new();
        for node in doc.nodes() {
            if node.name().value() != "project" {
                continue;
            }
            let id = node
                .entries()
                .first()
                .and_then(|e| e.value().as_string())
                .ok_or_else(|| {
                    "project node must have an id as its first argument".to_owned()
                })?
                .to_owned();
            let created_at = node
                .entry("created-at")
                .and_then(|e| e.value().as_string())
                .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                .unwrap_or_else(jiff::Timestamp::now);
            let mut paths = Vec::new();
            if let Some(children) = node.children() {
                for child in children.nodes() {
                    if child.name().value() != "path" {
                        continue;
                    }
                    if let Some(s) = child.entries().first().and_then(|e| e.value().as_string()) {
                        paths.push(PathBuf::from(s));
                    }
                }
            }
            entries.push(ProjectEntry {
                id,
                paths,
                created_at,
            });
        }
        Ok(Self { entries })
    }

    fn emit(&self) -> String {
        let mut doc = KdlDocument::new();
        for entry in &self.entries {
            let mut node = KdlNode::new("project");
            node.entries_mut().push(kdl_string_arg(&entry.id));
            node.entries_mut()
                .push(kdl_string_prop("created-at", &entry.created_at.to_string()));
            let mut children = KdlDocument::new();
            for path in &entry.paths {
                let mut child = KdlNode::new("path");
                child
                    .entries_mut()
                    .push(kdl_string_arg(&path.to_string_lossy()));
                children.nodes_mut().push(child);
            }
            if !entry.paths.is_empty() {
                node.set_children(children);
            }
            doc.nodes_mut().push(node);
        }
        doc.to_string()
    }
}

// ---------------------------------------------------------------------------
// Slugify + KDL helpers
// ---------------------------------------------------------------------------

/// Lowercase ASCII alphanumeric + hyphens; non-alphanumeric collapses to a
/// single hyphen; leading/trailing hyphens trimmed. Empty input → empty
/// output (caller handles fallback).
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_dash = true; // suppresses leading dashes
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn kdl_string_arg(s: &str) -> kdl::KdlEntry {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    let parsed: kdl::KdlDocument = format!("v \"{escaped}\"").parse().expect("valid quoted kdl");
    parsed
        .nodes()
        .first()
        .expect("parsed node")
        .entries()
        .first()
        .expect("parsed entry")
        .clone()
}

fn kdl_string_prop(key: &str, value: &str) -> kdl::KdlEntry {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    let parsed: kdl::KdlDocument = format!("v {key}=\"{escaped}\"")
        .parse()
        .expect("valid quoted kdl");
    parsed
        .nodes()
        .first()
        .expect("parsed node")
        .entries()
        .first()
        .expect("parsed entry")
        .clone()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic_lowercase() {
        assert_eq!(slugify("My Cool Project"), "my-cool-project");
    }

    #[test]
    fn slugify_strips_special_chars() {
        assert_eq!(slugify("Hello!! @World??"), "hello-world");
    }

    #[test]
    fn slugify_collapses_consecutive_separators() {
        assert_eq!(slugify("foo___bar...baz"), "foo-bar-baz");
    }

    #[test]
    fn slugify_trims_leading_and_trailing() {
        assert_eq!(slugify("___foo___"), "foo");
        assert_eq!(slugify("---foo---"), "foo");
    }

    #[test]
    fn slugify_unicode_drops_non_ascii() {
        assert_eq!(slugify("café"), "caf");
    }

    #[test]
    fn slugify_empty_returns_empty() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn emit_then_parse_round_trip() {
        let mut reg = ProjectRegistry::default();
        reg.entries.push(ProjectEntry {
            id: "alpha".to_owned(),
            paths: vec![PathBuf::from("/tmp/alpha"), PathBuf::from("/tmp/alpha-fork")],
            created_at: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        });
        let body = reg.emit();
        let parsed = ProjectRegistry::parse(&body).unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].id, "alpha");
        assert_eq!(parsed.entries[0].paths.len(), 2);
    }
}
