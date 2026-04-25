//! Integration tests for the lightweight fork `discard` path (Phase 3 Task 3).
//!
//! Verifies:
//! - AC4.5: `discard()` drops the fork's child state without propagating to
//!   the parent; the parent sees only its own writes.
//! - The child's `CancelState` is set after `discard` so any in-flight work
//!   observes the cancellation signal.
//! - `discard()` on a `Persistent` fork variant returns `WrongIsolation`.
//!
//! Note: calling `discard` twice is prevented at compile time — `discard(self)`
//! consumes the handle. The `ForkError::AlreadyResolved` variant is reserved
//! for a future `&mut self`-based API; it is not exercisable here.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType};
use pattern_memory::MemoryCache;
use pattern_runtime::spawn::fork::{ForkError, ForkHandle, ForkIsolationState};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn open_cache(parent_id: &str, child_id: &str) -> Arc<MemoryCache> {
    let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"));
    for id in [parent_id, child_id] {
        let agent = pattern_db::models::Agent {
            id: id.to_string(),
            name: format!("Discard Test Agent {id}"),
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
    cache.create_block(agent_id, bc).expect("create_block");
    let doc = cache
        .get(agent_id, label)
        .expect("get after create")
        .expect("block must exist after create");
    doc.set_text(content, true).expect("set_text");
}

// ---------------------------------------------------------------------------
// AC4.5 — discard: child changes do not propagate to parent
// ---------------------------------------------------------------------------

/// Parent writes `"parent-only"`. Fork. Fork writes `"fork-only"`. Discard.
/// Parent still reads `"parent-only"`.
#[test]
fn discard_drops_child_state_does_not_propagate_ac4_5() {
    let parent_id = "fd-ac4-5-parent";
    let child_id = "fd-ac4-5-child";

    let parent_cache = open_cache(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "parent-only");

    // Ensure block is loaded into cache before forking.
    let _ = parent_cache.get(parent_id, "notes").unwrap().unwrap();

    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(parent_id, child_id)
            .expect("fork_for_child"),
    );

    let cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-discard-ac4-5".into(),
        child_id.into(),
        Arc::clone(&child_cache),
        parent_id.into(),
        Arc::downgrade(&parent_cache),
        Arc::clone(&cancel),
    );

    // Fork writes divergent content.
    {
        let child_doc = child_cache
            .get_cached_doc(child_id, "notes")
            .expect("child must have notes block");
        child_doc
            .set_text("fork-only", true)
            .expect("set_text on child");
    }

    // Discard: child state is dropped, parent unchanged.
    handle.discard().expect("discard must succeed");

    // Parent observes only its own content; fork's write is gone.
    let parent_doc = parent_cache
        .get(parent_id, "notes")
        .expect("get")
        .expect("block present");
    assert_eq!(
        parent_doc.text_content(),
        "parent-only",
        "parent must not observe child's discarded write"
    );
}

// ---------------------------------------------------------------------------
// CancelState is set after discard
// ---------------------------------------------------------------------------

/// `discard` must call `request_cancel()` on the child's cancel state so
/// any in-flight turns observe the cancellation signal.
#[test]
fn discard_signals_cancel_state() {
    let parent_id = "fd-cancel-parent";
    let child_id = "fd-cancel-child";

    let parent_cache = open_cache(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "content");
    let _ = parent_cache.get(parent_id, "notes").unwrap().unwrap();

    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(parent_id, child_id)
            .expect("fork_for_child"),
    );

    let cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-cancel-signal".into(),
        child_id.into(),
        child_cache,
        parent_id.into(),
        Arc::downgrade(&parent_cache),
        Arc::clone(&cancel),
    );

    assert!(
        !cancel.is_cancelled(),
        "cancel state must be clear before discard"
    );

    handle.discard().expect("discard must succeed");

    assert!(
        cancel.is_cancelled(),
        "discard must set the child's cancel state"
    );
}

// ---------------------------------------------------------------------------
// Persistent discard: requires jj
// ---------------------------------------------------------------------------

/// `discard` on a `Persistent` fork pointing at a non-existent workspace
/// either returns `JjUnavailable` (when `jj` is not installed on the host)
/// or surfaces the cleanup failures via `DiscardCleanup` (when `jj` IS
/// installed but the workspace/bookmark don't exist). Both are valid
/// outcomes for this synthetic handle — the assertion is that we get a
/// typed `ForkError` rather than a panic or a silent success.
#[test]
fn discard_persistent_synthetic_handle_surfaces_typed_error() {
    let db = std::sync::Arc::new(
        pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"),
    );
    let child_cache = std::sync::Arc::new(pattern_memory::MemoryCache::new(db));
    let cancel_state = std::sync::Arc::new(pattern_runtime::timeout::CancelState::new());
    let handle = ForkHandle {
        fork_id: "test-fork".into(),
        child_id: "test-child".into(),
        isolation_state: ForkIsolationState::Persistent {
            workspace_path: std::path::PathBuf::from("/tmp/nonexistent-fork-ws"),
            bookmark_name: "agent/test".into(),
            repo_root: std::path::PathBuf::from("/tmp/nonexistent-fork-repo"),
            child_cache,
            parent_cache: std::sync::Weak::new(),
            parent_agent_id: "test-parent".into(),
            cancel_state: cancel_state.clone(),
        },
        spawner_capabilities: pattern_core::CapabilitySet::all(),
    };

    match handle.discard() {
        Err(ForkError::JjUnavailable)
        | Err(ForkError::DiscardCleanup { .. })
        | Err(ForkError::JjOp { .. }) => {}
        other => panic!("expected JjUnavailable, DiscardCleanup, or JjOp; got {other:?}"),
    }
    // Cancellation is set regardless of jj outcome (it runs first).
    assert!(
        cancel_state.is_cancelled(),
        "discard must set cancel state before attempting jj cleanup"
    );
}

// ---------------------------------------------------------------------------
// ForkError::AlreadyResolved — documented unavailability
// ---------------------------------------------------------------------------

/// `ForkError::AlreadyResolved` is a valid, displayable error variant.
///
/// The current API prevents double-discard at compile time (consuming `self`).
/// This test confirms the variant is present and has a non-empty display in
/// case a future `&mut self` API is added.
#[test]
fn already_resolved_error_is_displayable() {
    let err = ForkError::AlreadyResolved;
    let msg = err.to_string();
    assert!(
        !msg.is_empty(),
        "AlreadyResolved error must have a non-empty display message"
    );
    assert!(
        msg.contains("resolved") || msg.contains("already"),
        "AlreadyResolved display should describe the double-resolution; got: {msg:?}"
    );
}
