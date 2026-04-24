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

// ---------------------------------------------------------------------------
// AC3 scope enforcement via MemoryScope for TaskList blocks
// ---------------------------------------------------------------------------

/// Scope enforcement for TaskList blocks: a TaskList block created under the
/// project agent is visible through a project-scope `MemoryScope` binding but
/// invisible through a persona-scope `Full`-isolation binding.
///
/// This is the `MemoryScope`-layer complement to the SQL-layer isolation test
/// in `subscriber_task_list_concurrent.rs::scope_enforcement_project_only`.
/// Both tests are needed: the SQL test verifies that `reconcile_task_list`
/// stores rows under the correct `block_handle`; this test verifies that the
/// `MemoryScope` routing layer enforces the same boundary at the block level.
///
/// Mirrors the requirement from v3-task-skill-blocks.AC10.3:
/// "Scope enforcement: project-scope blocks invisible to persona session."
#[test]
fn tasklist_block_invisible_to_persona_under_full_isolation() {
    // ---- Part 1: Full isolation hides persona blocks ----
    // A project session with Full isolation sees the project's TaskList block
    // but cannot see the persona's block, and cannot write to the persona agent.
    {
        let store = ScopeTestStore::new();
        // Seed a block under the project agent (simulates a TaskList block owned
        // by the project). ScopeTestStore::seed uses text schema, but MemoryScope
        // routing is schema-agnostic — it routes purely by agent_id.
        store.seed(
            "project-agent",
            "sprint-tasks",
            "- [ ] write tests\n- [ ] deploy",
        );
        // Seed a separate block under the persona agent.
        store.seed("persona-agent", "personal-notes", "my personal notes");

        let scope = MemoryScope::new(
            store,
            ScopeBinding::with_project("persona-agent", "project-agent", IsolatePolicy::Full),
        );

        // Project's block IS visible through the Full-isolation scope.
        let project_block = scope
            .get_rendered_content("any", "sprint-tasks")
            .expect("get_rendered_content must not error");
        assert!(
            project_block.is_some(),
            "project-agent's TaskList block must be visible through Full-isolation MemoryScope"
        );
        assert_eq!(
            project_block.as_deref(),
            Some("- [ ] write tests\n- [ ] deploy"),
            "content must match what was seeded under project-agent"
        );

        // Persona's block is INVISIBLE through Full isolation.
        let persona_block = scope
            .get_rendered_content("any", "personal-notes")
            .expect("must not error");
        assert!(
            persona_block.is_none(),
            "persona block must be invisible through Full-isolation MemoryScope"
        );

        // Writes targeting the persona agent are DENIED.
        let write_result = scope.create_block(
            "persona-agent",
            BlockCreate::new(
                "new-persona-block",
                MemoryBlockType::Working,
                BlockSchema::text(),
            ),
        );
        assert!(
            matches!(
                write_result.unwrap_err(),
                MemoryError::IsolationDenied { .. }
            ),
            "Full isolation must deny writes targeting the persona agent"
        );
    }

    // ---- Part 2: Persona passthrough cannot see project blocks ----
    // A persona-only session (passthrough, no project) cannot see the project's
    // TaskList block because passthrough delegates by agent_id — the project
    // agent's block does not exist under the persona agent's namespace.
    {
        let store = ScopeTestStore::new();
        store.seed("project-agent", "sprint-tasks", "project task content");
        store.seed("persona-agent", "personal-notes", "persona content");

        // Passthrough scope: the persona agent sees only its own blocks.
        let scope = MemoryScope::new(store, ScopeBinding::passthrough("persona-agent"));

        // Persona can see its own block.
        let persona_notes = scope
            .get_rendered_content("persona-agent", "personal-notes")
            .expect("must not error");
        assert!(
            persona_notes.is_some(),
            "persona-agent's block must be visible through passthrough scope"
        );

        // Persona scope cannot see project's TaskList block — the scope
        // delegates directly to the store with the caller's agent_id, and
        // "persona-agent" does not own "sprint-tasks".
        let project_block_via_persona = scope
            .get_rendered_content("persona-agent", "sprint-tasks")
            .expect("must not error");
        assert!(
            project_block_via_persona.is_none(),
            "project-agent's TaskList block must not be visible through persona passthrough scope"
        );
    }
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

// ---------------------------------------------------------------------------
// AC9 (Task 9): Skill blocks + MemoryScope::Full isolation
// ---------------------------------------------------------------------------

/// Skill blocks created in project scope are invisible to persona-default
/// sessions under `IsolatePolicy::Full`.
///
/// Scope isolation operates at the routing layer (agent_id scoping) and is
/// schema-agnostic — it applies identically to Skill blocks, TaskList blocks,
/// and any other schema. This test documents the property explicitly for Skill
/// blocks by using a store with a skill-labelled block in project scope and
/// verifying that it is invisible to a persona-only session.
///
/// The implementation property is: `MemoryScope` never inspects `BlockSchema`;
/// routing is entirely based on `agent_id` matching and `IsolatePolicy`. Thus
/// Skill blocks need no special handling and get Full isolation for free.
#[test]
fn skill_block_in_project_scope_invisible_to_persona_under_full_isolation() {
    // Project scope has a skill block; persona scope has a scratchpad.
    // Under Full isolation, the persona session cannot see either block from
    // the other scope.
    let store = ScopeTestStore::new();
    store.seed("project", "my-skill", "# Skill body");
    store.seed("persona", "scratch", "persona scratchpad");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::Full),
    );

    // Persona block ("scratch") is invisible under Full isolation.
    assert!(
        scope
            .get_rendered_content("any", "scratch")
            .unwrap()
            .is_none(),
        "persona 'scratch' block must be invisible to session under Full isolation"
    );

    // Project Skill block ("my-skill") is visible because it belongs to the
    // project agent_id which IS accessible under Full isolation.
    let skill = scope.get_rendered_content("project", "my-skill").unwrap();
    assert!(
        skill.is_some(),
        "project 'my-skill' block must be visible to session under Full isolation"
    );
}

/// Sibling test that uses a real `BlockSchema::Skill` block populated via
/// `seed_skill`, verifying that the LoroDoc metadata is wired correctly and
/// that `get_rendered_content` returns the emitted markdown under Full
/// isolation.
///
/// Demonstrates that `BlockSchema::Skill` obeys scope isolation exactly the
/// same as other schemas — the property holds because `MemoryScope` routes
/// on `agent_id` and `IsolatePolicy`, never on the block schema.
#[test]
fn skill_block_with_real_schema_is_invisible_to_persona_under_full_isolation() {
    use pattern_core::types::memory_types::{SkillMetadata, SkillTrustTier};

    let store = ScopeTestStore::new();

    // Seed a genuine Skill block (BlockSchema::Skill) in the project scope.
    let skill_meta = SkillMetadata {
        name: "my-real-skill".to_string(),
        trust_tier: SkillTrustTier::AdHoc,
        description: Some("A test skill.".to_string()),
        keywords: vec!["test".to_string()],
        hooks: serde_json::Value::Null,
    };
    store.seed_skill(
        "project",
        "my-real-skill",
        skill_meta,
        "# Real Skill\nBody.\n",
    );

    // Seed a plain text block in the persona scope for contrast.
    store.seed("persona", "scratch", "persona scratchpad");

    let scope = MemoryScope::new(
        store,
        ScopeBinding::with_project("persona", "project", IsolatePolicy::Full),
    );

    // Persona block is invisible under Full isolation.
    assert!(
        scope
            .get_rendered_content("any", "scratch")
            .unwrap()
            .is_none(),
        "persona 'scratch' must be invisible under Full isolation"
    );

    // Project Skill block is visible under Full isolation because it belongs
    // to the project agent_id which is accessible.
    let rendered = scope
        .get_rendered_content("project", "my-real-skill")
        .unwrap();
    assert!(
        rendered.is_some(),
        "project Skill block must be visible under Full isolation"
    );
    let content = rendered.unwrap();
    // The rendered content is the emitted markdown — verify key fields are present.
    assert!(
        content.contains("my-real-skill"),
        "rendered content should include the skill name; got: {content}"
    );
    assert!(
        content.contains("ad-hoc"),
        "rendered content should include the trust_tier; got: {content}"
    );
}
