//! Persona discovery: scan global and project-scoped persona directories.
//!
//! Enumerates available personas by walking directories that follow the
//! `@<agent_id>/persona.kdl` convention. Project-scoped personas take
//! precedence on agent_id collision (project overwrites global).
//!
//! # Canonical key and aliases
//!
//! The canonical key for a persona is its `agent_id`, which by convention
//! equals the directory name (with any leading `@` stripped). The persona's
//! `name` field, when it differs from `agent_id`, is registered as an alias
//! that resolves to the canonical id.
//!
//! Lookup tries the canonical key first, then the alias map. Collisions —
//! where a name alias would resolve a key that's already a different
//! canonical id, or where two personas' names alias to different canonical
//! ids — are surfaced as errors at discovery time rather than silently
//! picking a winner.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use miette::Diagnostic;
use thiserror::Error;

use crate::PatternPaths;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced during persona discovery.
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum PersonaDiscoveryError {
    /// Could not read the personas directory.
    #[error("could not read personas directory at {path}: {source}")]
    #[diagnostic(code(pattern_memory::persona::io_error))]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A persona's KDL file could not be parsed during alias extraction.
    /// The file remains discoverable by its directory name, but its `name`
    /// field is not registered as an alias.
    #[error("could not parse persona KDL at {path}: {message}")]
    #[diagnostic(code(pattern_memory::persona::kdl_parse_error))]
    KdlParse { path: PathBuf, message: String },

    /// The persona's `agent-id` field disagrees with its directory name.
    /// By convention these must match (the directory name is the canonical
    /// addressing key).
    #[error(
        "persona at {path}: directory name {dir_name:?} does not match agent-id {agent_id:?}"
    )]
    #[diagnostic(code(pattern_memory::persona::agent_id_mismatch))]
    AgentIdMismatch {
        path: PathBuf,
        dir_name: String,
        agent_id: String,
    },

    /// Two or more personas would resolve the same alias to different
    /// canonical ids. Examples: persona A has `name "foo"` and persona B
    /// has `agent-id "foo"`; or two personas both have `name "foo"`.
    #[error(
        "persona alias {alias:?} is ambiguous: resolves to both {first:?} and {second:?}"
    )]
    #[diagnostic(
        code(pattern_memory::persona::alias_collision),
        help("rename one of the personas' `name` field, or address by canonical id directly")
    )]
    AliasCollision {
        alias: String,
        first: String,
        second: String,
    },
}

// ---------------------------------------------------------------------------
// PersonaIndex
// ---------------------------------------------------------------------------

/// Result of persona discovery: canonical map + alias index.
#[derive(Debug, Default, Clone)]
pub struct PersonaIndex {
    /// Canonical map: `agent_id` → path to persona.kdl.
    /// `agent_id` equals the directory name (validated at build time).
    by_id: HashMap<String, PathBuf>,

    /// Alias map: alternative addressable key → canonical `agent_id`.
    /// Populated from each persona's `name` field when it differs from the
    /// canonical id. Empty when a persona's name and id match.
    aliases: HashMap<String, String>,
}

impl PersonaIndex {
    /// Resolve a key (either canonical id or alias) to a canonical agent id.
    /// Returns `None` if the key matches neither.
    pub fn resolve(&self, key: &str) -> Option<&str> {
        if let Some((canonical, _)) = self.by_id.get_key_value(key) {
            return Some(canonical.as_str());
        }
        self.aliases.get(key).map(|s| s.as_str())
    }

    /// Resolve a key to the persona file path, trying canonical id first
    /// and then alias.
    pub fn path_for(&self, key: &str) -> Option<&Path> {
        let id = self.resolve(key)?;
        self.by_id.get(id).map(|p| p.as_path())
    }

    /// Iterate canonical ids and their paths.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.by_id
            .iter()
            .map(|(id, path)| (id.as_str(), path.as_path()))
    }

    /// Iterate alias entries (alias, canonical_id).
    pub fn iter_aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.aliases
            .iter()
            .map(|(alias, id)| (alias.as_str(), id.as_str()))
    }

    /// Number of canonical personas (does not count aliases).
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Returns `true` if `key` matches any canonical id.
    pub fn contains_id(&self, key: &str) -> bool {
        self.by_id.contains_key(key)
    }

    /// Returns `true` if `key` matches any alias.
    pub fn contains_alias(&self, key: &str) -> bool {
        self.aliases.contains_key(key)
    }

    /// All canonical agent ids in the index.
    pub fn canonical_ids(&self) -> impl Iterator<Item = &str> {
        self.by_id.keys().map(|s| s.as_str())
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Enumerate available personas across global and project scopes.
///
/// Scans:
/// 1. `<paths.base()>/personas/@<agent_id>/persona.kdl` — global personas.
/// 2. `<project_mount>/personas/@<agent_id>/persona.kdl` — project-scoped.
///
/// Project-scoped personas overwrite globals on canonical-id collision.
///
/// # Errors
///
/// - [`PersonaDiscoveryError::Io`] — directory read failure.
/// - [`PersonaDiscoveryError::AgentIdMismatch`] — a persona's `agent-id`
///   field disagrees with its directory name.
/// - [`PersonaDiscoveryError::AliasCollision`] — a persona's `name` would
///   resolve to a different canonical id than another persona already in
///   the index.
/// - [`PersonaDiscoveryError::KdlParse`] — a persona file is malformed.
pub fn discover_personas(
    paths: &PatternPaths,
    project_mount: Option<&Path>,
) -> Result<PersonaIndex, PersonaDiscoveryError> {
    let mut index = PersonaIndex::default();

    // 1. Global personas.
    let global = paths.base().join("personas");
    if global.is_dir() {
        collect_personas(&global, &mut index)?;
    }

    // 2. Project-scoped personas. Project wins on canonical-id collision.
    if let Some(mount) = project_mount {
        let project_personas = mount.join("personas");
        if project_personas.is_dir() {
            collect_personas(&project_personas, &mut index)?;
        }
    }

    Ok(index)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Walk a personas directory and collect entries into the index.
fn collect_personas(
    dir: &Path,
    index: &mut PersonaIndex,
) -> Result<(), PersonaDiscoveryError> {
    let entries = std::fs::read_dir(dir).map_err(|e| PersonaDiscoveryError::Io {
        path: dir.to_owned(),
        source: e,
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| PersonaDiscoveryError::Io {
            path: dir.to_owned(),
            source: e,
        })?;

        let ft = entry.file_type().map_err(|e| PersonaDiscoveryError::Io {
            path: entry.path(),
            source: e,
        })?;
        if !ft.is_dir() {
            continue;
        }

        let kdl_path = entry.path().join("persona.kdl");
        if !kdl_path.is_file() {
            continue;
        }

        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let canonical_id = dir_name.trim_start_matches('@').to_owned();

        // Extract agent-id and name fields for validation + alias building.
        let fields = read_persona_fields(&kdl_path)?;

        // Validate that agent-id, when present, matches the directory name.
        if let Some(agent_id) = fields.agent_id.as_deref() {
            if agent_id != canonical_id {
                return Err(PersonaDiscoveryError::AgentIdMismatch {
                    path: kdl_path.clone(),
                    dir_name: canonical_id,
                    agent_id: agent_id.to_owned(),
                });
            }
        }

        // Reject if the canonical id collides with an existing alias that
        // points at a different canonical. This catches the order-independent
        // case where persona B (name=foo, id=bar) is processed before
        // persona A (id=foo): when A is then inserted, "foo" already lives
        // in the alias map pointing to "bar".
        if let Some(existing_target) = index.aliases.get(&canonical_id) {
            if existing_target != &canonical_id {
                return Err(PersonaDiscoveryError::AliasCollision {
                    alias: canonical_id.clone(),
                    first: existing_target.clone(),
                    second: canonical_id.clone(),
                });
            }
        }

        // Insert or overwrite canonical entry. Project scope overwriting
        // global is the existing semantic; we preserve it.
        index.by_id.insert(canonical_id.clone(), kdl_path);

        // Register the `name` field as an alias when it differs from
        // canonical id.
        if let Some(name) = fields.name.as_deref() {
            if name != canonical_id {
                // Reject if the alias collides with a different canonical id.
                if let Some(other_id_path) = index.by_id.get(name) {
                    let this_path = index.by_id.get(&canonical_id).unwrap();
                    if other_id_path != this_path {
                        return Err(PersonaDiscoveryError::AliasCollision {
                            alias: name.to_owned(),
                            first: name.to_owned(),
                            second: canonical_id.clone(),
                        });
                    }
                }
                // Reject if the alias collides with a different alias target.
                if let Some(existing_target) = index.aliases.get(name) {
                    if existing_target != &canonical_id {
                        return Err(PersonaDiscoveryError::AliasCollision {
                            alias: name.to_owned(),
                            first: existing_target.clone(),
                            second: canonical_id.clone(),
                        });
                    }
                }
                index.aliases.insert(name.to_owned(), canonical_id.clone());
            }
        }
    }

    Ok(())
}

/// Lightweight extraction of the `name` and `agent-id` top-level fields
/// from a persona KDL file. Used during discovery to build the alias
/// index without invoking the full persona loader.
struct PersonaFields {
    name: Option<String>,
    agent_id: Option<String>,
}

fn read_persona_fields(path: &Path) -> Result<PersonaFields, PersonaDiscoveryError> {
    let source = std::fs::read_to_string(path).map_err(|e| PersonaDiscoveryError::Io {
        path: path.to_owned(),
        source: e,
    })?;

    let doc: kdl::KdlDocument = source
        .parse()
        .map_err(|e: kdl::KdlError| PersonaDiscoveryError::KdlParse {
            path: path.to_owned(),
            message: e.to_string(),
        })?;

    let extract = |field: &str| -> Option<String> {
        doc.nodes()
            .iter()
            .find(|n| n.name().value() == field)
            .and_then(|n| n.entries().first())
            .and_then(|e| e.value().as_string())
            .map(|s| s.to_owned())
    };

    Ok(PersonaFields {
        name: extract("name"),
        agent_id: extract("agent-id"),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a persona.kdl with name and (optional) agent-id.
    fn create_persona(base: &Path, dir_name: &str, name: &str, agent_id: Option<&str>) {
        let persona_dir = base.join("personas").join(dir_name);
        std::fs::create_dir_all(&persona_dir).unwrap();
        let mut kdl = format!("name \"{name}\"\n");
        if let Some(id) = agent_id {
            kdl.push_str(&format!("agent-id \"{id}\"\n"));
        }
        std::fs::write(persona_dir.join("persona.kdl"), kdl).unwrap();
    }

    #[test]
    fn discovers_canonical_id_from_directory_name() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        create_persona(tmp.path(), "@reviewer", "reviewer", Some("reviewer"));

        let index = discover_personas(&paths, None).unwrap();
        assert_eq!(index.len(), 1);
        assert!(index.contains_id("reviewer"));
        assert_eq!(index.resolve("reviewer"), Some("reviewer"));
        assert!(index.path_for("reviewer").unwrap().ends_with("persona.kdl"));
    }

    #[test]
    fn registers_name_as_alias_when_different_from_id() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        // Directory and agent-id are "pattern-default"; the display name is "pattern".
        create_persona(
            tmp.path(),
            "@pattern-default",
            "pattern",
            Some("pattern-default"),
        );

        let index = discover_personas(&paths, None).unwrap();
        assert_eq!(index.len(), 1);
        assert!(index.contains_id("pattern-default"));
        assert!(index.contains_alias("pattern"));
        assert_eq!(index.resolve("pattern-default"), Some("pattern-default"));
        assert_eq!(index.resolve("pattern"), Some("pattern-default"));
    }

    #[test]
    fn no_alias_registered_when_name_equals_id() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        create_persona(tmp.path(), "@solo", "solo", Some("solo"));

        let index = discover_personas(&paths, None).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index.iter_aliases().count(), 0);
        assert_eq!(index.resolve("solo"), Some("solo"));
    }

    #[test]
    fn agent_id_field_must_match_directory_name() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        // Directory is "alpha" but agent-id is "beta" — should error.
        create_persona(tmp.path(), "@alpha", "alpha", Some("beta"));

        let result = discover_personas(&paths, None);
        assert!(matches!(
            result,
            Err(PersonaDiscoveryError::AgentIdMismatch { .. })
        ));
    }

    #[test]
    fn missing_agent_id_field_is_tolerated() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        // No agent-id field; directory name is the canonical id.
        create_persona(tmp.path(), "@helper", "helper", None);

        let index = discover_personas(&paths, None).unwrap();
        assert_eq!(index.resolve("helper"), Some("helper"));
    }

    #[test]
    fn alias_resolves_through_path_for() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        create_persona(
            tmp.path(),
            "@pattern-default",
            "pattern",
            Some("pattern-default"),
        );

        let index = discover_personas(&paths, None).unwrap();
        let path_via_canonical = index.path_for("pattern-default").unwrap().to_owned();
        let path_via_alias = index.path_for("pattern").unwrap().to_owned();
        assert_eq!(path_via_canonical, path_via_alias);
    }

    #[test]
    fn alias_colliding_with_canonical_id_errors() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        // Persona A: canonical id "foo".
        create_persona(tmp.path(), "@foo", "foo", Some("foo"));
        // Persona B: canonical id "bar", with name "foo" — alias collides
        // with persona A's canonical id (different targets).
        create_persona(tmp.path(), "@bar", "foo", Some("bar"));

        let result = discover_personas(&paths, None);
        assert!(matches!(
            result,
            Err(PersonaDiscoveryError::AliasCollision { .. })
        ));
    }

    #[test]
    fn project_scoped_takes_precedence_on_canonical_collision() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let mount = TempDir::new().unwrap();

        create_persona(
            tmp.path(),
            "@reviewer",
            "global-reviewer",
            Some("reviewer"),
        );
        create_persona(
            mount.path(),
            "@reviewer",
            "project-reviewer",
            Some("reviewer"),
        );

        let index = discover_personas(&paths, Some(mount.path())).unwrap();
        let path = index.path_for("reviewer").unwrap();
        assert!(path.starts_with(mount.path()));
        // Both names are registered as aliases; project scope wins canonical
        // overwrite, but global's alias entry was already pointing at the
        // same canonical id "reviewer", so this is fine.
    }

    #[test]
    fn no_personas_dir_returns_empty_index() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let index = discover_personas(&paths, None).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn directories_without_persona_kdl_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let dir = tmp.path().join("personas").join("@incomplete");
        std::fs::create_dir_all(&dir).unwrap();
        let index = discover_personas(&paths, None).unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn merges_global_and_project_personas() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let mount = TempDir::new().unwrap();

        create_persona(
            tmp.path(),
            "@global-only",
            "global-only",
            Some("global-only"),
        );
        create_persona(
            mount.path(),
            "@project-only",
            "project-only",
            Some("project-only"),
        );

        let index = discover_personas(&paths, Some(mount.path())).unwrap();
        assert_eq!(index.len(), 2);
        assert!(index.contains_id("global-only"));
        assert!(index.contains_id("project-only"));
    }

    #[test]
    fn unknown_key_returns_none() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        create_persona(tmp.path(), "@solo", "solo", Some("solo"));

        let index = discover_personas(&paths, None).unwrap();
        assert!(index.resolve("unknown").is_none());
        assert!(index.path_for("unknown").is_none());
    }
}
