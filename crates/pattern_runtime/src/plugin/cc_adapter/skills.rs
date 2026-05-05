//! CC SKILL.md → Pattern skill block translator.

use std::path::{Path, PathBuf};

use smol_str::SmolStr;

use pattern_core::plugin::manifest::{ComponentSpec, PluginManifest};
use pattern_core::traits::plugin::{PluginContext, PluginError};
use pattern_core::traits::MemoryStore;
use pattern_core::types::memory_types::{MemoryBlockType, Scope, SkillTrustTier};

/// Walk the plugin's skills directory and install each SKILL.md as a
/// Pattern skill block.
pub async fn install_skills(
    plugin_id: &SmolStr,
    plugin_root: &Path,
    manifest: &PluginManifest,
    ctx: &PluginContext,
) -> Result<(), PluginError> {
    let skill_dirs = resolve_skill_dirs(plugin_root, manifest);

    let (store, scope) = match (&ctx.memory_store, &ctx.scope) {
        (Some(s), Some(sc)) => (s.clone(), sc.clone()),
        _ => {
            tracing::debug!(plugin = %plugin_id, "no memory store, skills not persisted");
            return Ok(());
        }
    };

    for skill_dir in skill_dirs {
        if !skill_dir.is_dir() {
            continue;
        }

        let entries = match std::fs::read_dir(&skill_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(path = %skill_dir.display(), error = %e, "failed to read skill dir");
                continue;
            }
        };

        for entry in entries.flatten() {
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }

            let raw = match std::fs::read(&skill_md) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(path = %skill_md.display(), error = %e, "failed to read SKILL.md");
                    continue;
                }
            };

            let mut parsed = match pattern_memory::fs::markdown_skill::parse::parse(&raw) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(path = %skill_md.display(), error = %e, "failed to parse SKILL.md");
                    continue;
                }
            };

            parsed.metadata.trust_tier = SkillTrustTier::PluginInstalled;
            parsed.metadata.source_plugin_id = Some(plugin_id.clone());

            let label = format!("skill-{}", parsed.metadata.name);

            // Create or replace the block.
            let create = pattern_core::types::block::BlockCreate::new(
                label.clone(),
                MemoryBlockType::Working,
                pattern_core::types::memory_types::BlockSchema::Skill { expected_keys: vec![] },
            )
            .with_description(format!("Skill: {} (plugin: {})", parsed.metadata.name, plugin_id));

            let doc = match store.create_or_replace_block(&scope, create) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(skill = %parsed.metadata.name, error = %e, "failed to create skill block");
                    continue;
                }
            };

            // Write to "content" (the standard text container for all blocks).
            if let Err(e) = doc.set_text(&parsed.body, true) {
                tracing::warn!(skill = %parsed.metadata.name, error = %e, "set_text failed");
            }
            doc.inner().commit();

            // Persist to disk.
            if let Err(e) = store.mark_dirty(&scope, &label) {
                tracing::warn!(skill = %parsed.metadata.name, error = %e, "mark_dirty failed");
            }
            if let Err(e) = store.persist_block(&scope, &label) {
                tracing::warn!(skill = %parsed.metadata.name, error = %e, "persist failed");
            }

            tracing::info!(skill = %parsed.metadata.name, plugin = %plugin_id, "skill loaded");
        }
    }
    Ok(())
}

/// Resolve skill directories from the manifest.
fn resolve_skill_dirs(plugin_root: &Path, manifest: &PluginManifest) -> Vec<PathBuf> {
    if manifest.skills.is_empty() {
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
            _ => None,
        })
        .collect()
}
