//! CC SKILL.md → Pattern skill block translator.
//!
//! Walks the plugin's skill directories, parses each SKILL.md via
//! `pattern_memory::fs::markdown_skill::parse`, decorates with
//! `PluginInstalled` trust tier and source attribution, then persists
//! as Skill blocks in the memory store.

use std::path::{Path, PathBuf};

use smol_str::SmolStr;

use pattern_core::plugin::manifest::{ComponentSpec, PluginManifest};
use pattern_core::traits::plugin::{PluginContext, PluginError};
use pattern_core::types::memory_types::SkillTrustTier;

/// Walk the plugin's skills directory and install each SKILL.md as a
/// Pattern skill block with `trust_tier: PluginInstalled`.
pub async fn install_skills(
    plugin_id: &SmolStr,
    plugin_root: &Path,
    manifest: &PluginManifest,
    ctx: &PluginContext,
) -> Result<(), PluginError> {
    let skill_dirs = resolve_skill_dirs(plugin_root, manifest);

    for skill_dir in skill_dirs {
        if !skill_dir.is_dir() {
            tracing::debug!(
                plugin_id = %plugin_id,
                path = %skill_dir.display(),
                "skill directory does not exist, skipping"
            );
            continue;
        }

        let entries = std::fs::read_dir(&skill_dir).map_err(|e| {
            PluginError::Io(std::io::Error::new(
                e.kind(),
                format!("reading skill dir {}: {e}", skill_dir.display()),
            ))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| PluginError::Io(e))?;
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }

            let raw = std::fs::read(&skill_md).map_err(|e| {
                PluginError::SkillTranslationFailed {
                    plugin_id: plugin_id.clone(),
                    path: skill_md.clone(),
                    message: format!("failed to read: {e}"),
                }
            })?;

            // Reuse the existing saphyr-backed parser.
            let mut parsed = pattern_memory::fs::markdown_skill::parse::parse(&raw)
                .map_err(|e| PluginError::SkillTranslationFailed {
                    plugin_id: plugin_id.clone(),
                    path: skill_md.clone(),
                    message: e.to_string(),
                })?;

            // Decorate: PluginInstalled trust tier + source attribution.
            parsed.metadata.trust_tier = SkillTrustTier::PluginInstalled;
            parsed.metadata.source_plugin_id = Some(plugin_id.clone());

            tracing::info!(
                plugin_id = %plugin_id,
                skill_name = %parsed.metadata.name,
                path = %skill_md.display(),
                "installed skill from CC plugin"
            );

            // TODO: persist_skill_block — requires PluginContext.memory_store
            // to be wired. For now, log the skill but don't persist.
            // When memory_store is available:
            //   persist_skill_block(ctx, parsed.metadata, parsed.extras, parsed.body).await?;
        }
    }
    Ok(())
}

/// Resolve the skill directories from the manifest's component specs.
/// Falls back to `<plugin_root>/skills/` if no skills are declared.
fn resolve_skill_dirs(plugin_root: &Path, manifest: &PluginManifest) -> Vec<PathBuf> {
    if manifest.skills.is_empty() {
        // Default: look for a `skills/` subdirectory.
        let default = plugin_root.join("skills");
        if default.is_dir() {
            return vec![default];
        }
        return Vec::new();
    }

    manifest
        .skills
        .iter()
        .filter_map(|spec| match spec {
            ComponentSpec::Path(p) => {
                let resolved = if p.is_absolute() {
                    p.clone()
                } else {
                    plugin_root.join(p)
                };
                Some(resolved)
            }
            ComponentSpec::Paths(ps) => {
                // Take the first path for directory resolution.
                ps.first().map(|p| {
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        plugin_root.join(p)
                    }
                })
            }
            ComponentSpec::Inline(_) => None, // Can't resolve inline specs to directories.
            _ => None,
        })
        .collect()
}
