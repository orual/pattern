//! `LoroValue` ↔ `KdlDocument` converter for Map, List, and Composite blocks.
//!
//! KDL-native shape conventions:
//! - **Map** → each key becomes a named node. Scalar values are single
//!   positional arguments (`foo "bar"`). Nested Map/List values use children.
//! - **List** → each item is a node with the reserved name `"-"`. Scalar items
//!   carry their value as a single argument. Complex items use children.
//! - **Composite** → structurally a Map at top level (section names are node
//!   names); handled via [`TopShape::Map`].
//!
//! Schema-directed disambiguation: the caller passes [`TopShape::Map`] or
//! [`TopShape::List`] based on the block's [`BlockSchema`]. No in-file sentinel
//! needed.

use std::collections::HashMap;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use loro::LoroValue;

/// Errors specific to the KDL ↔ LoroValue conversion.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum KdlConversionError {
    /// A LoroValue variant that has no KDL representation was encountered.
    #[error("unsupported LoroValue variant: {0}")]
    UnsupportedVariant(String),

    /// `LoroValue::Binary` was encountered — blocks must not contain raw bytes.
    #[error("unsupported Binary LoroValue — blocks must not contain raw bytes")]
    UnsupportedBinary,

    /// KDL syntax could not be parsed.
    #[error("KDL parse error: {0}")]
    ParseError(String),

    /// The declared schema shape does not match the actual LoroValue or KDL
    /// document structure.
    #[error("shape mismatch: expected {expected:?}, got: {actual}")]
    ShapeMismatch { expected: TopShape, actual: String },

    /// A Map-shaped KDL document contains duplicate keys.
    #[error("duplicate key in map: {key}")]
    DuplicateKey { key: String },

    /// The KDL node has both positional arguments and children, which is
    /// ambiguous for LoroValue mapping.
    #[error("ambiguous KDL node: has both arguments and children")]
    AmbiguousNode,

    /// A `TaskEdgeRef` inside a `blocks` node failed to parse.
    #[error("invalid TaskEdgeRef: {source}")]
    #[diagnostic(code(pattern_memory::kdl::task_edge_ref))]
    TaskEdgeRef {
        /// The byte-offset span of the offending entry in the KDL source.
        #[label("invalid block reference here")]
        span: miette::SourceSpan,
        source: pattern_core::types::memory_types::TaskEdgeRefParseError,
    },

    /// A `blocks` child entry is missing the `(block)` type annotation.
    #[error("missing (block) type annotation")]
    #[diagnostic(code(pattern_memory::kdl::missing_block_annotation))]
    MissingBlockAnnotation {
        /// The byte-offset span of the offending entry in the KDL source.
        #[label("expected (block) type annotation here")]
        span: miette::SourceSpan,
    },
}

/// Top-level shape hint for the KDL converter.
///
/// Composite blocks are structurally maps at the top level (section names are
/// node names), so they use [`TopShape::Map`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopShape {
    Map,
    List,
    TaskList,
}

/// Serialize a `LoroValue` to a `KdlDocument`.
///
/// The caller supplies a top-level shape hint (matching the block's
/// `BlockSchema`) so the output format matches. The returned document is
/// auto-formatted for human readability.
pub fn loro_value_to_kdl(
    value: &LoroValue,
    shape: TopShape,
) -> Result<KdlDocument, KdlConversionError> {
    let mut doc = KdlDocument::new();
    match (shape, value) {
        (TopShape::Map, LoroValue::Map(m)) => {
            // Sort keys for deterministic output — FxHashMap iteration order
            // is nondeterministic, and the content hash must be stable.
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                doc.nodes_mut()
                    .push(loro_value_to_kdl_node(k, m.get(k).unwrap())?);
            }
        }
        (TopShape::List, LoroValue::List(l)) => {
            for v in l.iter() {
                doc.nodes_mut().push(loro_value_to_kdl_node("-", v)?);
            }
        }
        (TopShape::TaskList, _) => {
            return super::kdl_task_list::task_list_to_kdl(value);
        }
        (shape, other) => {
            return Err(KdlConversionError::ShapeMismatch {
                expected: shape,
                actual: format!("{other:?}"),
            });
        }
    }
    doc.autoformat();
    Ok(doc)
}

/// Deserialize a `KdlDocument` into a `LoroValue` using the block's declared
/// shape.
///
/// The caller consults the block's `BlockSchema` (from memory.db metadata) and
/// passes the matching `TopShape`. This makes the Map/List distinction
/// unambiguous.
pub fn kdl_to_loro_value(
    doc: &KdlDocument,
    shape: TopShape,
) -> Result<LoroValue, KdlConversionError> {
    let nodes = doc.nodes();
    match shape {
        TopShape::Map => {
            let mut out = HashMap::new();
            for n in nodes {
                let key = n.name().value().to_owned();
                if key == "-" {
                    return Err(KdlConversionError::ShapeMismatch {
                        expected: TopShape::Map,
                        actual: "document contains list-item sentinel `-` but schema is Map".into(),
                    });
                }
                if out.contains_key(&key) {
                    return Err(KdlConversionError::DuplicateKey { key });
                }
                out.insert(key, kdl_node_to_loro_value(n)?);
            }
            Ok(LoroValue::Map(out.into()))
        }
        TopShape::List => {
            let mut out = Vec::with_capacity(nodes.len());
            for n in nodes {
                if n.name().value() != "-" {
                    return Err(KdlConversionError::ShapeMismatch {
                        expected: TopShape::List,
                        actual: format!(
                            "list schema requires all top-level nodes named `-`; found `{}`",
                            n.name().value()
                        ),
                    });
                }
                out.push(kdl_node_to_loro_value(n)?);
            }
            Ok(LoroValue::List(out.into()))
        }
        TopShape::TaskList => super::kdl_task_list::kdl_to_task_list(doc),
    }
}

/// Parse a KDL string into a `KdlDocument`, wrapping parse errors.
pub fn parse_kdl(input: &str) -> Result<KdlDocument, KdlConversionError> {
    KdlDocument::parse(input).map_err(|e| KdlConversionError::ParseError(e.to_string()))
}

/// Convert a `LoroValue` to a `serde_json::Value`.
///
/// Used by the external-edit import path to bridge from the KDL parse output
/// (`LoroValue`) to the `StructuredDocument::import_from_json` API.
/// Returns `None` for `Binary` and `Container` variants that have no JSON
/// representation.
pub fn loro_value_to_json(value: &LoroValue) -> Option<serde_json::Value> {
    match value {
        LoroValue::Null => Some(serde_json::Value::Null),
        LoroValue::Bool(b) => Some(serde_json::Value::Bool(*b)),
        LoroValue::Double(d) => serde_json::Number::from_f64(*d).map(serde_json::Value::Number),
        LoroValue::I64(i) => Some(serde_json::Value::Number((*i).into())),
        LoroValue::String(s) => Some(serde_json::Value::String(s.to_string())),
        LoroValue::List(list) => {
            let items: Vec<serde_json::Value> =
                list.iter().filter_map(loro_value_to_json).collect();
            Some(serde_json::Value::Array(items))
        }
        LoroValue::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map.iter() {
                if let Some(json_v) = loro_value_to_json(v) {
                    obj.insert(k.to_string(), json_v);
                }
            }
            Some(serde_json::Value::Object(obj))
        }
        LoroValue::Binary(_) | LoroValue::Container(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a single `LoroValue` into a `KdlNode` with the given name.
pub(super) fn loro_value_to_kdl_node(
    name: &str,
    value: &LoroValue,
) -> Result<KdlNode, KdlConversionError> {
    let mut node = KdlNode::new(name);
    match value {
        LoroValue::Null => {
            node.push(KdlEntry::new(KdlValue::Null));
        }
        LoroValue::Bool(b) => {
            node.push(KdlEntry::new(*b));
        }
        LoroValue::Double(d) => {
            node.push(KdlEntry::new(*d));
        }
        LoroValue::I64(i) => {
            node.push(KdlEntry::new(i128::from(*i)));
        }
        LoroValue::String(s) => {
            node.push(KdlEntry::new(s.as_str()));
        }
        LoroValue::List(l) => {
            if l.is_empty() {
                // Empty list: use a type annotation to distinguish from empty
                // Map (which also produces `{ }`) and from Null (no children).
                node.set_ty("list");
                node.set_children(KdlDocument::new());
            } else {
                // Non-empty list: if there are 2+ items, all are scalar, and
                // no string contains a newline, collapse into positional
                // arguments on this node. Single-element lists MUST use
                // children form because a single positional arg is
                // indistinguishable from a bare scalar in the reverse
                // converter.
                let can_collapse = l.len() >= 2 && l.iter().all(is_scalar_single_line);
                if can_collapse {
                    for v in l.iter() {
                        node.push(scalar_loro_to_kdl_entry(v)?);
                    }
                } else {
                    let mut children = KdlDocument::new();
                    for v in l.iter() {
                        children.nodes_mut().push(loro_value_to_kdl_node("-", v)?);
                    }
                    node.set_children(children);
                }
            }
        }
        LoroValue::Map(m) => {
            // Always set children for maps, even empty ones, so that the
            // reverse converter can distinguish `Map({})` from `Null`.
            // Sort keys for deterministic output.
            let mut children = KdlDocument::new();
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                children
                    .nodes_mut()
                    .push(loro_value_to_kdl_node(k, m.get(k).unwrap())?);
            }
            node.set_children(children);
        }
        LoroValue::Binary(_) => return Err(KdlConversionError::UnsupportedBinary),
        LoroValue::Container(cid) => {
            let mut entry = KdlEntry::new(cid.to_string());
            entry.set_ty("container");
            node.push(entry);
        }
    }
    Ok(node)
}

/// Check whether a `LoroValue` is a scalar that can be represented as a single
/// KDL positional argument on a single line.
fn is_scalar_single_line(value: &LoroValue) -> bool {
    match value {
        LoroValue::Null | LoroValue::Bool(_) | LoroValue::Double(_) | LoroValue::I64(_) => true,
        LoroValue::String(s) => !s.contains('\n'),
        _ => false,
    }
}

/// Convert a scalar `LoroValue` into a `KdlEntry` (positional argument).
///
/// The caller must ensure the value is scalar; non-scalar variants produce an
/// error.
fn scalar_loro_to_kdl_entry(value: &LoroValue) -> Result<KdlEntry, KdlConversionError> {
    match value {
        LoroValue::Null => Ok(KdlEntry::new(KdlValue::Null)),
        LoroValue::Bool(b) => Ok(KdlEntry::new(*b)),
        LoroValue::Double(d) => Ok(KdlEntry::new(*d)),
        LoroValue::I64(i) => Ok(KdlEntry::new(i128::from(*i))),
        LoroValue::String(s) => Ok(KdlEntry::new(s.as_str())),
        other => Err(KdlConversionError::UnsupportedVariant(format!(
            "scalar-only context, got {other:?}"
        ))),
    }
}

/// Convert a `KdlNode` back into a `LoroValue`.
///
/// Shape decision rules per node:
/// 1. Node has >1 positional arg, no children → `LoroValue::List` of scalars.
/// 2. Node has 1 positional arg, no children → scalar `LoroValue`.
/// 3. Node has 0 args, children all named "-" → `LoroValue::List`.
/// 4. Node has 0 args, children with distinct names → `LoroValue::Map`.
/// 5. Node has 0 args, 0 children → `LoroValue::Null`.
/// 6. Node has args AND children → error (ambiguous).
fn kdl_node_to_loro_value(node: &KdlNode) -> Result<LoroValue, KdlConversionError> {
    let positional_entries: Vec<&KdlEntry> = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .collect();
    let children_block = node.children();
    let has_children_block = children_block.is_some();
    let child_nodes = children_block.map(|c| c.nodes()).unwrap_or_default();
    let has_nonempty_children = !child_nodes.is_empty();

    // Check for type annotation — `(list)` marks an empty list node.
    if node.ty().map(|t| t.value()) == Some("list") {
        if child_nodes.is_empty() {
            return Ok(LoroValue::List(vec![].into()));
        }
        // Non-empty `(list)` node: parse children as list items.
        let items: Vec<LoroValue> = child_nodes
            .iter()
            .map(kdl_node_to_loro_value)
            .collect::<Result<_, _>>()?;
        return Ok(LoroValue::List(items.into()));
    }

    if !positional_entries.is_empty() && has_nonempty_children {
        // Rule 6: ambiguous.
        return Err(KdlConversionError::AmbiguousNode);
    }

    if positional_entries.len() > 1 {
        // Rule 1: multiple positional args → list of scalars.
        let items: Vec<LoroValue> = positional_entries
            .iter()
            .map(|e| kdl_value_to_loro_value(e.value()))
            .collect::<Result<_, _>>()?;
        return Ok(LoroValue::List(items.into()));
    }

    if positional_entries.len() == 1 && !has_nonempty_children {
        // Rule 2: single positional arg → scalar.
        let entry = positional_entries[0];
        // Check if it has a type annotation "container".
        if entry.ty().map(|t| t.value()) == Some("container")
            && let KdlValue::String(s) = entry.value()
        {
            use std::convert::TryFrom;
            return Ok(LoroValue::Container(
                loro::ContainerID::try_from(s.as_str()).map_err(|_| {
                    KdlConversionError::ParseError(format!("invalid ContainerID: {s}"))
                })?,
            ));
        }
        return kdl_value_to_loro_value(entry.value());
    }

    // No positional args (or single arg with children — handled above).
    if has_children_block {
        if child_nodes.is_empty() {
            // Empty children block `{ }` without a `(list)` type annotation
            // means empty Map. Empty lists use `(list)` type annotation.
            return Ok(LoroValue::Map(HashMap::new().into()));
        }

        // Check if all children are named "-" → list (Rule 3).
        let all_dash = child_nodes.iter().all(|n| n.name().value() == "-");
        if all_dash {
            // Rule 3: list.
            let items: Vec<LoroValue> = child_nodes
                .iter()
                .map(kdl_node_to_loro_value)
                .collect::<Result<_, _>>()?;
            return Ok(LoroValue::List(items.into()));
        }

        // Rule 4: map.
        let mut out = HashMap::new();
        for n in child_nodes {
            let key = n.name().value().to_owned();
            if out.contains_key(&key) {
                return Err(KdlConversionError::DuplicateKey { key });
            }
            out.insert(key, kdl_node_to_loro_value(n)?);
        }
        return Ok(LoroValue::Map(out.into()));
    }

    // Rule 5: no args, no children block at all.
    Ok(LoroValue::Null)
}

/// Convert a `KdlValue` to a `LoroValue`.
fn kdl_value_to_loro_value(value: &KdlValue) -> Result<LoroValue, KdlConversionError> {
    match value {
        KdlValue::Null => Ok(LoroValue::Null),
        KdlValue::Bool(b) => Ok(LoroValue::Bool(*b)),
        KdlValue::Integer(i) => {
            // LoroValue::I64 is i64; KDL uses i128. Clamp with error on
            // overflow.
            let val = i64::try_from(*i).map_err(|_| {
                KdlConversionError::ParseError(format!("integer {i} out of i64 range"))
            })?;
            Ok(LoroValue::I64(val))
        }
        KdlValue::Float(f) => Ok(LoroValue::Double(*f)),
        KdlValue::String(s) => Ok(LoroValue::String(s.clone().into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Forward converter tests (LoroValue → KdlDocument)
    // -----------------------------------------------------------------------

    #[test]
    fn map_with_scalars() {
        let value = LoroValue::Map(
            vec![
                ("name".to_string(), LoroValue::String("alice".into())),
                ("age".to_string(), LoroValue::I64(30)),
            ]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let text = doc.to_string();
        // Should contain both key nodes.
        assert!(text.contains("name"));
        assert!(text.contains("alice"));
        assert!(text.contains("age"));
        assert!(text.contains("30"));
    }

    #[test]
    fn list_with_scalars() {
        let value = LoroValue::List(
            vec![
                LoroValue::String("first".into()),
                LoroValue::String("second".into()),
            ]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::List).unwrap();
        let text = doc.to_string();
        // All nodes should be named "-".
        for node in doc.nodes() {
            assert_eq!(node.name().value(), "-");
        }
        assert!(text.contains("first"));
        assert!(text.contains("second"));
    }

    #[test]
    fn map_with_nested_map() {
        let inner = LoroValue::Map(vec![("x".to_string(), LoroValue::I64(1))].into());
        let value = LoroValue::Map(vec![("nested".to_string(), inner)].into());
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let text = doc.to_string();
        assert!(text.contains("nested"));
        assert!(text.contains("x"));
    }

    #[test]
    fn list_with_nested_maps_uses_children() {
        let item =
            LoroValue::Map(vec![("key".to_string(), LoroValue::String("val".into()))].into());
        let value = LoroValue::List(vec![item].into());
        let doc = loro_value_to_kdl(&value, TopShape::List).unwrap();
        // The list item node should have children (not positional args).
        let node = &doc.nodes()[0];
        assert!(node.children().is_some());
    }

    #[test]
    fn scalar_list_collapses_to_args() {
        // A node whose value is a list of scalars should use positional args.
        let list =
            LoroValue::List(vec![LoroValue::I64(1), LoroValue::I64(2), LoroValue::I64(3)].into());
        let value = LoroValue::Map(vec![("nums".to_string(), list)].into());
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        // The "nums" node should have 3 positional entries, no children.
        let node = doc
            .nodes()
            .iter()
            .find(|n| n.name().value() == "nums")
            .unwrap();
        let positional: Vec<_> = node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .collect();
        assert_eq!(positional.len(), 3);
        assert!(node.children().is_none_or(|c| c.nodes().is_empty()));
    }

    #[test]
    fn list_with_newline_string_uses_children() {
        let list = LoroValue::List(
            vec![LoroValue::String("line1\nline2".into()), LoroValue::I64(42)].into(),
        );
        let value = LoroValue::Map(vec![("data".to_string(), list)].into());
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let node = doc
            .nodes()
            .iter()
            .find(|n| n.name().value() == "data")
            .unwrap();
        // Should use children form because of the newline.
        assert!(node.children().is_some());
    }

    #[test]
    fn null_value_in_map() {
        let value = LoroValue::Map(vec![("nothing".to_string(), LoroValue::Null)].into());
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let text = doc.to_string();
        assert!(text.contains("nothing"));
        assert!(text.contains("#null"));
    }

    #[test]
    fn bool_values() {
        let value = LoroValue::Map(
            vec![
                ("yes".to_string(), LoroValue::Bool(true)),
                ("no".to_string(), LoroValue::Bool(false)),
            ]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let text = doc.to_string();
        assert!(text.contains("#true"));
        assert!(text.contains("#false"));
    }

    #[test]
    fn binary_value_errors() {
        let value = LoroValue::Map(
            vec![(
                "data".to_string(),
                LoroValue::Binary(vec![0xDE, 0xAD].into()),
            )]
            .into(),
        );
        let result = loro_value_to_kdl(&value, TopShape::Map);
        assert!(matches!(
            result.unwrap_err(),
            KdlConversionError::UnsupportedBinary
        ));
    }

    #[test]
    fn shape_mismatch_map_value_with_list_shape() {
        let value = LoroValue::Map(vec![("k".to_string(), LoroValue::I64(1))].into());
        let result = loro_value_to_kdl(&value, TopShape::List);
        assert!(matches!(
            result.unwrap_err(),
            KdlConversionError::ShapeMismatch { .. }
        ));
    }

    #[test]
    fn shape_mismatch_list_value_with_map_shape() {
        let value = LoroValue::List(vec![LoroValue::I64(1)].into());
        let result = loro_value_to_kdl(&value, TopShape::Map);
        assert!(matches!(
            result.unwrap_err(),
            KdlConversionError::ShapeMismatch { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // Reverse converter tests (KdlDocument → LoroValue)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_map_with_scalars() {
        let doc = parse_kdl("name \"alice\"\nage 30\n").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &value {
            LoroValue::Map(m) => {
                assert_eq!(m.get("name"), Some(&LoroValue::String("alice".into())));
                assert_eq!(m.get("age"), Some(&LoroValue::I64(30)));
            }
            _ => panic!("expected map, got {value:?}"),
        }
    }

    #[test]
    fn parse_list_with_scalars() {
        let doc = parse_kdl("- \"first\"\n- \"second\"\n").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::List).unwrap();
        match &value {
            LoroValue::List(l) => {
                assert_eq!(l.len(), 2);
                assert_eq!(l[0], LoroValue::String("first".into()));
                assert_eq!(l[1], LoroValue::String("second".into()));
            }
            _ => panic!("expected list, got {value:?}"),
        }
    }

    #[test]
    fn parse_map_rejects_dash_key() {
        let doc = parse_kdl("- \"item\"\n").unwrap();
        let result = kdl_to_loro_value(&doc, TopShape::Map);
        assert!(matches!(
            result.unwrap_err(),
            KdlConversionError::ShapeMismatch { .. }
        ));
    }

    #[test]
    fn parse_list_rejects_non_dash_names() {
        let doc = parse_kdl("foo \"bar\"\n").unwrap();
        let result = kdl_to_loro_value(&doc, TopShape::List);
        assert!(matches!(
            result.unwrap_err(),
            KdlConversionError::ShapeMismatch { .. }
        ));
    }

    #[test]
    fn parse_map_rejects_duplicate_keys() {
        let doc = parse_kdl("foo 1\nfoo 2\n").unwrap();
        let result = kdl_to_loro_value(&doc, TopShape::Map);
        assert!(matches!(
            result.unwrap_err(),
            KdlConversionError::DuplicateKey { .. }
        ));
    }

    #[test]
    fn parse_nested_map() {
        let doc = parse_kdl("outer {\n    inner 42\n}\n").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &value {
            LoroValue::Map(m) => {
                let inner = m.get("outer").unwrap();
                match inner {
                    LoroValue::Map(im) => {
                        assert_eq!(im.get("inner"), Some(&LoroValue::I64(42)));
                    }
                    _ => panic!("expected nested map"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn parse_multi_arg_node_as_list() {
        let doc = parse_kdl("tags \"a\" \"b\" \"c\"\n").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &value {
            LoroValue::Map(m) => {
                let tags = m.get("tags").unwrap();
                match tags {
                    LoroValue::List(l) => {
                        assert_eq!(l.len(), 3);
                        assert_eq!(l[0], LoroValue::String("a".into()));
                    }
                    _ => panic!("expected list for multi-arg node"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn empty_document_as_map() {
        let doc = parse_kdl("").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &value {
            LoroValue::Map(m) => assert!(m.is_empty()),
            _ => panic!("expected empty map"),
        }
    }

    #[test]
    fn empty_document_as_list() {
        let doc = parse_kdl("").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::List).unwrap();
        match &value {
            LoroValue::List(l) => assert!(l.is_empty()),
            _ => panic!("expected empty list"),
        }
    }

    // -----------------------------------------------------------------------
    // Edge cases (AC6.7, AC6.8)
    // -----------------------------------------------------------------------

    #[test]
    fn i64_boundary_values() {
        let value = LoroValue::Map(
            vec![
                ("max".to_string(), LoroValue::I64(i64::MAX)),
                ("min".to_string(), LoroValue::I64(i64::MIN)),
            ]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        // We can't compare maps directly because FxHashMap iteration order
        // is nondeterministic. Compare individual fields.
        match &rt {
            LoroValue::Map(m) => {
                assert_eq!(m.get("max"), Some(&LoroValue::I64(i64::MAX)));
                assert_eq!(m.get("min"), Some(&LoroValue::I64(i64::MIN)));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn float_special_values() {
        let value = LoroValue::Map(
            vec![
                ("inf".to_string(), LoroValue::Double(f64::INFINITY)),
                ("neg_inf".to_string(), LoroValue::Double(f64::NEG_INFINITY)),
                ("nan".to_string(), LoroValue::Double(f64::NAN)),
            ]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &rt {
            LoroValue::Map(m) => {
                assert_eq!(m.get("inf"), Some(&LoroValue::Double(f64::INFINITY)));
                assert_eq!(
                    m.get("neg_inf"),
                    Some(&LoroValue::Double(f64::NEG_INFINITY))
                );
                // NaN != NaN, so check with is_nan().
                match m.get("nan") {
                    Some(LoroValue::Double(d)) => assert!(d.is_nan()),
                    other => panic!("expected NaN, got {other:?}"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn strings_with_quotes_and_backslashes() {
        let value = LoroValue::Map(
            vec![(
                "quoted".to_string(),
                LoroValue::String("he said \"hello\" and \\n".into()),
            )]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &rt {
            LoroValue::Map(m) => {
                assert_eq!(
                    m.get("quoted"),
                    Some(&LoroValue::String("he said \"hello\" and \\n".into()))
                );
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn strings_with_newlines_and_unicode() {
        let value = LoroValue::Map(
            vec![(
                "multi".to_string(),
                LoroValue::String("line1\nline2\n日本語".into()),
            )]
            .into(),
        );
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &rt {
            LoroValue::Map(m) => {
                assert_eq!(
                    m.get("multi"),
                    Some(&LoroValue::String("line1\nline2\n日本語".into()))
                );
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn null_round_trip() {
        let value = LoroValue::Map(vec![("nope".to_string(), LoroValue::Null)].into());
        let doc = loro_value_to_kdl(&value, TopShape::Map).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &rt {
            LoroValue::Map(m) => {
                assert_eq!(m.get("nope"), Some(&LoroValue::Null));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn node_with_no_args_no_children_is_null() {
        let doc = parse_kdl("empty\n").unwrap();
        let value = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        match &value {
            LoroValue::Map(m) => {
                assert_eq!(m.get("empty"), Some(&LoroValue::Null));
            }
            _ => panic!("expected map"),
        }
    }

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    /// Helper: round-trip a LoroValue through KDL and compare field by field.
    /// Maps use field-by-field comparison because FxHashMap iteration order is
    /// nondeterministic.
    fn assert_round_trip_map(original: &LoroValue) {
        let doc = loro_value_to_kdl(original, TopShape::Map).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::Map).unwrap();
        assert_loro_values_equal(original, &rt);
    }

    fn assert_round_trip_list(original: &LoroValue) {
        let doc = loro_value_to_kdl(original, TopShape::List).unwrap();
        let rt = kdl_to_loro_value(&doc, TopShape::List).unwrap();
        assert_loro_values_equal(original, &rt);
    }

    /// Deep equality for LoroValue that handles NaN correctly and is
    /// order-independent for maps.
    fn assert_loro_values_equal(a: &LoroValue, b: &LoroValue) {
        match (a, b) {
            (LoroValue::Null, LoroValue::Null) => {}
            (LoroValue::Bool(a), LoroValue::Bool(b)) => assert_eq!(a, b),
            (LoroValue::I64(a), LoroValue::I64(b)) => assert_eq!(a, b),
            (LoroValue::Double(a), LoroValue::Double(b)) => {
                if a.is_nan() {
                    assert!(b.is_nan(), "expected NaN, got {b}");
                } else {
                    assert_eq!(a, b);
                }
            }
            (LoroValue::String(a), LoroValue::String(b)) => {
                assert_eq!(a.as_str(), b.as_str());
            }
            (LoroValue::List(a), LoroValue::List(b)) => {
                assert_eq!(a.len(), b.len(), "list lengths differ");
                for (i, (ai, bi)) in a.iter().zip(b.iter()).enumerate() {
                    assert_loro_values_equal(ai, bi);
                    let _ = i; // suppress unused warning.
                }
            }
            (LoroValue::Map(a), LoroValue::Map(b)) => {
                assert_eq!(a.len(), b.len(), "map sizes differ");
                for (k, v) in a.iter() {
                    let bv = b
                        .get(k)
                        .unwrap_or_else(|| panic!("key {k:?} missing in round-tripped map"));
                    assert_loro_values_equal(v, bv);
                }
            }
            _ => panic!("LoroValue shape mismatch:\n  left:  {a:?}\n  right: {b:?}"),
        }
    }

    #[test]
    fn round_trip_map_scalars() {
        let value = LoroValue::Map(
            vec![
                ("s".to_string(), LoroValue::String("hello".into())),
                ("i".to_string(), LoroValue::I64(42)),
                ("d".to_string(), LoroValue::Double(2.72)),
                ("b".to_string(), LoroValue::Bool(true)),
                ("n".to_string(), LoroValue::Null),
            ]
            .into(),
        );
        assert_round_trip_map(&value);
    }

    #[test]
    fn round_trip_list_scalars() {
        let value = LoroValue::List(
            vec![
                LoroValue::String("a".into()),
                LoroValue::I64(1),
                LoroValue::Double(2.5),
                LoroValue::Bool(false),
                LoroValue::Null,
            ]
            .into(),
        );
        assert_round_trip_list(&value);
    }

    #[test]
    fn round_trip_nested_map_in_map() {
        let inner = LoroValue::Map(
            vec![
                ("x".to_string(), LoroValue::I64(1)),
                ("y".to_string(), LoroValue::I64(2)),
            ]
            .into(),
        );
        let value = LoroValue::Map(vec![("point".to_string(), inner)].into());
        assert_round_trip_map(&value);
    }

    #[test]
    fn round_trip_list_of_maps() {
        let item1 =
            LoroValue::Map(vec![("name".to_string(), LoroValue::String("alice".into()))].into());
        let item2 =
            LoroValue::Map(vec![("name".to_string(), LoroValue::String("bob".into()))].into());
        let value = LoroValue::List(vec![item1, item2].into());
        assert_round_trip_list(&value);
    }

    #[test]
    fn round_trip_empty_map() {
        let value = LoroValue::Map(HashMap::new().into());
        assert_round_trip_map(&value);
    }

    #[test]
    fn round_trip_empty_list() {
        let value = LoroValue::List(vec![].into());
        assert_round_trip_list(&value);
    }

    #[test]
    fn round_trip_deeply_nested() {
        let deep =
            LoroValue::Map(vec![("leaf".to_string(), LoroValue::String("deep".into()))].into());
        let mid = LoroValue::Map(vec![("child".to_string(), deep)].into());
        let value = LoroValue::Map(vec![("root".to_string(), mid)].into());
        assert_round_trip_map(&value);
    }
}
