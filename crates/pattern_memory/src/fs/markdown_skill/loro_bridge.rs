//! Bridge between Skill CRDT state (LoroDoc) and the typed [`SkillFile`] representation.
//!
//! Skills store their content in a LoroDoc with three root-level containers:
//!
//! - `"metadata"` — `LoroMap` carrying scalar fields from [`SkillMetadata`].
//!   Each field is stored as a JSON-serialized string so values survive
//!   CRDT merge without type coercion. Field keys:
//!   - `"name"` (String)
//!   - `"trust_tier"` (String, kebab-case)
//!   - `"description"` (String, omitted when None)
//!   - `"keywords_json"` (String, JSON array of strings; omitted when empty)
//!   - `"hooks_json"` (String, serialized [`serde_json::Value`]; omitted when Null)
//! - `"extras"` — `LoroMap` carrying unknown frontmatter keys as JSON-encoded
//!   strings. Each key maps to a single JSON string value representing a
//!   (potentially nested) [`LoroValue`]. Using JSON strings here avoids the
//!   need to recursively mirror arbitrary LoroValue trees into nested Loro
//!   containers, which would require separate sub-container lifecycle management.
//!   On projection, the JSON strings are decoded back to [`LoroValue`] before
//!   being assembled into the `extras` map passed to [`super::emit`].
//! - `"body"` — `LoroText` holding the raw markdown body.
//!
//! # Why JSON strings?
//!
//! Storing complex values (nested maps, lists, the hooks manifest) as JSON
//! strings follows the same pattern that Map/Composite blocks use for their
//! field values (see `apply_json_to_loro_doc` in `cache.rs`, lines 1358-1364).
//! The trade-off is coarser CRDT granularity (whole-field LWW instead of
//! per-entry merge), which is acceptable for Skill blocks — they are
//! read-mostly skill definitions, not collaboratively-edited task lists.

use std::collections::HashMap;

use loro::{LoroDoc, LoroMapValue, LoroValue};
use pattern_core::types::memory_types::{SkillMetadata, SkillTrustTier};
use serde_json::Value as JsonValue;

use super::emit::SkillEmitError;
use super::parse::SkillFile;

// region: write_skill_to_loro_doc

/// Write the contents of a parsed [`SkillFile`] into a [`LoroDoc`].
///
/// Populates three root-level containers:
/// - `"metadata"` — typed scalar fields from [`SkillMetadata`].
/// - `"extras"` — unknown frontmatter keys, each encoded as a JSON string.
/// - `"body"` — the raw markdown body text.
///
/// Each call fully replaces the prior state; this function is suitable for the
/// external-edit inbound path where a watcher has detected a file change and
/// needs to reconcile the on-disk state into the CRDT document.
///
/// The caller is responsible for calling `doc.commit()` after this function
/// returns.
pub fn write_skill_to_loro_doc(skill_file: &SkillFile, doc: &LoroDoc) -> Result<(), String> {
    write_metadata_to_loro_map(doc, &skill_file.metadata).map_err(|e| e.to_string())?;
    write_extras_to_loro_map(doc, &skill_file.extras)?;

    let body_text = doc.get_text("body");
    body_text
        .update(&skill_file.body, Default::default())
        .map_err(|e| format!("LoroText update for 'body' failed: {e}"))?;

    Ok(())
}

// endregion: write_skill_to_loro_doc

// region: project_skill_from_loro_root

/// Project a [`SkillMetadata`] from the root [`LoroValue::Map`] produced by
/// `disk_doc.get_deep_value()`.
///
/// The map is expected to contain a `"metadata"` sub-map written by
/// [`write_skill_to_loro_doc`]. Missing or absent optional fields default to
/// their zero-values.
///
/// Returns an error string on malformed data (e.g., unparseable JSON or an
/// unrecognised trust-tier string). These errors surface as rendering failures
/// in the subscriber worker and cause the emission cycle to be skipped for
/// this commit.
pub fn project_metadata_from_loro(root: &LoroMapValue) -> Result<SkillMetadata, String> {
    let metadata_map = match root.get("metadata") {
        Some(LoroValue::Map(m)) => m.clone(),
        Some(other) => {
            return Err(format!(
                "Skill 'metadata' container is not a LoroMap; got {other:?}"
            ));
        }
        // No metadata container yet — likely a newly-created empty block.
        // Return a sentinel with an empty name that will cause emit to fail
        // loudly rather than silently writing a broken file.
        None => {
            return Err(
                "Skill disk_doc has no 'metadata' container; block may not have been \
                 initialized via the inbound parser"
                    .to_string(),
            );
        }
    };

    let name = read_string_field(&metadata_map, "name")?.unwrap_or_default();
    if name.is_empty() {
        return Err("Skill 'name' field is empty or absent in disk_doc metadata".to_string());
    }

    let trust_tier_str =
        read_string_field(&metadata_map, "trust_tier")?.unwrap_or_else(|| "ad-hoc".to_string());
    let trust_tier = parse_trust_tier(&trust_tier_str)?;

    let description = read_string_field(&metadata_map, "description")?;

    let keywords: Vec<String> = match read_string_field(&metadata_map, "keywords_json")? {
        Some(json_str) if !json_str.is_empty() && json_str != "[]" => {
            serde_json::from_str(&json_str)
                .map_err(|e| format!("failed to parse 'keywords_json': {e}"))?
        }
        _ => Vec::new(),
    };

    let hooks: JsonValue = match read_string_field(&metadata_map, "hooks_json")? {
        Some(json_str) if !json_str.is_empty() => serde_json::from_str(&json_str)
            .map_err(|e| format!("failed to parse 'hooks_json': {e}"))?,
        _ => JsonValue::Null,
    };

    Ok(SkillMetadata {
        name,
        trust_tier,
        description,
        keywords,
        hooks,
    })
}

/// Project the `"extras"` sub-map from the root produced by `get_deep_value()`.
///
/// Each value in the stored extras map is a JSON-encoded string that is decoded
/// back to a [`LoroValue`]. Missing or non-map `"extras"` containers default to
/// an empty map.
pub fn project_extras_from_loro(root: &LoroMapValue) -> Result<LoroValue, String> {
    let extras_stored = match root.get("extras") {
        Some(LoroValue::Map(m)) => m.clone(),
        Some(_) | None => return Ok(LoroValue::Map(Default::default())),
    };

    let mut result: HashMap<String, LoroValue> = HashMap::new();
    for (key, val) in extras_stored.iter() {
        let loro_val = match val {
            LoroValue::String(json_str) => {
                // Decode the JSON-encoded LoroValue back to its original form.
                let json: JsonValue = serde_json::from_str(json_str.as_ref())
                    .map_err(|e| format!("failed to decode extras[{key}] JSON: {e}"))?;
                json_to_loro_value_bridge(&json)
            }
            // If somehow a non-string value ended up here, pass it through.
            other => other.clone(),
        };
        result.insert(key.to_string(), loro_val);
    }

    Ok(LoroValue::Map(result.into()))
}

// endregion: project_skill_from_loro_root

// region: internal write helpers

fn write_metadata_to_loro_map(doc: &LoroDoc, meta: &SkillMetadata) -> Result<(), SkillEmitError> {
    let m = doc.get_map("metadata");

    m.insert("name", LoroValue::String(meta.name.clone().into()))
        .map_err(|_| SkillEmitError::Fmt)?;

    let tier_str = trust_tier_to_str(meta.trust_tier)?;
    m.insert("trust_tier", LoroValue::String(tier_str.into()))
        .map_err(|_| SkillEmitError::Fmt)?;

    match &meta.description {
        Some(d) => {
            m.insert("description", LoroValue::String(d.clone().into()))
                .map_err(|_| SkillEmitError::Fmt)?;
        }
        None => {
            // Explicitly set to Null so prior descriptions are cleared on
            // external-edit round-trips.
            m.insert("description", LoroValue::Null)
                .map_err(|_| SkillEmitError::Fmt)?;
        }
    }

    if meta.keywords.is_empty() {
        // Clear any prior keywords by writing an empty JSON array.
        m.insert("keywords_json", LoroValue::String("[]".into()))
            .map_err(|_| SkillEmitError::Fmt)?;
    } else {
        let json_str = serde_json::to_string(&meta.keywords).map_err(|_| SkillEmitError::Fmt)?;
        m.insert("keywords_json", LoroValue::String(json_str.into()))
            .map_err(|_| SkillEmitError::Fmt)?;
    }

    if meta.hooks.is_null() {
        // Clear any prior hooks.
        m.insert("hooks_json", LoroValue::Null)
            .map_err(|_| SkillEmitError::Fmt)?;
    } else {
        let json_str = serde_json::to_string(&meta.hooks).map_err(|_| SkillEmitError::Fmt)?;
        m.insert("hooks_json", LoroValue::String(json_str.into()))
            .map_err(|_| SkillEmitError::Fmt)?;
    }

    Ok(())
}

fn write_extras_to_loro_map(doc: &LoroDoc, extras: &LoroValue) -> Result<(), String> {
    let extras_map = match extras {
        LoroValue::Map(m) => m,
        _ => return Err(format!("extras must be a LoroValue::Map; got {extras:?}")),
    };

    let m = doc.get_map("extras");

    // Delete any keys that are no longer in the incoming extras. Without this
    // step, keys removed from a .md file on disk would persist in the LoroDoc
    // forever — resurrecting stale data on the next outbound render.
    let existing_keys: Vec<String> = {
        // get_deep_value materializes the current map contents; collect key
        // names so we can delete anything absent from extras_map.
        let deep = m.get_deep_value();
        if let LoroValue::Map(current) = deep {
            current
                .keys()
                .filter(|k| !extras_map.contains_key(k.as_str()))
                .map(|k| k.to_string())
                .collect()
        } else {
            Vec::new()
        }
    };
    for key in existing_keys {
        m.delete(&key)
            .map_err(|e| format!("extras delete('{key}') failed: {e}"))?;
    }

    // Insert each extras value as a JSON string so we can handle arbitrary
    // nesting without creating deep LoroDoc container hierarchies.
    for (key, val) in extras_map.iter() {
        let json_val = loro_value_to_json_bridge(val)
            .ok_or_else(|| format!("extras[{key}] contains a LoroValue variant that cannot be JSON-encoded (binary or container handle)"))?;
        let json_str = serde_json::to_string(&json_val)
            .map_err(|e| format!("extras[{key}] JSON serialize failed: {e}"))?;
        m.insert(key.as_ref(), LoroValue::String(json_str.into()))
            .map_err(|e| format!("extras insert('{key}') failed: {e}"))?;
    }

    Ok(())
}

// endregion: internal write helpers

// region: value conversion helpers

/// Convert a [`LoroValue`] to a [`serde_json::Value`] for serialization into
/// the LoroDoc's extras string slots. Returns `None` for LoroValue variants
/// without JSON equivalents (binary blobs, container handles).
fn loro_value_to_json_bridge(v: &LoroValue) -> Option<JsonValue> {
    match v {
        LoroValue::Null => Some(JsonValue::Null),
        LoroValue::Bool(b) => Some(JsonValue::Bool(*b)),
        LoroValue::I64(i) => Some(serde_json::json!(i)),
        LoroValue::Double(f) => serde_json::Number::from_f64(*f).map(JsonValue::Number),
        LoroValue::String(s) => Some(JsonValue::String(s.to_string())),
        LoroValue::List(items) => {
            let arr: Option<Vec<JsonValue>> = items.iter().map(loro_value_to_json_bridge).collect();
            arr.map(JsonValue::Array)
        }
        LoroValue::Map(m) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in m.iter() {
                let jv = loro_value_to_json_bridge(v)?;
                obj.insert(k.to_string(), jv);
            }
            Some(JsonValue::Object(obj))
        }
        LoroValue::Binary(_) | LoroValue::Container(_) => None,
    }
}

/// Convert a [`serde_json::Value`] to a [`LoroValue`] for reconstruction
/// when projecting extras back from the stored JSON strings. This is the
/// inverse of [`loro_value_to_json_bridge`].
fn json_to_loro_value_bridge(v: &JsonValue) -> LoroValue {
    match v {
        JsonValue::Null => LoroValue::Null,
        JsonValue::Bool(b) => LoroValue::Bool(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                LoroValue::I64(i)
            } else if let Some(f) = n.as_f64() {
                LoroValue::Double(f)
            } else {
                // u64 values exceeding i64 max: represent as string to avoid
                // precision loss (consistent with emit.rs json_to_yaml handling).
                LoroValue::String(n.to_string().into())
            }
        }
        JsonValue::String(s) => LoroValue::String(s.clone().into()),
        JsonValue::Array(items) => {
            let list: Vec<LoroValue> = items.iter().map(json_to_loro_value_bridge).collect();
            LoroValue::List(list.into())
        }
        JsonValue::Object(obj) => {
            let mut map: HashMap<String, LoroValue> = HashMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_loro_value_bridge(v));
            }
            LoroValue::Map(map.into())
        }
    }
}

// endregion: value conversion helpers

// region: trust tier helpers

fn trust_tier_to_str(tier: SkillTrustTier) -> Result<&'static str, SkillEmitError> {
    match tier {
        SkillTrustTier::FirstParty => Ok("first-party"),
        SkillTrustTier::ProjectLocal => Ok("project-local"),
        SkillTrustTier::PluginInstalled => Ok("plugin-installed"),
        SkillTrustTier::AdHoc => Ok("ad-hoc"),
        // Fail loud if a new variant is added upstream without updating this
        // match. Silently coercing to "ad-hoc" would hide the bug.
        _ => Err(SkillEmitError::UnsupportedTrustTier),
    }
}

fn parse_trust_tier(s: &str) -> Result<SkillTrustTier, String> {
    match s {
        "first-party" => Ok(SkillTrustTier::FirstParty),
        "project-local" => Ok(SkillTrustTier::ProjectLocal),
        "plugin-installed" => Ok(SkillTrustTier::PluginInstalled),
        "ad-hoc" => Ok(SkillTrustTier::AdHoc),
        other => Err(format!("unrecognised trust tier '{other}'")),
    }
}

// endregion: trust tier helpers

// region: scalar read helper

fn read_string_field(map: &LoroMapValue, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        Some(LoroValue::String(s)) => Ok(Some(s.as_ref().to_string())),
        Some(LoroValue::Null) | None => Ok(None),
        Some(other) => Err(format!(
            "expected string or null for metadata['{key}'], got {other:?}"
        )),
    }
}

// endregion: scalar read helper

// region: tests

#[cfg(test)]
mod tests {
    use super::*;

    fn make_loro_doc() -> LoroDoc {
        LoroDoc::new()
    }

    fn minimal_skill_file() -> SkillFile {
        SkillFile {
            metadata: SkillMetadata {
                name: "my-skill".to_string(),
                trust_tier: SkillTrustTier::ProjectLocal,
                description: None,
                keywords: vec![],
                hooks: JsonValue::Null,
            },
            extras: LoroValue::Map(Default::default()),
            body: "body text\n".to_string(),
        }
    }

    #[test]
    fn write_and_project_minimal_skill_roundtrip() {
        let doc = make_loro_doc();
        let sf = minimal_skill_file();

        write_skill_to_loro_doc(&sf, &doc).unwrap();
        doc.commit();

        let deep = doc.get_deep_value();
        let root = match &deep {
            LoroValue::Map(m) => m,
            _ => panic!("expected root map"),
        };

        let projected_meta = project_metadata_from_loro(root).unwrap();
        assert_eq!(projected_meta.name, "my-skill");
        assert_eq!(projected_meta.trust_tier, SkillTrustTier::ProjectLocal);
        assert_eq!(projected_meta.description, None);
        assert!(projected_meta.keywords.is_empty());
        assert_eq!(projected_meta.hooks, JsonValue::Null);

        let projected_extras = project_extras_from_loro(root).unwrap();
        assert!(matches!(projected_extras, LoroValue::Map(m) if m.is_empty()));
    }

    #[test]
    fn write_and_project_full_skill_roundtrip() {
        let doc = make_loro_doc();
        let mut extras: HashMap<String, LoroValue> = HashMap::new();
        extras.insert("author".to_string(), LoroValue::String("@me".into()));
        extras.insert("version".to_string(), LoroValue::I64(3));
        let sf = SkillFile {
            metadata: SkillMetadata {
                name: "full-skill".to_string(),
                trust_tier: SkillTrustTier::FirstParty,
                description: Some("A full skill.".to_string()),
                keywords: vec!["a".to_string(), "b".to_string()],
                hooks: serde_json::json!({"on_load": [{"log": "loaded"}]}),
            },
            extras: LoroValue::Map(extras.into()),
            body: "# Title\n\nBody.\n".to_string(),
        };

        write_skill_to_loro_doc(&sf, &doc).unwrap();
        doc.commit();

        let deep = doc.get_deep_value();
        let root = match &deep {
            LoroValue::Map(m) => m,
            _ => panic!("expected root map"),
        };

        let projected_meta = project_metadata_from_loro(root).unwrap();
        assert_eq!(projected_meta.name, "full-skill");
        assert_eq!(projected_meta.trust_tier, SkillTrustTier::FirstParty);
        assert_eq!(
            projected_meta.description,
            Some("A full skill.".to_string())
        );
        assert_eq!(projected_meta.keywords, vec!["a", "b"]);
        assert_eq!(
            projected_meta.hooks,
            serde_json::json!({"on_load": [{"log": "loaded"}]})
        );

        let projected_extras = project_extras_from_loro(root).unwrap();
        let LoroValue::Map(emap) = &projected_extras else {
            panic!("extras must be map");
        };
        assert!(matches!(emap.get("author"), Some(LoroValue::String(s)) if s.as_ref() == "@me"));
        assert!(matches!(emap.get("version"), Some(LoroValue::I64(3))));

        // Body text.
        let body = match root.get("body") {
            Some(LoroValue::String(s)) => s.as_ref().to_string(),
            other => panic!("expected body string, got {other:?}"),
        };
        assert_eq!(body, "# Title\n\nBody.\n");
    }

    #[test]
    fn missing_metadata_container_returns_error() {
        // A LoroDoc with no "metadata" container should surface a clear error.
        let doc = make_loro_doc();
        doc.commit();

        let deep = doc.get_deep_value();
        let root = match &deep {
            LoroValue::Map(m) => m,
            _ => panic!("expected root map"),
        };

        let err = project_metadata_from_loro(root).unwrap_err();
        assert!(
            err.contains("no 'metadata' container"),
            "expected error about missing metadata container; got: {err}"
        );
    }

    #[test]
    fn extras_with_nested_map_roundtrips_via_json_encoding() {
        let doc = make_loro_doc();
        let mut nested: HashMap<String, LoroValue> = HashMap::new();
        nested.insert("leaf".to_string(), LoroValue::String("hello".into()));
        nested.insert("count".to_string(), LoroValue::I64(7));
        let mut extras: HashMap<String, LoroValue> = HashMap::new();
        extras.insert("custom".to_string(), LoroValue::Map(nested.into()));
        let sf = SkillFile {
            metadata: minimal_skill_file().metadata,
            extras: LoroValue::Map(extras.into()),
            body: String::new(),
        };

        write_skill_to_loro_doc(&sf, &doc).unwrap();
        doc.commit();

        let deep = doc.get_deep_value();
        let root = match &deep {
            LoroValue::Map(m) => m,
            _ => panic!("expected root map"),
        };
        let projected_extras = project_extras_from_loro(root).unwrap();
        let LoroValue::Map(emap) = &projected_extras else {
            panic!("extras must be map");
        };
        let LoroValue::Map(custom) = emap.get("custom").unwrap() else {
            panic!("custom must be map");
        };
        assert!(matches!(custom.get("leaf"), Some(LoroValue::String(s)) if s.as_ref() == "hello"));
        assert!(matches!(custom.get("count"), Some(LoroValue::I64(7))));
    }

    /// C1: when extras is written twice and the second call is missing a key
    /// that was present in the first, `project_extras_from_loro` must NOT
    /// return the removed key. Without the key-deletion step in
    /// `write_extras_to_loro_map`, the LoroDoc would resurrect stale entries.
    #[test]
    fn write_extras_twice_removes_deleted_keys() {
        let doc = make_loro_doc();

        // First write: two keys.
        let mut extras_first: HashMap<String, LoroValue> = HashMap::new();
        extras_first.insert("keep".to_string(), LoroValue::String("alive".into()));
        extras_first.insert("drop".to_string(), LoroValue::String("dead".into()));
        let sf_first = SkillFile {
            metadata: minimal_skill_file().metadata,
            extras: LoroValue::Map(extras_first.into()),
            body: String::new(),
        };
        write_skill_to_loro_doc(&sf_first, &doc).unwrap();
        doc.commit();

        // Second write: only "keep" key. "drop" was removed from the file.
        let mut extras_second: HashMap<String, LoroValue> = HashMap::new();
        extras_second.insert("keep".to_string(), LoroValue::String("alive".into()));
        let sf_second = SkillFile {
            metadata: sf_first.metadata.clone(),
            extras: LoroValue::Map(extras_second.into()),
            body: String::new(),
        };
        write_skill_to_loro_doc(&sf_second, &doc).unwrap();
        doc.commit();

        let deep = doc.get_deep_value();
        let root = match &deep {
            LoroValue::Map(m) => m,
            _ => panic!("expected root map"),
        };
        let projected = project_extras_from_loro(root).unwrap();
        let LoroValue::Map(emap) = &projected else {
            panic!("extras must be map");
        };

        // "keep" must still be present.
        assert!(
            matches!(emap.get("keep"), Some(LoroValue::String(s)) if s.as_ref() == "alive"),
            "'keep' key must survive the second write; got: {emap:?}"
        );
        // "drop" must have been deleted by the second write.
        assert!(
            emap.get("drop").is_none(),
            "'drop' key must be absent after second write (data resurrection check); got: {emap:?}"
        );
    }
}

// endregion: tests
