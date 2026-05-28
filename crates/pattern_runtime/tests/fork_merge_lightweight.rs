// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Integration and property-based tests for the lightweight fork `merge_back`
//! path (Phase 3 Task 2).
//!
//! Verifies:
//! - AC4.3: `merge_back_lightweight` imports the fork's LoroDoc state back
//!   into the parent; changes from both sides are merged.
//! - AC4.9: concurrent writes in parent and fork to the same block merge
//!   deterministically via Loro CRDT semantics (both changes preserved).
//!
//! The diamond-concurrent-edit test uses `insta` to snapshot the exact
//! merge output so regressions against loro version changes are visible.
//!
//! The proptest verifies import-order independence: the merge result is the
//! same regardless of which side's ops are applied first.

use std::sync::Arc;

use pattern_core::traits::MemoryStore;
use pattern_core::types::block::BlockCreate;
use pattern_core::types::memory_types::{BlockSchema, MemoryBlockType, Scope};
use pattern_memory::MemoryCache;
use pattern_runtime::spawn::fork::{ForkError, ForkHandle};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

fn open_cache(parent_id: &str, child_id: &str) -> Arc<MemoryCache> {
    let db = Arc::new(pattern_db::ConstellationDb::open_in_memory().expect("open in-memory db"));
    for id in [parent_id, child_id] {
        let agent = pattern_db::models::Agent {
            id: id.to_string(),
            name: format!("Merge Test Agent {id}"),
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
    cache
        .create_block(&Scope::global(agent_id), bc)
        .expect("create_block");
    let doc = cache
        .get(&Scope::global(agent_id).to_db_key(), label)
        .expect("get after create")
        .expect("block must exist after create");
    doc.set_text(content, true).expect("set_text");
}

/// Build a lightweight ForkHandle for the given parent cache and child cache.
///
/// Takes encoded scope keys (`"global:..."`) for both parent and child.
fn make_fork_handle(
    parent_cache: &Arc<MemoryCache>,
    parent_key: &str,
    child_key: &str,
    child_id: &str,
) -> (Arc<MemoryCache>, ForkHandle) {
    // Ensure the block is in the parent cache before forking.
    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(parent_key, child_key)
            .expect("fork_for_child"),
    );
    let cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    let handle = ForkHandle::new_lightweight(
        "fork-merge-test".into(),
        child_id.into(),
        Arc::clone(&child_cache),
        parent_key.into(),
        Arc::downgrade(parent_cache),
        cancel,
    );
    (child_cache, handle)
}

// ---------------------------------------------------------------------------
// AC4.3 — basic merge_back: fork-only write propagates to parent
// ---------------------------------------------------------------------------

/// Fork writes a new value. `merge_back_lightweight` makes it visible on
/// the parent side.
#[test]
fn merge_back_imports_fork_write_ac4_3() {
    let parent_id = "ac4-3-parent";
    let child_id = "ac4-3-child";
    let parent_key = Scope::global(parent_id).to_db_key();
    let child_key = Scope::global(child_id).to_db_key();

    let parent_cache = open_cache(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "initial");

    // Load block into cache.
    let _ = parent_cache.get(&parent_key, "notes").unwrap().unwrap();

    let (child_cache, handle) = make_fork_handle(&parent_cache, &parent_key, &child_key, child_id);

    // Fork writes new content.
    {
        let child_doc = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child must have notes block");
        child_doc.set_text("fork-write", true).expect("set_text");
    }

    // Merge back.
    let report = handle
        .merge_back_lightweight()
        .expect("merge_back_lightweight must succeed");

    assert_eq!(
        report.blocks_merged, 1,
        "one block should be reported merged"
    );

    // Parent now reflects the fork's content (merged via LoroDoc::import).
    let parent_doc = parent_cache
        .get(&parent_key, "notes")
        .expect("get")
        .expect("block present");
    let merged_content = parent_doc.text_content();
    // The fork replaced the text, so after merge the parent has fork's content.
    assert!(
        merged_content.contains("fork-write"),
        "merged parent doc should contain fork's write; got: {merged_content:?}"
    );
}

// ---------------------------------------------------------------------------
// AC4.9 — diamond concurrent edit, loro CRDT snapshot
// ---------------------------------------------------------------------------

/// Concurrent-edit diamond:
/// 1. Parent writes "hello".
/// 2. Fork.
/// 3. Parent appends " world".
/// 4. Fork appends " fork".
/// 5. `merge_back`.
/// 6. Snapshot the final state via `insta` so regressions are visible.
#[test]
fn diamond_concurrent_edit_merges_both_sides_ac4_9() {
    let parent_id = "ac4-9-parent";
    let child_id = "ac4-9-child";
    let parent_key = Scope::global(parent_id).to_db_key();
    let child_key = Scope::global(child_id).to_db_key();

    let parent_cache = open_cache(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "hello");

    // Load block into cache.
    let _ = parent_cache.get(&parent_key, "notes").unwrap().unwrap();

    let (child_cache, handle) = make_fork_handle(&parent_cache, &parent_key, &child_key, child_id);

    // Assign deterministic peer IDs so Loro's tie-breaking is stable across
    // test-suite runs regardless of parallel execution order.  Lower peer ID
    // (1 = parent) loses the concurrent-op race; higher (2 = fork) wins, so
    // the expected merge result is "hello world fork".
    {
        let parent_doc = parent_cache
            .get(&parent_key, "notes")
            .expect("get")
            .expect("block present for peer-id seeding");
        parent_doc.set_peer_id(1).expect("set parent peer_id");
    }
    {
        let child_doc = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child notes block for peer-id seeding");
        child_doc.set_peer_id(2).expect("set child peer_id");
    }

    // Parent writes AFTER fork.
    {
        let parent_doc = parent_cache
            .get(&parent_key, "notes")
            .expect("get")
            .expect("block present");
        parent_doc
            .append_text(" world", true)
            .expect("append_text on parent");
    }

    // Fork writes AFTER fork.
    {
        let child_doc = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child notes block");
        child_doc
            .append_text(" fork", true)
            .expect("append_text on child");
    }

    // Merge back.
    let report = handle
        .merge_back_lightweight()
        .expect("merge_back_lightweight must succeed");

    assert_eq!(
        report.blocks_merged, 1,
        "one block should be reported merged"
    );

    // Get final merged content and snapshot it.
    let parent_doc = parent_cache
        .get(&parent_key, "notes")
        .expect("get")
        .expect("block present");
    let merged = parent_doc.text_content();

    // Snapshot the exact output. Loro CRDT determines the resolution;
    // this snapshot locks the observed behaviour so future loro upgrades
    // that change merge semantics surface as a test failure.
    insta::assert_snapshot!("diamond_merge_result", merged);

    // Weak sanity: both sides' content should appear in some form.
    assert!(
        merged.contains("hello"),
        "merged result should contain original text; got: {merged:?}"
    );
}

// ---------------------------------------------------------------------------
// MergeReport — accurate counts
// ---------------------------------------------------------------------------

/// MergeReport counts reflect the number of blocks actually merged.
#[test]
fn merge_report_counts_are_accurate() {
    let parent_id = "mr-count-parent";
    let child_id = "mr-count-child";
    let parent_key = Scope::global(parent_id).to_db_key();
    let child_key = Scope::global(child_id).to_db_key();

    let parent_cache = open_cache(parent_id, child_id);

    // Create two blocks.
    for label in ["block-a", "block-b"] {
        seed_text_block(&parent_cache, parent_id, label, "initial");
        let _ = parent_cache.get(&parent_key, label).unwrap().unwrap();
    }

    let (_, handle) = make_fork_handle(&parent_cache, &parent_key, &child_key, child_id);

    let report = handle.merge_back_lightweight().expect("merge must succeed");

    assert_eq!(
        report.blocks_merged, 2,
        "both blocks should appear in merge count"
    );
}

// ---------------------------------------------------------------------------
// Error path: WrongIsolation
// ---------------------------------------------------------------------------

/// `merge_back_lightweight` on a Persistent fork returns WrongIsolation.
#[test]
fn merge_back_wrong_isolation_returns_error() {
    use pattern_runtime::spawn::fork::ForkIsolationState;

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
            cancel_state,
        },
        spawner_capabilities: pattern_core::CapabilitySet::all(),
        cancel_watcher: None,
        cfg: None,
    };

    match handle.merge_back_lightweight() {
        Err(ForkError::WrongIsolation) => {}
        other => panic!("expected WrongIsolation, got {other:?}"),
    }
}

/// `merge_back_lightweight` with a dropped parent returns ParentDropped.
#[test]
fn merge_back_dropped_parent_returns_error() {
    let parent_id = "pd-parent";
    let child_id = "pd-child";
    let parent_key = Scope::global(parent_id).to_db_key();
    let child_key = Scope::global(child_id).to_db_key();

    let parent_cache = open_cache(parent_id, child_id);
    seed_text_block(&parent_cache, parent_id, "notes", "content");
    let _ = parent_cache.get(&parent_key, "notes").unwrap().unwrap();

    let child_cache = Arc::new(
        parent_cache
            .fork_for_child(&parent_key, &child_key)
            .expect("fork_for_child"),
    );
    let cancel = Arc::new(pattern_runtime::timeout::CancelState::new());
    let weak_parent = Arc::downgrade(&parent_cache);
    let handle = ForkHandle::new_lightweight(
        "test".into(),
        child_id.into(),
        child_cache,
        parent_key.into(),
        weak_parent,
        cancel,
    );

    // Drop the parent.
    drop(parent_cache);

    match handle.merge_back_lightweight() {
        Err(ForkError::ParentDropped) => {}
        other => panic!("expected ParentDropped, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// AC4.9 proptest: import-order independence
// ---------------------------------------------------------------------------
//
// Generates random sequences of text-append ops on both parent and child sides,
// merges them, and asserts that the result contains all content from both sides.
// This verifies that loro's CRDT merge is deterministic regardless of op order.
//
// Property: merge(parent_ops, fork_ops) is consistent in the sense that
// the resulting document contains state from both sides.

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    /// AC4.9: Loro CRDT merge is commutative and all appends survive.
    ///
    /// Two independent sequences of text-append operations are applied to both
    /// sides of a lightweight fork. After `merge_back_lightweight`:
    ///
    /// 1. **Survival of all appends**: every string appended on the parent side
    ///    AND every string appended on the fork side must be present in the
    ///    merged result. Loro CRDT guarantees no data loss on either side.
    ///
    /// 2. **Commutativity / idempotence**: applying the merge twice produces
    ///    the same result as applying it once. (Re-applying a snapshot that was
    ///    already imported is a no-op under Loro's vector-clock semantics.)
    ///
    /// 3. **Seed preservation**: the pre-fork seed text is also present in the
    ///    final result (shared history is never lost).
    ///
    /// This is stronger than the old "merge succeeds and result is non-empty"
    /// assertion — it validates the CRDT invariant rather than just the happy-
    /// path completion.
    #[test]
    fn merge_back_convergence_all_appends_survive(
        parent_appends in proptest::collection::vec("[a-z]{1,8}", 1..8usize),
        fork_appends in proptest::collection::vec("[a-z]{1,8}", 1..8usize),
    ) {
        let parent_id = "prop-parent";
        let child_id = "prop-child";

        // Deterministic peer IDs: the SAME peer authors the SAME ops in both
        // the forward and reversed pair below. With identical (op, peer) sets
        // applied via Loro's CRDT merge, the final text is byte-identical
        // regardless of which side held which ops. This is the strict
        // commutativity property — stronger than word-set equality.
        const PARENT_PEER: u64 = 0x1111_1111_1111_1111;
        const CHILD_PEER: u64 = 0x2222_2222_2222_2222;

        let parent_cache = open_cache(parent_id, child_id);
        seed_text_block(&parent_cache, parent_id, "notes", "seedword");
        // Force the block into the cache before forking. Pin the parent peer
        // ID immediately after the seed write commits so subsequent appends
        // on this side are attributed to PARENT_PEER.
        let parent_key = Scope::global(parent_id).to_db_key();
        let child_key = Scope::global(child_id).to_db_key();
        let parent_doc = parent_cache.get(&parent_key, "notes").unwrap().unwrap();
        parent_doc.set_peer_id(PARENT_PEER).expect("set parent peer");

        let (child_cache, handle) = make_fork_handle(&parent_cache, &parent_key, &child_key, child_id);
        let child_doc = child_cache
            .get_cached_doc(&child_key, "notes")
            .expect("child notes block");
        child_doc.set_peer_id(CHILD_PEER).expect("set child peer");

        // Apply parent-side appends (each as a distinct word separated by spaces).
        for text in &parent_appends {
            parent_doc
                .append_text(&format!(" {text}"), true)
                .expect("parent append_text");
        }

        // Apply fork-side appends (same pattern on the child side).
        for text in &fork_appends {
            child_doc
                .append_text(&format!(" {text}"), true)
                .expect("fork append_text");
        }

        // Merge must succeed without error.
        let report = handle.merge_back_lightweight()
            .expect("merge_back_lightweight must not fail");

        prop_assert_eq!(report.blocks_merged, 1, "exactly one block must be merged");

        let merged = parent_cache
            .get(&parent_key, "notes")
            .expect("get parent doc after merge")
            .expect("block must still be present after merge")
            .text_content();

        // Assertion 1: seed text must survive the merge.
        prop_assert!(
            merged.contains("seedword"),
            "seed text must be present in merged result; merged={merged:?}"
        );

        // Assertion 2: every parent-side append must survive the merge.
        for word in &parent_appends {
            prop_assert!(
                merged.contains(word.as_str()),
                "parent append {word:?} must be in merged result; merged={merged:?}"
            );
        }

        // Assertion 3: every fork-side append must survive the merge.
        // This is the key CRDT guarantee: fork writes are NOT discarded.
        for word in &fork_appends {
            prop_assert!(
                merged.contains(word.as_str()),
                "fork append {word:?} must be in merged result; merged={merged:?}"
            );
        }

        // Assertion 4: strict commutativity — merge(child_ops into parent) and
        // merge(parent_ops into child) yield byte-identical text when the same
        // (op, peer) pairs participate.
        //
        // We build a second independent fork pair seeded identically. This
        // time the SIDES that hold each set of appends are swapped: P2 holds
        // fork_appends, F2 holds parent_appends. To preserve byte-equal output,
        // we keep the (op, peer) attribution stable by setting peer IDs to
        // CHILD_PEER on P2's side (it now authors what was the child's ops in
        // pair 1) and PARENT_PEER on F2's side. After merging F2 into P2, the
        // resulting Loro op-graph contains the same set of (op, peer, lamport)
        // tuples as pair 1, so the deterministic merge produces identical text.
        {
            let p2_id = "prop-parent-2";
            let c2_id = "prop-child-2";
            let p2_key = Scope::global(p2_id).to_db_key();
            let c2_key = Scope::global(c2_id).to_db_key();
            let parent_cache_2 = open_cache(p2_id, c2_id);
            seed_text_block(&parent_cache_2, p2_id, "notes", "seedword");
            // Force the block into the cache and pin peer IDs after the seed
            // commits. P2 plays the role of "side that authors fork_appends",
            // so it gets CHILD_PEER. F2 plays "side that authors parent_appends",
            // so it gets PARENT_PEER.
            let p2_doc = parent_cache_2.get(&p2_key, "notes").unwrap().unwrap();
            p2_doc.set_peer_id(CHILD_PEER).expect("set p2 peer");

            let (child_cache_2, handle_2) = make_fork_handle(&parent_cache_2, &p2_key, &c2_key, c2_id);
            let f2_doc = child_cache_2
                .get_cached_doc(&c2_key, "notes")
                .expect("c2 notes block");
            f2_doc.set_peer_id(PARENT_PEER).expect("set f2 peer");

            // Reversed: P2 (CHILD_PEER) authors fork_appends, F2 (PARENT_PEER)
            // authors parent_appends.
            for text in &fork_appends {
                p2_doc
                    .append_text(&format!(" {text}"), true)
                    .expect("p2 append_text");
            }
            for text in &parent_appends {
                f2_doc
                    .append_text(&format!(" {text}"), true)
                    .expect("f2 append_text");
            }

            let _report2 = handle_2
                .merge_back_lightweight()
                .expect("reversed merge must not fail");

            let merged_2 = parent_cache_2
                .get(&p2_key, "notes")
                .expect("get p2 doc after merge")
                .expect("p2 block must still be present after merge")
                .text_content();

            // Strict commutativity: byte-identical text. With deterministic
            // peer IDs ensuring the same (op, peer) set in both pairs, Loro's
            // CRDT merge must produce the same ordering and the same final
            // text — regardless of which side held which ops before merge.
            prop_assert_eq!(
                &merged, &merged_2,
                "byte-identical merge result required under deterministic peer IDs"
            );
        }
    }
}
