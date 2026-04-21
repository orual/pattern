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
    assert!(result.contains_key("reviewer"));
    assert!(result["reviewer"].ends_with("persona.kdl"));
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
        !result.contains_key("reviewer"),
        "project-scoped persona should not be visible from a different mount"
    );
}

/// AC13.3: A global persona at `~/.pattern/personas/@name/` works across
/// projects.
#[test]
fn global_persona_visible_across_projects() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());

    create_persona(global.path(), "@assistant", &valid_persona_kdl("assistant"));

    // No mount — global still visible.
    let result = discover_personas(&paths, None).unwrap();
    assert!(result.contains_key("assistant"));

    // With an unrelated mount — global still visible.
    let mount = TempDir::new().unwrap();
    let result = discover_personas(&paths, Some(mount.path())).unwrap();
    assert!(result.contains_key("assistant"));
}

/// Discovery finds malformed persona files without failing — it only
/// locates files, not parses them. Parse errors surface at load time.
///
/// AC13.4 load-time error coverage lives in pattern_runtime's
/// `error_clarity.rs` (`ac9_5_persona_missing_name_field_fails` and
/// `ac9_5_persona_malformed_kdl_fails_with_parse_error`) since the
/// loader is in that crate.
#[test]
fn discovery_finds_malformed_persona_file() {
    let global = TempDir::new().unwrap();
    let paths = PatternPaths::with_base(global.path());

    // Create a persona.kdl missing the required `name` node.
    create_persona(global.path(), "@broken", "description \"no name field\"\n");

    let result = discover_personas(&paths, None).unwrap();
    assert!(
        result.contains_key("broken"),
        "discovery should find the file even though it's malformed"
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
        global.path(),
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
    let path = &result["reviewer"];
    assert!(
        path.starts_with(mount.path()),
        "project-scoped should take precedence, got: {path:?}"
    );

    // Without the mount (or different mount): global is used.
    let other_mount = TempDir::new().unwrap();
    let result2 = discover_personas(&paths, Some(other_mount.path())).unwrap();
    assert!(result2.contains_key("reviewer"));
    assert!(
        result2["reviewer"].starts_with(global.path()),
        "global should be used when project doesn't have it"
    );
}
