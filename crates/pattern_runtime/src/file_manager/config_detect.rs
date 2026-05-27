// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Pattern config-file shape detection.
//!
//! Fast-path checks the filename; slow-path parses as KDL and looks for
//! pattern-reserved top-level keys. Used by `FileManager::write` to
//! gate config-file writes through human approval.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pattern_core::permission::PermissionScope;
use pattern_core::types::origin::MessageOrigin;

use crate::file_manager::error::FileError;
use crate::permission::PermissionBridge;

// Uses the canonical key list from the handler-level shape guard.
// Both the handler guard and this FileManager guard check for the same
// keys as defense-in-depth.
use crate::policy::config_guard::PATTERN_TOP_LEVEL_KEYS as RESERVED_KEYS;

/// Returns `true` if writing `content` to `path` looks like a Pattern
/// config-file write that should be gated through human approval.
pub fn is_pattern_config_write(path: &Path, content: &[u8]) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == ".pattern.kdl" || name.ends_with(".pattern.kdl") {
        return true;
    }
    let Ok(text) = std::str::from_utf8(content) else {
        return false;
    };
    let Ok(doc) = kdl::KdlDocument::parse(text) else {
        return false;
    };
    doc.nodes()
        .iter()
        .any(|n| RESERVED_KEYS.contains(&n.name().value()))
}

/// Find the matched reserved keys in the content (for the scope's
/// `matched_keys` field).
fn find_matched_reserved_keys(content: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(content) else {
        return Vec::new();
    };
    let Ok(doc) = kdl::KdlDocument::parse(text) else {
        return Vec::new();
    };
    doc.nodes()
        .iter()
        .filter(|n| RESERVED_KEYS.contains(&n.name().value()))
        .map(|n| n.name().value().to_string())
        .collect()
}

/// Build a preview of the first N lines of content for the permission
/// request metadata.
fn preview_lines(content: &[u8], max_lines: usize) -> String {
    let text = String::from_utf8_lossy(content);
    text.lines().take(max_lines).collect::<Vec<_>>().join("\n")
}

/// Request human approval for a config-file write via the permission
/// bridge. Returns `Ok(())` on approval, `Err(ConfigApprovalDenied)` on
/// denial/timeout/bridge-closed.
pub(crate) fn await_approval(
    bridge: &Arc<PermissionBridge>,
    agent_id: &pattern_core::AgentId,
    path: &Path,
    content: &[u8],
) -> Result<(), FileError> {
    let matched = find_matched_reserved_keys(content);
    let scope = PermissionScope::FileWriteConfig {
        path: path.to_owned(),
        matched_keys: matched,
    };
    let preview_md = serde_json::json!({ "preview": preview_lines(content, 20) });

    // Build a synthetic agent origin — config-write approval never
    // bypasses the gate (agents cannot self-approve config writes).
    let origin = MessageOrigin::new(
        pattern_core::types::origin::Author::Agent(pattern_core::types::origin::AgentAuthor {
            agent_id: agent_id.clone(),
        }),
        pattern_core::types::origin::Sphere::Internal,
    );

    let grant_opt = bridge.request_sync(
        agent_id.clone(),
        "Pattern.File.Write".to_string(),
        scope,
        &origin,
        Some(format!(
            "config-file shape detected, {} bytes",
            content.len()
        )),
        Some(preview_md),
        Duration::from_secs(300),
    );

    match grant_opt {
        Some(_grant) => Ok(()),
        None => Err(FileError::ConfigApprovalDenied {
            path: path.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_fast_path_accepts_dot_pattern_kdl() {
        assert!(is_pattern_config_write(
            Path::new("/project/.pattern.kdl"),
            b"anything",
        ));
    }

    #[test]
    fn reserved_top_level_key_triggers_detection() {
        let content = b"capabilities {\n  effects { memory; file }\n}\n";
        assert!(is_pattern_config_write(
            Path::new("/project/agent.kdl"),
            content,
        ));
    }

    #[test]
    fn arbitrary_kdl_does_not_trigger() {
        let content = b"greeting \"hello\"\nage 30\n";
        assert!(!is_pattern_config_write(
            Path::new("/project/config.kdl"),
            content,
        ));
    }

    #[test]
    fn non_utf8_does_not_trigger() {
        let content = &[0xFF, 0xFE, 0x00, 0x01];
        assert!(!is_pattern_config_write(
            Path::new("/project/binary.kdl"),
            content,
        ));
    }

    #[test]
    fn malformed_kdl_does_not_trigger() {
        let content = b"this is not { valid kdl because";
        assert!(!is_pattern_config_write(
            Path::new("/project/broken.kdl"),
            content,
        ));
    }
}
