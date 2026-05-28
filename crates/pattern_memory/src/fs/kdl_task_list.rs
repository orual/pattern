// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! KDL serialization for `BlockSchema::TaskList` blocks.
//!
//! Forward: `LoroValue::Map { schema: "task-list", items: [...], ... }` → KDL.
//! Reverse: KDL → `LoroValue::Map` with `schema: "task-list"` discriminator.
//!
//! The KDL shape is:
//! ```kdl
//! task-list default_status="pending" display_limit=20 {
//!     item id="..." status="pending" owner="@agent" {
//!         subject "Write the spec"
//!         description "Full markdown body..."
//!         active_form "writing the spec"
//!         blocks {
//!             (block)"handle"
//!             (block)"handle#item_id"
//!         }
//!         comments {
//!             entry author="@r" timestamp="2026-01-01T00:00:00Z" {
//!                 text "Comment body"
//!             }
//!         }
//!         metadata {
//!             priority "high"
//!         }
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::str::FromStr;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use loro::LoroValue;
use pattern_core::types::memory_types::TaskEdgeRef;

use super::kdl::{KdlConversionError, kdl_string_entry};

/// Convert a task-list `LoroValue::Map` to a `KdlDocument`.
///
/// The input map must have `schema: "task-list"` and an `items` list.
pub(super) fn task_list_to_kdl(value: &LoroValue) -> Result<KdlDocument, KdlConversionError> {
    let map = match value {
        LoroValue::Map(m) => m,
        other => {
            return Err(KdlConversionError::ShapeMismatch {
                expected: super::kdl::TopShape::TaskList,
                actual: format!("{other:?}"),
            });
        }
    };

    let mut root_node = KdlNode::new("task-list");

    // Properties.
    if let Some(LoroValue::String(s)) = map.get("default_status") {
        let mut entry = kdl_string_entry(s.as_str())?;
        entry.set_name(Some("default_status"));
        root_node.push(entry);
    }
    if let Some(LoroValue::String(s)) = map.get("default_owner") {
        let mut entry = kdl_string_entry(s.as_str())?;
        entry.set_name(Some("default_owner"));
        root_node.push(entry);
    }
    if let Some(LoroValue::I64(n)) = map.get("display_limit") {
        let mut entry = KdlEntry::new(i128::from(*n));
        entry.set_name(Some("display_limit"));
        root_node.push(entry);
    }

    // Items children.
    let mut children = KdlDocument::new();
    if let Some(LoroValue::List(items)) = map.get("items") {
        for item in items.iter() {
            children.nodes_mut().push(task_item_to_kdl_node(item)?);
        }
    }
    root_node.set_children(children);

    let mut doc = KdlDocument::new();
    doc.nodes_mut().push(root_node);
    // Note: doc.autoformat() is intentionally NOT called here.
    // See the equivalent comment in kdl.rs: autoformat() strips
    // double-quote format metadata from strings that look like KDL
    // number literals (e.g. "+.0"), breaking the round-trip.
    Ok(doc)
}

/// Convert a KDL document (with a single `task-list` root node) back to a
/// `LoroValue::Map` with the `schema: "task-list"` discriminator.
pub(super) fn kdl_to_task_list(doc: &KdlDocument) -> Result<LoroValue, KdlConversionError> {
    let nodes = doc.nodes();
    let root = nodes
        .iter()
        .find(|n| n.name().value() == "task-list")
        .ok_or_else(|| KdlConversionError::ShapeMismatch {
            expected: super::kdl::TopShape::TaskList,
            actual: "no `task-list` root node".into(),
        })?;

    let mut out: HashMap<String, LoroValue> = HashMap::new();
    out.insert("schema".into(), LoroValue::String("task-list".into()));

    // Properties.
    for entry in root.entries() {
        if let Some(name) = entry.name() {
            match name.value() {
                "default_status" => {
                    if let KdlValue::String(s) = entry.value() {
                        out.insert("default_status".into(), LoroValue::String(s.clone().into()));
                    }
                }
                "default_owner" => {
                    if let KdlValue::String(s) = entry.value() {
                        out.insert("default_owner".into(), LoroValue::String(s.clone().into()));
                    }
                }
                "display_limit" => {
                    if let KdlValue::Integer(n) = entry.value() {
                        out.insert("display_limit".into(), LoroValue::I64(*n as i64));
                    }
                }
                _ => {}
            }
        }
    }

    // Items.
    let mut items = Vec::new();
    if let Some(children) = root.children() {
        for node in children.nodes() {
            if node.name().value() == "item" {
                items.push(kdl_node_to_task_item(node)?);
            }
        }
    }
    out.insert("items".into(), LoroValue::List(items.into()));

    Ok(LoroValue::Map(out.into()))
}

// ---------------------------------------------------------------------------
// Forward helpers
// ---------------------------------------------------------------------------

/// Null-normalization convention for optional fields:
///
/// `metadata`, `active_form`, and `owner` may arrive as `LoroValue::Null`
/// (e.g., when constructed from JSON via `json_to_loro` and the JSON value
/// is `null`). The forward path pattern-matches on `LoroValue::String` so
/// `Null` variants are simply skipped — nothing is emitted. The reverse path
/// (`kdl_node_to_task_item`) inserts `LoroValue::Map({})` for missing
/// metadata and omits `owner`/`active_form` entirely (they remain absent from
/// the output map). This means `Null` and absent are treated identically: a
/// `Null` normalises to absent on the first round-trip. Subsequent round-trips
/// are stable (idempotent after the first pass). This is intentional: agents
/// should use the explicit types (`String`, `Map`) rather than `Null`.
fn task_item_to_kdl_node(value: &LoroValue) -> Result<KdlNode, KdlConversionError> {
    let map = match value {
        LoroValue::Map(m) => m,
        other => {
            return Err(KdlConversionError::UnsupportedVariant(format!(
                "expected Map for task item, got {other:?}"
            )));
        }
    };

    let mut node = KdlNode::new("item");

    // Properties on the node itself.
    push_str_prop(&mut node, "id", map)?;
    push_str_prop(&mut node, "status", map)?;
    push_str_prop(&mut node, "owner", map)?;

    // Children.
    let mut children = KdlDocument::new();

    // subject.
    if let Some(LoroValue::String(s)) = map.get("subject") {
        let mut n = KdlNode::new("subject");
        n.push(kdl_string_entry(s.as_str())?);
        children.nodes_mut().push(n);
    }

    // description — convention: empty string equals absent. We omit the node
    // when the value is empty, and the reverse path (`kdl_node_to_task_item`)
    // defaults to `LoroValue::String("")` when no description node is found.
    // This means an empty description survives round-trips as "" → omit → ""
    // without loss. `TaskItem::active_form` is `Option<String>` and uses a
    // separate absent/present distinction; description is always `String` and
    // uses the empty-equals-absent convention documented here.
    if let Some(LoroValue::String(s)) = map.get("description")
        && !s.is_empty()
    {
        let mut n = KdlNode::new("description");
        n.push(kdl_string_entry(s.as_str())?);
        children.nodes_mut().push(n);
    }

    // active_form.
    if let Some(LoroValue::String(s)) = map.get("active_form") {
        let mut n = KdlNode::new("active_form");
        n.push(kdl_string_entry(s.as_str())?);
        children.nodes_mut().push(n);
    }

    // blocks.
    if let Some(LoroValue::List(blocks)) = map.get("blocks")
        && !blocks.is_empty()
    {
        let mut blocks_node = KdlNode::new("blocks");
        let mut blocks_children = KdlDocument::new();
        for b in blocks.iter() {
            if let LoroValue::Map(m) = b {
                let handle = m
                    .get("block")
                    .and_then(|v| match v {
                        LoroValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let item_id = m.get("task_item").and_then(|v| match v {
                    LoroValue::String(s) => Some(s.to_string()),
                    _ => None,
                });
                let display = match item_id {
                    Some(id) => format!("{handle}#{id}"),
                    None => handle,
                };
                let mut entry = kdl_string_entry(display.as_str())?;
                entry.set_ty("block");
                // Each typed entry is a child node named "-".
                let mut entry_node = KdlNode::new("-");
                entry_node.push(entry);
                blocks_children.nodes_mut().push(entry_node);
            }
        }
        blocks_node.set_children(blocks_children);
        children.nodes_mut().push(blocks_node);
    }

    // comments.
    if let Some(LoroValue::List(comments)) = map.get("comments")
        && !comments.is_empty()
    {
        let mut comments_node = KdlNode::new("comments");
        let mut comments_children = KdlDocument::new();
        for c in comments.iter() {
            if let LoroValue::Map(cm) = c {
                let mut entry_node = KdlNode::new("entry");
                push_str_prop(&mut entry_node, "author", cm)?;
                push_str_prop(&mut entry_node, "timestamp", cm)?;
                // text child.
                if let Some(LoroValue::String(t)) = cm.get("text") {
                    let mut text_node = KdlNode::new("text");
                    text_node.push(kdl_string_entry(t.as_str())?);
                    let mut inner = KdlDocument::new();
                    inner.nodes_mut().push(text_node);
                    entry_node.set_children(inner);
                }
                comments_children.nodes_mut().push(entry_node);
            }
        }
        comments_node.set_children(comments_children);
        children.nodes_mut().push(comments_node);
    }

    // metadata — reuse generic Map converter.
    if let Some(LoroValue::Map(meta)) = map.get("metadata")
        && !meta.is_empty()
    {
        let mut meta_node = KdlNode::new("metadata");
        let mut meta_children = KdlDocument::new();
        let mut keys: Vec<&String> = meta.keys().collect();
        keys.sort();
        for k in keys {
            meta_children
                .nodes_mut()
                .push(super::kdl::loro_value_to_kdl_node(k, meta.get(k).unwrap())?);
        }
        meta_node.set_children(meta_children);
        children.nodes_mut().push(meta_node);
    }

    // created_at / updated_at.
    push_str_child(&mut children, "created_at", map)?;
    push_str_child(&mut children, "updated_at", map)?;

    node.set_children(children);
    Ok(node)
}

fn push_str_prop(
    node: &mut KdlNode,
    key: &str,
    map: &loro::LoroMapValue,
) -> Result<(), KdlConversionError> {
    if let Some(LoroValue::String(s)) = map.get(key) {
        let mut entry = kdl_string_entry(s.as_str())?;
        entry.set_name(Some(key));
        node.push(entry);
    }
    Ok(())
}

fn push_str_child(
    children: &mut KdlDocument,
    key: &str,
    map: &loro::LoroMapValue,
) -> Result<(), KdlConversionError> {
    if let Some(LoroValue::String(s)) = map.get(key) {
        let mut n = KdlNode::new(key);
        n.push(kdl_string_entry(s.as_str())?);
        children.nodes_mut().push(n);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reverse helpers
// ---------------------------------------------------------------------------

fn kdl_node_to_task_item(node: &KdlNode) -> Result<LoroValue, KdlConversionError> {
    let mut out: HashMap<String, LoroValue> = HashMap::new();

    // Properties.
    for entry in node.entries() {
        if let Some(name) = entry.name() {
            let key = name.value();
            match entry.value() {
                KdlValue::String(s) => {
                    out.insert(key.to_owned(), LoroValue::String(s.clone().into()));
                }
                KdlValue::Integer(n) => {
                    out.insert(key.to_owned(), LoroValue::I64(*n as i64));
                }
                _ => {}
            }
        }
    }

    // Children.
    if let Some(children) = node.children() {
        for child in children.nodes() {
            let name = child.name().value();
            match name {
                "subject" | "description" | "active_form" | "created_at" | "updated_at" => {
                    if let Some(entry) = child.entries().first()
                        && let KdlValue::String(s) = entry.value()
                    {
                        out.insert(name.to_owned(), LoroValue::String(s.clone().into()));
                    }
                }
                "blocks" => {
                    let mut blocks = Vec::new();
                    if let Some(blocks_children) = child.children() {
                        for block_node in blocks_children.nodes() {
                            // Each block_node is a "-" node with a typed entry.
                            if let Some(entry) = block_node.entries().first() {
                                let has_block_annotation =
                                    entry.ty().map(|t| t.value() == "block").unwrap_or(false);
                                if !has_block_annotation {
                                    return Err(KdlConversionError::MissingBlockAnnotation {
                                        span: entry.span(),
                                    });
                                }
                                if let KdlValue::String(s) = entry.value() {
                                    let edge_ref = TaskEdgeRef::from_str(s).map_err(|e| {
                                        KdlConversionError::TaskEdgeRef {
                                            span: entry.span(),
                                            source: e,
                                        }
                                    })?;
                                    let mut edge_map: HashMap<String, LoroValue> = HashMap::new();
                                    edge_map.insert(
                                        "block".into(),
                                        LoroValue::String(edge_ref.block.as_str().into()),
                                    );
                                    if let Some(item_id) = edge_ref.task_item {
                                        edge_map.insert(
                                            "task_item".into(),
                                            LoroValue::String(item_id.as_str().into()),
                                        );
                                    }
                                    blocks.push(LoroValue::Map(edge_map.into()));
                                }
                            }
                        }
                    }
                    out.insert("blocks".into(), LoroValue::List(blocks.into()));
                }
                "comments" => {
                    let mut comments = Vec::new();
                    if let Some(comments_children) = child.children() {
                        for entry_node in comments_children.nodes() {
                            if entry_node.name().value() == "entry" {
                                let mut cm: HashMap<String, LoroValue> = HashMap::new();
                                for e in entry_node.entries() {
                                    if let Some(n) = e.name()
                                        && let KdlValue::String(s) = e.value()
                                    {
                                        cm.insert(
                                            n.value().to_owned(),
                                            LoroValue::String(s.clone().into()),
                                        );
                                    }
                                }
                                if let Some(inner) = entry_node.children() {
                                    for text_node in inner.nodes() {
                                        if text_node.name().value() == "text"
                                            && let Some(e) = text_node.entries().first()
                                            && let KdlValue::String(s) = e.value()
                                        {
                                            cm.insert(
                                                "text".into(),
                                                LoroValue::String(s.clone().into()),
                                            );
                                        }
                                    }
                                }
                                comments.push(LoroValue::Map(cm.into()));
                            }
                        }
                    }
                    out.insert("comments".into(), LoroValue::List(comments.into()));
                }
                "metadata" => {
                    // Reuse generic KDL-to-LoroValue Map converter.
                    if let Some(meta_children) = child.children() {
                        let meta_value = super::kdl::kdl_to_loro_value(
                            meta_children,
                            super::kdl::TopShape::Map,
                        )?;
                        out.insert("metadata".into(), meta_value);
                    } else {
                        out.insert("metadata".into(), LoroValue::Map(HashMap::new().into()));
                    }
                }
                _ => {
                    // Unknown child — skip silently for forward compat.
                }
            }
        }
    }

    // Default missing optional fields so round-trips are stable.
    out.entry("blocks".into())
        .or_insert_with(|| LoroValue::List(vec![].into()));
    out.entry("comments".into())
        .or_insert_with(|| LoroValue::List(vec![].into()));
    out.entry("description".into())
        .or_insert_with(|| LoroValue::String("".into()));
    out.entry("metadata".into())
        .or_insert_with(|| LoroValue::Map(HashMap::new().into()));

    Ok(LoroValue::Map(out.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal task-list LoroValue with the discriminator.
    fn make_task_list(items: Vec<LoroValue>) -> LoroValue {
        let mut map: HashMap<String, LoroValue> = HashMap::new();
        map.insert("schema".into(), LoroValue::String("task-list".into()));
        map.insert("items".into(), LoroValue::List(items.into()));
        LoroValue::Map(map.into())
    }

    fn make_item_full(
        id: &str,
        subject: &str,
        status: &str,
        blocks: Vec<LoroValue>,
        metadata: HashMap<String, LoroValue>,
        comments: Vec<LoroValue>,
    ) -> LoroValue {
        let mut m: HashMap<String, LoroValue> = HashMap::new();
        m.insert("id".into(), LoroValue::String(id.into()));
        m.insert("subject".into(), LoroValue::String(subject.into()));
        m.insert("description".into(), LoroValue::String("".into()));
        m.insert("status".into(), LoroValue::String(status.into()));
        m.insert("blocks".into(), LoroValue::List(blocks.into()));
        m.insert("metadata".into(), LoroValue::Map(metadata.into()));
        m.insert("comments".into(), LoroValue::List(comments.into()));
        m.insert(
            "created_at".into(),
            LoroValue::String("2026-01-01T00:00:00Z".into()),
        );
        m.insert(
            "updated_at".into(),
            LoroValue::String("2026-01-01T00:00:00Z".into()),
        );
        LoroValue::Map(m.into())
    }

    #[allow(dead_code)]
    fn make_item(id: &str, subject: &str, status: &str) -> LoroValue {
        make_item_full(id, subject, status, vec![], HashMap::new(), vec![])
    }

    fn make_block_edge(handle: &str, item_id: Option<&str>) -> LoroValue {
        let mut m: HashMap<String, LoroValue> = HashMap::new();
        m.insert("block".into(), LoroValue::String(handle.into()));
        if let Some(id) = item_id {
            m.insert("task_item".into(), LoroValue::String(id.into()));
        }
        LoroValue::Map(m.into())
    }

    /// Round-trip helper: LoroValue → KDL string → parse → LoroValue.
    fn round_trip(value: &LoroValue) -> LoroValue {
        let kdl_doc = task_list_to_kdl(value).expect("forward failed");
        let kdl_str = kdl_doc.to_string();
        let parsed = super::super::kdl::parse_kdl(&kdl_str).expect("KDL parse failed");
        kdl_to_task_list(&parsed).expect("reverse failed")
    }

    /// Compare two LoroValues by serializing to sorted JSON.
    fn assert_loro_eq(a: &LoroValue, b: &LoroValue) {
        let ja = super::super::kdl::loro_value_to_json(a).unwrap();
        let jb = super::super::kdl::loro_value_to_json(b).unwrap();
        assert_eq!(ja, jb, "LoroValue mismatch:\nleft:  {ja}\nright: {jb}");
    }

    #[test]
    fn empty_task_list_round_trips() {
        let input = make_task_list(vec![]);
        let output = round_trip(&input);
        assert_loro_eq(&input, &output);
    }

    #[test]
    fn self_edge_round_trips() {
        let item = make_item_full(
            "abc",
            "self-ref",
            "pending",
            vec![make_block_edge("my-block", Some("abc"))],
            HashMap::new(),
            vec![],
        );
        let input = make_task_list(vec![item]);
        let output = round_trip(&input);
        assert_loro_eq(&input, &output);
    }

    #[test]
    fn five_items_with_edges_round_trip() {
        let items: Vec<LoroValue> = (0..5)
            .map(|i| {
                let blocks = if i == 2 || i == 3 {
                    vec![make_block_edge("target", Some("id4"))]
                } else {
                    vec![]
                };
                make_item_full(
                    &format!("id{i}"),
                    &format!("task {i}"),
                    "pending",
                    blocks,
                    HashMap::new(),
                    vec![],
                )
            })
            .collect();
        let input = make_task_list(items);
        let output = round_trip(&input);
        assert_loro_eq(&input, &output);
    }

    #[test]
    fn nested_metadata_round_trips() {
        let mut meta: HashMap<String, LoroValue> = HashMap::new();
        meta.insert("priority".into(), LoroValue::String("high".into()));
        meta.insert("estimated_hours".into(), LoroValue::Double(2.5));
        let item = make_item_full("m1", "with meta", "in-progress", vec![], meta, vec![]);
        let input = make_task_list(vec![item]);
        let output = round_trip(&input);
        assert_loro_eq(&input, &output);
    }

    #[test]
    fn comments_round_trip() {
        let mut comment: HashMap<String, LoroValue> = HashMap::new();
        comment.insert("author".into(), LoroValue::String("@r".into()));
        comment.insert(
            "timestamp".into(),
            LoroValue::String("2026-04-01T12:00:00Z".into()),
        );
        comment.insert(
            "text".into(),
            LoroValue::String("This needs review.".into()),
        );
        let item = make_item_full(
            "c1",
            "commented",
            "blocked",
            vec![],
            HashMap::new(),
            vec![LoroValue::Map(comment.into())],
        );
        let input = make_task_list(vec![item]);
        let output = round_trip(&input);
        assert_loro_eq(&input, &output);
    }

    // Error path tests (Task 11 scope but colocated here per plan).

    /// Check that a `miette::Report` wrapping the error renders with source-span
    /// gutter characters when the KDL source is attached. Also verifies that the
    /// expected label text appears in the rendered output.
    ///
    /// This confirms that `KdlConversionError` implements `miette::Diagnostic`
    /// and that the `#[label]` span and text are wired correctly.
    fn assert_miette_renders_source_span(
        err: KdlConversionError,
        kdl_str: &str,
        expected_label: &str,
    ) {
        use miette::{GraphicalReportHandler, GraphicalTheme, NamedSource};
        let report = miette::Report::new(err)
            .with_source_code(NamedSource::new("test.kdl", kdl_str.to_owned()));
        // Use GraphicalReportHandler directly rather than `{:?}` so we get the
        // rich formatted output without requiring a global miette::set_hook call.
        // GraphicalTheme::none() strips ANSI colour codes so the assertion is
        // purely structural (gutter characters, not colour escapes).
        let handler = GraphicalReportHandler::new_themed(GraphicalTheme::none());
        let mut rendered = String::new();
        handler
            .render_report(&mut rendered, report.as_ref())
            .expect("miette render_report failed");
        // GraphicalReportHandler emits `,-[file:line:col]` source-location
        // markers and line-number gutter lines (e.g., `4 |`) only when a
        // `SourceCode` is attached and a `#[label]` span is present.
        // Asserting on `,-[` is conservative: the plain error message alone
        // would never produce this codespan framing sequence.
        assert!(
            rendered.contains(",-["),
            "expected miette source-location marker ',-[' in rendered report, got:\n{rendered}"
        );
        // The label text from #[label("...")] must appear in the rendered output.
        assert!(
            rendered.contains(expected_label),
            "expected label text {:?} in rendered report, got:\n{rendered}",
            expected_label,
        );
        // A line-number gutter marker (`| ` or `│`) must also appear,
        // confirming a real source location is being rendered.
        assert!(
            rendered.contains("| ") || rendered.contains("│"),
            "expected line-number gutter marker in rendered report, got:\n{rendered}"
        );
    }

    #[test]
    fn empty_block_ref_returns_task_edge_ref_error() {
        let kdl_str = r#"task-list {
            item id="x" status="pending" {
                subject "test"
                blocks {
                    - (block)""
                }
            }
        }"#;
        let doc = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err = kdl_to_task_list(&doc).unwrap_err();
        assert!(
            matches!(err, KdlConversionError::TaskEdgeRef { .. }),
            "expected TaskEdgeRef error, got: {err:?}"
        );
        // Rebuild the error to assert miette rendering (unwrap_err() consumed it).
        let doc2 = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err2 = kdl_to_task_list(&doc2).unwrap_err();
        assert_miette_renders_source_span(err2, kdl_str, "invalid block reference here");
    }

    #[test]
    fn missing_block_annotation_returns_error() {
        let kdl_str = r#"task-list {
            item id="x" status="pending" {
                subject "test"
                blocks {
                    - "handle-without-annotation"
                }
            }
        }"#;
        let doc = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err = kdl_to_task_list(&doc).unwrap_err();
        assert!(
            matches!(err, KdlConversionError::MissingBlockAnnotation { .. }),
            "expected MissingBlockAnnotation error, got: {err:?}"
        );
        let doc2 = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err2 = kdl_to_task_list(&doc2).unwrap_err();
        assert_miette_renders_source_span(err2, kdl_str, "expected (block) type annotation here");
    }

    #[test]
    fn hash_no_handle_returns_empty_handle_error() {
        let kdl_str = r##"task-list {
            item id="x" status="pending" {
                subject "test"
                blocks {
                    - (block)"#no-handle"
                }
            }
        }"##;
        let doc = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err = kdl_to_task_list(&doc).unwrap_err();
        match err {
            KdlConversionError::TaskEdgeRef { source, .. } => {
                assert!(
                    matches!(
                        source,
                        pattern_core::types::memory_types::TaskEdgeRefParseError::EmptyHandle
                    ),
                    "expected EmptyHandle, got: {source:?}"
                );
            }
            other => panic!("expected TaskEdgeRef error, got: {other:?}"),
        }
        let doc2 = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err2 = kdl_to_task_list(&doc2).unwrap_err();
        assert_miette_renders_source_span(err2, kdl_str, "invalid block reference here");
    }

    #[test]
    fn handle_hash_no_item_returns_empty_item_id_error() {
        let kdl_str = r#"task-list {
            item id="x" status="pending" {
                subject "test"
                blocks {
                    - (block)"handle#"
                }
            }
        }"#;
        let doc = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err = kdl_to_task_list(&doc).unwrap_err();
        match err {
            KdlConversionError::TaskEdgeRef { source, .. } => {
                assert!(
                    matches!(
                        source,
                        pattern_core::types::memory_types::TaskEdgeRefParseError::EmptyItemId
                    ),
                    "expected EmptyItemId, got: {source:?}"
                );
            }
            other => panic!("expected TaskEdgeRef error, got: {other:?}"),
        }
        let doc2 = super::super::kdl::parse_kdl(kdl_str).unwrap();
        let err2 = kdl_to_task_list(&doc2).unwrap_err();
        assert_miette_renders_source_span(err2, kdl_str, "invalid block reference here");
    }
}
