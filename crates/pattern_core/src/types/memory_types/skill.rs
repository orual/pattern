// Copyright 2026 Pattern contributors
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, you can obtain one at http://mozilla.org/MPL/2.0/.

//! Skill metadata and provenance types.
//!
//! Defines the core types for skills:
//! - [`SkillMetadata`] — author-defined content from frontmatter.
//! - [`SkillTrustTier`] — provenance classification governing hook permissions.
//! - [`SkillUsageStats`] — per-local-install runtime statistics (not serialized).
//! - [`SkillInfo`] — summary of a skill for listings and search results.
//! - [`SkillError`] — errors specific to skill operations.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::types::block::BlockHandle;
use crate::types::ids::AgentId;

// region: SkillMetadata

/// Author-defined metadata captured from a skill's YAML frontmatter.
///
/// Serialized to the canonical `<mount>/skills/<name>.md` file's
/// `---` frontmatter block. Round-trips via the `markdown_skill`
/// converter (pattern_memory/src/fs/markdown_skill/). Unknown
/// frontmatter keys that aren't in this struct are preserved in a
/// separate `extras` LoroMap (handled by the converter) so they
/// survive write-back without loss.
///
/// # `hooks` shape contract (Phase 5+ forward-compat note)
///
/// `hooks` is intentionally typed as `serde_json::Value` at the
/// data layer — authors can embed arbitrary nested structure and
/// the type stays stable as we grow the hook vocabulary. The
/// intended shape that a future runtime will parse is a mapping
/// of event names to ordered action lists:
///
/// ```yaml
/// hooks:
///   on_turn_start:
///     - inject_context: "Remember the checklist before acting."
///   on_memory_write:
///     - log: "scratchpad touched"
///   on_tool_use:
///     - match: { tool: "shell" }
///       action:
///         log: "agent used shell"
/// ```
///
/// Expected event keys include (not exhaustive, defined in Phase 5+):
/// `on_load`, `on_unload`, `on_turn_start`, `on_turn_end`,
/// `on_memory_write`, `on_tool_use`, `on_message_received`,
/// `on_compaction`. Actions are maps with a single action-type key.
/// Trust tier gates which actions an event may invoke (e.g., only
/// `FirstParty` / `PluginInstalled` can register hooks that
/// inject context or invoke tools).
///
/// For Phase 4 this field is preserved opaquely through round-trip.
/// Phase 5+ introduces a typed hook manifest parser + runtime
/// subscription; skill authors who write hooks now get forward
/// compatibility if they follow the documented shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// Stable, kebab-case identifier for this skill. Required.
    pub name: String,
    /// Provenance tier. Most tiers are derived from source at load
    /// time; only `PluginInstalled` is preserved from the declared
    /// frontmatter value. See [`SkillTrustTier`] for the policy.
    pub trust_tier: SkillTrustTier,
    /// Short human description. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Keywords for FTS5 search. Optional; empty vec default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Opaque author-defined hook manifest. Phase 4 preserves this
    /// verbatim through round-trip; Phase 5+ parses it as a typed
    /// event→action map. See type-level doc for the intended shape.
    #[serde(default)]
    pub hooks: serde_json::Value,
    /// Plugin that installed this skill. `None` for built-in or user-authored skills.
    /// Set by Phase 3's CC skill translator when importing plugin skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plugin_id: Option<smol_str::SmolStr>,
}

// endregion: SkillMetadata

// region: SkillTrustTier

/// Provenance tier governing what a skill's hooks are permitted to do
/// and how much the runtime trusts its claims.
///
/// # Policy
///
/// Only `PluginInstalled` is preserved from the skill's own
/// frontmatter. All other tiers are derived from the skill's source
/// location at load time — authors cannot self-declare `FirstParty`
/// or `ProjectLocal` by writing it in their frontmatter.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillTrustTier {
    /// Ships with pattern_runtime (SDK resource directory).
    FirstParty,
    /// Found at `<mount>/skills/<name>.md` OR stored as a project-scope
    /// Skill block. Agent/user authored, not runtime-supplied.
    ProjectLocal,
    /// Installed by a plugin system (future). Preserved from frontmatter
    /// when explicitly declared; emits a warning metric if the plugin
    /// system isn't active (see Phase 4 Task 8).
    PluginInstalled,
    /// Created at runtime via `MemoryStore::put_block` (e.g., drafted by
    /// an agent). Least trusted tier — hooks that would mutate shared
    /// state are gated.
    AdHoc,
}

// endregion: SkillTrustTier

// region: SkillUsageStats

/// Per-local-install usage statistics. NOT serialized into the
/// canonical `.md` file — lives in the `skill_usage_stats` sqlite
/// table only.
///
/// Per-install observability, not replicated content. Two nodes with
/// divergent use counts should NOT merge via CRDT semantics.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillUsageStats {
    /// Most recent load timestamp, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Timestamp>,
    /// Agent that most recently loaded this skill, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_by: Option<AgentId>,
    /// Monotonic count of loads since the table was created.
    #[serde(default)]
    pub use_count: u64,
}

// endregion: SkillUsageStats

// region: SkillInfo

/// Summary information about a skill for listings and search results.
///
/// Combines metadata fields with runtime usage statistics from the sqlite
/// table. Used in responses from `Pattern.Skills.list` and `Pattern.Skills.search`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillInfo {
    /// The stable handle (label) by which agents refer to this skill block.
    pub handle: BlockHandle,
    /// Skill name from metadata (required field).
    pub name: String,
    /// Short human description, if provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Provenance tier of this skill.
    pub trust_tier: SkillTrustTier,
    /// Keywords for full-text search, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Most recent load timestamp, if any. Populated from sqlite at list/search time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<Timestamp>,
}

// endregion: SkillInfo

// region: SkillError

/// Errors specific to skill operations.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// The block at the given handle is not a Skill block.
    #[error("block `{0}` is not a Skill block")]
    NotASkill(BlockHandle),

    /// The skill's LoroDoc metadata could not be read or parsed.
    #[error("skill metadata for `{0}` could not be read from LoroDoc")]
    MalformedMetadata(BlockHandle),
}

// endregion: SkillError

// region: tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trust_tier_serializes_to_kebab() {
        // Test all 4 variants serialize to kebab-case strings.
        let first_party = SkillTrustTier::FirstParty;
        let project_local = SkillTrustTier::ProjectLocal;
        let plugin_installed = SkillTrustTier::PluginInstalled;
        let ad_hoc = SkillTrustTier::AdHoc;

        let fp_str = serde_json::to_string(&first_party).unwrap();
        let pl_str = serde_json::to_string(&project_local).unwrap();
        let pi_str = serde_json::to_string(&plugin_installed).unwrap();
        let ah_str = serde_json::to_string(&ad_hoc).unwrap();

        assert_eq!(fp_str, r#""first-party""#);
        assert_eq!(pl_str, r#""project-local""#);
        assert_eq!(pi_str, r#""plugin-installed""#);
        assert_eq!(ah_str, r#""ad-hoc""#);

        // Round-trip back.
        assert_eq!(
            serde_json::from_str::<SkillTrustTier>(&fp_str).unwrap(),
            first_party
        );
        assert_eq!(
            serde_json::from_str::<SkillTrustTier>(&pl_str).unwrap(),
            project_local
        );
        assert_eq!(
            serde_json::from_str::<SkillTrustTier>(&pi_str).unwrap(),
            plugin_installed
        );
        assert_eq!(
            serde_json::from_str::<SkillTrustTier>(&ah_str).unwrap(),
            ad_hoc
        );
    }

    #[test]
    fn skill_metadata_with_nested_hooks_round_trips() {
        // Build a SkillMetadata with nested hooks object.
        let hooks_value = json!({
            "on_turn_start": [
                {"inject_context": "Remember the checklist before acting."}
            ],
            "on_memory_write": [
                {"log": "scratchpad touched"}
            ],
            "on_tool_use": [
                {
                    "match": {"tool": "shell"},
                    "action": {"log": "agent used shell"}
                }
            ]
        });

        let original = SkillMetadata {
            name: "my-skill".to_string(),
            trust_tier: SkillTrustTier::ProjectLocal,
            description: Some("A test skill".to_string()),
            keywords: vec!["test".to_string(), "example".to_string()],
            hooks: hooks_value.clone(),
            source_plugin_id: None,
        };

        // Serialize to JSON.
        let json_str = serde_json::to_string(&original).unwrap();

        // Deserialize back.
        let deserialized: SkillMetadata = serde_json::from_str(&json_str).unwrap();

        // Assert all fields match exactly, including nested hooks structure.
        assert_eq!(deserialized.name, original.name);
        assert_eq!(deserialized.trust_tier, original.trust_tier);
        assert_eq!(deserialized.description, original.description);
        assert_eq!(deserialized.keywords, original.keywords);
        assert_eq!(deserialized.hooks, original.hooks);
        assert_eq!(deserialized, original);
    }

    #[test]
    fn skill_usage_stats_default_is_all_empty() {
        let default_stats = SkillUsageStats::default();

        assert_eq!(default_stats.last_used, None);
        assert_eq!(default_stats.last_used_by, None);
        assert_eq!(default_stats.use_count, 0);
    }

    #[test]
    fn trust_tier_invalid_kebab_is_error() {
        // Try to deserialize an invalid trust_tier value.
        let invalid_json = json!({
            "name": "test-skill",
            "trust_tier": "foo"
        });

        let result: serde_json::Result<SkillMetadata> = serde_json::from_value(invalid_json);

        // Should fail because "foo" is not a valid variant.
        assert!(result.is_err());
    }

    #[test]
    fn skill_metadata_minimal_round_trips() {
        // Only name and trust_tier set; description, keywords, hooks should default.
        let metadata = SkillMetadata {
            name: "minimal".to_string(),
            trust_tier: SkillTrustTier::AdHoc,
            description: None,
            keywords: vec![],
            hooks: serde_json::Value::Null,
            source_plugin_id: None,
        };

        // Serialize to JSON string.
        let json_str = serde_json::to_string(&metadata).unwrap();

        // Because of skip_serializing_if attributes, description, keywords,
        // and hooks should not be in the serialized output (or hooks as null).
        // Let's verify skip_serializing_if is working by deserializing back.
        let deserialized: SkillMetadata = serde_json::from_str(&json_str).unwrap();

        assert_eq!(deserialized.name, metadata.name);
        assert_eq!(deserialized.trust_tier, metadata.trust_tier);
        assert_eq!(deserialized.description, metadata.description);
        assert_eq!(deserialized.keywords, metadata.keywords);

        // Both should be equal.
        assert_eq!(deserialized, metadata);

        // Re-serialize and ensure byte stability (idempotency).
        let json_str_2 = serde_json::to_string(&deserialized).unwrap();
        assert_eq!(json_str, json_str_2);
    }

    #[test]
    fn skill_info_round_trips() {
        use smol_str::SmolStr;

        let handle = SmolStr::new("my-skill");
        let now = Timestamp::now();

        let skill_info = SkillInfo {
            handle: handle.clone(),
            name: "my-skill".to_string(),
            description: Some("A useful skill".to_string()),
            trust_tier: SkillTrustTier::ProjectLocal,
            keywords: vec!["useful".to_string(), "practical".to_string()],
            last_used: Some(now),
        };

        // Serialize to JSON.
        let json_str = serde_json::to_string(&skill_info).unwrap();

        // Deserialize back.
        let deserialized: SkillInfo = serde_json::from_str(&json_str).unwrap();

        // Verify all fields match.
        assert_eq!(deserialized.handle, skill_info.handle);
        assert_eq!(deserialized.name, skill_info.name);
        assert_eq!(deserialized.description, skill_info.description);
        assert_eq!(deserialized.trust_tier, skill_info.trust_tier);
        assert_eq!(deserialized.keywords, skill_info.keywords);
        assert_eq!(deserialized.last_used, skill_info.last_used);
        assert_eq!(deserialized, skill_info);
    }

    #[test]
    fn skill_error_not_a_skill_display_includes_handle() {
        use smol_str::SmolStr;

        let handle = SmolStr::new("my-text-block");
        let error = SkillError::NotASkill(handle.clone());

        // Display message should contain the handle.
        let display_msg = format!("{}", error);
        assert!(
            display_msg.contains("my-text-block"),
            "error message '{}' should contain handle",
            display_msg
        );
        assert!(display_msg.contains("not a Skill block"));
    }
}

// endregion: tests
