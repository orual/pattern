//! Parser for skill `.md` files: `---\n<yaml>\n---\n\n<body>`.
//!
//! Uses saphyr 0.0.6 to load the frontmatter YAML, then a hand-written
//! visitor that:
//! - Extracts typed fields from [`SkillMetadata`] (name, trust_tier,
//!   description, keywords, hooks).
//! - Converts the opaque `hooks` value to [`serde_json::Value`].
//! - Preserves any unknown top-level keys into an `extras` [`LoroValue::Map`]
//!   so writes back round-trip cleanly without data loss.

use std::collections::HashMap;

use loro::LoroValue;
use miette::SourceSpan;
use saphyr::{LoadableYamlNode, Scalar, Yaml};
use serde_json::Value as JsonValue;

use pattern_core::types::memory_types::{SkillMetadata, SkillTrustTier};

use super::errors::SkillParseError;

// region: SkillFile

/// Result of parsing a skill `.md` file.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillFile {
    /// Typed frontmatter fields.
    pub metadata: SkillMetadata,
    /// Unknown frontmatter keys preserved for round-trip. Always a
    /// `LoroValue::Map`; may be empty.
    pub extras: LoroValue,
    /// The markdown body, post-frontmatter.
    pub body: String,
}

// endregion: SkillFile

// region: entry point

/// Parse a skill `.md` file's bytes into typed [`SkillFile`].
///
/// The input must start with a `---\n` (or `---\r\n`) frontmatter delimiter,
/// contain a YAML mapping, and be closed by another `---\n` line, followed by
/// the markdown body.
pub fn parse(bytes: &[u8]) -> Result<SkillFile, SkillParseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| SkillParseError::NonUtf8Body)?;
    let (frontmatter_src, body_src) = split_frontmatter(text)?;

    // Clone once so every error from this parse shares the same source text.
    let source_text = frontmatter_src.to_string();

    let docs = Yaml::load_from_str(frontmatter_src).map_err(|e| {
        let marker = e.marker();
        // Marker index is in chars (YAML marker convention). Use the
        // character index as the byte offset approximation — it coincides
        // for ASCII-only YAML, which is overwhelmingly common for skill
        // frontmatter. For non-ASCII, the reported span may be slightly off
        // but still useful for humans.
        let span = SourceSpan::from((marker.index(), 1));
        SkillParseError::Yaml {
            source_text: source_text.clone(),
            span,
            source: e,
        }
    })?;

    if docs.is_empty() {
        return Err(SkillParseError::MissingRequiredKey {
            key: "name",
            source_text,
            span: None,
        });
    }

    let root = &docs[0];
    let (metadata, extras) = visit_root(root, &source_text)?;

    // Normalize CRLF → LF in the body so files edited on Windows (or
    // received via HTTP with CRLF line endings) round-trip cleanly. The
    // emit path always produces LF; a CRLF body would otherwise produce
    // a file that parses back to a different body string than it emitted.
    let body = if body_src.contains('\r') {
        body_src.replace("\r\n", "\n")
    } else {
        body_src.to_string()
    };

    Ok(SkillFile {
        metadata,
        extras,
        body,
    })
}

// endregion: entry point

// region: frontmatter splitter

/// Split a source text into `(frontmatter, body)`. Both delimiters must be on
/// lines by themselves (`---` alone, followed by `\n` or `\r\n`).
fn split_frontmatter(text: &str) -> Result<(&str, &str), SkillParseError> {
    // Opening delimiter.
    let after_open = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or(SkillParseError::MissingDelimiters)?;

    // Scan for a `\n---\n`, `\n---\r\n`, or trailing `\n---` (EOF case).
    let mut search_from = 0;
    while let Some(rel) = after_open[search_from..].find("\n---") {
        let abs = search_from + rel;
        let after_dashes = abs + 4; // "\n---"
        let tail = &after_open[after_dashes..];
        // Must be start of a complete line.
        if tail.is_empty() {
            // `---` is the last thing in the file; body is empty.
            return Ok((&after_open[..abs], ""));
        }
        if let Some(body) = tail.strip_prefix("\n") {
            return Ok((&after_open[..abs], body));
        }
        if let Some(body) = tail.strip_prefix("\r\n") {
            return Ok((&after_open[..abs], body));
        }
        // Not a valid closing delimiter (e.g. `---abc`); advance past and retry.
        search_from = after_dashes;
    }
    Err(SkillParseError::MissingDelimiters)
}

// endregion: frontmatter splitter

// region: root visitor

fn visit_root(
    yaml: &Yaml,
    source_text: &str,
) -> Result<(SkillMetadata, LoroValue), SkillParseError> {
    let mapping = match yaml {
        Yaml::Mapping(m) => m,
        other => {
            return Err(SkillParseError::TypeMismatch {
                key: "<root>".to_string(),
                expected: "mapping",
                actual: yaml_kind(other),
                source_text: source_text.to_string(),
                span: None,
            });
        }
    };

    let mut name: Option<String> = None;
    let mut trust_tier: Option<SkillTrustTier> = None;
    let mut description: Option<String> = None;
    let mut keywords: Vec<String> = Vec::new();
    let mut hooks = JsonValue::Null;
    let mut extras: HashMap<String, LoroValue> = HashMap::new();

    for (k, v) in mapping.iter() {
        let key_str = match k {
            Yaml::Value(Scalar::String(s)) => s.as_ref().to_string(),
            other => {
                return Err(SkillParseError::TypeMismatch {
                    key: "<mapping-key>".to_string(),
                    expected: "string",
                    actual: yaml_kind(other),
                    source_text: source_text.to_string(),
                    span: None,
                });
            }
        };

        match key_str.as_str() {
            "name" => name = Some(extract_string(v, "name", source_text)?),
            "trust_tier" => {
                let s = extract_string(v, "trust_tier", source_text)?;
                trust_tier = Some(parse_trust_tier(&s, source_text)?);
            }
            "description" => {
                description = match v {
                    Yaml::Value(Scalar::Null) => None,
                    _ => Some(extract_string(v, "description", source_text)?),
                };
            }
            "keywords" => keywords = extract_string_sequence(v, "keywords", source_text)?,
            "hooks" => hooks = yaml_to_json(v),
            _ => {
                extras.insert(key_str, yaml_to_loro(v));
            }
        }
    }

    let name = name.ok_or(SkillParseError::MissingRequiredKey {
        key: "name",
        source_text: source_text.to_string(),
        span: None,
    })?;
    if name.is_empty() {
        return Err(SkillParseError::TypeMismatch {
            key: "name".to_string(),
            expected: "non-empty string",
            actual: "empty string",
            source_text: source_text.to_string(),
            span: None,
        });
    }
    let trust_tier = trust_tier.ok_or(SkillParseError::MissingRequiredKey {
        key: "trust_tier",
        source_text: source_text.to_string(),
        span: None,
    })?;

    let metadata = SkillMetadata {
        name,
        trust_tier,
        description,
        keywords,
        hooks,
        source_plugin_id: None,
    };
    Ok((metadata, LoroValue::Map(extras.into())))
}

// endregion: root visitor

// region: scalar extractors

fn extract_string(yaml: &Yaml, key: &str, source_text: &str) -> Result<String, SkillParseError> {
    match yaml {
        Yaml::Value(Scalar::String(s)) => Ok(s.as_ref().to_string()),
        Yaml::Representation(s, _, _) => Ok(s.as_ref().to_string()),
        other => Err(SkillParseError::TypeMismatch {
            key: key.to_string(),
            expected: "string",
            actual: yaml_kind(other),
            source_text: source_text.to_string(),
            span: None,
        }),
    }
}

fn extract_string_sequence(
    yaml: &Yaml,
    key: &str,
    source_text: &str,
) -> Result<Vec<String>, SkillParseError> {
    match yaml {
        Yaml::Sequence(items) => items
            .iter()
            .map(|v| extract_string(v, key, source_text))
            .collect(),
        other => Err(SkillParseError::TypeMismatch {
            key: key.to_string(),
            expected: "sequence",
            actual: yaml_kind(other),
            source_text: source_text.to_string(),
            span: None,
        }),
    }
}

fn parse_trust_tier(s: &str, source_text: &str) -> Result<SkillTrustTier, SkillParseError> {
    match s {
        "first-party" => Ok(SkillTrustTier::FirstParty),
        "project-local" => Ok(SkillTrustTier::ProjectLocal),
        "plugin-installed" => Ok(SkillTrustTier::PluginInstalled),
        "ad-hoc" => Ok(SkillTrustTier::AdHoc),
        other => Err(SkillParseError::InvalidTrustTier {
            value: other.to_string(),
            source_text: source_text.to_string(),
            span: None,
        }),
    }
}

// endregion: scalar extractors

// region: yaml kind classifier

fn yaml_kind(yaml: &Yaml) -> &'static str {
    match yaml {
        Yaml::Value(Scalar::Null) => "null",
        Yaml::Value(Scalar::Boolean(_)) => "boolean",
        Yaml::Value(Scalar::Integer(_)) => "integer",
        Yaml::Value(Scalar::FloatingPoint(_)) => "float",
        Yaml::Value(Scalar::String(_)) => "string",
        Yaml::Sequence(_) => "sequence",
        Yaml::Mapping(_) => "mapping",
        Yaml::Tagged(_, _) => "tagged",
        Yaml::Alias(_) => "alias",
        Yaml::BadValue => "bad-value",
        Yaml::Representation(_, _, _) => "raw",
    }
}

// endregion: yaml kind classifier

// region: generic converters

/// Convert a saphyr [`Yaml`] node to [`serde_json::Value`] for opaque
/// preservation (used for the `hooks` field). Non-string map keys are
/// skipped — JSON doesn't support non-string keys. Aliases are dropped
/// (saphyr 0.0.6 doesn't fully resolve them). Tagged values unwrap to
/// their inner value; the tag itself is not preserved on the JSON side.
pub(super) fn yaml_to_json(yaml: &Yaml) -> JsonValue {
    match yaml {
        Yaml::Value(Scalar::Null) => JsonValue::Null,
        Yaml::Value(Scalar::Boolean(b)) => JsonValue::Bool(*b),
        Yaml::Value(Scalar::Integer(i)) => serde_json::json!(*i),
        Yaml::Value(Scalar::FloatingPoint(f)) => serde_json::json!(f.into_inner()),
        Yaml::Value(Scalar::String(s)) => JsonValue::String(s.as_ref().to_string()),
        Yaml::Sequence(items) => JsonValue::Array(items.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m.iter() {
                if let Some(ks) = yaml_as_str(k) {
                    obj.insert(ks, yaml_to_json(v));
                }
            }
            JsonValue::Object(obj)
        }
        Yaml::Tagged(_, inner) => yaml_to_json(inner),
        Yaml::Alias(_) => JsonValue::Null,
        Yaml::BadValue => JsonValue::Null,
        Yaml::Representation(s, _, _) => JsonValue::String(s.as_ref().to_string()),
    }
}

/// Convert a saphyr [`Yaml`] node to [`LoroValue`] for opaque preservation
/// in the `extras` LoroMap. Same contract as [`yaml_to_json`] but produces
/// loro values; non-string map keys are skipped.
pub(super) fn yaml_to_loro(yaml: &Yaml) -> LoroValue {
    match yaml {
        Yaml::Value(Scalar::Null) => LoroValue::Null,
        Yaml::Value(Scalar::Boolean(b)) => LoroValue::Bool(*b),
        Yaml::Value(Scalar::Integer(i)) => LoroValue::I64(*i),
        Yaml::Value(Scalar::FloatingPoint(f)) => LoroValue::Double(f.into_inner()),
        Yaml::Value(Scalar::String(s)) => LoroValue::String(s.as_ref().to_string().into()),
        Yaml::Sequence(items) => {
            let vec: Vec<LoroValue> = items.iter().map(yaml_to_loro).collect();
            LoroValue::List(vec.into())
        }
        Yaml::Mapping(m) => {
            let mut map: HashMap<String, LoroValue> = HashMap::new();
            for (k, v) in m.iter() {
                if let Some(ks) = yaml_as_str(k) {
                    map.insert(ks, yaml_to_loro(v));
                }
            }
            LoroValue::Map(map.into())
        }
        Yaml::Tagged(_, inner) => yaml_to_loro(inner),
        Yaml::Alias(_) => LoroValue::Null,
        Yaml::BadValue => LoroValue::Null,
        Yaml::Representation(s, _, _) => LoroValue::String(s.as_ref().to_string().into()),
    }
}

fn yaml_as_str(yaml: &Yaml) -> Option<String> {
    match yaml {
        Yaml::Value(Scalar::String(s)) => Some(s.as_ref().to_string()),
        Yaml::Representation(s, _, _) => Some(s.as_ref().to_string()),
        _ => None,
    }
}

// endregion: generic converters

#[cfg(test)]
mod tests {
    use super::*;

    // region: split_frontmatter

    #[test]
    fn split_frontmatter_basic() {
        let src = "---\nname: foo\n---\nbody text\n";
        let (fm, body) = split_frontmatter(src).unwrap();
        assert_eq!(fm, "name: foo");
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn split_frontmatter_crlf() {
        let src = "---\r\nname: foo\r\n---\r\nbody\r\n";
        let (fm, body) = split_frontmatter(src).unwrap();
        assert_eq!(fm, "name: foo\r");
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn split_frontmatter_empty_body() {
        let src = "---\nname: foo\n---\n";
        let (fm, body) = split_frontmatter(src).unwrap();
        assert_eq!(fm, "name: foo");
        assert_eq!(body, "");
    }

    #[test]
    fn split_frontmatter_missing_open_errors() {
        let src = "no frontmatter here";
        let err = split_frontmatter(src).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingDelimiters));
    }

    #[test]
    fn split_frontmatter_missing_close_errors() {
        let src = "---\nname: foo\nno closing delim";
        let err = split_frontmatter(src).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingDelimiters));
    }

    #[test]
    fn split_frontmatter_triple_dash_mid_line_is_not_delim() {
        // `---foo` mid-frontmatter is not a valid closing delimiter.
        let src = "---\nkey: ---foo\n---\nbody\n";
        let (fm, body) = split_frontmatter(src).unwrap();
        assert_eq!(fm, "key: ---foo");
        assert_eq!(body, "body\n");
    }

    // endregion: split_frontmatter

    // region: parse happy-path

    #[test]
    fn parse_minimal_frontmatter_uses_defaults() {
        // Only required keys: name + trust_tier (AC6.7).
        let src = "---\nname: my-skill\ntrust_tier: project-local\n---\n# Title\nBody.\n";
        let sf = parse(src.as_bytes()).unwrap();
        assert_eq!(sf.metadata.name, "my-skill");
        assert_eq!(sf.metadata.trust_tier, SkillTrustTier::ProjectLocal);
        assert_eq!(sf.metadata.description, None);
        assert!(sf.metadata.keywords.is_empty());
        assert_eq!(sf.metadata.hooks, JsonValue::Null);
        let LoroValue::Map(extras) = &sf.extras else {
            panic!("extras must be a map");
        };
        assert!(extras.is_empty());
        assert_eq!(sf.body, "# Title\nBody.\n");
    }

    #[test]
    fn parse_frontmatter_with_all_typed_fields() {
        let src = "---\n\
                   name: fix-auth\n\
                   trust_tier: first-party\n\
                   description: Fix the authentication bug.\n\
                   keywords:\n  - auth\n  - bug\n  - urgent\n\
                   ---\n\
                   Body.\n";
        let sf = parse(src.as_bytes()).unwrap();
        assert_eq!(sf.metadata.name, "fix-auth");
        assert_eq!(sf.metadata.trust_tier, SkillTrustTier::FirstParty);
        assert_eq!(
            sf.metadata.description,
            Some("Fix the authentication bug.".to_string())
        );
        assert_eq!(sf.metadata.keywords, vec!["auth", "bug", "urgent"]);
    }

    #[test]
    fn parse_nested_hooks_preserves_shape() {
        // AC6.4: hooks preserves nested structure as serde_json::Value.
        let src = "---\n\
                   name: k\n\
                   trust_tier: ad-hoc\n\
                   hooks:\n  \
                     on_turn_start:\n    \
                       - inject_context: Remember the plan.\n  \
                     on_memory_write:\n    \
                       - log: scratchpad-touched\n\
                   ---\n\
                   body\n";
        let sf = parse(src.as_bytes()).unwrap();
        let h = &sf.metadata.hooks;
        assert!(h.is_object());
        let obj = h.as_object().unwrap();
        assert!(obj.contains_key("on_turn_start"));
        assert!(obj.contains_key("on_memory_write"));

        let on_turn = &obj["on_turn_start"];
        assert!(on_turn.is_array());
        let first = &on_turn.as_array().unwrap()[0];
        assert_eq!(
            first.get("inject_context").and_then(|v| v.as_str()),
            Some("Remember the plan.")
        );
    }

    #[test]
    fn parse_unknown_keys_land_in_extras() {
        // AC6.3: unknown top-level keys preserved in extras LoroMap.
        let src = "---\n\
                   name: k\n\
                   trust_tier: ad-hoc\n\
                   author: \"@me\"\n\
                   version: 2\n\
                   ---\n\
                   body\n";
        let sf = parse(src.as_bytes()).unwrap();
        let LoroValue::Map(extras) = &sf.extras else {
            panic!("extras must be a map");
        };
        assert_eq!(extras.len(), 2);
        assert!(matches!(
            extras.get("author"),
            Some(LoroValue::String(s)) if s.as_str() == "@me"
        ));
        assert!(matches!(extras.get("version"), Some(LoroValue::I64(2))));
    }

    // endregion: parse happy-path

    // region: parse error paths

    #[test]
    fn parse_missing_delimiters_errors() {
        let src = "name: foo\ntrust_tier: ad-hoc\n";
        let err = parse(src.as_bytes()).unwrap_err();
        assert!(matches!(err, SkillParseError::MissingDelimiters));
    }

    #[test]
    fn parse_missing_name_errors_specifically() {
        // AC6.5 support: required-key error names the missing key.
        let src = "---\ntrust_tier: ad-hoc\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        assert!(
            matches!(err, SkillParseError::MissingRequiredKey { key: "name", .. }),
            "expected MissingRequiredKey for name, got {err:?}"
        );
    }

    #[test]
    fn parse_missing_trust_tier_errors_specifically() {
        let src = "---\nname: foo\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        assert!(
            matches!(
                err,
                SkillParseError::MissingRequiredKey {
                    key: "trust_tier",
                    ..
                }
            ),
            "expected MissingRequiredKey for trust_tier, got {err:?}"
        );
    }

    #[test]
    fn parse_invalid_trust_tier_errors_specifically() {
        // AC7.6: invalid enum value is InvalidTrustTier, NOT silently
        // defaulting or a generic TypeMismatch.
        let src = "---\nname: foo\ntrust_tier: foo\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        assert!(
            matches!(err, SkillParseError::InvalidTrustTier { ref value, .. } if value == "foo"),
            "expected InvalidTrustTier with value \"foo\", got {err:?}"
        );
    }

    #[test]
    fn parse_keywords_wrong_type_errors_type_mismatch() {
        let src = "---\nname: foo\ntrust_tier: ad-hoc\nkeywords: 42\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        match err {
            SkillParseError::TypeMismatch { key, expected, .. } => {
                assert_eq!(key, "keywords");
                assert_eq!(expected, "sequence");
            }
            other => panic!("expected TypeMismatch for keywords, got {other:?}"),
        }
    }

    #[test]
    fn parse_keywords_entry_wrong_type_errors_type_mismatch() {
        let src = "---\nname: foo\ntrust_tier: ad-hoc\nkeywords:\n  - a\n  - 99\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        match err {
            SkillParseError::TypeMismatch { key, expected, .. } => {
                assert_eq!(key, "keywords");
                assert_eq!(expected, "string");
            }
            other => panic!("expected TypeMismatch for keyword entry, got {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_yaml_returns_yaml_variant_with_span() {
        // AC6.5: YAML syntax error carries a span.
        let src = "---\nname: [unclosed\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        match err {
            SkillParseError::Yaml { span, .. } => {
                // Span is non-zero offset (we parsed something before erroring).
                assert!(span.offset() > 0, "expected non-zero span offset");
            }
            other => panic!("expected Yaml error, got {other:?}"),
        }
    }

    #[test]
    fn parse_non_utf8_errors() {
        let bytes = b"---\nname: \xFF\xFE\n---\nbody\n";
        let err = parse(bytes).unwrap_err();
        assert!(matches!(err, SkillParseError::NonUtf8Body));
    }

    #[test]
    fn parse_empty_name_errors() {
        let src = "---\nname: \"\"\ntrust_tier: ad-hoc\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        match err {
            SkillParseError::TypeMismatch { key, expected, .. } => {
                assert_eq!(key, "name");
                assert_eq!(expected, "non-empty string");
            }
            other => panic!("expected TypeMismatch for empty name, got {other:?}"),
        }
    }

    #[test]
    fn parse_root_not_mapping_errors() {
        // Frontmatter is a sequence, not a mapping.
        let src = "---\n- item1\n- item2\n---\nbody\n";
        let err = parse(src.as_bytes()).unwrap_err();
        match err {
            SkillParseError::TypeMismatch { key, expected, .. } => {
                assert_eq!(key, "<root>");
                assert_eq!(expected, "mapping");
            }
            other => panic!("expected root TypeMismatch, got {other:?}"),
        }
    }

    // endregion: parse error paths

    // region: hooks + extras edge cases

    #[test]
    fn parse_description_null_is_none() {
        let src = "---\nname: foo\ntrust_tier: ad-hoc\ndescription: null\n---\nbody\n";
        let sf = parse(src.as_bytes()).unwrap();
        assert_eq!(sf.metadata.description, None);
    }

    #[test]
    fn parse_empty_keywords_sequence() {
        let src = "---\nname: foo\ntrust_tier: ad-hoc\nkeywords: []\n---\nbody\n";
        let sf = parse(src.as_bytes()).unwrap();
        assert!(sf.metadata.keywords.is_empty());
    }

    #[test]
    fn extras_nested_map_preserved() {
        let src = "---\n\
                   name: k\n\
                   trust_tier: ad-hoc\n\
                   custom:\n  \
                     nested:\n      \
                       leaf: hello\n      \
                       count: 3\n\
                   ---\n\
                   body\n";
        let sf = parse(src.as_bytes()).unwrap();
        let LoroValue::Map(extras) = &sf.extras else {
            panic!("extras map");
        };
        let LoroValue::Map(custom) = extras.get("custom").expect("custom key") else {
            panic!("custom is a map");
        };
        let LoroValue::Map(nested) = custom.get("nested").expect("nested key") else {
            panic!("nested is a map");
        };
        assert!(matches!(
            nested.get("leaf"),
            Some(LoroValue::String(s)) if s.as_str() == "hello"
        ));
        assert!(matches!(nested.get("count"), Some(LoroValue::I64(3))));
    }

    // endregion: hooks + extras edge cases

    // region: CRLF normalization (M6)

    /// M6: a body that contains CRLF line endings must be normalized to LF
    /// before the SkillFile is returned.
    ///
    /// This matters because `emit()` always produces LF output. A CRLF body
    /// would produce a file whose parse re-yields a different body string,
    /// breaking content-hash stability and proptest round-trip equality.
    #[test]
    fn parse_normalizes_crlf_body_to_lf() {
        let src = "---\r\nname: foo\r\ntrust_tier: ad-hoc\r\n---\r\nline one\r\nline two\r\n";
        let sf = parse(src.as_bytes()).unwrap();
        assert_eq!(
            sf.body, "line one\nline two\n",
            "body must have CRLF normalized to LF; got {:?}",
            sf.body
        );
    }

    /// M6: a CRLF round-trip: parse CRLF → emit (LF) → parse again → bodies match.
    ///
    /// Confirms that the content-hash suppression path (emit(parse(file)) == file)
    /// holds even when the original file has CRLF line endings.
    #[test]
    fn crlf_body_parse_emit_parse_produces_lf() {
        use super::super::emit::emit;
        use std::collections::HashMap;

        let src = "---\r\nname: test\r\ntrust_tier: ad-hoc\r\n---\r\nsome body\r\n";
        let first = parse(src.as_bytes()).unwrap();
        // After parse, body must be LF-normalized.
        assert_eq!(first.body, "some body\n");

        let extras_empty = loro::LoroValue::Map(HashMap::<String, loro::LoroValue>::new().into());
        let emitted = emit(&first.metadata, &extras_empty, &first.body).unwrap();
        let second = parse(emitted.as_bytes()).unwrap();
        assert_eq!(
            first.body, second.body,
            "body must survive CRLF → LF normalization across two parse-emit cycles"
        );
        assert_eq!(first.metadata, second.metadata);
    }

    // endregion: CRLF normalization (M6)
}
