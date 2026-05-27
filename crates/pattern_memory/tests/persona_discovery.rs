// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for persona discovery across global and project scopes.
//!
//! Verifies AC13.1 through AC13.5 for the v3-memory-rework Phase 8 plan.

use std::path::Path;

use pattern_memory::PatternPaths;
use pattern_memory::persona::discover_personas;
use tempfile::TempDir;

/// Create a minimal valid persona.kdl in a personas directory.
fn create_persona(base: &Path, dir_name: &str, kdl_content: &str) {
    let persona_dir = base.join("personas").join(dir_name);
    std::fs::create_dir_all(&persona_dir).unwrap();
    std::fs::write(persona_dir.join("persona.kdl"), kdl_content).unwrap();
}

/// Minimal valid persona KDL.
fn valid_persona_kdl(name: &str) -> String {
    format!(
        r#"name "{name}"

model provider="anthropic" model-id="claude-sonnet-4-6" {{
}}

budgets {{
    wall-ms 30000
}}
"#
    )
}

/// AC13.1: Persona at `<mount>/personas/@reviewer/persona.kdl` loads and is
/// discoverable as `@reviewer` within the project.
#[test]
fn project_persona_is_discoverable() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());
    let mount = TempDir::new().unwrap();

    create_persona(mount.path(), "@reviewer", &valid_persona_kdl("reviewer"));

    let result = discover_personas(&paths, Some(mount.path())).unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.contains_id("reviewer"));
    assert!(result.path_for("reviewer").unwrap().ends_with("persona.kdl"));
}

/// AC13.2: A project-scoped persona is not visible when attaching a
/// different project.
#[test]
fn project_persona_invisible_from_different_mount() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());
    let mount_a = TempDir::new().unwrap();
    let mount_b = TempDir::new().unwrap();

    create_persona(mount_a.path(), "@reviewer", &valid_persona_kdl("reviewer"));

    // Discovering from mount_b should NOT see mount_a's persona.
    let result = discover_personas(&paths, Some(mount_b.path())).unwrap();
    assert!(
        !result.contains_id("reviewer"),
        "project-scoped persona should not be visible from a different mount"
    );
}

/// AC13.3: A global persona at `~/.pattern/personas/@name/` works across
/// projects.
#[test]
fn global_persona_visible_across_projects() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());

    create_persona(paths.data_root(), "@assistant", &valid_persona_kdl("assistant"));

    // No mount — global still visible.
    let result = discover_personas(&paths, None).unwrap();
    assert!(result.contains_id("assistant"));

    // With an unrelated mount — global still visible.
    let mount = TempDir::new().unwrap();
    let result = discover_personas(&paths, Some(mount.path())).unwrap();
    assert!(result.contains_id("assistant"));
}

/// Discovery now parses each persona's top-level fields to extract aliases
/// and validate `agent-id`. A persona missing the `name` field is still
/// indexed by its directory name (no alias to register, but still
/// discoverable). Parse errors at the KDL syntax level surface as
/// `KdlParse`; semantic errors at load time surface from the persona loader.
///
/// AC13.4 load-time error coverage lives in pattern_runtime's
/// `error_clarity.rs` (`ac9_5_persona_missing_name_field_fails` and
/// `ac9_5_persona_malformed_kdl_fails_with_parse_error`) since the
/// loader is in that crate.
#[test]
fn discovery_finds_persona_without_name_field() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());

    // Persona file missing the optional `name` node — valid KDL, no alias.
    create_persona(paths.data_root(), "@broken", "description \"no name field\"\n");

    let result = discover_personas(&paths, None).unwrap();
    assert!(
        result.contains_id("broken"),
        "discovery should index by directory name even with no `name` field"
    );
}

/// AC13.5: Global + project-scoped personas with the same name: project-scoped
/// takes precedence within that project; global available elsewhere.
#[test]
fn project_scoped_takes_precedence_on_collision() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());
    let mount = TempDir::new().unwrap();

    // Global version.
    create_persona(
        paths.data_root(),
        "@reviewer",
        &valid_persona_kdl("reviewer-global"),
    );
    // Project version.
    create_persona(
        mount.path(),
        "@reviewer",
        &valid_persona_kdl("reviewer-project"),
    );

    // With the mount: project version wins.
    let result = discover_personas(&paths, Some(mount.path())).unwrap();
    assert_eq!(result.len(), 1);
    let path = result.path_for("reviewer").unwrap();
    assert!(
        path.starts_with(mount.path()),
        "project-scoped should take precedence, got: {path:?}"
    );

    // Without the mount (or different mount): global is used.
    let other_mount = TempDir::new().unwrap();
    let result2 = discover_personas(&paths, Some(other_mount.path())).unwrap();
    assert!(result2.contains_id("reviewer"));
    let path2 = result2.path_for("reviewer").unwrap();
    assert!(
        path2.starts_with(global.path()),
        "global should be used when project doesn't have it"
    );
}
