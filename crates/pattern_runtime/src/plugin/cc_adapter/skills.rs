//! CC SKILL.md → Pattern skill block translator.

use std::path::Path;

use smol_str::SmolStr;

use pattern_core::plugin::manifest::PluginManifest;
use pattern_core::traits::plugin::{PluginContext, PluginError};

/// Walk the plugin's skills directory and install each SKILL.md as a
/// Pattern skill block with `trust_tier: PluginInstalled`.
pub async fn install_skills(
    _plugin_id: &SmolStr,
    _plugin_root: &Path,
    _manifest: &PluginManifest,
    _ctx: &PluginContext,
) -> Result<(), PluginError> {
    // TODO: Task 4 implements this.
    // Walk <plugin_root>/skills/<name>/SKILL.md
    // Parse with pattern_memory::fs::markdown_skill::parse
    // Set trust_tier = PluginInstalled, source_plugin_id = Some(plugin_id)
    // Create skill blocks via the memory store
    Ok(())
}
