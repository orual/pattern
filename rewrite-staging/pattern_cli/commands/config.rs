use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use pattern_core::config::{self, PatternConfig};
use std::path::{Path, PathBuf};

use crate::output::Output;

/// Show current configuration
pub async fn show(config: &PatternConfig, output: &Output) -> Result<()> {
    output.section("Current Configuration");
    output.print("");

    // Display the current config in TOML format
    let toml_str = toml::to_string_pretty(config).into_diagnostic()?;
    // Print the TOML directly without indentation since it's already formatted
    for line in toml_str.lines() {
        output.print(line);
    }

    Ok(())
}

/// Save current configuration to file
pub async fn save(config: &PatternConfig, path: &PathBuf, output: &Output) -> Result<()> {
    output.info(
        "💾",
        &format!("Saving configuration to: {}", path.display()),
    );

    // Save the current config
    config::save_config(config, path).await?;

    output.success("Configuration saved successfully!");
    output.print("");
    output.status("To use this configuration, run:");
    output.status(&format!(
        "{} --config {}",
        "pattern-cli".bright_green(),
        path.display()
    ));

    Ok(())
}

/// Migrate a config file from old format to new.
///
/// Converts:
/// - [agent] to [[agents]]
/// - Removes [user] block
pub async fn migrate(path: &Path, in_place: bool) -> Result<()> {
    use toml_edit::DocumentMut;

    let output = Output::new();

    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| miette::miette!("Failed to read {}: {}", path.display(), e))?;

    let mut doc: DocumentMut = content
        .parse()
        .map_err(|e| miette::miette!("Failed to parse TOML: {}", e))?;

    let mut changed = false;

    // Convert [agent] to [[agents]].
    if let Some(agent) = doc.remove("agent") {
        // Create array of tables with the agent.
        let mut arr = toml_edit::ArrayOfTables::new();
        if let toml_edit::Item::Table(table) = agent {
            arr.push(table);
        }
        doc["agents"] = toml_edit::Item::ArrayOfTables(arr);
        changed = true;
        output.success("Converted [agent] to [[agents]]");
    }

    // Remove [user].
    if doc.remove("user").is_some() {
        changed = true;
        output.success("Removed [user] block");
    }

    if !changed {
        output.status("No changes needed - config is already in new format");
        return Ok(());
    }

    let migrated_content = doc.to_string();

    if in_place {
        tokio::fs::write(path, &migrated_content)
            .await
            .map_err(|e| miette::miette!("Failed to write {}: {}", path.display(), e))?;
        output.success(&format!("Migrated {}", path.display()));
    } else {
        output.section("Migrated config");
        output.print(&migrated_content);
    }

    Ok(())
}
