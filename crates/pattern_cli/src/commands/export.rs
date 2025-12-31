//! Export and import commands for agents, groups, and constellations
//!
//! This module provides CAR (Content Addressable aRchive) export/import
//! for agents, including message history and memory blocks.

use miette::Result;
use owo_colors::OwoColorize;
use std::path::PathBuf;
use tokio::fs::File;
use tokio::io::BufReader;

use pattern_core::config::PatternConfig;
use pattern_core::export::{ExportOptions, ExportTarget, Exporter, ImportOptions, Importer};

use crate::helpers::{get_db, require_agent_by_name, require_group_by_name};
use crate::output::Output;

// =============================================================================
// Agent Export
// =============================================================================

/// Export an agent to a CAR file
///
/// Exports the agent with all memory blocks, messages, archival entries,
/// and archive summaries.
pub async fn export_agent(
    name: &str,
    output_path: Option<PathBuf>,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Look up agent by name
    let agent = require_agent_by_name(&db, name).await?;

    // Determine output file path
    let file_path = output_path.unwrap_or_else(|| {
        let safe_name = name.replace(' ', "-");
        PathBuf::from(format!("{}.car", safe_name))
    });

    output.status(&format!(
        "Exporting agent '{}' to {}...",
        name.bright_cyan(),
        file_path.display()
    ));

    // Create export options
    let options = ExportOptions {
        target: ExportTarget::Agent(agent.id.clone()),
        include_messages: true,
        include_archival: true,
        ..Default::default()
    };

    // Create exporter and export
    let exporter = Exporter::new(db.pool().clone());
    let file = File::create(&file_path)
        .await
        .map_err(|e| miette::miette!("Failed to create output file: {}", e))?;

    let manifest = exporter
        .export_agent(&agent.id, file, &options)
        .await
        .map_err(|e| miette::miette!("Export failed: {:?}", e))?;

    // Display results
    output.success(&format!("Exported agent '{}'", name.bright_cyan()));
    output.kv("File", &file_path.display().to_string());
    output.kv("Version", &manifest.version.to_string());
    output.kv("Export type", &format!("{:?}", manifest.export_type));
    output.kv("Messages", &manifest.stats.message_count.to_string());
    output.kv(
        "Memory blocks",
        &manifest.stats.memory_block_count.to_string(),
    );
    output.kv(
        "Archival entries",
        &manifest.stats.archival_entry_count.to_string(),
    );
    output.kv(
        "Archive summaries",
        &manifest.stats.archive_summary_count.to_string(),
    );
    output.kv("Total blocks", &manifest.stats.total_blocks.to_string());
    output.kv("Total bytes", &format_bytes(manifest.stats.total_bytes));

    Ok(())
}

// =============================================================================
// Group Export
// =============================================================================

/// Export a group to a CAR file
///
/// Exports the group configuration and all member agents with their data.
pub async fn export_group(
    name: &str,
    output_path: Option<PathBuf>,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Look up group by name
    let group = require_group_by_name(&db, name).await?;

    // Determine output file path
    let file_path = output_path.unwrap_or_else(|| {
        let safe_name = name.replace(' ', "-");
        PathBuf::from(format!("{}.car", safe_name))
    });

    output.status(&format!(
        "Exporting group '{}' to {}...",
        name.bright_cyan(),
        file_path.display()
    ));

    // Create export options (full export, not thin)
    let options = ExportOptions {
        target: ExportTarget::Group {
            id: group.id.clone(),
            thin: false,
        },
        include_messages: true,
        include_archival: true,
        ..Default::default()
    };

    // Create exporter and export
    let exporter = Exporter::new(db.pool().clone());
    let file = File::create(&file_path)
        .await
        .map_err(|e| miette::miette!("Failed to create output file: {}", e))?;

    let manifest = exporter
        .export_group(&group.id, file, &options)
        .await
        .map_err(|e| miette::miette!("Export failed: {:?}", e))?;

    // Display results
    output.success(&format!("Exported group '{}'", name.bright_cyan()));
    output.kv("File", &file_path.display().to_string());
    output.kv("Version", &manifest.version.to_string());
    output.kv("Export type", &format!("{:?}", manifest.export_type));
    output.kv("Agents", &manifest.stats.agent_count.to_string());
    output.kv("Groups", &manifest.stats.group_count.to_string());
    output.kv("Messages", &manifest.stats.message_count.to_string());
    output.kv(
        "Memory blocks",
        &manifest.stats.memory_block_count.to_string(),
    );
    output.kv(
        "Archival entries",
        &manifest.stats.archival_entry_count.to_string(),
    );
    output.kv("Total blocks", &manifest.stats.total_blocks.to_string());
    output.kv("Total bytes", &format_bytes(manifest.stats.total_bytes));

    Ok(())
}

// =============================================================================
// Constellation Export
// =============================================================================

/// Export a constellation to a CAR file
///
/// Exports all agents and groups for the current user with agent deduplication.
pub async fn export_constellation(
    output_path: Option<PathBuf>,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Use a default owner ID for CLI exports
    let owner_id = "cli-user";

    // Determine output file path
    let file_path = output_path.unwrap_or_else(|| PathBuf::from("constellation.car"));

    output.status(&format!(
        "Exporting constellation to {}...",
        file_path.display()
    ));

    // Create export options
    let options = ExportOptions {
        target: ExportTarget::Constellation,
        include_messages: true,
        include_archival: true,
        ..Default::default()
    };
    // Create exporter and export
    let exporter = Exporter::new(db.pool().clone());
    let file = File::create(&file_path)
        .await
        .map_err(|e| miette::miette!("Failed to create output file: {}", e))?;

    let manifest = exporter
        .export_constellation(owner_id, file, &options)
        .await
        .map_err(|e| miette::miette!("Export failed: {:?}", e))?;

    // Display results
    output.success("Exported constellation");
    output.kv("File", &file_path.display().to_string());
    output.kv("Version", &manifest.version.to_string());
    output.kv("Export type", &format!("{:?}", manifest.export_type));
    output.kv("Agents", &manifest.stats.agent_count.to_string());
    output.kv("Groups", &manifest.stats.group_count.to_string());
    output.kv("Messages", &manifest.stats.message_count.to_string());
    output.kv(
        "Memory blocks",
        &manifest.stats.memory_block_count.to_string(),
    );
    output.kv(
        "Archival entries",
        &manifest.stats.archival_entry_count.to_string(),
    );
    output.kv(
        "Archive summaries",
        &manifest.stats.archive_summary_count.to_string(),
    );
    output.kv("Total blocks", &manifest.stats.total_blocks.to_string());
    output.kv("Total bytes", &format_bytes(manifest.stats.total_bytes));

    Ok(())
}

// =============================================================================
// Import
// =============================================================================

/// Import from a CAR file
///
/// Auto-detects the export type (agent, group, or constellation) and imports
/// the data into the database.
pub async fn import(
    file_path: PathBuf,
    rename_to: Option<String>,
    preserve_ids: bool,
    config: &PatternConfig,
) -> Result<()> {
    let output = Output::new();
    let db = get_db(config).await?;

    // Check file exists
    if !file_path.exists() {
        return Err(miette::miette!("File not found: {}", file_path.display()));
    }

    output.status(&format!(
        "Importing from {}...",
        file_path.display().to_string().bright_cyan()
    ));

    // Open the file
    let file = File::open(&file_path)
        .await
        .map_err(|e| miette::miette!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);

    // Create import options
    let mut options = ImportOptions::new("cli-user");
    if let Some(name) = &rename_to {
        options = options.with_rename(name);
    }
    options = options.with_preserve_ids(preserve_ids);

    // Create importer and import
    let importer = Importer::new(db.pool().clone());
    let result = importer
        .import(reader, &options)
        .await
        .map_err(|e| miette::miette!("Import failed: {:?}", e))?;

    // Display results
    output.success(&format!(
        "Imported from {}",
        file_path.display().to_string().bright_cyan()
    ));

    if !result.agent_ids.is_empty() {
        output.kv("Agents imported", &result.agent_ids.len().to_string());
        for agent_id in &result.agent_ids {
            output.list_item(agent_id);
        }
    }

    if !result.group_ids.is_empty() {
        output.kv("Groups imported", &result.group_ids.len().to_string());
        for group_id in &result.group_ids {
            output.list_item(group_id);
        }
    }

    output.kv("Messages", &result.message_count.to_string());
    output.kv("Memory blocks", &result.memory_block_count.to_string());
    output.kv("Archival entries", &result.archival_entry_count.to_string());
    output.kv(
        "Archive summaries",
        &result.archive_summary_count.to_string(),
    );

    if let Some(name) = rename_to {
        output.info("Renamed to:", &name);
    }

    if preserve_ids {
        output.info("IDs:", "preserved from original");
    } else {
        output.info("IDs:", "newly generated");
    }

    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Format bytes into a human-readable string
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}
