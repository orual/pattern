// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Log block ↔ `.jsonl` serialization.
//!
//! Log blocks store entries as a `LoroValue::List` of JSON-shaped values.
//! The `.jsonl` file format serializes one JSON value per line (newline-
//! delimited JSON / NDJSON). Append order is preserved.

use std::path::PathBuf;

use crate::fs::FsError;

/// Serialize a list of log entries to JSONL bytes.
///
/// Each entry is written as a single line of compact JSON followed by a
/// newline (`\n`). The output is always valid UTF-8.
pub fn log_entries_to_jsonl(entries: &[serde_json::Value]) -> Result<Vec<u8>, FsError> {
    let mut out = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut out, entry)?;
        out.push(b'\n');
    }
    Ok(out)
}

/// Parse JSONL content into a list of log entries.
///
/// Blank lines are skipped. Malformed JSON on any non-blank line produces an
/// error that includes the 1-indexed line number — no silent data loss.
pub fn jsonl_to_log_entries(content: &str) -> Result<Vec<serde_json::Value>, FsError> {
    let mut out = Vec::new();
    for (lineno, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| FsError::ParseError {
            path: PathBuf::from("<jsonl>"),
            reason: format!("line {}: {}", lineno + 1, e),
        })?;
        out.push(v);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_simple_entries() {
        let entries = vec![
            json!({"timestamp": "2026-01-01T00:00:00Z", "message": "hello"}),
            json!({"timestamp": "2026-01-01T00:01:00Z", "message": "world"}),
        ];
        let bytes = log_entries_to_jsonl(&entries).unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let parsed = jsonl_to_log_entries(content).unwrap();
        assert_eq!(parsed, entries);
    }

    #[test]
    fn round_trip_preserves_order() {
        let entries: Vec<serde_json::Value> = (0..100).map(|i| json!({"index": i})).collect();
        let bytes = log_entries_to_jsonl(&entries).unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let parsed = jsonl_to_log_entries(content).unwrap();
        assert_eq!(parsed, entries);
    }

    #[test]
    fn round_trip_various_json_types() {
        let entries = vec![
            json!(42),
            json!("just a string"),
            json!(null),
            json!(true),
            json!([1, 2, 3]),
            json!({"nested": {"deep": true}}),
        ];
        let bytes = log_entries_to_jsonl(&entries).unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let parsed = jsonl_to_log_entries(content).unwrap();
        assert_eq!(parsed, entries);
    }

    #[test]
    fn blank_lines_skipped() {
        let content = "{\"a\":1}\n\n{\"b\":2}\n\n\n";
        let parsed = jsonl_to_log_entries(content).unwrap();
        assert_eq!(parsed, vec![json!({"a": 1}), json!({"b": 2})]);
    }

    #[test]
    fn empty_input() {
        let parsed = jsonl_to_log_entries("").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn empty_entries_produce_empty_output() {
        let bytes = log_entries_to_jsonl(&[]).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn malformed_line_reports_line_number() {
        let content = "{\"ok\":true}\nnot json\n{\"also_ok\":true}\n";
        let err = jsonl_to_log_entries(content).unwrap_err();
        match err {
            FsError::ParseError { reason, .. } => {
                assert!(
                    reason.contains("line 2"),
                    "error should mention line 2, got: {reason}"
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn large_single_entry() {
        // 1 MB string entry.
        let big = "x".repeat(1_000_000);
        let entries = vec![json!({"data": big})];
        let bytes = log_entries_to_jsonl(&entries).unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let parsed = jsonl_to_log_entries(content).unwrap();
        assert_eq!(parsed, entries);
    }

    #[test]
    fn each_entry_on_its_own_line() {
        let entries = vec![json!(1), json!(2), json!(3)];
        let bytes = log_entries_to_jsonl(&entries).unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "1");
        assert_eq!(lines[1], "2");
        assert_eq!(lines[2], "3");
    }

    #[test]
    fn entries_with_unicode() {
        let entries = vec![json!({"msg": "日本語 🎉 café"})];
        let bytes = log_entries_to_jsonl(&entries).unwrap();
        let content = std::str::from_utf8(&bytes).unwrap();
        let parsed = jsonl_to_log_entries(content).unwrap();
        assert_eq!(parsed, entries);
    }
}
