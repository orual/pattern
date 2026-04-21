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

use pattern_memory::config::{
    BackupSection, ConfigError, ModeKind, load_mount_config, parse_duration_str,
};
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

// ---------------------------------------------------------------------------
// Backup section tests
// ---------------------------------------------------------------------------

const VALID_MODE_A_WITH_BACKUP: &str = r#"
mount mode="A" memory-db="memory.db"

project name="pattern-dev" created-at="2026-04-19T12:00:00Z"

backup snapshot-interval="30m" {
    keep-recent 12
    hourly-days 2
    daily-months 3
    monthly-forever false
}
"#;

#[test]
fn parse_backup_section_present() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(&tmp, VALID_MODE_A_WITH_BACKUP);
    let config = load_mount_config(&path).expect("config with backup section should parse");

    let backup = config
        .backup
        .as_ref()
        .expect("backup section should be present");
    assert_eq!(backup.snapshot_interval, "30m");
    assert_eq!(backup.keep_recent, 12);
    assert_eq!(backup.hourly_days, 2);
    assert_eq!(backup.daily_months, 3);
    assert!(!backup.monthly_forever);
}

#[test]
fn missing_backup_section_is_none() {
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
project name="minimal" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let config = load_mount_config(&path).expect("minimal config should parse");
    assert!(
        config.backup.is_none(),
        "absent backup section should be None"
    );
}

#[test]
fn backup_section_defaults_applied_for_omitted_children() {
    let tmp = TempDir::new().unwrap();
    // Only the snapshot-interval property; all children omitted → use defaults.
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
project name="defaults" created-at="2026-01-01T00:00:00Z"
backup snapshot-interval="2h"
"#,
    );
    let config = load_mount_config(&path).expect("backup with defaults should parse");
    let backup = config
        .backup
        .as_ref()
        .expect("backup section should be present");
    assert_eq!(backup.snapshot_interval, "2h");
    assert_eq!(backup.keep_recent, 24, "default keep_recent");
    assert_eq!(backup.hourly_days, 1, "default hourly_days");
    assert_eq!(backup.daily_months, 1, "default daily_months");
    assert!(backup.monthly_forever, "default monthly_forever");
}

#[test]
fn backup_section_defaults_from_default_impl() {
    let section = BackupSection::default();
    assert_eq!(section.snapshot_interval, "1h");
    assert_eq!(section.keep_recent, 24);
    assert_eq!(section.hourly_days, 1);
    assert_eq!(section.daily_months, 1);
    assert!(section.monthly_forever);
    // parse_interval should produce a 1-hour duration.
    let dur = section
        .parse_interval()
        .expect("default interval must be valid");
    assert_eq!(dur.as_secs(), 3600);
}

// ---------------------------------------------------------------------------
// parse_duration_str tests
// ---------------------------------------------------------------------------

#[test]
fn parse_duration_str_hours() {
    assert_eq!(parse_duration_str("1h").unwrap().as_secs(), 3600);
    assert_eq!(parse_duration_str("2h").unwrap().as_secs(), 7200);
    assert_eq!(parse_duration_str("24h").unwrap().as_secs(), 86400);
}

#[test]
fn parse_duration_str_minutes() {
    assert_eq!(parse_duration_str("30m").unwrap().as_secs(), 1800);
    assert_eq!(parse_duration_str("1m").unwrap().as_secs(), 60);
}

#[test]
fn parse_duration_str_seconds() {
    assert_eq!(parse_duration_str("60s").unwrap().as_secs(), 60);
    assert_eq!(parse_duration_str("3600s").unwrap().as_secs(), 3600);
}

#[test]
fn parse_duration_str_rejects_invalid() {
    assert!(parse_duration_str("").is_err(), "empty string must fail");
    assert!(parse_duration_str("0h").is_err(), "zero must fail");
    assert!(parse_duration_str("1d").is_err(), "days not supported");
    assert!(parse_duration_str("abc").is_err(), "no digits must fail");
    assert!(parse_duration_str("-1h").is_err(), "negative must fail");
    assert!(parse_duration_str("1hour").is_err(), "word unit must fail");
}

#[test]
fn invalid_backup_interval_fails_config_validation() {
    let dir = tempfile::tempdir().unwrap();
    let kdl_path = dir.path().join(".pattern.kdl");
    std::fs::write(
        &kdl_path,
        r#"
mount mode="A" memory-db="memory.db"
project name="test" created-at="2026-04-20T00:00:00Z"
backup snapshot-interval="banana"
"#,
    )
    .unwrap();
    let err = pattern_memory::config::load_mount_config(&kdl_path);
    assert!(err.is_err(), "invalid interval must fail validation");
    let msg = err.unwrap_err().to_string();
    assert!(
        msg.contains("banana") || msg.contains("snapshot-interval") || msg.contains("duration"),
        "error should mention the invalid value: {msg}"
    );
}

// ---------------------------------------------------------------------------
// IsolateSection.resolve() tests
// ---------------------------------------------------------------------------

#[test]
fn isolate_section_resolve_none() {
    use pattern_core::types::memory_types::IsolatePolicy;
    use pattern_memory::config::IsolateSection;

    let section = IsolateSection::default();
    assert_eq!(section.resolve().unwrap(), IsolatePolicy::None);
}

#[test]
fn isolate_section_resolve_core_only() {
    use pattern_core::types::memory_types::IsolatePolicy;

    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
isolate-from-persona policy="core-only"
project name="test" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let config = load_mount_config(&path).expect("core-only config should parse");
    assert_eq!(
        config.isolate_from_persona.resolve().unwrap(),
        IsolatePolicy::CoreOnly
    );
}

#[test]
fn isolate_section_resolve_full() {
    use pattern_core::types::memory_types::IsolatePolicy;

    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
isolate-from-persona policy="full"
project name="test" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let config = load_mount_config(&path).expect("full config should parse");
    assert_eq!(
        config.isolate_from_persona.resolve().unwrap(),
        IsolatePolicy::Full
    );
}

#[test]
fn isolate_section_resolve_invalid_rejected_at_parse() {
    // Invalid policy strings are caught by validate_config at parse time,
    // not by resolve(). Verify parse-time rejection.
    let tmp = TempDir::new().unwrap();
    let path = write_config(
        &tmp,
        r#"
mount mode="A" memory-db="memory.db"
isolate-from-persona policy="bogus"
project name="test" created-at="2026-01-01T00:00:00Z"
"#,
    );
    let err = load_mount_config(&path).expect_err("bogus policy should fail validation");
    match err {
        ConfigError::Validation { reason, .. } => {
            assert!(
                reason.contains("isolate-from-persona"),
                "validation should mention field: {reason}"
            );
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}
