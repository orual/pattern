//! Phase 3 Task 7 — `ForkHandle::promote` integration tests.
//!
//! Verifies:
//! - AC4.7: spawner with `SpawnNewIdentities` flag → `promote(cfg)` returns
//!   a `PersonaId` and writes a draft KDL file at the expected path.
//! - AC4.8: spawner WITHOUT the flag → `ForkError::CapabilityDenied`.
//!
//! Persistent-fork promote round-trips are out of scope here — the live-jj
//! exercise is covered by Subcomponent B's `fork_persistent.rs` discard
//! round trip; promotion's commit-failure path is covered by a unit test
//! on the synthetic non-existent workspace.

use std::sync::Arc;

use pattern_core::spawn::PersonaConfig;
use pattern_core::types::ids::PersonaId;
use pattern_core::{CapabilityFlag, CapabilitySet};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_runtime::spawn::fork::{ForkError, ForkHandle};
use pattern_runtime::timeout::CancelState;

fn build_lightweight_fork(spawner_caps: CapabilitySet) -> ForkHandle {
    let db = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    let parent_cache = Arc::new(MemoryCache::new(db.clone()));
    let child_cache = Arc::new(MemoryCache::new(db));
    let cancel = Arc::new(CancelState::new());
    ForkHandle::new_lightweight(
        "fork-promote".into(),
        "child-promote".into(),
        child_cache,
        "parent-agent".into(),
        Arc::downgrade(&parent_cache),
        cancel,
    )
    .with_spawner_capabilities(spawner_caps)
}

fn sample_persona_cfg(name: &str) -> PersonaConfig {
    PersonaConfig::new(
        name,
        "you are a fork-promoted draft",
        CapabilitySet::empty(),
    )
}

/// AC4.7 — spawner with `SpawnNewIdentities` can promote a lightweight fork
/// into a draft persona; the draft KDL file lands on disk.
#[test]
fn promote_lightweight_with_flag_creates_draft() {
    let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);
    let handle = build_lightweight_fork(caps);

    let drafts = tempfile::TempDir::new().expect("tempdir");
    let cfg = sample_persona_cfg("teal-draft");
    let pid: PersonaId = handle
        .promote(cfg, drafts.path())
        .expect("promote must succeed when flag is held");

    assert_eq!(pid.as_str(), "teal-draft", "promote returns the cfg name");
    let kdl_path = drafts.path().join("teal-draft.kdl");
    assert!(
        kdl_path.exists(),
        "draft KDL must be written at <drafts>/<id>.kdl"
    );
    let content = std::fs::read_to_string(&kdl_path).expect("read draft");
    assert!(
        content.contains("name \"teal-draft\""),
        "draft KDL must contain the persona name; got:\n{content}"
    );
    assert!(
        content.contains("system_prompt \"you are a fork-promoted draft\""),
        "draft KDL must contain the system prompt; got:\n{content}"
    );
    assert!(
        content.contains("capabilities"),
        "draft KDL must include a capabilities block; got:\n{content}"
    );
}

/// AC4.8 — spawner without `SpawnNewIdentities` is denied.
#[test]
fn promote_without_flag_is_capability_denied() {
    // Caps = all categories, but NO flags.
    let caps = CapabilitySet::all().with_flags(std::iter::empty());
    let handle = build_lightweight_fork(caps);

    let drafts = tempfile::TempDir::new().expect("tempdir");
    let cfg = sample_persona_cfg("denied-draft");
    let err = handle
        .promote(cfg, drafts.path())
        .expect_err("promote must fail without SpawnNewIdentities");

    match err {
        ForkError::CapabilityDenied => {}
        other => panic!("expected CapabilityDenied, got {other:?}"),
    }

    // Draft must NOT have been written.
    assert!(
        !drafts.path().join("denied-draft.kdl").exists(),
        "draft KDL must not exist when capability gate denies"
    );
}

/// Persistent fork without jj on PATH (or pointed at a nonexistent workspace)
/// surfaces the right error variant. This is the unit-level guard around the
/// jj-commit step that runs before the draft is written.
#[test]
fn promote_persistent_synthetic_jj_error_or_unavailable() {
    use pattern_runtime::spawn::fork::ForkIsolationState;

    let db = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    let parent_cache = Arc::new(MemoryCache::new(db.clone()));
    let child_cache = Arc::new(MemoryCache::new(db));
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle {
        fork_id: "persist-promote".into(),
        child_id: "child".into(),
        isolation_state: ForkIsolationState::Persistent {
            workspace_path: std::path::PathBuf::from("/tmp/nonexistent-fork-ws-promote"),
            bookmark_name: "agent/promote".into(),
            repo_root: std::path::PathBuf::from("/tmp/nonexistent-fork-repo-promote"),
            child_cache,
            parent_cache: Arc::downgrade(&parent_cache),
            parent_agent_id: "parent".into(),
            cancel_state: cancel,
        },
        spawner_capabilities: CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]),
        cancel_watcher: None,
    };

    let drafts = tempfile::TempDir::new().expect("tempdir");
    let cfg = sample_persona_cfg("persistent-draft");
    let err = handle
        .promote(cfg, drafts.path())
        .expect_err("promote on synthetic persistent fork must fail at jj commit step");

    match err {
        ForkError::JjUnavailable | ForkError::JjOp { .. } => {}
        other => panic!("expected JjUnavailable or JjOp; got {other:?}"),
    }
    // Draft must not have been written when the commit step fails.
    assert!(
        !drafts.path().join("persistent-draft.kdl").exists(),
        "no draft on persistent commit failure"
    );
}
