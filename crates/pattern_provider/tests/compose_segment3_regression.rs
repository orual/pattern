// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Compose pipeline snapshot regression tests for segment-3 rendering.
//!
//! Verifies that `render_current_state` produces stable output across
//! refactors. Covers Core + Working blocks, log-schema working blocks,
//! description presence/absence, and the empty-blocks edge case.

use pattern_core::memory::StructuredDocument;
use pattern_core::types::memory_types::{
    BlockMetadata, BlockSchema, LogEntrySchema, MemoryBlockType,
};
use pattern_provider::compose::current_state::render_current_state;

// ---- helpers ---------------------------------------------------------------

/// Extract the full joined text from a `ChatMessage`.
fn msg_text(msg: &genai::chat::ChatMessage) -> String {
    msg.content.joined_texts().unwrap_or_default()
}

/// Build a minimal `StructuredDocument` for testing.
///
/// For `BlockSchema::Text`, writes content to the text container.
/// For `BlockSchema::Log`, splits `content` on newlines and appends each
/// line as a log entry (as `{"message": line}` objects) so the log renderer
/// has real data to render. Using `set_text` for a log document would write to
/// the "content" text container which the log renderer ignores.
fn make_doc(
    label: &str,
    description: &str,
    content: &str,
    block_type: MemoryBlockType,
    schema: BlockSchema,
) -> StructuredDocument {
    let mut metadata = BlockMetadata::standalone(schema);
    metadata.label = label.to_string();
    metadata.description = description.to_string();
    metadata.block_type = block_type;
    let doc = StructuredDocument::new_with_metadata(metadata, None);
    match doc.schema() {
        BlockSchema::Log { .. } => {
            // Log blocks store entries in the "entries" list container, not the
            // "content" text container.  Parse each non-empty line as a
            // timestamp-prefixed entry (`"<timestamp>: <message>"`) and store
            // the fields the `LogEntrySchema` actually renders.
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }
                // Try to split off the timestamp prefix (ISO-8601 followed by ": ").
                let entry = if let Some((ts, msg)) = line.split_once(": ") {
                    serde_json::json!({ "timestamp": ts, "message": msg })
                } else {
                    serde_json::json!({ "message": line })
                };
                doc.append_log_entry(entry, true).unwrap();
            }
        }
        _ => {
            doc.set_text(content, true).unwrap();
        }
    }
    doc
}

// ---- snapshot tests --------------------------------------------------------

/// AC7.6: empty block list still produces a message.
#[test]
fn snapshot_empty_blocks() {
    let msg = render_current_state(&[]);
    let text = msg_text(&msg);
    insta::assert_snapshot!(text);
}

/// Representative constellation: persona (Core) + scratchpad (Working).
#[test]
fn snapshot_core_and_working_blocks() {
    let blocks = vec![
        make_doc(
            "persona",
            "The agent's identity and role.",
            "I am Aria, a Pattern executive-function agent.",
            MemoryBlockType::Core,
            BlockSchema::text(),
        ),
        make_doc(
            "scratchpad",
            "Working notes for the current session.",
            "- reviewed PR #42\n- waiting on CI",
            MemoryBlockType::Working,
            BlockSchema::text(),
        ),
    ];
    let msg = render_current_state(&blocks);
    let text = msg_text(&msg);
    insta::assert_snapshot!(text);
}

/// AC3.6: a log-schema block renders correctly on the Working tier.
#[test]
fn snapshot_log_schema_on_working_tier() {
    let log_schema = BlockSchema::Log {
        display_limit: 5,
        entry_schema: LogEntrySchema {
            timestamp: true,
            agent_id: false,
            fields: vec![],
        },
    };
    let blocks = vec![make_doc(
        "session_log",
        "Recent session activity.",
        "2026-04-19T10:00:00Z: started session\n2026-04-19T10:05:00Z: reviewed memory",
        MemoryBlockType::Working,
        log_schema,
    )];
    let msg = render_current_state(&blocks);
    let text = msg_text(&msg);
    insta::assert_snapshot!(text);
}

/// AC3.6: the same log-schema content renders on Core tier as well.
#[test]
fn snapshot_log_schema_on_core_tier() {
    let log_schema = BlockSchema::Log {
        display_limit: 5,
        entry_schema: LogEntrySchema {
            timestamp: true,
            agent_id: false,
            fields: vec![],
        },
    };
    let blocks = vec![make_doc(
        "system_log",
        "",
        "2026-04-19T10:00:00Z: system boot",
        MemoryBlockType::Core,
        log_schema,
    )];
    let msg = render_current_state(&blocks);
    let text = msg_text(&msg);
    insta::assert_snapshot!(text);
}

/// Mixed block types with descriptions and without.
#[test]
fn snapshot_mixed_blocks_with_and_without_description() {
    let blocks = vec![
        make_doc(
            "human",
            "Information about the partner.",
            "Name: Alex\nPreferences: concise responses",
            MemoryBlockType::Core,
            BlockSchema::text(),
        ),
        make_doc(
            "task_queue",
            "",
            "1. Fix bug #123\n2. Write tests",
            MemoryBlockType::Working,
            BlockSchema::text(),
        ),
    ];
    let msg = render_current_state(&blocks);
    let text = msg_text(&msg);
    insta::assert_snapshot!(text);
}
