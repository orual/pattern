//! Integration tests for MemoryScope isolation policies.
//!
//! Verifies AC12.1–AC12.6 acceptance criteria using the MemoryScope wrapper
//! around an InMemoryMemoryStore-style stub.

use pattern_core::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{
    BlockFilter, BlockMetadataPatch, BlockSchema, IsolatePolicy, MemoryBlockType, MemoryError,
};
use pattern_memory::scope::{MemoryScope, ScopeBinding};
use pattern_memory::testing::ScopeTestStore;

// ---------------------------------------------------------------------------
// AC12.1: IsolatePolicy::None — bidirectional merge
// ---------------------------------------------------------------------------

#[test]
fn ac12_1_none_reads_merge_persona_and_project() {
    let store = ScopeTestStore::new();
    store.seed("persona", "scratchpad", "persona scratchpad content");
    store.seed("project", "notes", "project notes content");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::None),
    );

    // Both scopes' blocks are visible.
    let scratch = scope.get_rendered_content("persona", "scratchpad").unwrap();
    assert_eq!(scratch.as_deref(), Some("persona scratchpad content"));

    let notes = scope.get_rendered_content("project", "notes").unwrap();
    assert_eq!(notes.as_deref(), Some("project notes content"));
}

#[test]
fn ac12_1_none_write_to_persona_flows_through() {
    let store = ScopeTestStore::new();
    store.seed("persona", "scratchpad", "original");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::None),
    );

    // Write to persona succeeds under None (bidirectional).
    scope
        .update_block_metadata(
            "persona",
            "scratchpad",
            BlockMetadataPatch::default().pinned(true),
        )
        .expect("write to persona should succeed under None");
}

// ---------------------------------------------------------------------------
// AC12.2: IsolatePolicy::CoreOnly — persona core read-only
// ---------------------------------------------------------------------------

#[test]
fn ac12_2_core_only_reads_persona_as_readonly() {
    let store = ScopeTestStore::new();
    store.seed("persona", "scratchpad", "persona content");
    store.seed("project", "readme", "project content");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::CoreOnly),
    );

    // Persona block visible but read-only.
    let doc = scope.get_block("any", "scratchpad").unwrap().unwrap();
    assert_eq!(
        doc.metadata().permission,
        pattern_core::types::memory_types::MemoryPermission::ReadOnly,
    );

    // Project block is writable (default permission).
    let project_doc = scope.get_block("any", "readme").unwrap().unwrap();
    assert_ne!(
        project_doc.metadata().permission,
        pattern_core::types::memory_types::MemoryPermission::ReadOnly,
    );
}

#[test]
fn ac12_2_core_only_denies_persona_write() {
    let store = ScopeTestStore::new();
    store.seed("persona", "scratchpad", "content");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::CoreOnly),
    );

    let result = scope.update_block_metadata(
        "persona",
        "scratchpad",
        BlockMetadataPatch::default().pinned(true),
    );
    assert!(result.is_err());
    match result.unwrap_err() {
        MemoryError::IsolationDenied { policy, .. } => {
            assert_eq!(policy, IsolatePolicy::CoreOnly);
        }
        other => panic!("expected IsolationDenied, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// AC12.3: IsolatePolicy::Full — persona invisible
// ---------------------------------------------------------------------------

#[test]
fn ac12_3_full_persona_blocks_invisible() {
    let store = ScopeTestStore::new();
    store.seed("persona", "scratchpad", "persona content");
    store.seed("project", "readme", "project content");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::Full),
    );

    // Persona block invisible.
    assert!(
        scope
            .get_rendered_content("any", "scratchpad")
            .unwrap()
            .is_none()
    );

    // Project block visible.
    assert_eq!(
        scope
            .get_rendered_content("any", "readme")
            .unwrap()
            .as_deref(),
        Some("project content")
    );
}

#[test]
fn ac12_3_full_search_is_project_only() {
    let store = ScopeTestStore::new();
    store.seed("persona", "persona-block", "persona");
    store.seed("project", "project-block", "project");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::Full),
    );

    let blocks = scope.list_blocks(BlockFilter::all()).unwrap();
    let labels: Vec<&str> = blocks.iter().map(|b| b.label.as_str()).collect();
    assert!(labels.contains(&"project-block"));
    assert!(!labels.contains(&"persona-block"));
}

// ---------------------------------------------------------------------------
// AC12.6: Default write target is project scope
// ---------------------------------------------------------------------------

#[test]
fn ac12_6_none_default_write_goes_to_project() {
    let store = ScopeTestStore::new();

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::None),
    );

    // Write to project-id (the default write target in the SDK handler).
    let doc = scope
        .create_block(
            "project",
            BlockCreate::new("task-list", MemoryBlockType::Working, BlockSchema::text()),
        )
        .expect("write to project should succeed");

    // Verify the block was created under the project scope.
    assert_eq!(doc.metadata().agent_id, "project");

    // Reading back via the scope should find it.
    let inner = scope.inner();
    let fetched = inner
        .get_block("project", "task-list")
        .unwrap()
        .expect("block should exist in project scope");
    assert_eq!(fetched.metadata().agent_id, "project");
}

// ---------------------------------------------------------------------------
// Edge: Passthrough (no project) works as pure delegation
// ---------------------------------------------------------------------------

#[test]
fn passthrough_no_project_is_transparent() {
    let store = ScopeTestStore::new();
    store.seed("agent-1", "notes", "hello world");

    let scope = MemoryScope::new(store, ScopeBinding::passthrough("agent-1"));

    let content = scope.get_rendered_content("agent-1", "notes").unwrap();
    assert_eq!(content.as_deref(), Some("hello world"));

    // Write also works.
    scope
        .create_block(
            "agent-1",
            BlockCreate::new("new", MemoryBlockType::Core, BlockSchema::text()),
        )
        .expect("passthrough write should succeed");
}

// ---------------------------------------------------------------------------
// AC12.search_archival: search_archival under IsolatePolicy::None merges
// results from both the persona and project stores.
// ---------------------------------------------------------------------------

/// search_archival under IsolatePolicy::None must return entries from BOTH
/// the persona store and the project store, up to the requested limit.
///
/// Regression test for the review finding that the None-policy merge path in
/// `MemoryScope::search_archival` was untested with real data (the original
/// stub previously returned empty results for all archival queries).
#[test]
fn search_archival_none_policy_merges_persona_and_project() {
    let store = ScopeTestStore::new();

    // Seed 2 archival entries under the persona agent_id.
    store.seed_archival("persona", "p-entry-1", "persona note one");
    store.seed_archival("persona", "p-entry-2", "persona note two");

    // Seed 3 archival entries under the project agent_id.
    store.seed_archival("project", "proj-entry-1", "project note alpha");
    store.seed_archival("project", "proj-entry-2", "project note beta");
    store.seed_archival("project", "proj-entry-3", "project note gamma");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::None),
    );

    // Full merge: limit=10 — expect all 5 entries (2 persona + 3 project).
    let results = scope
        .search_archival("persona", "note", 10)
        .expect("search_archival should succeed under None policy");

    assert_eq!(
        results.len(),
        5,
        "None policy should merge persona (2) + project (3) = 5 entries, got {}",
        results.len()
    );

    // Verify entries from both scopes are present.
    let ids: Vec<&str> = results.iter().map(|e| e.id.as_str()).collect();
    assert!(
        ids.contains(&"p-entry-1") || ids.contains(&"p-entry-2"),
        "results must include at least one persona entry; got ids: {ids:?}"
    );
    assert!(
        ids.contains(&"proj-entry-1")
            || ids.contains(&"proj-entry-2")
            || ids.contains(&"proj-entry-3"),
        "results must include at least one project entry; got ids: {ids:?}"
    );

    // Limit enforcement: limit=3 should return at most 3 entries.
    let limited = scope
        .search_archival("persona", "note", 3)
        .expect("search_archival with limit=3 should succeed");

    assert!(
        limited.len() <= 3,
        "limit=3 must cap results to at most 3, got {}",
        limited.len()
    );
}

/// search_archival under IsolatePolicy::Full returns only project entries,
/// not persona entries.
#[test]
fn search_archival_full_policy_returns_project_only() {
    let store = ScopeTestStore::new();

    store.seed_archival("persona", "p-1", "persona secret note");
    store.seed_archival("project", "proj-1", "project note");
    store.seed_archival("project", "proj-2", "another project note");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::Full),
    );

    let results = scope
        .search_archival("persona", "note", 10)
        .expect("search_archival should succeed under Full policy");

    // Under Full, only project entries are returned.
    assert_eq!(
        results.len(),
        2,
        "Full policy should return only project entries (2), got {}",
        results.len()
    );

    let ids: Vec<&str> = results.iter().map(|e| e.id.as_str()).collect();
    assert!(
        !ids.contains(&"p-1"),
        "persona entry must not appear under Full policy; got ids: {ids:?}"
    );
}
