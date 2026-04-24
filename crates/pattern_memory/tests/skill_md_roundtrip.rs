//! Property-based round-trip tests for the skill `.md` converter.
//!
//! Generates bounded [`SkillMetadata`], [`LoroValue`] extras, and a
//! UTF-8 body, then verifies `parse(emit(m, extras, body)).unwrap() == (m, extras, body)`.

use std::collections::HashMap;

use loro::LoroValue;
use pattern_core::types::memory_types::{SkillMetadata, SkillTrustTier};
use pattern_memory::fs::markdown_skill::{SkillFile, emit, parse};
use proptest::prelude::*;
use serde_json::Value as JsonValue;

// region: strategies

/// Safe string content for all text fields — avoids YAML control chars,
/// leading/trailing whitespace, and the frontmatter delimiter sequence.
///
/// The emitter delegates quoting to saphyr, which handles YAML-ambiguous
/// forms (`null`, `42`, etc.); `need_quotes` in saphyr 0.0.6 does not
/// cover strings with embedded newlines for round-trip purposes, so we
/// exclude those here and unit-test multiline separately.
///
/// Unicode is included (α-ω range, 0391-03C9) to exercise multi-byte
/// UTF-8 paths through the saphyr emitter and span-offset approximation
/// in `parse()`.
fn safe_text() -> impl Strategy<Value = String> {
    // Includes ASCII punctuation to exercise saphyr quoting rules plus
    // a subset of Greek Unicode to exercise multi-byte UTF-8 paths.
    // Excludes newlines, NUL, and the three-dash sequence (frontmatter delimiter).
    "[A-Za-z0-9_ .,;!?:#'\"\\[\\]{}@*&<>=|%\\-\u{03B1}-\u{03C9}]{1,30}"
        .prop_filter("trim-safe", |s| !s.starts_with(' ') && !s.ends_with(' '))
}

fn safe_short_text() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_-]{1,20}".prop_map(|s| s)
}

fn trust_tier_strategy() -> impl Strategy<Value = SkillTrustTier> {
    prop_oneof![
        Just(SkillTrustTier::FirstParty),
        Just(SkillTrustTier::ProjectLocal),
        Just(SkillTrustTier::PluginInstalled),
        Just(SkillTrustTier::AdHoc),
    ]
}

fn keywords_strategy() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(safe_short_text(), 0..=5)
}

// Bounded JsonValue strategy for hooks — avoids f64 (NaN/Inf issues),
// non-ASCII-identifier map keys, and too-deep recursion.
fn hooks_leaf() -> impl Strategy<Value = JsonValue> {
    // Whole-number f64 (C3): `json!(1.0)` must survive the emit→parse
    // round-trip with its decimal point preserved. The emitter uses
    // `float_to_yaml`, which forces `1.0` (not `1`) so saphyr parses it back
    // as a floating-point number, not an integer.
    //
    // Large f64 values (beyond i32 range) and fractional floats are excluded
    // here because the proptest property asserts full `SkillMetadata`
    // equality; fractional floats in hooks survive round-trip fine but the
    // test setup complexity would grow. Large u64 values that exceed i64::MAX
    // are tested in the dedicated `hooks_large_u64_no_precision_loss` proptest
    // below, which relaxes the equality assertion to account for the known
    // string coercion on the parse side.
    prop_oneof![
        Just(JsonValue::Null),
        any::<bool>().prop_map(JsonValue::Bool),
        any::<i64>().prop_map(|i| serde_json::json!(i)),
        safe_text().prop_map(JsonValue::String),
        // Whole-number floats: must round-trip with decimal point preserved.
        prop_oneof![
            Just(serde_json::json!(0.0_f64)),
            Just(serde_json::json!(1.0_f64)),
            Just(serde_json::json!(-1.0_f64)),
            Just(serde_json::json!(2.0_f64)),
        ],
    ]
}

fn hooks_strategy() -> impl Strategy<Value = JsonValue> {
    // Either Null (omitted in output) or a small object of event→array[action].
    prop_oneof![
        Just(JsonValue::Null),
        prop::collection::hash_map(
            safe_short_text(),
            prop::collection::vec(hooks_leaf(), 0..=3).prop_map(JsonValue::Array),
            0..=3,
        )
        .prop_map(|m| {
            let mut obj = serde_json::Map::new();
            for (k, v) in m {
                obj.insert(k, v);
            }
            JsonValue::Object(obj)
        }),
    ]
}

fn optional_description() -> impl Strategy<Value = Option<String>> {
    prop_oneof![Just(None), safe_text().prop_map(Some)]
}

fn skill_metadata_strategy() -> impl Strategy<Value = SkillMetadata> {
    (
        safe_short_text(),
        trust_tier_strategy(),
        optional_description(),
        keywords_strategy(),
        hooks_strategy(),
    )
        .prop_map(
            |(name, trust_tier, description, keywords, hooks)| SkillMetadata {
                name,
                trust_tier,
                description,
                keywords,
                hooks,
            },
        )
}

// Extras strategy — bounded LoroValue tree. Scalars + one level of
// list/map nesting is enough to cover interesting round-trip surface.
fn loro_scalar() -> impl Strategy<Value = LoroValue> {
    // Whole-number f64 values (C3): `1.0`, `0.0`, etc. must round-trip as
    // `LoroValue::Double`, not be coerced to `LoroValue::I64` by the YAML
    // parser. The emitter forces a decimal point (`1.0` not `1`) to preserve
    // the float type. NaN and Inf are excluded — they cannot be emitted to
    // canonical YAML (no standard representation) and are not valid Skill
    // frontmatter values.
    let whole_double = prop_oneof![
        Just(LoroValue::Double(0.0)),
        Just(LoroValue::Double(1.0)),
        Just(LoroValue::Double(-1.0)),
        Just(LoroValue::Double(2.0)),
        Just(LoroValue::Double(100.0)),
        Just(LoroValue::Double(-100.0)),
    ];
    prop_oneof![
        Just(LoroValue::Null),
        any::<bool>().prop_map(LoroValue::Bool),
        any::<i64>().prop_map(LoroValue::I64),
        safe_text().prop_map(|s| LoroValue::String(s.into())),
        whole_double,
    ]
}

fn loro_value_strategy() -> impl Strategy<Value = LoroValue> {
    let leaf = loro_scalar();
    leaf.prop_recursive(2, 8, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..=3).prop_map(|v| LoroValue::List(v.into())),
            prop::collection::hash_map(safe_short_text(), inner, 0..=3).prop_map(|m| {
                let map: HashMap<String, LoroValue> = m.into_iter().collect();
                LoroValue::Map(map.into())
            }),
        ]
    })
}

fn extras_strategy() -> impl Strategy<Value = LoroValue> {
    // Top-level is always a Map, with keys that don't collide with the
    // typed frontmatter keys.
    prop::collection::hash_map(
        safe_short_text().prop_filter("no reserved keys", |s| {
            !matches!(
                s.as_str(),
                "name" | "trust_tier" | "description" | "keywords" | "hooks"
            )
        }),
        loro_value_strategy(),
        0..=4,
    )
    .prop_map(|m| {
        let map: HashMap<String, LoroValue> = m.into_iter().collect();
        LoroValue::Map(map.into())
    })
}

// Body strategy: ASCII text that is pre-normalized (ends with `\n` or
// empty) so direct equality holds after round-trip.
fn body_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[A-Za-z0-9 \\n.,;!?_-]{0,200}".prop_map(|s| {
            if s.ends_with('\n') {
                s
            } else {
                format!("{s}\n")
            }
        }),
    ]
}

// endregion: strategies

// region: round-trip property

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        ..ProptestConfig::default()
    })]

    /// Core round-trip property: emit then parse yields the original tuple.
    #[test]
    fn parse_emit_parse_roundtrip(
        meta in skill_metadata_strategy(),
        extras in extras_strategy(),
        body in body_strategy(),
    ) {
        let emitted = emit(&meta, &extras, &body).expect("emit must succeed");
        let parsed: SkillFile = parse(emitted.as_bytes())
            .unwrap_or_else(|e| panic!("parse failed for emit output: {e:?}\noutput was:\n{emitted}"));

        prop_assert_eq!(&parsed.metadata, &meta, "metadata mismatch");
        prop_assert_eq!(&parsed.extras, &extras, "extras mismatch");
        prop_assert_eq!(&parsed.body, &body, "body mismatch");

        // And emit is idempotent on a round-tripped value.
        let re_emitted = emit(&parsed.metadata, &parsed.extras, &parsed.body)
            .expect("re-emit must succeed");
        prop_assert_eq!(emitted, re_emitted, "emit should be idempotent post-parse");
    }

    /// C2: hooks values that contain u64 > i64::MAX survive emit without
    /// precision loss — the decimal string representation must appear verbatim
    /// in the emitted YAML.
    ///
    /// Round-trip type identity is NOT asserted here because the emitter
    /// uses a double-quoted string for u64 > i64::MAX (to avoid f64 precision
    /// loss), which the parse path reads back as `JsonValue::String`. This is a
    /// known, documented limitation: precision is preserved but the JSON type
    /// changes from Number to String on the inbound parse side.
    ///
    /// What this test verifies:
    /// - `emit` does not return an error.
    /// - `parse` of the emitted bytes does not return an error.
    /// - The decimal string for the large u64 value appears in the output,
    ///   not a lossy f64 approximation.
    #[test]
    fn hooks_large_u64_no_precision_loss(
        // Generate u64 values strictly above i64::MAX to exercise the
        // "emit as double-quoted string" branch added in C2.
        big in (i64::MAX as u64 + 1)..=u64::MAX,
        name in "[A-Za-z][A-Za-z0-9_-]{0,10}",
    ) {
        let meta = SkillMetadata {
            name,
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: Vec::new(),
            hooks: serde_json::json!({"counter": big}),
        };
        let extras = LoroValue::Map(HashMap::<String, LoroValue>::new().into());

        let emitted = emit(&meta, &extras, "body\n").expect("emit must succeed for large u64 hooks");
        // The decimal string must appear verbatim — not as a rounded f64.
        let big_str = big.to_string();
        prop_assert!(
            emitted.contains(&big_str),
            "emitted YAML must contain the exact decimal for {big}: got:\n{emitted}"
        );

        // Parse must not fail.
        let _parsed = parse(emitted.as_bytes())
            .unwrap_or_else(|e| panic!("parse failed for large u64 emit output: {e:?}\noutput was:\n{emitted}"));
    }
}

// endregion: round-trip property
