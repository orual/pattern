//! Emit direction for skill `.md` files: `SkillMetadata` + extras + body
//! → `---\n<yaml>\n---\n\n<body>`.
//!
//! Uses saphyr 0.0.6's [`YamlEmitter`] for canonical YAML output. Field
//! ordering is fixed (name, trust_tier, description, keywords, hooks, then
//! extras in sorted key order) so two emits of the same input produce
//! byte-identical output — required for content-hash stability.

use std::borrow::Cow;

use loro::LoroValue;
use miette::Diagnostic;
use saphyr::{Mapping, Scalar, Yaml, YamlEmitter};
use serde_json::Value as JsonValue;
use thiserror::Error;

use pattern_core::types::memory_types::{SkillMetadata, SkillTrustTier};

// region: SkillEmitError

/// Errors raised by [`emit`].
#[non_exhaustive]
#[derive(Debug, Error, Diagnostic)]
pub enum SkillEmitError {
    /// `extras` was not a [`LoroValue::Map`].
    #[error("extras must be a LoroValue::Map; got {kind}")]
    ExtrasNotMap { kind: &'static str },
    /// Underlying saphyr emitter write failure. In practice this only
    /// surfaces for I/O errors on the writer, which cannot happen when
    /// emitting into an owned `String`.
    #[error("yaml emitter write failure")]
    Fmt,
    /// Extras map contained a [`LoroValue`] variant that has no YAML
    /// representation (binary blobs or live container handles).
    #[error("extras contains unsupported LoroValue variant: {kind}")]
    UnsupportedLoroValue { kind: &'static str },
    /// A [`SkillTrustTier`] variant was added upstream but this emitter
    /// has no string encoding for it yet. Fail loud rather than silently
    /// coerce.
    #[error("unsupported SkillTrustTier variant; emitter is out of date")]
    UnsupportedTrustTier,
}

// endregion: SkillEmitError

// region: entry point

/// Emit a skill `.md` file from typed metadata + preserved extras + body.
///
/// Field ordering in the frontmatter is fixed and deterministic:
/// `name`, `trust_tier`, `description` (if `Some`), `keywords` (if
/// non-empty), `hooks` (if non-null), then extras keys in sorted order.
/// This makes the output content-hash stable for a given logical input.
///
/// Body normalization: if `body` is empty, emit empty; otherwise ensure it
/// ends with a single `\n`. A non-empty body without a trailing newline
/// cannot round-trip through [`super::parse::parse`] because the parser's
/// split always produces a body that starts immediately after `\n---\n`.
pub fn emit(
    metadata: &SkillMetadata,
    extras: &LoroValue,
    body: &str,
) -> Result<String, SkillEmitError> {
    let extras_map = match extras {
        LoroValue::Map(m) => m,
        other => {
            return Err(SkillEmitError::ExtrasNotMap {
                kind: loro_kind(other),
            });
        }
    };

    let mut mapping: Mapping<'static> = Mapping::new();

    mapping.insert(
        yaml_owned_string("name".to_string()),
        yaml_owned_string(metadata.name.clone()),
    );
    mapping.insert(
        yaml_owned_string("trust_tier".to_string()),
        yaml_owned_string(trust_tier_str(metadata.trust_tier)?.to_string()),
    );
    if let Some(d) = &metadata.description {
        mapping.insert(
            yaml_owned_string("description".to_string()),
            yaml_owned_string(d.clone()),
        );
    }
    if !metadata.keywords.is_empty() {
        let items: Vec<Yaml<'static>> = metadata
            .keywords
            .iter()
            .map(|k| yaml_owned_string(k.clone()))
            .collect();
        mapping.insert(
            yaml_owned_string("keywords".to_string()),
            Yaml::Sequence(items),
        );
    }
    if !metadata.hooks.is_null() {
        mapping.insert(
            yaml_owned_string("hooks".to_string()),
            json_to_yaml(&metadata.hooks)?,
        );
    }

    let mut extras_keys: Vec<String> = extras_map.keys().map(|s| s.to_string()).collect();
    extras_keys.sort();
    for k in extras_keys {
        if let Some(v) = extras_map.get(&k) {
            let yaml_v = loro_to_yaml(v)?;
            mapping.insert(yaml_owned_string(k), yaml_v);
        }
    }

    let root = Yaml::Mapping(mapping);

    let mut yaml_out = String::new();
    YamlEmitter::new(&mut yaml_out)
        .dump(&root)
        .map_err(|_| SkillEmitError::Fmt)?;

    // saphyr's `dump` prepends `---\n` and emits no trailing newline after
    // the final node. We strip that prefix and reintroduce our own
    // delimiter pair plus the body.
    let yaml_inner = yaml_out.strip_prefix("---\n").unwrap_or(&yaml_out);

    let body_out = normalize_body(body);

    // The closing delimiter is `\n---\n`; one `\n` is consumed by the
    // parser. The body follows verbatim. A non-empty body that needs
    // visual separation from the delimiter should include its own leading
    // blank line in `body_out`.
    Ok(format!("---\n{yaml_inner}\n---\n{body_out}"))
}

// endregion: entry point

// region: body normalization

fn normalize_body(body: &str) -> String {
    if body.is_empty() || body.ends_with('\n') {
        body.to_string()
    } else {
        let mut s = String::with_capacity(body.len() + 1);
        s.push_str(body);
        s.push('\n');
        s
    }
}

// endregion: body normalization

// region: trust tier

fn trust_tier_str(tier: SkillTrustTier) -> Result<&'static str, SkillEmitError> {
    match tier {
        SkillTrustTier::FirstParty => Ok("first-party"),
        SkillTrustTier::ProjectLocal => Ok("project-local"),
        SkillTrustTier::PluginInstalled => Ok("plugin-installed"),
        SkillTrustTier::AdHoc => Ok("ad-hoc"),
        _ => Err(SkillEmitError::UnsupportedTrustTier),
    }
}

// endregion: trust tier

// region: yaml builders

fn yaml_owned_string(s: String) -> Yaml<'static> {
    Yaml::Value(Scalar::String(Cow::Owned(s)))
}

// endregion: yaml builders

// region: json → yaml

fn json_to_yaml(v: &JsonValue) -> Result<Yaml<'static>, SkillEmitError> {
    Ok(match v {
        JsonValue::Null => Yaml::Value(Scalar::Null),
        JsonValue::Bool(b) => Yaml::Value(Scalar::Boolean(*b)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Yaml::Value(Scalar::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Yaml::Value(Scalar::FloatingPoint(f.into()))
            } else {
                // u64 values that exceed i64 range fall through to string
                // representation so they're preserved losslessly.
                yaml_owned_string(n.to_string())
            }
        }
        JsonValue::String(s) => yaml_owned_string(s.clone()),
        JsonValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for i in items {
                out.push(json_to_yaml(i)?);
            }
            Yaml::Sequence(out)
        }
        JsonValue::Object(obj) => {
            let mut mapping: Mapping<'static> = Mapping::new();
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                mapping.insert(yaml_owned_string(k.clone()), json_to_yaml(&obj[k])?);
            }
            Yaml::Mapping(mapping)
        }
    })
}

// endregion: json → yaml

// region: loro → yaml

fn loro_to_yaml(v: &LoroValue) -> Result<Yaml<'static>, SkillEmitError> {
    Ok(match v {
        LoroValue::Null => Yaml::Value(Scalar::Null),
        LoroValue::Bool(b) => Yaml::Value(Scalar::Boolean(*b)),
        LoroValue::I64(i) => Yaml::Value(Scalar::Integer(*i)),
        LoroValue::Double(f) => Yaml::Value(Scalar::FloatingPoint((*f).into())),
        LoroValue::String(s) => yaml_owned_string(s.to_string()),
        LoroValue::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for i in items.iter() {
                out.push(loro_to_yaml(i)?);
            }
            Yaml::Sequence(out)
        }
        LoroValue::Map(m) => {
            let mut mapping: Mapping<'static> = Mapping::new();
            let mut keys: Vec<String> = m.keys().map(|k| k.to_string()).collect();
            keys.sort();
            for k in keys {
                if let Some(inner) = m.get(&k) {
                    mapping.insert(yaml_owned_string(k), loro_to_yaml(inner)?);
                }
            }
            Yaml::Mapping(mapping)
        }
        LoroValue::Binary(_) => {
            return Err(SkillEmitError::UnsupportedLoroValue { kind: "binary" });
        }
        LoroValue::Container(_) => {
            return Err(SkillEmitError::UnsupportedLoroValue { kind: "container" });
        }
    })
}

fn loro_kind(v: &LoroValue) -> &'static str {
    match v {
        LoroValue::Null => "null",
        LoroValue::Bool(_) => "bool",
        LoroValue::I64(_) => "i64",
        LoroValue::Double(_) => "double",
        LoroValue::String(_) => "string",
        LoroValue::List(_) => "list",
        LoroValue::Map(_) => "map",
        LoroValue::Binary(_) => "binary",
        LoroValue::Container(_) => "container",
    }
}

// endregion: loro → yaml

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::markdown_skill::parse::parse;
    use serde_json::json;
    use std::collections::HashMap;

    fn empty_extras() -> LoroValue {
        LoroValue::Map(HashMap::<String, LoroValue>::new().into())
    }

    fn meta_minimal() -> SkillMetadata {
        SkillMetadata {
            name: "my-skill".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: Vec::new(),
            hooks: JsonValue::Null,
        }
    }

    // region: stability

    #[test]
    fn emit_is_byte_stable_across_many_calls() {
        let meta = SkillMetadata {
            name: "x".to_string(),
            trust_tier: SkillTrustTier::ProjectLocal,
            description: Some("d".to_string()),
            keywords: vec!["a".to_string(), "b".to_string()],
            hooks: json!({
                "z_event": [{ "inner_b": 1, "inner_a": 2 }],
                "a_event": [{ "log": "msg" }],
            }),
        };
        let mut extras = HashMap::<String, LoroValue>::new();
        extras.insert("z_extra".to_string(), LoroValue::I64(1));
        extras.insert(
            "a_extra".to_string(),
            LoroValue::String("v".to_string().into()),
        );
        let extras_val = LoroValue::Map(extras.into());

        let first = emit(&meta, &extras_val, "body\n").unwrap();
        for _ in 0..1000 {
            let next = emit(&meta, &extras_val, "body\n").unwrap();
            assert_eq!(first, next, "emit output must be byte-stable");
        }
    }

    // endregion: stability

    // region: shape

    #[test]
    fn emit_minimal_produces_only_required_keys() {
        let out = emit(&meta_minimal(), &empty_extras(), "hello\n").unwrap();
        // No description / keywords / hooks lines.
        assert!(out.contains("name: my-skill"));
        assert!(out.contains("trust_tier: ad-hoc"));
        assert!(!out.contains("description"));
        assert!(!out.contains("keywords"));
        assert!(!out.contains("hooks"));
        // Delimiter layout — parser strips one `\n` after closing `---`.
        assert!(out.starts_with("---\n"));
        assert!(out.contains("\n---\nhello\n"));
    }

    #[test]
    fn emit_body_normalization_appends_newline() {
        let out = emit(&meta_minimal(), &empty_extras(), "no-newline").unwrap();
        assert!(out.ends_with("no-newline\n"));
    }

    #[test]
    fn emit_empty_body_stays_empty() {
        let out = emit(&meta_minimal(), &empty_extras(), "").unwrap();
        assert!(out.ends_with("---\n"));
    }

    #[test]
    fn emit_preserves_body_with_newline() {
        let out = emit(&meta_minimal(), &empty_extras(), "line\n").unwrap();
        assert!(out.ends_with("line\n"));
        // No double newline at end.
        assert!(!out.ends_with("line\n\n"));
    }

    #[test]
    fn emit_rejects_non_map_extras() {
        let err = emit(&meta_minimal(), &LoroValue::I64(42), "body\n").unwrap_err();
        assert!(matches!(err, SkillEmitError::ExtrasNotMap { kind: "i64" }));
    }

    // endregion: shape

    // region: round-trip

    #[test]
    fn round_trip_all_typed_fields() {
        let meta = SkillMetadata {
            name: "fix-auth".to_string(),
            trust_tier: SkillTrustTier::FirstParty,
            description: Some("Fix the authentication bug.".to_string()),
            keywords: vec!["auth".to_string(), "bug".to_string(), "urgent".to_string()],
            hooks: JsonValue::Null,
        };
        let out = emit(&meta, &empty_extras(), "Body.\n").unwrap();
        let parsed = parse(out.as_bytes()).unwrap();
        assert_eq!(parsed.metadata, meta);
        assert_eq!(parsed.body, "Body.\n");
        let LoroValue::Map(extras) = &parsed.extras else {
            panic!("extras should be a map");
        };
        assert!(extras.is_empty());
    }

    #[test]
    fn round_trip_with_nested_hooks() {
        let meta = SkillMetadata {
            name: "k".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: Vec::new(),
            hooks: json!({
                "on_turn_start": [
                    {"inject_context": "Remember the plan."}
                ],
                "on_memory_write": [
                    {"log": "scratchpad-touched"}
                ]
            }),
        };
        let out = emit(&meta, &empty_extras(), "body\n").unwrap();
        let parsed = parse(out.as_bytes()).unwrap();
        assert_eq!(parsed.metadata.hooks, meta.hooks);
    }

    #[test]
    fn round_trip_with_extras_preserves_values() {
        let mut extras = HashMap::<String, LoroValue>::new();
        extras.insert(
            "author".to_string(),
            LoroValue::String("@me".to_string().into()),
        );
        extras.insert("version".to_string(), LoroValue::I64(2));
        let mut nested = HashMap::<String, LoroValue>::new();
        nested.insert(
            "leaf".to_string(),
            LoroValue::String("v".to_string().into()),
        );
        nested.insert("count".to_string(), LoroValue::I64(7));
        extras.insert("nested".to_string(), LoroValue::Map(nested.into()));
        let extras_val = LoroValue::Map(extras.into());

        let out = emit(&meta_minimal(), &extras_val, "body\n").unwrap();
        let parsed = parse(out.as_bytes()).unwrap();
        let LoroValue::Map(got) = &parsed.extras else {
            panic!("extras should be a map");
        };
        assert_eq!(got.len(), 3);
        assert!(matches!(got.get("version"), Some(LoroValue::I64(2))));
        assert!(matches!(
            got.get("author"),
            Some(LoroValue::String(s)) if s.as_str() == "@me"
        ));
        let LoroValue::Map(nested_got) = got.get("nested").unwrap() else {
            panic!("nested should be a map");
        };
        assert!(matches!(nested_got.get("count"), Some(LoroValue::I64(7))));
    }

    #[test]
    fn parse_emit_parse_fixture_is_stable() {
        // parse → emit → parse should produce identical second parse, even
        // when input has unusual formatting that emit canonicalises.
        let input = "---\n\
                     name: my-skill\n\
                     trust_tier: first-party\n\
                     description: desc\n\
                     keywords:\n  - a\n  - b\n\
                     hooks:\n  on_load:\n    - log: x\n\
                     custom: value\n\
                     ---\n\
                     # Title\n\nBody\n";
        let first = parse(input.as_bytes()).unwrap();
        let emitted = emit(&first.metadata, &first.extras, &first.body).unwrap();
        let second = parse(emitted.as_bytes()).unwrap();
        assert_eq!(first.metadata, second.metadata);
        assert_eq!(first.extras, second.extras);
        assert_eq!(first.body, second.body);
        // And emit is idempotent after the first normalization pass.
        let emitted_again = emit(&second.metadata, &second.extras, &second.body).unwrap();
        assert_eq!(emitted, emitted_again);
    }

    // endregion: round-trip

    // region: string quoting edge cases

    #[test]
    fn ambiguous_strings_survive_round_trip() {
        // Values that parse as non-string YAML scalars (null, true, 42) must
        // be quoted by the emitter so parse() sees strings, not ints/bools.
        let mut extras = HashMap::<String, LoroValue>::new();
        extras.insert(
            "looks_null".to_string(),
            LoroValue::String("null".to_string().into()),
        );
        extras.insert(
            "looks_bool".to_string(),
            LoroValue::String("true".to_string().into()),
        );
        extras.insert(
            "looks_int".to_string(),
            LoroValue::String("42".to_string().into()),
        );
        let extras_val = LoroValue::Map(extras.into());
        let out = emit(&meta_minimal(), &extras_val, "b\n").unwrap();
        let parsed = parse(out.as_bytes()).unwrap();
        let LoroValue::Map(got) = &parsed.extras else {
            panic!("map")
        };
        assert!(matches!(
            got.get("looks_null"),
            Some(LoroValue::String(s)) if s.as_str() == "null"
        ));
        assert!(matches!(
            got.get("looks_bool"),
            Some(LoroValue::String(s)) if s.as_str() == "true"
        ));
        assert!(matches!(
            got.get("looks_int"),
            Some(LoroValue::String(s)) if s.as_str() == "42"
        ));
    }

    // endregion: string quoting edge cases
}
