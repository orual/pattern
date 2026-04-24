//! Skill metadata and provenance types.
//!
//! Defines the core types for skills:
//! - [`SkillMetadata`] — author-defined content from frontmatter.
//! - [`SkillTrustTier`] — provenance classification governing hook permissions.
//! - [`SkillUsageStats`] — per-local-install runtime statistics (not serialized).

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

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
}

// endregion: tests
