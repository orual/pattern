// TODO: CLI Refactoring for pattern_db (SQLite/sqlx) migration
//!
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
    _config: &PatternConfig,
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
    output.info("Preserve IDs:", if preserve_ids { "yes" } else { "no" });
    output.info("Reason:", "Needs pattern_db queries for entity creation");
    output.status("CAR import previously supported:");
    output.list_item("Auto-detection of export type (agent, group, constellation)");
    output.list_item("Creating/updating agents");
    output.list_item("Importing memory blocks");
    output.list_item("Replaying message history");
    output.list_item("Preserving batch boundaries");
    output.list_item("ID remapping for conflict resolution");

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
// Convert Legacy CAR Files (requires legacy-convert feature)
// =============================================================================

/// Convert a v1/v2 CAR file to v3 format
///
/// Reads the old CAR file, converts all data structures to v3 format,
/// and writes a new CAR file.
#[cfg(feature = "legacy-convert")]
pub async fn convert_car(input_path: PathBuf, output_path: Option<PathBuf>) -> Result<()> {
    let output = Output::new();

    // Determine output path
    let out_path = output_path.unwrap_or_else(|| {
        let stem = input_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("converted");
        input_path
            .parent()
            .unwrap_or(&PathBuf::from("."))
            .join(format!("{}_v3.car", stem))
    });

    output.status(&format!(
        "Converting {} to v3 format...",
        input_path.display().to_string().bright_cyan()
    ));

    // Run the conversion with default options
    let options = pattern_surreal_compat::convert::ConversionOptions::default();
    let stats =
        pattern_surreal_compat::convert::convert_car_v1v2_to_v3(&input_path, &out_path, &options)
            .await
            .map_err(|e| miette::miette!("Conversion failed: {:?}", e))?;

    // Display results
    output.success("Conversion complete");
    output.kv("Input version", &format!("v{}", stats.input_version));
    output.kv("Output file", &out_path.display().to_string());
    output.kv("Agents converted", &stats.agents_converted.to_string());
    output.kv("Groups converted", &stats.groups_converted.to_string());
    output.kv("Messages converted", &stats.messages_converted.to_string());
    output.kv(
        "Memory blocks converted",
        &stats.memory_blocks_converted.to_string(),
    );
    output.kv(
        "Archival entries converted",
        &stats.archival_entries_converted.to_string(),
    );

    Ok(())
}

// =============================================================================
// Convert Letta Agent Files
// =============================================================================

/// Convert a Letta agent file (.af) to v3 CAR format
///
/// Reads the Letta JSON format and converts to Pattern's v3 CAR format.
pub async fn convert_letta(input_path: PathBuf, output_path: Option<PathBuf>) -> Result<()> {
    use pattern_core::export::{LettaConversionOptions, convert_letta_to_car};

    let output = Output::new();

    // Determine output path
    let out_path = output_path.unwrap_or_else(|| {
        let stem = input_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("converted");
        input_path
            .parent()
            .unwrap_or(&PathBuf::from("."))
            .join(format!("{}.car", stem))
    });

    output.status(&format!(
        "Converting Letta agent file {} to v3 CAR...",
        input_path.display().to_string().bright_cyan()
    ));

    // Run the conversion with default options
    let options = LettaConversionOptions::default();
    let stats = convert_letta_to_car(&input_path, &out_path, &options)
        .await
        .map_err(|e| miette::miette!("Conversion failed: {:?}", e))?;

    // Display results
    output.success("Conversion complete");
    output.kv("Output file", &out_path.display().to_string());
    output.kv("Agents converted", &stats.agents_converted.to_string());
    output.kv("Groups converted", &stats.groups_converted.to_string());
    output.kv("Messages converted", &stats.messages_converted.to_string());
    output.kv(
        "Memory blocks converted",
        &stats.memory_blocks_converted.to_string(),
    );
    output.kv("Tools mapped", &stats.tools_mapped.to_string());
    output.kv("Tools dropped", &stats.tools_dropped.to_string());

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
