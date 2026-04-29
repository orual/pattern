//! Regression test: the documented `.pattern.kdl` reference at
//! `docs/reference/pattern-kdl-reference.kdl` must always parse cleanly
//! with the current schema.
//!
//! This file is annotated documentation showing every supported section
//! and field. If it stops parsing, either the schema changed (and the
//! reference needs updating) or the reference drifted (and needs to be
//! brought back into line).

use pattern_memory::config::{FilePolicyMode, ModeKind, load_mount_config};

fn reference_path() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR points at crates/pattern_memory; the workspace
    // root is two levels up.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root must be reachable from CARGO_MANIFEST_DIR")
        .join("docs/reference/pattern-kdl-reference.kdl")
}

#[test]
fn reference_kdl_parses_cleanly() {
    let path = reference_path();
    let config = load_mount_config(&path)
        .unwrap_or_else(|e| panic!("reference {} failed to parse: {e:?}", path.display()));

    // mount
    assert_eq!(config.mount.mode, ModeKind::Sidecar);
    assert_eq!(config.mount.memory_db, "memory.db");

    // project
    assert_eq!(config.project.id.as_deref(), Some("my-project"));
    assert_eq!(config.project.name, "My Project");
    assert_eq!(config.project.id(), "my-project");
    assert!(!config.project.created_at.is_empty());

    // partner
    let partner = config.partner.as_ref().expect("partner section present");
    assert_eq!(partner.display_name.as_deref(), Some("orual"));

    // personas
    assert_eq!(config.personas.entries.len(), 2);
    let slots: Vec<&str> = config
        .personas
        .entries
        .iter()
        .map(|e| e.slot.as_str())
        .collect();
    assert!(slots.contains(&"default"));
    assert!(slots.contains(&"focused"));

    // isolation
    assert_eq!(config.isolate_from_persona.policy, "none");

    // jj
    assert!(config.jj.enabled, "sidecar mode requires jj enabled");
    assert_eq!(config.jj.max_new_file_size, "100MiB");

    // backup
    let backup = config.backup.as_ref().expect("backup block present");
    assert_eq!(backup.snapshot_interval, "1h");
    assert_eq!(backup.keep_recent, 24);
    assert_eq!(backup.hourly_days, 7);
    assert_eq!(backup.daily_months, 3);
    assert!(backup.monthly_forever);
    assert_eq!(backup.parse_interval().unwrap().as_secs(), 3600);

    // file-policy: order matters, last-match-wins
    let rules = &config.file_policy.rules;
    assert_eq!(rules.len(), 4);
    assert_eq!(
        rules[0],
        (FilePolicyMode::Allow, "/project/**".to_string())
    );
    assert_eq!(rules[1], (FilePolicyMode::Deny, "/project/.env".to_string()));
    assert_eq!(
        rules[2],
        (FilePolicyMode::Deny, "/project/secrets/**".to_string())
    );
    assert_eq!(
        rules[3],
        (FilePolicyMode::Allow, "/tmp/pattern-*".to_string())
    );
}
