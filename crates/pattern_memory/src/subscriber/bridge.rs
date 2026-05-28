// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! `BlockSchemaBridge` — bridge adapter between `LoroDocBridge` and
//! memory-block schemas.
//!
//! Wraps the existing per-schema render and external-edit-apply logic from
//! `render_canonical_from_disk_doc` and `MemoryCache::apply_external_edit`
//! behind the `LoroDocBridge` trait so that `SyncedDoc<BlockSchemaBridge>`
//! can manage block files generically.

use std::path::Path;

use loro::LoroDoc;
use pattern_core::types::memory_types::BlockSchema;
use smol_str::SmolStr;

use crate::loro_sync::bridge::{BridgeError, LoroDocBridge};

/// `LoroDocBridge` implementation for typed memory-block schemas.
///
/// Delegates rendering to `render_canonical_from_disk_doc` and external-edit
/// application to `apply_block_external_edit`. Both functions are the
/// mechanical ports of the per-schema match arms that previously lived inline
/// in `worker.rs` and `cache.rs`.
pub struct BlockSchemaBridge {
    schema: BlockSchema,
}

impl BlockSchemaBridge {
    pub fn new(schema: BlockSchema) -> Self {
        Self { schema }
    }

    pub fn schema(&self) -> &BlockSchema {
        &self.schema
    }
}

impl LoroDocBridge for BlockSchemaBridge {
    fn render(&self, disk_doc: &LoroDoc) -> Result<(SmolStr, Vec<u8>), BridgeError> {
        let (ext, bytes) =
            crate::subscriber::worker::render_canonical_from_disk_doc(disk_doc, &self.schema)
                .map_err(BridgeError::Render)?;
        Ok((SmolStr::new(ext), bytes))
    }

    fn apply_external(
        &self,
        disk_doc: &LoroDoc,
        content: &[u8],
        path: &Path,
    ) -> Result<(), BridgeError> {
        apply_block_external_edit(disk_doc, &self.schema, content, path)
    }
}

/// Apply external file content to a block's `disk_doc` according to its schema.
///
/// This is a mechanical port of the per-schema match arms from
/// `MemoryCache::apply_external_edit` (cache.rs). Each `BlockSchema` variant
/// has its own parsing and Loro-application logic:
///
/// - **Text**: UTF-8 decode → strip markdown → `text.update` on `"content"`.
/// - **Map / Composite**: UTF-8 → KDL parse (Map shape) → JSON → `apply_json_to_loro_doc`.
/// - **List**: UTF-8 → KDL parse (List shape) → JSON → `apply_json_to_loro_doc`.
/// - **Log**: UTF-8 → JSONL parse → JSON array → `apply_json_to_loro_doc`.
/// - **TaskList**: UTF-8 → KDL parse (TaskList shape) → JSON → `apply_json_to_loro_doc`.
/// - **Skill**: YAML-frontmatter + markdown body parse → `write_skill_to_loro_doc`.
///
/// String errors from the original code are translated to `BridgeError`
/// variants (`Utf8`, `Parse`, `Loro`).
///
/// # Skill trust-tier enforcement contract
///
/// The `Skill` arm applies the `metadata.trust_tier` from the parsed file
/// as-is. It cannot enforce provenance-based trust because it has no access
/// to `mount_path` or `first_party_skills_dir`.
///
/// **Callers must NEVER pass raw external Skill bytes directly to
/// `SyncedDoc::apply_external_bytes`.** Instead, route through
/// `MemoryCache::apply_external_edit`, which parses the content, enforces the
/// trust tier via `resolve_source_for_path` + `assign_trust_tier`, and
/// re-emits the corrected bytes before calling `apply_external_bytes`. This
/// gate-before-call pattern is the only place in the codebase that holds both
/// `mount_path` and `first_party_skills_dir`, so it is the only place where
/// provenance can be resolved correctly.
///
/// The `BlockFanoutRouter` is the only other external-edit entry point for
/// block files. It must apply the same trust-tier enforcement before calling
/// `SyncedDoc::apply_external_bytes` for Skill blocks.
pub(crate) fn apply_block_external_edit(
    disk_doc: &LoroDoc,
    schema: &BlockSchema,
    content: &[u8],
    path: &Path,
) -> Result<(), BridgeError> {
    match schema {
        BlockSchema::Text { .. } => {
            let text = std::str::from_utf8(content).map_err(|e| BridgeError::Utf8 {
                path: path.to_owned(),
                source: e,
            })?;
            let stripped = crate::fs::markdown::markdown_to_text(text);
            let disk_text = disk_doc.get_text("content");
            disk_text
                .update(&stripped, Default::default())
                .map_err(|e| BridgeError::Loro(format!("disk_doc text update failed: {e}")))?;
            disk_doc.commit();
            Ok(())
        }
        BlockSchema::Map { .. } | BlockSchema::Composite { .. } => {
            let text = std::str::from_utf8(content).map_err(|e| BridgeError::Utf8 {
                path: path.to_owned(),
                source: e,
            })?;
            let kdl_doc = crate::fs::kdl::parse_kdl(text).map_err(|e| BridgeError::Parse {
                path: path.to_owned(),
                message: format!("KDL parse failed: {e}"),
            })?;
            let loro_value =
                crate::fs::kdl::kdl_to_loro_value(&kdl_doc, crate::fs::kdl::TopShape::Map)
                    .map_err(|e| BridgeError::Parse {
                        path: path.to_owned(),
                        message: format!("KDL→LoroValue failed: {e}"),
                    })?;
            let json = crate::fs::kdl::loro_value_to_json(&loro_value).ok_or_else(|| {
                BridgeError::Parse {
                    path: path.to_owned(),
                    message: "LoroValue→JSON conversion failed".to_string(),
                }
            })?;
            crate::cache::apply_json_to_loro_doc(disk_doc, &json, schema)
                .map_err(|e| BridgeError::Loro(format!("disk_doc JSON import failed: {e}")))?;
            disk_doc.commit();
            Ok(())
        }
        BlockSchema::List { .. } => {
            let text = std::str::from_utf8(content).map_err(|e| BridgeError::Utf8 {
                path: path.to_owned(),
                source: e,
            })?;
            let kdl_doc = crate::fs::kdl::parse_kdl(text).map_err(|e| BridgeError::Parse {
                path: path.to_owned(),
                message: format!("KDL parse failed: {e}"),
            })?;
            let loro_value =
                crate::fs::kdl::kdl_to_loro_value(&kdl_doc, crate::fs::kdl::TopShape::List)
                    .map_err(|e| BridgeError::Parse {
                        path: path.to_owned(),
                        message: format!("KDL→LoroValue failed: {e}"),
                    })?;
            let json = crate::fs::kdl::loro_value_to_json(&loro_value).ok_or_else(|| {
                BridgeError::Parse {
                    path: path.to_owned(),
                    message: "LoroValue→JSON conversion failed".to_string(),
                }
            })?;
            crate::cache::apply_json_to_loro_doc(disk_doc, &json, schema)
                .map_err(|e| BridgeError::Loro(format!("disk_doc JSON import failed: {e}")))?;
            disk_doc.commit();
            Ok(())
        }
        BlockSchema::Log { .. } => {
            let text = std::str::from_utf8(content).map_err(|e| BridgeError::Utf8 {
                path: path.to_owned(),
                source: e,
            })?;
            let entries =
                crate::fs::jsonl::jsonl_to_log_entries(text).map_err(|e| BridgeError::Parse {
                    path: path.to_owned(),
                    message: format!("JSONL parse failed: {e}"),
                })?;
            let arr = serde_json::Value::Array(entries);
            crate::cache::apply_json_to_loro_doc(disk_doc, &arr, schema)
                .map_err(|e| BridgeError::Loro(format!("disk_doc JSON import failed: {e}")))?;
            disk_doc.commit();
            Ok(())
        }
        BlockSchema::TaskList { .. } => {
            let text = std::str::from_utf8(content).map_err(|e| BridgeError::Utf8 {
                path: path.to_owned(),
                source: e,
            })?;
            let kdl_doc = crate::fs::kdl::parse_kdl(text).map_err(|e| BridgeError::Parse {
                path: path.to_owned(),
                message: format!("KDL parse failed: {e}"),
            })?;
            let loro_value =
                crate::fs::kdl::kdl_to_loro_value(&kdl_doc, crate::fs::kdl::TopShape::TaskList)
                    .map_err(|e| BridgeError::Parse {
                        path: path.to_owned(),
                        message: format!("KDL→LoroValue failed: {e}"),
                    })?;
            let json = crate::fs::kdl::loro_value_to_json(&loro_value).ok_or_else(|| {
                BridgeError::Parse {
                    path: path.to_owned(),
                    message: "LoroValue→JSON conversion failed".to_string(),
                }
            })?;
            crate::cache::apply_json_to_loro_doc(disk_doc, &json, schema)
                .map_err(|e| BridgeError::Loro(format!("disk_doc JSON import failed: {e}")))?;
            disk_doc.commit();
            Ok(())
        }
        BlockSchema::Skill { .. } => {
            // Skill blocks: parse YAML-frontmatter + markdown body, then write
            // to disk_doc. Trust-tier enforcement from provenance is NOT done
            // here — see the function-level doc comment for the enforcement
            // contract. Callers MUST route Skill blocks through
            // `MemoryCache::apply_external_edit`, which enforces the trust tier
            // before calling `SyncedDoc::apply_external_bytes`.
            let skill_file =
                crate::fs::markdown_skill::parse(content).map_err(|e| BridgeError::Parse {
                    path: path.to_owned(),
                    message: format!("Skill parse failed: {e}"),
                })?;
            crate::fs::markdown_skill::write_skill_to_loro_doc(&skill_file, disk_doc).map_err(
                |e| BridgeError::Loro(format!("Skill write_skill_to_loro_doc failed: {e}")),
            )?;
            disk_doc.commit();
            Ok(())
        }
        _ => Err(BridgeError::Parse {
            path: path.to_owned(),
            message: format!("unsupported schema: {schema:?}"),
        }),
    }
}
