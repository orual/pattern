//! Persona discovery: scan global and project-scoped persona directories.
//!
//! Enumerates available personas by walking directories that follow the
//! `@<name>/persona.kdl` convention. Project-scoped personas take precedence
//! on name collision (HashMap insert semantics — project overwrites global).

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
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Enumerate available personas across global and project scopes.
///
/// Scans:
/// 1. `<paths.base()>/personas/@<name>/persona.kdl` — global personas.
/// 2. `<project_mount>/personas/@<name>/persona.kdl` — project-scoped.
///
/// Project-scoped personas overwrite globals on name collision (HashMap
/// insert semantics). The returned map is `normalized_name → path_to_persona_kdl`.
///
/// Normalized name: the directory name with any leading `@` stripped.
///
/// # Errors
///
/// Returns [`PersonaDiscoveryError::Io`] if a directory that exists cannot be read.
pub fn discover_personas(
    paths: &PatternPaths,
    project_mount: Option<&Path>,
) -> Result<HashMap<String, PathBuf>, PersonaDiscoveryError> {
    let mut personas = HashMap::new();

    // 1. Global personas at <base>/personas/@<name>/persona.kdl.
    let global = paths.base().join("personas");
    if global.is_dir() {
        collect_personas(&global, &mut personas)?;
    }

    // 2. Project-scoped personas at <mount>/personas/@<name>/persona.kdl.
    // Project-scoped wins on name collision (insert overwrites).
    if let Some(mount) = project_mount {
        let project_personas = mount.join("personas");
        if project_personas.is_dir() {
            collect_personas(&project_personas, &mut personas)?;
        }
    }

    Ok(personas)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Walk a personas directory and collect entries into the output map.
///
/// Each entry is a subdirectory (optionally prefixed with `@`) containing a
/// `persona.kdl` file. Directories without a `persona.kdl` are silently
/// skipped (they may be work-in-progress or unrelated).
fn collect_personas(
    dir: &Path,
    out: &mut HashMap<String, PathBuf>,
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

        // Only consider directories.
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
        // Normalize: strip leading '@' for lookup key.
        let normalized = dir_name.trim_start_matches('@').to_owned();
        out.insert(normalized, kdl_path);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal valid persona.kdl in a directory.
    fn create_persona(base: &Path, dir_name: &str, name: &str) {
        let persona_dir = base.join("personas").join(dir_name);
        std::fs::create_dir_all(&persona_dir).unwrap();
        std::fs::write(
            persona_dir.join("persona.kdl"),
            format!("name \"{name}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn discovers_global_persona() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        create_persona(tmp.path(), "@reviewer", "reviewer");

        let result = discover_personas(&paths, None).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("reviewer"));
        assert!(result["reviewer"].ends_with("persona.kdl"));
    }

    #[test]
    fn discovers_project_persona() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let mount = TempDir::new().unwrap();
        create_persona(mount.path(), "@helper", "helper");

        let result = discover_personas(&paths, Some(mount.path())).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("helper"));
    }

    #[test]
    fn project_scoped_takes_precedence_on_collision() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let mount = TempDir::new().unwrap();

        // Global version.
        create_persona(tmp.path(), "@reviewer", "reviewer-global");
        // Project version.
        create_persona(mount.path(), "@reviewer", "reviewer-project");

        let result = discover_personas(&paths, Some(mount.path())).unwrap();
        assert_eq!(result.len(), 1);
        // Project version wins.
        let path = &result["reviewer"];
        assert!(
            path.starts_with(mount.path()),
            "project-scoped should take precedence, got: {path:?}"
        );
    }

    #[test]
    fn global_visible_when_different_mount() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        create_persona(tmp.path(), "@reviewer", "reviewer-global");

        // Different mount with no personas.
        let other_mount = TempDir::new().unwrap();

        let result = discover_personas(&paths, Some(other_mount.path())).unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("reviewer"));
    }

    #[test]
    fn directories_without_persona_kdl_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());

        // Create a directory without persona.kdl.
        let dir = tmp.path().join("personas").join("@incomplete");
        std::fs::create_dir_all(&dir).unwrap();

        let result = discover_personas(&paths, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn no_personas_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());

        let result = discover_personas(&paths, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn accepts_name_without_at_prefix() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        // Persona dir without '@' prefix.
        create_persona(tmp.path(), "plain-name", "plain-name");

        let result = discover_personas(&paths, None).unwrap();
        assert!(result.contains_key("plain-name"));
    }

    #[test]
    fn merges_global_and_project_personas() {
        let tmp = TempDir::new().unwrap();
        let paths = PatternPaths::with_base(tmp.path());
        let mount = TempDir::new().unwrap();

        create_persona(tmp.path(), "@global-only", "global-only");
        create_persona(mount.path(), "@project-only", "project-only");

        let result = discover_personas(&paths, Some(mount.path())).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("global-only"));
        assert!(result.contains_key("project-only"));
    }
}
