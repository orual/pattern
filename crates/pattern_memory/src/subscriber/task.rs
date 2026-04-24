//! TaskList block reconciler for the sync subscriber.
//!
//! Reads the LoroDoc's `items` movable list, diffs against the current
//! `tasks` + `task_edges` SQL rows, and applies upserts/deletes in a
//! single transaction. Called from the worker loop when the block schema
//! is `BlockSchema::TaskList`.

use std::collections::HashSet;

use loro::LoroValue;
use rusqlite::Transaction;

use pattern_db::queries::{
    delete_task_edges_for_item, delete_task_row, upsert_task_edges, upsert_task_row,
};
use pattern_db::queries::task_row::{TaskRow, TaskStatus};

// region: error

/// Errors during TaskList reconciliation.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[non_exhaustive]
pub enum ReconcileError {
    /// A task item in the LoroDoc has an unexpected shape (missing map, wrong type).
    #[error("invalid item shape at index {index}: {detail}")]
    InvalidItemShape {
        /// Position in the movable list.
        index: usize,
        /// Human-readable explanation.
        detail: String,
    },

    /// A required field is missing from a task item map.
    #[error("missing required field '{field}' at index {index}")]
    MissingRequiredField {
        /// Position in the movable list.
        index: usize,
        /// Field name.
        field: &'static str,
    },

    /// A field has an unexpected type.
    #[error("wrong type for field '{field}' at index {index}: {detail}")]
    WrongFieldType {
        /// Position in the movable list.
        index: usize,
        /// Field name.
        field: &'static str,
        /// Human-readable explanation.
        detail: String,
    },

    /// An underlying SQLite operation failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

// endregion: error

// region: extracted item

/// Intermediate representation extracted from a LoroValue::Map for one task item.
struct ExtractedItem {
    id: String,
    subject: String,
    description: Option<String>,
    status: TaskStatus,
    owner: Option<String>,
    comments_json: String,
    /// Outgoing edges as `(target_block, Option<target_item>)`.
    edges: Vec<(String, Option<String>)>,
}

// endregion: extracted item

// region: extraction helpers

/// Extract a string field from a LoroValue map.
fn get_str(map: &loro::LoroMapValue, key: &'static str) -> Option<String> {
    match map.get(key) {
        Some(LoroValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Extract a required string field, returning a ReconcileError on failure.
fn require_str(
    map: &loro::LoroMapValue,
    key: &'static str,
    index: usize,
) -> Result<String, ReconcileError> {
    get_str(map, key).ok_or(ReconcileError::MissingRequiredField { index, field: key })
}

/// Parse a TaskStatus from a LoroValue map's `status` field.
fn extract_status(
    map: &loro::LoroMapValue,
    index: usize,
) -> Result<TaskStatus, ReconcileError> {
    let s = require_str(map, "status", index)?;
    s.parse::<TaskStatus>().map_err(|_| ReconcileError::WrongFieldType {
        index,
        field: "status",
        detail: format!("unknown status '{s}'"),
    })
}

/// Extract the `blocks` field (a list of maps with `block` + `task_item` keys)
/// into `(target_block, Option<target_item>)` pairs.
fn extract_edges(
    map: &loro::LoroMapValue,
    index: usize,
) -> Result<Vec<(String, Option<String>)>, ReconcileError> {
    let list = match map.get("blocks") {
        Some(LoroValue::List(l)) => l,
        Some(LoroValue::Null) | None => return Ok(Vec::new()),
        Some(other) => {
            return Err(ReconcileError::WrongFieldType {
                index,
                field: "blocks",
                detail: format!("expected list, got {other:?}"),
            });
        }
    };

    let mut edges = Vec::with_capacity(list.len());
    for edge_val in list.iter() {
        match edge_val {
            LoroValue::Map(edge_map) => {
                let block = match edge_map.get("block") {
                    Some(LoroValue::String(s)) => s.to_string(),
                    _ => {
                        return Err(ReconcileError::WrongFieldType {
                            index,
                            field: "blocks[].block",
                            detail: "missing or non-string 'block' in edge".into(),
                        });
                    }
                };
                let task_item = match edge_map.get("task_item") {
                    Some(LoroValue::String(s)) => Some(s.to_string()),
                    Some(LoroValue::Null) | None => None,
                    _ => None,
                };
                edges.push((block, task_item));
            }
            _ => {
                return Err(ReconcileError::WrongFieldType {
                    index,
                    field: "blocks",
                    detail: format!("expected map in blocks list, got {edge_val:?}"),
                });
            }
        }
    }
    Ok(edges)
}

/// Extract the `comments` field to a JSON string.
fn extract_comments_json(map: &loro::LoroMapValue) -> String {
    match map.get("comments") {
        Some(LoroValue::List(l)) if !l.is_empty() => {
            // Convert LoroValue list to serde_json and stringify.
            let json_val = loro_value_to_json(&LoroValue::List(l.clone()));
            serde_json::to_string(&json_val).unwrap_or_else(|_| "[]".to_string())
        }
        _ => "[]".to_string(),
    }
}

/// Convert a LoroValue to serde_json::Value for JSON serialization.
fn loro_value_to_json(val: &LoroValue) -> serde_json::Value {
    match val {
        LoroValue::Null => serde_json::Value::Null,
        LoroValue::Bool(b) => serde_json::Value::Bool(*b),
        LoroValue::I64(i) => serde_json::json!(*i),
        LoroValue::Double(f) => serde_json::json!(*f),
        LoroValue::String(s) => serde_json::Value::String(s.to_string()),
        LoroValue::List(l) => {
            serde_json::Value::Array(l.iter().map(loro_value_to_json).collect())
        }
        LoroValue::Map(m) => {
            let obj: serde_json::Map<String, serde_json::Value> = m
                .iter()
                .map(|(k, v)| (k.clone(), loro_value_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

/// Extract a single task item from a LoroValue::Map.
fn extract_task_item(
    value: &LoroValue,
    index: usize,
) -> Result<ExtractedItem, ReconcileError> {
    let map = match value {
        LoroValue::Map(m) => m,
        other => {
            return Err(ReconcileError::InvalidItemShape {
                index,
                detail: format!("expected Map, got {other:?}"),
            });
        }
    };

    let id = require_str(map, "id", index)?;
    let subject = require_str(map, "subject", index)?;
    let description = get_str(map, "description");
    let status = extract_status(map, index)?;
    let owner = get_str(map, "owner");
    let comments_json = extract_comments_json(map);
    let edges = extract_edges(map, index)?;

    Ok(ExtractedItem {
        id,
        subject,
        description,
        status,
        owner,
        comments_json,
        edges,
    })
}

// endregion: extraction helpers

// region: reconcile

/// Reconcile a TaskList LoroDoc's state against the `tasks` and `task_edges`
/// SQL index tables.
///
/// Must be called inside a `rusqlite::Transaction`. On error, the caller
/// should roll back the transaction.
pub fn reconcile_task_list(
    tx: &Transaction,
    block_handle: &str,
    doc: &loro::LoroDoc,
) -> Result<(), ReconcileError> {
    // Step 1: read items from the LoroDoc.
    let deep = doc.get_deep_value();
    let root_map = match &deep {
        LoroValue::Map(m) => m,
        _ => {
            // Empty or non-map doc — treat as zero items (delete all existing).
            delete_all_for_block(tx, block_handle)?;
            return Ok(());
        }
    };

    let items_value = root_map.get("items");
    let items_list = match items_value {
        Some(LoroValue::List(l)) => l.as_ref(),
        _ => {
            // No items key or not a list — treat as zero items.
            delete_all_for_block(tx, block_handle)?;
            return Ok(());
        }
    };

    // Extract all items from loro.
    let mut extracted: Vec<ExtractedItem> = Vec::with_capacity(items_list.len());
    for (i, val) in items_list.iter().enumerate() {
        extracted.push(extract_task_item(val, i)?);
    }

    // Step 2: fetch existing task_item_ids from SQL.
    let mut existing_ids: HashSet<String> = HashSet::new();
    {
        let mut stmt = tx.prepare(
            "SELECT task_item_id FROM tasks WHERE block_handle = ?1 AND task_item_id IS NOT NULL",
        )?;
        let rows = stmt.query_map(rusqlite::params![block_handle], |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            existing_ids.insert(row?);
        }
    }

    // Step 3: build set of loro item ids.
    let loro_ids: HashSet<&str> = extracted.iter().map(|e| e.id.as_str()).collect();

    // Step 4: delete items that exist in SQL but not in loro.
    for existing_id in &existing_ids {
        if !loro_ids.contains(existing_id.as_str()) {
            delete_task_row(tx, block_handle, existing_id)?;
            delete_task_edges_for_item(tx, block_handle, existing_id)?;
        }
    }

    // Step 5: upsert all items from loro + their edges.
    let now = chrono::Utc::now();
    for item in &extracted {
        let row = TaskRow {
            rowid: 0, // ignored by upsert (delete-then-insert).
            id: item.id.clone(),
            agent_id: None,
            subject: item.subject.clone(),
            description: item.description.clone(),
            status: item.status,
            due_at: None,
            scheduled_at: None,
            completed_at: None,
            parent_task_id: None,
            block_handle: Some(block_handle.to_string()),
            task_item_id: Some(item.id.clone()),
            owner_agent_id: item.owner.clone(),
            comments_json: item.comments_json.clone(),
            created_at: now,
            updated_at: now,
        };
        upsert_task_row(tx, &row)?;
        upsert_task_edges(tx, block_handle, &item.id, &item.edges)?;
    }

    Ok(())
}

/// Delete all tasks and edges for a block handle.
fn delete_all_for_block(tx: &Transaction, block_handle: &str) -> Result<(), ReconcileError> {
    // Get all item ids for this block, then delete edges and rows.
    let mut stmt = tx.prepare(
        "SELECT task_item_id FROM tasks WHERE block_handle = ?1 AND task_item_id IS NOT NULL",
    )?;
    let ids: Vec<String> = stmt
        .query_map(rusqlite::params![block_handle], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for id in &ids {
        delete_task_edges_for_item(tx, block_handle, id)?;
        delete_task_row(tx, block_handle, id)?;
    }
    Ok(())
}

// endregion: reconcile
