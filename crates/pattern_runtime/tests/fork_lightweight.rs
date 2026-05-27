// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration tests for the lightweight fork path (Phase 3 Tasks 1-3).
//!
//! Verifies:
//! - AC4.1: parent and fork can write to their respective memory states
//!   independently (no cross-contamination).
//! - AC4.5: `discard()` drops the fork's child state without propagating to
//!   the parent.
//!
//! These tests use `MemoryCache` directly (no LLM round-trip) to keep the
//! suite fast and CI-safe.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope};
use pattern_db::ConstellationDb;
use pattern_memory::MemoryCache;
use pattern_runtime::spawn::fork::{ForkError, ForkHandle};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn open_cache_with_extra_agent(parent_id: &str, child_id: &str) -> Arc<MemoryCache> {
    let db = Arc::new(ConstellationDb::open_in_memory().expect("open in-memory db"));
    for id in [parent_id, child_id] {
        let agent = pattern_db::models::Agent {
            id: id.to_string(),
            name: format!("Fork Test Agent {id}"),
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
        pattern_db::queries::create_agent(&db.get().unwrap(), &agent)
            .expect("create_agent FK seed");
    }
    Arc::new(MemoryCache::new(db))
}

fn seed_text_block(cache: &MemoryCache, agent_id: &str, label: &str, content: &str) {
    let bc = BlockCreate::new(
        label.to_string(),
        MemoryBlockType::Working,
        BlockSchema::text(),
    );
    cache.create_block(&Scope::global(agent_id), bc).expect("create_block");
    let doc = cache
        .get(&Scope::global(agent_id).to_db_key(), label)
        .expect("get after create")
        .expect("block must exist after create");
    doc.set_text(content, true).expect("set_text");
}

// ---------------------------------------------------------------------------
// AC4.1 — lightweight fork: independent writes, no cross-contamination
// ---------------------------------------------------------------------------

/// Parent has block `notes` with content `"initial"`. Spawn a lightweight
/// fork. Fork writes `"fork-change"` to its copy. Parent writes
/// `"parent-change"` to its copy. Assert each side observes only its own write.
#[test]
fn lightweight_fork_isolates_writes_ac4_1() {
    let parent_id = "ac4-1-parent";
    let child_id = "ac4-1-child";
    let parent_key = Scope::global(parent_id).to_db_key();
    let child_key = Scope::global(child_id).to_db_key();

    let parent_cache = open_cache_with_extra_agent(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "initial");

    // Load the block into cache so fork sees it.
    let _doc = parent_cache
        .get(&parent_key, "notes")
        .expect("get")
        .expect("block present");

    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(&parent_key, &child_key)
            .expect("fork_for_child"),
    );

    let child_cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    let _handle = ForkHandle::new_lightweight(
        "fork-ac4-1".into(),
        child_id.into(),
        Arc::clone(&child_cache),
        parent_key.clone().into(),
        Arc::downgrade(&parent_cache),
        Arc::clone(&child_cancel),
    );

    // Write divergent content on the child side.
    {
        let child_doc = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child must have a block named 'notes'");
        child_doc
            .set_text("fork-change", true)
            .expect("set_text on child");
    }

    // Write on the parent side.
    {
        let parent_doc = parent_cache
            .get(&parent_key, "notes")
            .expect("get")
            .expect("block present");
        parent_doc
            .set_text("parent-change", true)
            .expect("set_text on parent");
    }

    // Assert isolation: parent sees "parent-change", child sees "fork-change".
    {
        let parent_doc = parent_cache
            .get(&parent_key, "notes")
            .expect("get")
            .expect("block present");
        assert_eq!(
            parent_doc.text_content(),
            "parent-change",
            "parent should observe its own write"
        );
    }
    {
        let child_doc = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child must have a block named 'notes'");
        assert_eq!(
            child_doc.text_content(),
            "fork-change",
            "child should observe its own write, not the parent's"
        );
    }
}

// ---------------------------------------------------------------------------
// AC4.5 — discard: child changes do not propagate to parent
// ---------------------------------------------------------------------------

/// Parent writes `"parent-only"`. Fork. Fork writes `"fork-only"`. Discard.
/// Parent still reads `"parent-only"`.
#[test]
fn discard_drops_child_state_ac4_5() {
    let parent_id = "ac4-5-parent";
    let child_id = "ac4-5-child";
    let parent_key = Scope::global(parent_id).to_db_key();
    let child_key = Scope::global(child_id).to_db_key();

    let parent_cache = open_cache_with_extra_agent(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "parent-only");

    // Ensure block is loaded into the cache before forking.
    let _ = parent_cache.get(&parent_key, "notes").unwrap().unwrap();

    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(&parent_key, &child_key)
            .expect("fork_for_child"),
    );

    let child_cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-ac4-5".into(),
        child_id.into(),
        Arc::clone(&child_cache),
        parent_key.clone().into(),
        Arc::downgrade(&parent_cache),
        Arc::clone(&child_cancel),
    );

    // Write on fork side.
    {
        let entry = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child must have a block named 'notes'");
        entry
            .set_text("fork-only", true)
            .expect("set_text on child");
    }

    // Discard: child state should be dropped, cancel requested.
    handle.discard().expect("discard must succeed");

    // Parent should still read "parent-only".
    let parent_doc = parent_cache
        .get(&parent_key, "notes")
        .expect("get")
        .expect("block present");
    assert_eq!(
        parent_doc.text_content(),
        "parent-only",
        "parent must not observe child's discarded write"
    );

    // The cancel state should be set.
    assert!(
        child_cancel.is_cancelled(),
        "discard should request cancel on child cancel state"
    );
}

// ---------------------------------------------------------------------------
// ForkError semantics
// ---------------------------------------------------------------------------

/// `ForkError::WrongIsolation` is returned when `merge_back_lightweight` is
/// called on a `Persistent` variant (stub).
#[test]
fn wrong_isolation_error_is_distinct() {
    let err = ForkError::WrongIsolation;
    let display = err.to_string();
    assert!(
        display.contains("wrong fork isolation"),
        "WrongIsolation error should describe the mode mismatch; got: {display}"
    );
}

/// `ForkError::ParentDropped` describes the missing Weak upgrade case.
#[test]
fn parent_dropped_error_is_distinct() {
    let err = ForkError::ParentDropped;
    let display = err.to_string();
    assert!(
        display.contains("parent memory cache"),
        "ParentDropped error should mention parent memory cache; got: {display}"
    );
}
