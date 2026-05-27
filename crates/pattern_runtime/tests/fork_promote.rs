// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

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
use pattern_core::traits::MemoryStore;
use pattern_core::types::ids::PersonaId;
use pattern_core::types::memory_types::Scope;
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
/// into a draft persona; the draft KDL file lands on disk and the seed cache
/// is persisted to `<drafts>/<id>.cache/` (C3 fix).
#[test]
fn promote_lightweight_with_flag_creates_draft() {
    use pattern_core::types::block::BlockCreate;
    use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};

    let caps = CapabilitySet::all().with_flags([CapabilityFlag::SpawnNewIdentities]);

    // Build a fork handle whose child cache contains a seeded block so we can
    // verify the .cache dir content.
    let db = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    let parent_cache = Arc::new(MemoryCache::new(db.clone()));
    // Seed agent FK so create_block succeeds.
    let agent = pattern_db::models::Agent {
        id: "parent-agent".to_string(),
        name: "Parent Agent".to_string(),
        description: None,
        model_provider: "anthropic".to_string(),
        model_name: "claude".to_string(),
        system_prompt: "test".to_string(),
        config: pattern_db::Json(serde_json::json!({})),
        enabled_tools: pattern_db::Json(vec![]),
        tool_rules: None,
        status: pattern_db::models::AgentStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    pattern_db::queries::create_agent(&db.get().unwrap(), &agent).expect("seed agent");
    pattern_db::queries::create_agent(
        &db.get().unwrap(),
        &pattern_db::models::Agent {
            id: "child-promote".to_string(),
            name: "Child Agent Promote".to_string(),
            ..agent.clone()
        },
    )
    .expect("seed child agent");
    parent_cache
        .create_block(
            &Scope::global("parent-agent"),
            BlockCreate::new(
                "notes".to_string(),
                MemoryBlockType::Working,
                BlockSchema::text(),
            ),
        )
        .expect("create_block");
    let parent_key = Scope::global("parent-agent").to_db_key();
    let child_key = Scope::global("child-promote").to_db_key();
    let parent_doc = parent_cache.get(&parent_key, "notes").unwrap().unwrap();
    parent_doc.set_text("seed-text", true).expect("set_text");

    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(&parent_key, &child_key)
            .expect("fork_for_child"),
    );
    let cancel = Arc::new(CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-promote".into(),
        "child-promote".into(),
        child_cache,
        parent_key.into(),
        Arc::downgrade(&parent_cache),
        cancel,
    )
    .with_spawner_capabilities(caps);

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
    // The draft uses sibling::mint_draft_kdl format: hyphened field names.
    assert!(
        content.contains("name \"teal-draft\""),
        "draft KDL must contain the persona name; got:\n{content}"
    );
    assert!(
        content.contains("system-prompt \"you are a fork-promoted draft\""),
        "draft KDL must contain the system prompt; got:\n{content}"
    );
    // capabilities block only emitted when the capability set is non-empty;
    // the test uses CapabilitySet::empty() so it should NOT be present.
    assert!(
        !content.contains("capabilities"),
        "draft KDL must NOT include capabilities block for empty capability set; got:\n{content}"
    );
    // Required structural fields from sibling::mint_draft_kdl.
    assert!(
        content.contains("agent-id"),
        "draft KDL must contain agent-id field; got:\n{content}"
    );
    assert!(
        content.contains("model provider="),
        "draft KDL must contain model block; got:\n{content}"
    );

    // C3 fix: seed cache persisted to <drafts>/<id>.cache/*.loro
    let cache_dir = drafts.path().join("teal-draft.cache");
    assert!(
        cache_dir.exists(),
        "seed cache directory must be created at <drafts>/<id>.cache/"
    );
    let snap_files: Vec<_> = std::fs::read_dir(&cache_dir)
        .expect("read cache dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "loro").unwrap_or(false))
        .collect();
    assert!(
        !snap_files.is_empty(),
        "at least one .loro snapshot must be persisted in the seed cache dir"
    );

    // Round-trip: each .loro file must be importable via StructuredDocument
    // and must contain the seed text written before promote. This verifies
    // that the bytes on disk are a valid Loro snapshot, not a truncated or
    // corrupted write.
    use pattern_core::memory::StructuredDocument;
    use pattern_core::types::memory_types::BlockMetadata;

    let mut found_seed_text = false;
    for entry in &snap_files {
        let bytes = std::fs::read(entry.path()).expect("read .loro file");
        let doc = StructuredDocument::from_snapshot_with_metadata(
            &bytes,
            BlockMetadata::standalone(BlockSchema::text()),
            None,
        )
        .expect("round-trip .loro snapshot via StructuredDocument::from_snapshot_with_metadata");
        let text = doc.text_content();
        if text.contains("seed-text") {
            found_seed_text = true;
        }
    }
    assert!(
        found_seed_text,
        "round-tripped .loro snapshots must contain the original seed text 'seed-text'; \
         files inspected: {:?}",
        snap_files.iter().map(|e| e.path()).collect::<Vec<_>>()
    );

    // Phase 6 T6 followup: a `manifest.json` must accompany the snapshots so
    // promote can reconstruct the (label, schema, block_type) tuple required
    // by `MemoryCache::insert_from_snapshot`.
    use pattern_runtime::spawn::fork::{SEED_CACHE_MANIFEST_VERSION, SeedCacheManifest};
    let manifest_path = cache_dir.join("manifest.json");
    assert!(
        manifest_path.exists(),
        "seed cache must include manifest.json (Phase 6 T6 promote-time migration depends on it)"
    );
    let manifest_bytes = std::fs::read(&manifest_path).expect("read seed cache manifest.json");
    let manifest: SeedCacheManifest = serde_json::from_slice(&manifest_bytes)
        .expect("manifest.json must be valid SeedCacheManifest JSON");
    assert_eq!(manifest.version, SEED_CACHE_MANIFEST_VERSION);
    assert_eq!(manifest.persona_id, "teal-draft");
    assert_eq!(
        manifest.entries.len(),
        snap_files.len(),
        "manifest must have one entry per .loro file in the cache dir"
    );
    for entry in &manifest.entries {
        let snap_path = cache_dir.join(&entry.file);
        assert!(
            snap_path.exists(),
            "manifest entry refers to missing snapshot file {}",
            snap_path.display()
        );
    }
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
        cfg: None,
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
