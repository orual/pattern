//! Integration tests for `.pattern.kdl` config parsing.
//!
//! Covers:
//! - Valid configs for each mode (A, B, C) with insta snapshots.
//! - Invalid mode string → parse error.
//! - Missing required field → parse error.
//! - Mode B/C with `jj enabled=false` → validation error.
//!
//! KDL format notes: node and property names use kebab-case (idiomatic KDL).
//! The knus derive macros convert Rust snake_case field names to kebab-case,
//! so `memory_db` → `memory-db`, `isolate_from_persona` → `isolate-from-persona`,
//! `max_new_file_size` → `max-new-file-size`, `created_at` → `created-at`.

use pattern_memory::config::{ConfigError, ModeKind, load_mount_config};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `content` to `<tmpdir>/.pattern.kdl` and return the path.
fn write_config(tmp: &TempDir, content: &str) -> std::path::PathBuf {
    let path = tmp.path().join(".pattern.kdl");
    std::fs::write(&path, content).expect("write test config");
    path
}

// ---------------------------------------------------------------------------
// Valid fixture strings (kebab-case node/property names)
// ---------------------------------------------------------------------------

const VALID_MODE_A: &str = r#"
mount mode="A" memory-db="memory.db"

personas {
    default "@pattern-default"
}

isolate-from-persona policy="none"

jj enabled=false

project name="pattern-dev" created-at="2026-04-19T12:00:00Z"
"#;

const VALID_MODE_B: &str = r#"
mount mode="B" memory-db="memory.db"

personas {
    default "@pattern-default"
    focused "@pattern-focus"
}

isolate-from-persona policy="core-only"

jj enabled=true max-new-file-size="50MiB"

project name="pattern-research" created-at="2026-04-20T08:00:00Z"
"#;

const VALID_MODE_C: &str = r#"
mount mode="C" memory-db="memory.db"

personas {
    default "@pattern-default"
}

isolate-from-persona policy="full"

jj enabled=true

project name="colocated-project" created-at="2026-04-20T09:00:00Z"
"#;

// ---------------------------------------------------------------------------
// Valid config tests with insta snapshots
// ---------------------------------------------------------------------------

#[test]
fn parse_valid_mode_a() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, VALID_MODE_A);
    let config = load_mount_config(&path).expect("mode A should parse");
    assert_eq!(config.mount.mode, ModeKind::A);
    assert_eq!(config.mount.memory_db, "memory.db");
    assert_eq!(config.personas.entries.len(), 1);
    assert_eq!(config.personas.entries[0].slot, "default");
    assert_eq!(config.personas.entries[0].persona, "@pattern-default");
    assert_eq!(config.isolate_from_persona.policy, "none");
    assert!(!config.jj.enabled);
    assert_eq!(config.project.name, "pattern-dev");
    insta::assert_yaml_snapshot!("valid_mode_a_config", config);
}

#[test]
fn parse_valid_mode_b() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, VALID_MODE_B);
    let config = load_mount_config(&path).expect("mode B should parse");
    assert_eq!(config.mount.mode, ModeKind::B);
    assert_eq!(config.personas.entries.len(), 2);
    assert!(config.jj.enabled);
    assert_eq!(config.jj.max_new_file_size, "50MiB");
    insta::assert_yaml_snapshot!("valid_mode_b_config", config);
}

#[test]
fn parse_valid_mode_c() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, VALID_MODE_C);
    let config = load_mount_config(&path).expect("mode C should parse");
    assert_eq!(config.mount.mode, ModeKind::C);
    assert!(config.jj.enabled);
    insta::assert_yaml_snapshot!("valid_mode_c_config", config);
}

// ---------------------------------------------------------------------------
// Default section tests
// ---------------------------------------------------------------------------

#[test]
fn missing_optional_sections_use_defaults() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
project name="minimal" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let config = load_mount_config(&path).expect("minimal config should parse");
    assert_eq!(config.isolate_from_persona.policy, "none");
    assert!(!config.jj.enabled);
    assert_eq!(config.jj.max_new_file_size, "100MiB");
    assert!(config.personas.entries.is_empty());
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn invalid_mode_string_produces_parse_error() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="X" memory-db="memory.db"
project name="bad" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let err = load_mount_config(&path).expect_err("invalid mode should fail");
    assert!(
        matches!(err, ConfigError::Parse { .. }),
        "expected ConfigError::Parse, got {err:?}"
    );
}

#[test]
fn missing_required_mount_node_produces_parse_error() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
project name="no-mount" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let err = load_mount_config(&path).expect_err("missing mount should fail");
    assert!(
        matches!(err, ConfigError::Parse { .. }),
        "expected ConfigError::Parse, got {err:?}"
    );
}

#[test]
fn missing_required_project_node_produces_parse_error() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
"#,
    );
    let err = load_mount_config(&path).expect_err("missing project should fail");
    assert!(
        matches!(err, ConfigError::Parse { .. }),
        "expected ConfigError::Parse, got {err:?}"
    );
}

#[test]
fn mode_b_jj_disabled_produces_validation_error() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="B" memory-db="memory.db"
jj enabled=false
project name="broken" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let err = load_mount_config(&path).expect_err("mode B with jj disabled should fail validation");
    match &err {
        ConfigError::Validation { reason, .. } => {
            assert!(
                reason.contains("mode B"),
                "validation message should mention mode B: {reason}"
            );
        }
        other => panic!("expected ConfigError::Validation, got {other:?}"),
    }
}

#[test]
fn mode_c_jj_disabled_produces_validation_error() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="C" memory-db="memory.db"
jj enabled=false
project name="broken-c" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let err = load_mount_config(&path).expect_err("mode C with jj disabled should fail validation");
    assert!(
        matches!(err, ConfigError::Validation { .. }),
        "expected ConfigError::Validation, got {err:?}"
    );
}

#[test]
fn io_error_on_missing_file() {
    let path = std::path::PathBuf::from("/nonexistent/path/.pattern.kdl");
    let err = load_mount_config(&path).expect_err("missing file should fail");
    assert!(
        matches!(err, ConfigError::Io { .. }),
        "expected ConfigError::Io, got {err:?}"
    );
}
