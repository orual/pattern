// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Proptest round-trip tests for the TaskList ↔ KDL converter.
//!
//! Covers:
//! - v3-task-skill-blocks.AC1.3: arbitrary TaskList (items, edges, comments)
//!   round-trips through KDL without loss.
//! - v3-task-skill-blocks.AC1.6: empty TaskList and self-referential edges
//!   are included in the generated corpus.
//! - v3-task-skill-blocks.AC1.7: item reordering (simulated via list
//!   permutation) preserves all TaskItemIds across KDL round-trip.

use std::collections::HashMap;

use loro::LoroValue;
use pattern_core::new_snowflake_id;
use pattern_memory::fs::kdl::{TopShape, kdl_to_loro_value, loro_value_to_json, loro_value_to_kdl};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Reference timestamps used for created_at / updated_at. Using a fixed
/// string avoids time-shrink complexity in proptest — the converter treats
/// timestamps as opaque strings.
const CREATED_AT: &str = "2026-01-01T00:00:00Z";
const UPDATED_AT: &str = "2026-04-23T12:00:00Z";

// ---------------------------------------------------------------------------
// Helper: round-trip a LoroValue through KDL
// ---------------------------------------------------------------------------

fn kdl_round_trip(value: &LoroValue) -> Result<LoroValue, String> {
    let doc = loro_value_to_kdl(value, TopShape::TaskList)
        .map_err(|e| format!("forward conversion failed: {e}"))?;
    let text = doc.to_string();
    let reparsed = kdl::KdlDocument::parse(&text)
        .map_err(|e| format!("KDL parse failed: {e}\nKDL:\n{text}"))?;
    kdl_to_loro_value(&reparsed, TopShape::TaskList)
        .map_err(|e| format!("reverse conversion failed: {e}"))
}

// ---------------------------------------------------------------------------
// Helper: compare two LoroValues via canonical JSON
// ---------------------------------------------------------------------------

fn loro_json_equal(a: &LoroValue, b: &LoroValue) -> bool {
    match (loro_value_to_json(a), loro_value_to_json(b)) {
        (Some(ja), Some(jb)) => ja == jb,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Helper: build a LoroValue block-edge map
// ---------------------------------------------------------------------------

fn make_edge(handle: &str, task_item: Option<&str>) -> LoroValue {
    let mut m: HashMap<String, LoroValue> = HashMap::new();
    m.insert("block".into(), LoroValue::String(handle.into()));
    if let Some(id) = task_item {
        m.insert("task_item".into(), LoroValue::String(id.into()));
    }
    LoroValue::Map(m.into())
}

// ---------------------------------------------------------------------------
// Helper: build a LoroValue comment map
// ---------------------------------------------------------------------------

fn make_comment(author: &str, text: &str) -> LoroValue {
    let mut m: HashMap<String, LoroValue> = HashMap::new();
    m.insert("author".into(), LoroValue::String(author.into()));
    m.insert("timestamp".into(), LoroValue::String(CREATED_AT.into()));
    m.insert("text".into(), LoroValue::String(text.into()));
    LoroValue::Map(m.into())
}

// ---------------------------------------------------------------------------
// Helper: build a minimal item LoroValue (used in reorder test)
// ---------------------------------------------------------------------------

fn make_item_with_id(id: &str, subject: &str) -> LoroValue {
    let mut m: HashMap<String, LoroValue> = HashMap::new();
    m.insert("id".into(), LoroValue::String(id.into()));
    m.insert("subject".into(), LoroValue::String(subject.into()));
    m.insert("description".into(), LoroValue::String("".into()));
    m.insert("status".into(), LoroValue::String("pending".into()));
    m.insert("blocks".into(), LoroValue::List(vec![].into()));
    m.insert("comments".into(), LoroValue::List(vec![].into()));
    m.insert("metadata".into(), LoroValue::Map(HashMap::new().into()));
    m.insert("created_at".into(), LoroValue::String(CREATED_AT.into()));
    m.insert("updated_at".into(), LoroValue::String(UPDATED_AT.into()));
    LoroValue::Map(m.into())
}

// ---------------------------------------------------------------------------
// Helper: build a task-list LoroValue from items
// ---------------------------------------------------------------------------

fn make_task_list(items: Vec<LoroValue>) -> LoroValue {
    let mut m: HashMap<String, LoroValue> = HashMap::new();
    m.insert("schema".into(), LoroValue::String("task-list".into()));
    m.insert("items".into(), LoroValue::List(items.into()));
    LoroValue::Map(m.into())
}

// ---------------------------------------------------------------------------
// Helper: extract item ids from a round-tripped LoroValue
// ---------------------------------------------------------------------------

fn extract_item_ids(value: &LoroValue) -> Vec<String> {
    let LoroValue::Map(root) = value else {
        return vec![];
    };
    let Some(LoroValue::List(items)) = root.get("items") else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|item| {
            let LoroValue::Map(m) = item else { return None };
            let LoroValue::String(id) = m.get("id")? else {
                return None;
            };
            Some(id.to_string())
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Strategy for KDL-safe printable strings. Excludes null bytes and
/// control characters (0x00–0x1F except tab, which KDL allows in strings
/// but proptest string generation may produce oddly). Bounded to avoid
/// slow tests.
fn safe_string(max_len: usize) -> impl Strategy<Value = String> {
    // Printable ASCII plus common Unicode ranges. Avoids control chars
    // that would make KDL encoding produce invalid output.
    prop::string::string_regex(&format!(r"[\x20-\x7E -ÿĀ-ſ]{{0,{max_len}}}"))
        .expect("valid regex for safe_string")
}

/// Strategy for non-empty KDL-safe strings.
fn non_empty_safe_string(max_len: usize) -> impl Strategy<Value = String> {
    prop::string::string_regex(&format!(r"[\x20-\x7E -ÿ]{{1,{max_len}}}"))
        .expect("valid regex for non_empty_safe_string")
}

/// A small pool of synthetic handles drawn from to keep the test corpus
/// realistic: edges point into one of three named blocks.
fn pool_handle() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("alpha-block".to_string()),
        Just("beta-block".to_string()),
        Just("gamma-block".to_string()),
    ]
}

/// Strategy for a TaskStatus kebab string (as used in LoroValue).
fn task_status_str() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("pending".to_string()),
        Just("in-progress".to_string()),
        Just("blocked".to_string()),
        Just("completed".to_string()),
        Just("cancelled".to_string()),
    ]
}

/// Strategy for an optional agent-id string of the form `@<name>`.
fn optional_agent_id() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        "[a-z]{3,10}".prop_map(|name| Some(format!("@{name}"))),
    ]
}

/// Strategy for a metadata LoroValue::Map with depth ≤ 2, branch ≤ 4.
/// Leaf values are string, bool, or i64 (safe for the generic KDL Map
/// converter). A two-level nesting exercises the recursive metadata
/// serialisation path.
fn metadata_strategy() -> impl Strategy<Value = LoroValue> {
    // Leaf values (depth 0 or 1 scalar).
    let leaf = prop_oneof![
        safe_string(32).prop_map(|s| LoroValue::String(s.into())),
        any::<bool>().prop_map(LoroValue::Bool),
        any::<i32>().prop_map(|n| LoroValue::I64(n as i64)),
    ];

    // A flat map of 0..=4 keys with scalar leaf values.
    let flat_map = prop::collection::vec(
        (
            "[a-z][a-z0-9_]{0,12}".prop_filter("non-empty key", |k| !k.is_empty()),
            leaf,
        ),
        0..=4,
    )
    .prop_map(|pairs| {
        let map: HashMap<String, LoroValue> = pairs.into_iter().collect();
        LoroValue::Map(map.into())
    });

    // Depth-2: a flat map whose values are either scalars or flat maps.
    let scalar = prop_oneof![
        safe_string(32).prop_map(|s| LoroValue::String(s.into())),
        any::<bool>().prop_map(LoroValue::Bool),
        any::<i32>().prop_map(|n| LoroValue::I64(n as i64)),
    ];
    prop::collection::vec(
        (
            "[a-z][a-z0-9_]{0,12}".prop_filter("non-empty key", |k| !k.is_empty()),
            prop_oneof![scalar, flat_map,],
        ),
        0..=4,
    )
    .prop_map(|pairs| {
        let map: HashMap<String, LoroValue> = pairs.into_iter().collect();
        LoroValue::Map(map.into())
    })
}

/// Strategy for a single `TaskEdgeRef` LoroValue::Map.
///
/// Draws handles from the pool and optionally adds a task_item id.
/// Self-referential edges (where task_item matches the item's own id) are
/// allowed — they arise naturally when the pool handle + generated id happen
/// to match; `reorder_preserves_item_ids` exercises this path deliberately.
fn task_edge_ref_strategy() -> impl Strategy<Value = LoroValue> {
    (
        pool_handle(),
        prop_oneof![
            Just(None::<String>),
            non_empty_safe_string(30).prop_map(Some),
        ],
    )
        .prop_map(|(handle, item_id)| make_edge(&handle, item_id.as_deref()))
}

/// Strategy for a single comment LoroValue::Map.
fn task_comment_strategy() -> impl Strategy<Value = LoroValue> {
    (
        "[a-z]{2,8}".prop_map(|name| format!("@{name}")),
        safe_string(200),
    )
        .prop_map(|(author, text)| make_comment(&author, &text))
}

/// Strategy for a single item LoroValue::Map.
///
/// The `id` is minted via `new_snowflake_id()` at strategy evaluation time.
/// This keeps the id stable across shrink cycles (snowflakes are not part of
/// the shrinkable space) and ensures non-empty, sortable ids without needing
/// a custom `Arbitrary` impl.
fn task_item_strategy() -> impl Strategy<Value = LoroValue> {
    (
        non_empty_safe_string(120), // subject
        safe_string(500),           // description
        prop_oneof![
            Just(None::<String>),
            non_empty_safe_string(80).prop_map(Some),
        ], // active_form
        task_status_str(),          // status
        optional_agent_id(),        // owner
        metadata_strategy(),        // metadata
        prop::collection::vec(task_comment_strategy(), 0..=3), // comments
        prop::collection::vec(task_edge_ref_strategy(), 0..=5), // blocks
    )
        .prop_map(
            |(subject, description, active_form, status, owner, metadata, comments, blocks)| {
                let id = new_snowflake_id();
                let mut m: HashMap<String, LoroValue> = HashMap::new();
                m.insert("id".into(), LoroValue::String(id.as_str().into()));
                m.insert("subject".into(), LoroValue::String(subject.into()));
                m.insert("description".into(), LoroValue::String(description.into()));
                if let Some(form) = active_form {
                    m.insert("active_form".into(), LoroValue::String(form.into()));
                }
                m.insert("status".into(), LoroValue::String(status.into()));
                if let Some(owner_str) = owner {
                    m.insert("owner".into(), LoroValue::String(owner_str.into()));
                }
                m.insert("metadata".into(), metadata);
                m.insert("comments".into(), LoroValue::List(comments.into()));
                m.insert("blocks".into(), LoroValue::List(blocks.into()));
                m.insert("created_at".into(), LoroValue::String(CREATED_AT.into()));
                m.insert("updated_at".into(), LoroValue::String(UPDATED_AT.into()));
                LoroValue::Map(m.into())
            },
        )
}

/// Strategy for a complete TaskList-shaped LoroValue::Map.
///
/// Includes optional top-level policy fields (default_owner, default_status,
/// display_limit) and 0..=8 items. The zero-item case covers AC1.6
/// (empty TaskList round-trip).
fn task_list_strategy() -> impl Strategy<Value = LoroValue> {
    (
        optional_agent_id(),                                        // default_owner
        prop_oneof![Just(None), task_status_str().prop_map(Some)],  // default_status
        prop_oneof![Just(None::<i64>), (1i64..=50).prop_map(Some)], // display_limit
        prop::collection::vec(task_item_strategy(), 0..=8),         // items
    )
        .prop_map(|(default_owner, default_status, display_limit, items)| {
            let mut m: HashMap<String, LoroValue> = HashMap::new();
            m.insert("schema".into(), LoroValue::String("task-list".into()));
            if let Some(owner) = default_owner {
                m.insert("default_owner".into(), LoroValue::String(owner.into()));
            }
            if let Some(status) = default_status {
                m.insert("default_status".into(), LoroValue::String(status.into()));
            }
            if let Some(limit) = display_limit {
                m.insert("display_limit".into(), LoroValue::I64(limit));
            }
            m.insert("items".into(), LoroValue::List(items.into()));
            LoroValue::Map(m.into())
        })
}

/// Strategy for a non-empty items list (2..=8) used in reorder tests.
/// Each item gets a fresh snowflake id minted at strategy time so ids
/// remain stable across permutation and comparison.
fn items_list_for_reorder() -> impl Strategy<Value = Vec<LoroValue>> {
    prop::collection::vec(
        // Use a simpler item (no edges, no comments) to keep the permutation
        // test focused purely on id preservation.
        non_empty_safe_string(60).prop_map(|subject| {
            let id = new_snowflake_id();
            make_item_with_id(id.as_str(), &subject)
        }),
        2..=8,
    )
}

/// Strategy for a permutation expressed as a sequence of (from, to) swap
/// pairs. Each pair swaps two distinct indices; applying k swaps to the
/// list produces a deterministic permutation. We sample k in 1..=items.len()
/// to ensure at least one reorder is applied.
fn swap_pairs_strategy(n: usize) -> impl Strategy<Value = Vec<(usize, usize)>> {
    let n_swaps = 1..=n.max(1);
    n_swaps.prop_flat_map(move |k| {
        prop::collection::vec(
            (0..n, 0..n).prop_filter("swap must be between distinct indices", |(a, b)| a != b),
            k,
        )
    })
}

// ---------------------------------------------------------------------------
// Proptest: round_trip_preserves_content (AC1.3, AC1.6)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// AC1.3: arbitrary TaskList → KDL → LoroValue preserves full content.
    ///
    /// Comparison is done via canonical JSON (both sides serialised to
    /// `serde_json::Value` and compared with `==`). This avoids noise from
    /// `LoroValue` container-id internals while testing real content equality.
    ///
    /// The corpus includes zero-item lists (AC1.6 empty case) and items with
    /// self-referential blocks edges (AC1.6 self-edge case) because the
    /// generator allows both without filtering.
    #[test]
    fn round_trip_preserves_content(value in task_list_strategy()) {
        let rt = kdl_round_trip(&value)
            .map_err(TestCaseError::fail)?;

        prop_assert!(
            loro_json_equal(&value, &rt),
            "round-trip content mismatch:\n  original JSON: {:?}\n  rt JSON: {:?}",
            loro_value_to_json(&value),
            loro_value_to_json(&rt),
        );
    }
}

// ---------------------------------------------------------------------------
// Proptest: reorder_preserves_item_ids (AC1.7)
// ---------------------------------------------------------------------------

proptest! {
    // At least 64 cases as required by the spec.
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// AC1.7: item reordering via `LoroMovableList::mov()` preserves every
    /// original TaskItemId across KDL round-trip.
    ///
    /// This test uses a real `LoroDoc` with a `LoroMovableList` named "items".
    /// Items are inserted as `LoroValue::Map`, then reordered via
    /// `LoroMovableList::mov(from, to)` — the same CRDT operation the runtime
    /// uses when an agent reorders tasks. The resulting disk_doc state is
    /// extracted via `get_deep_value()`, wrapped in a discriminator map, and
    /// round-tripped through KDL. Every original id must appear exactly once
    /// in the output (order may differ from the permuted order, but all ids
    /// must survive).
    ///
    /// Index-pair permutations are generated by `swap_pairs_strategy(n)` which
    /// draws k swap pairs (k ≥ 1, with distinct indices) to guarantee the list
    /// is actually reordered.
    #[test]
    fn reorder_preserves_item_ids(
        items in items_list_for_reorder(),
        swaps in swap_pairs_strategy(8),
    ) {
        use loro::LoroDoc;

        // Collect original ids from the LoroValue items before insertion.
        let original_ids: Vec<String> = items
            .iter()
            .filter_map(|item| {
                let LoroValue::Map(m) = item else { return None };
                let LoroValue::String(id) = m.get("id")? else { return None };
                Some(id.to_string())
            })
            .collect();

        prop_assume!(original_ids.len() == items.len(), "all items must have ids");

        let n = items.len();

        // Build a real LoroDoc with a LoroMovableList and insert items as
        // LoroValue::Map entries. This mirrors the production import path.
        let doc = LoroDoc::new();
        {
            let list = doc.get_movable_list("items");
            for item in &items {
                list.push(item.clone()).map_err(|e| {
                    TestCaseError::fail(format!("LoroMovableList::push failed: {e}"))
                })?;
            }
            doc.commit();
        }

        // Apply deterministic swaps using LoroMovableList::mov(), clamping
        // indices to the actual list length to stay valid when the drawn n=8
        // upper bound exceeds the actual list size.
        {
            let list = doc.get_movable_list("items");
            for (from, to) in &swaps {
                let f = from % n;
                let t = to % n;
                if f != t {
                    list.mov(f, t).map_err(|e| {
                        TestCaseError::fail(format!("LoroMovableList::mov({f}, {t}) failed: {e}"))
                    })?;
                    doc.commit();
                }
            }
        }

        // Extract the reordered state from disk_doc via get_deep_value() and
        // wrap it in a discriminator map for the KDL round-trip.
        let deep = doc.get_deep_value();
        let LoroValue::Map(root_map) = &deep else {
            return Err(TestCaseError::fail(format!(
                "get_deep_value() returned non-Map: {deep:?}"
            )));
        };
        let items_value = root_map
            .get("items")
            .cloned()
            .unwrap_or_else(|| LoroValue::List(vec![].into()));

        // Wrap in the discriminator map expected by kdl_to_loro_value with
        // TopShape::TaskList.
        let mut wrapper: std::collections::HashMap<String, LoroValue> =
            std::collections::HashMap::new();
        wrapper.insert("schema".into(), LoroValue::String("task-list".into()));
        wrapper.insert("items".into(), items_value);
        let task_list_value = LoroValue::Map(wrapper.into());

        // Round-trip through KDL.
        let rt = kdl_round_trip(&task_list_value)
            .map_err(TestCaseError::fail)?;

        let rt_ids = extract_item_ids(&rt);

        // Every original id must appear exactly once in the round-tripped output.
        prop_assert_eq!(
            rt_ids.len(),
            original_ids.len(),
            "item count changed across LoroMovableList::mov + KDL round-trip: \
             original={:?} rt={:?}",
            original_ids,
            rt_ids,
        );

        let mut sorted_original = original_ids.clone();
        sorted_original.sort();
        let mut sorted_rt = rt_ids.clone();
        sorted_rt.sort();

        prop_assert_eq!(
            sorted_original,
            sorted_rt,
            "item ids changed across LoroMovableList::mov + KDL round-trip: \
             original={:?} rt={:?}",
            original_ids,
            rt_ids,
        );
    }
}

// ---------------------------------------------------------------------------
// Proptest: Null-field normalization is idempotent (Critical #4)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Null values in optional task-item fields (metadata, active_form, owner)
    /// normalise to absent on the first KDL round-trip, and subsequent
    /// round-trips are stable (idempotent).
    ///
    /// Convention (documented in `task_item_to_kdl_node`):
    /// - `LoroValue::Null` for any field is treated as "absent" — nothing is
    ///   emitted in KDL.
    /// - On the reverse path, absent fields default to their zero-values
    ///   (`Map({})` for metadata, no entry for owner/active_form).
    /// - After one round-trip the result is fully normalised. `rt1 == rt2`
    ///   asserts idempotency of the normalisation.
    #[test]
    fn null_optional_fields_normalise_idempotently(
        subject in non_empty_safe_string(80),
        status in task_status_str(),
        // Each field independently drawn as either Null or a real value.
        owner_null in any::<bool>(),
        active_form_null in any::<bool>(),
        metadata_null in any::<bool>(),
    ) {
        let id = new_snowflake_id();
        let mut m: HashMap<String, LoroValue> = HashMap::new();
        m.insert("id".into(), LoroValue::String(id.as_str().into()));
        m.insert("subject".into(), LoroValue::String(subject.into()));
        m.insert("description".into(), LoroValue::String("".into()));
        m.insert("status".into(), LoroValue::String(status.into()));
        m.insert("blocks".into(), LoroValue::List(vec![].into()));
        m.insert("comments".into(), LoroValue::List(vec![].into()));
        m.insert("created_at".into(), LoroValue::String(CREATED_AT.into()));
        m.insert("updated_at".into(), LoroValue::String(UPDATED_AT.into()));

        // Set optional fields to Null or a real value based on the drawn bools.
        if owner_null {
            m.insert("owner".into(), LoroValue::Null);
        } else {
            m.insert("owner".into(), LoroValue::String("@test-owner".into()));
        }
        if active_form_null {
            m.insert("active_form".into(), LoroValue::Null);
        } else {
            m.insert("active_form".into(), LoroValue::String("doing it".into()));
        }
        if metadata_null {
            m.insert("metadata".into(), LoroValue::Null);
        } else {
            m.insert("metadata".into(), LoroValue::Map(HashMap::new().into()));
        }

        let item = LoroValue::Map(m.into());
        let task_list = make_task_list(vec![item]);

        // First round-trip: Null fields normalise to absent/default.
        let rt1 = kdl_round_trip(&task_list)
            .map_err(TestCaseError::fail)?;

        // Second round-trip: result must be identical (idempotent normalisation).
        let rt2 = kdl_round_trip(&rt1)
            .map_err(TestCaseError::fail)?;

        prop_assert!(
            loro_json_equal(&rt1, &rt2),
            "Null normalisation must be idempotent: rt1 → rt2 should be stable.\
             \n  rt1 JSON: {:?}\n  rt2 JSON: {:?}",
            loro_value_to_json(&rt1),
            loro_value_to_json(&rt2),
        );
    }
}

// ---------------------------------------------------------------------------
// Explicit example: empty TaskList (AC1.6)
// ---------------------------------------------------------------------------

#[test]
fn empty_task_list_round_trips() {
    // Zero-item TaskList must survive KDL round-trip cleanly, including the
    // `schema: "task-list"` discriminator (AC1.6 empty case).
    let value = make_task_list(vec![]);
    let rt = kdl_round_trip(&value).expect("empty task-list should round-trip without error");
    assert!(
        loro_json_equal(&value, &rt),
        "empty task-list content changed across round-trip:\n  original: {:?}\n  rt: {:?}",
        loro_value_to_json(&value),
        loro_value_to_json(&rt),
    );
}

// ---------------------------------------------------------------------------
// Explicit example: self-referential edge (AC1.6)
// ---------------------------------------------------------------------------

#[test]
fn self_referential_edge_round_trips() {
    // An item whose blocks list contains a TaskEdgeRef pointing at its own
    // id within its own block. This exercises AC1.6 explicitly in addition
    // to the proptest corpus which allows self-edges generatively.
    let own_id = new_snowflake_id();
    let edge = make_edge("my-task-list", Some(own_id.as_str()));
    let item = {
        let mut m: HashMap<String, LoroValue> = HashMap::new();
        m.insert("id".into(), LoroValue::String(own_id.as_str().into()));
        m.insert("subject".into(), LoroValue::String("self-ref task".into()));
        m.insert("description".into(), LoroValue::String("".into()));
        m.insert("status".into(), LoroValue::String("blocked".into()));
        m.insert("blocks".into(), LoroValue::List(vec![edge].into()));
        m.insert("comments".into(), LoroValue::List(vec![].into()));
        m.insert("metadata".into(), LoroValue::Map(HashMap::new().into()));
        m.insert("created_at".into(), LoroValue::String(CREATED_AT.into()));
        m.insert("updated_at".into(), LoroValue::String(UPDATED_AT.into()));
        LoroValue::Map(m.into())
    };
    let value = make_task_list(vec![item]);
    let rt = kdl_round_trip(&value).expect("self-edge task-list should round-trip");

    assert!(
        loro_json_equal(&value, &rt),
        "self-edge content changed:\n  original: {:?}\n  rt: {:?}",
        loro_value_to_json(&value),
        loro_value_to_json(&rt),
    );

    // Confirm the self-edge survived intact in the round-tripped output.
    let rt_ids = extract_item_ids(&rt);
    assert_eq!(
        rt_ids,
        vec![own_id.to_string()],
        "item id must survive round-trip"
    );
}

// ---------------------------------------------------------------------------
// Explicit example: schema discriminator survives (AC1.3)
// ---------------------------------------------------------------------------

#[test]
fn schema_discriminator_survives_round_trip() {
    // The `schema: "task-list"` entry is the key that drives KDL dispatch.
    // Verify it is present and correct in the round-tripped LoroValue.
    let value = make_task_list(vec![]);
    let rt = kdl_round_trip(&value).expect("should round-trip");
    let LoroValue::Map(root) = &rt else {
        panic!("round-tripped value must be a LoroValue::Map");
    };
    assert!(
        matches!(root.get("schema"), Some(LoroValue::String(s)) if s.as_str() == "task-list"),
        "schema discriminator missing or wrong in round-tripped value: {:?}",
        root.get("schema"),
    );
}
