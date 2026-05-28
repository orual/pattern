// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Property-based round-trip tests for the KDL ↔ LoroValue converter.
//!
//! Generates arbitrary `LoroValue` trees (avoiding unsupported variants like
//! `Binary` and `Container`, and avoiding the reserved `-` key in maps) and
//! verifies that forward-convert → parse → reverse-convert produces an
//! equivalent value.

use loro::LoroValue;
use pattern_memory::fs::kdl::{TopShape, kdl_to_loro_value, loro_value_to_kdl};
use proptest::prelude::*;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// LoroValue strategy generators
// ---------------------------------------------------------------------------

/// Strategy for scalar LoroValue variants.
fn scalar_loro_value() -> impl Strategy<Value = LoroValue> {
    prop_oneof![
        Just(LoroValue::Null),
        any::<bool>().prop_map(LoroValue::Bool),
        any::<i64>().prop_map(LoroValue::I64),
        // Use finite f64 to avoid NaN equality issues in proptest comparisons.
        // NaN round-trip is tested separately in unit tests.
        prop::num::f64::NORMAL.prop_map(LoroValue::Double),
        "[a-zA-Z0-9_ ]{0,50}".prop_map(|s| LoroValue::String(s.into())),
    ]
}

/// Strategy for valid map keys (non-empty, no `-` which is reserved, and valid
/// as KDL identifiers).
fn map_key() -> impl Strategy<Value = String> {
    "[a-zA-Z][a-zA-Z0-9_]{0,15}"
        .prop_filter("key must not be the reserved list sentinel", |k| k != "-")
}

/// Recursive strategy for LoroValue trees. Max depth is limited to avoid
/// combinatorial explosion.
fn loro_value_tree(depth: u32) -> impl Strategy<Value = LoroValue> {
    if depth == 0 {
        scalar_loro_value().boxed()
    } else {
        prop_oneof![
            // Scalar leaf.
            scalar_loro_value(),
            // List of subtrees (1..=4 items).
            prop::collection::vec(loro_value_tree(depth - 1), 0..=4)
                .prop_map(|items| LoroValue::List(items.into())),
            // Map of subtrees (1..=4 entries).
            prop::collection::vec((map_key(), loro_value_tree(depth - 1)), 0..=4).prop_map(
                |pairs| {
                    let map: HashMap<String, LoroValue> = pairs.into_iter().collect();
                    LoroValue::Map(map.into())
                }
            ),
        ]
        .boxed()
    }
}

/// Strategy that produces a map-shaped LoroValue (top level).
fn map_loro_value() -> impl Strategy<Value = LoroValue> {
    prop::collection::vec((map_key(), loro_value_tree(2)), 0..=5).prop_map(|pairs| {
        let map: HashMap<String, LoroValue> = pairs.into_iter().collect();
        LoroValue::Map(map.into())
    })
}

/// Strategy that produces a list-shaped LoroValue (top level).
fn list_loro_value() -> impl Strategy<Value = LoroValue> {
    prop::collection::vec(loro_value_tree(2), 0..=5).prop_map(|items| LoroValue::List(items.into()))
}

// ---------------------------------------------------------------------------
// Deep equality with NaN handling
// ---------------------------------------------------------------------------

fn loro_values_equal(a: &LoroValue, b: &LoroValue) -> bool {
    match (a, b) {
        (LoroValue::Null, LoroValue::Null) => true,
        (LoroValue::Bool(a), LoroValue::Bool(b)) => a == b,
        (LoroValue::I64(a), LoroValue::I64(b)) => a == b,
        (LoroValue::Double(a), LoroValue::Double(b)) => {
            if a.is_nan() && b.is_nan() {
                true
            } else {
                a == b
            }
        }
        (LoroValue::String(a), LoroValue::String(b)) => a.as_str() == b.as_str(),
        (LoroValue::List(a), LoroValue::List(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(ai, bi)| loro_values_equal(ai, bi))
        }
        (LoroValue::Map(a), LoroValue::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|bv| loro_values_equal(v, bv)))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn map_round_trip(value in map_loro_value()) {
        let doc = loro_value_to_kdl(&value, TopShape::Map)
            .expect("forward conversion should succeed");
        let text = doc.to_string();
        let reparsed = kdl::KdlDocument::parse(&text)
            .expect("KDL output should be valid KDL");
        let rt = kdl_to_loro_value(&reparsed, TopShape::Map)
            .expect("reverse conversion should succeed");
        prop_assert!(
            loro_values_equal(&value, &rt),
            "round-trip mismatch:\n  original: {:?}\n  kdl text: {}\n  round-tripped: {:?}",
            value, text, rt
        );
    }

    #[test]
    fn list_round_trip(value in list_loro_value()) {
        let doc = loro_value_to_kdl(&value, TopShape::List)
            .expect("forward conversion should succeed");
        let text = doc.to_string();
        let reparsed = kdl::KdlDocument::parse(&text)
            .expect("KDL output should be valid KDL");
        let rt = kdl_to_loro_value(&reparsed, TopShape::List)
            .expect("reverse conversion should succeed");
        prop_assert!(
            loro_values_equal(&value, &rt),
            "round-trip mismatch:\n  original: {:?}\n  kdl text: {}\n  round-tripped: {:?}",
            value, text, rt
        );
    }
}
