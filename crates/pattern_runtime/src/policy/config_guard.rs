// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Shape-based detection for writes that target a Pattern config KDL.
//!
//! The File handler (Task 15) consults this predicate **before**
//! evaluating the policy pipeline so that writes to pattern config
//! files (`.pattern.kdl`, persona KDLs with pattern-shaped top-level
//! nodes) escalate directly to the broker without passing through
//! [`pattern_core::PolicySet`]. Because no rule of any precedence
//! (`RustDefault`, `KdlConfig`, `RuntimeOverride`) is consulted on
//! this path, no rule can loosen the gate; the locked invariant is a
//! structural property of the File handler, not of the policy system.
//! See `sdk/handlers/file.rs::evaluate_write` for the short-circuit.
//!
//! The user can still grant temporary access via the broker's
//! `ApproveForDuration` / `ApproveForScope` flow — those grants live
//! in the broker's in-memory `scope_cache` only and die with the
//! session (see `pattern_core::permission` for the ephemerality
//! invariant).
//!
//! Detection prefers false-positives over false-negatives per design:
//! a benign `.kdl` file that happens to use one of the pattern-specific
//! top-level identifiers will be gated. Acceptable cost — the gate
//! surfaces an approval prompt; the user clears it once.

use std::path::Path;

/// Outcome of a shape check.
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigGuardVerdict {
    /// File is not a Pattern config.
    NotConfig,
    /// File looks like a Pattern config; lists the keys / signals that
    /// fired (filename match, top-level identifiers seen). Useful for
    /// audit logging in the gate-prompt UI.
    LikelyConfig { matched_keys: Vec<String> },
}

impl ConfigGuardVerdict {
    /// Convenience for matcher integration: a verdict either fires the
    /// gate or it doesn't.
    pub fn is_config(&self) -> bool {
        matches!(self, Self::LikelyConfig { .. })
    }
}

/// Pattern-specific top-level node identifiers we treat as a "this is
/// a Pattern config" signal when found at column 0 (or column-0 after
/// optional `-` / whitespace, which KDL allows).
///
/// This is the **canonical** list shared between the handler-level shape
/// guard (`is_pattern_config_kdl`) and the FileManager-level guard
/// (`config_detect::is_pattern_config_write`). Both check sites exist
/// as defense-in-depth; consolidating the key list ensures they agree.
pub const PATTERN_TOP_LEVEL_KEYS: &[&str] = &[
    "backup",
    "capabilities",
    "file-policy",
    "isolation",
    "isolate-from-persona",
    "jj",
    "mount",
    "name",
    "persona",
    "personas",
    "policy",
    "project",
    "storage-mode",
    "system-prompt",
];

/// Decide whether a write to `path` with the given `content` looks
/// like a Pattern config KDL.
///
/// The check is deliberately cheap: it doesn't reach for `knus::parse`
/// — a hand-rolled scan over the leading bytes is enough for the
/// shape signal we want, and it gracefully tolerates partial / random
/// content on the false-positive side.
pub fn is_pattern_config_kdl(path: &Path, content: &[u8]) -> ConfigGuardVerdict {
    // (1) filename rule — covers `.pattern.kdl` exactly.
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && (name == ".pattern.kdl" || name.ends_with(".pattern.kdl"))
    {
        return ConfigGuardVerdict::LikelyConfig {
            matched_keys: vec!["filename".into()],
        };
    }

    // (2) Non-`.kdl` paths short-circuit as not config — KDL files
    // outside this extension are out of scope for the shape guard.
    let extension_is_kdl = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("kdl"))
        .unwrap_or(false);
    if !extension_is_kdl {
        return ConfigGuardVerdict::NotConfig;
    }

    // (3) Scan leading content for top-level pattern-shaped keys.
    // KDL identifiers can be the first non-whitespace token on a line.
    // Limit the scan to the first ~8 KiB so a multi-MB write doesn't
    // pay a quadratic cost; pattern config files are small in practice.
    let scan_window = &content[..std::cmp::min(content.len(), 8 * 1024)];
    let scan_text = match std::str::from_utf8(scan_window) {
        Ok(s) => s,
        Err(_) => return ConfigGuardVerdict::NotConfig,
    };
    let mut matched = Vec::new();
    for line in scan_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        // Take the leading identifier token: alphanumeric + `-` + `_`.
        let ident_end = trimmed
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(trimmed.len());
        let ident = &trimmed[..ident_end];
        if ident.is_empty() {
            continue;
        }
        if PATTERN_TOP_LEVEL_KEYS.contains(&ident) {
            matched.push(ident.to_string());
        }
    }
    if matched.is_empty() {
        ConfigGuardVerdict::NotConfig
    } else {
        ConfigGuardVerdict::LikelyConfig {
            matched_keys: matched,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::PathBuf;

    fn check(path: &str, content: &[u8]) -> ConfigGuardVerdict {
        is_pattern_config_kdl(&PathBuf::from(path), content)
    }

    #[test]
    fn dotted_pattern_kdl_filename_matches_via_filename_rule() {
        match check("/foo/.pattern.kdl", b"") {
            ConfigGuardVerdict::LikelyConfig { matched_keys } => {
                assert!(matched_keys.contains(&"filename".to_string()));
            }
            v => panic!("expected LikelyConfig, got {v:?}"),
        }
    }

    #[test]
    fn persona_kdl_with_pattern_keys_matches_via_top_level_scan() {
        let content = b"name \"Alice\"\nsystem-prompt \"helper\"\n";
        match check("/foo/personas/alice.kdl", content) {
            ConfigGuardVerdict::LikelyConfig { matched_keys } => {
                assert!(matched_keys.contains(&"name".to_string()));
                assert!(matched_keys.contains(&"system-prompt".to_string()));
            }
            v => panic!("expected LikelyConfig, got {v:?}"),
        }
    }

    #[test]
    fn non_kdl_extension_is_not_config_even_with_pattern_keys() {
        let content = b"mount mode=\"A\"\n";
        assert_eq!(
            check("/foo/notes.md", content),
            ConfigGuardVerdict::NotConfig
        );
    }

    #[test]
    fn unrelated_kdl_with_no_pattern_keys_is_not_config() {
        let content = b"greeting \"hello\"\n";
        assert_eq!(
            check("/foo/unrelated.kdl", content),
            ConfigGuardVerdict::NotConfig
        );
    }

    #[test]
    fn mount_top_level_node_matches_kdl_file() {
        let content = b"mount mode=\"A\"\n";
        match check("/foo/pattern.kdl", content) {
            ConfigGuardVerdict::LikelyConfig { matched_keys } => {
                assert_eq!(matched_keys, vec!["mount".to_string()]);
            }
            v => panic!("expected LikelyConfig, got {v:?}"),
        }
    }

    #[test]
    fn capabilities_block_at_top_level_matches() {
        let content = b"capabilities { memory; message; }\n";
        match check("/foo/my.kdl", content) {
            ConfigGuardVerdict::LikelyConfig { matched_keys } => {
                assert_eq!(matched_keys, vec!["capabilities".to_string()]);
            }
            v => panic!("expected LikelyConfig, got {v:?}"),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Fuzz guard: random byte strings with `.kdl` extension must
        /// never panic.
        #[test]
        fn random_bytes_at_kdl_path_does_not_panic(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
            let _ = is_pattern_config_kdl(&PathBuf::from("/tmp/random.kdl"), &bytes);
        }

        /// Fuzz guard: same but for explicit pattern.kdl filename.
        #[test]
        fn random_bytes_at_pattern_kdl_path_returns_likely_via_filename(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
            // Filename rule short-circuits regardless of content.
            let v = is_pattern_config_kdl(&PathBuf::from("/tmp/.pattern.kdl"), &bytes);
            prop_assert!(v.is_config());
        }
    }
}
