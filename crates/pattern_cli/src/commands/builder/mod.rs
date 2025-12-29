//! Interactive builder system for agents and groups.
//!
//! Provides a shared infrastructure for building and editing configurations
//! interactively via the terminal.

mod display;
mod editors;
mod save;

pub mod agent;
pub mod group;

use std::path::PathBuf;

use miette::Result;

/// Where the configuration originated from.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// Created from scratch with defaults.
    New,
    /// Loaded from a TOML file.
    #[allow(dead_code)]
    FromFile(PathBuf),
    /// Loaded from an existing database entry.
    #[allow(dead_code)]
    FromDb(String),
}

/// Section identifiers for the builder menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    BasicInfo,
    Model,
    MemoryBlocks,
    ToolsAndRules,
    ContextOptions,
    Integrations,
    // Group-specific sections
    Pattern,
    Members,
    #[allow(dead_code)]
    SharedMemory,
    #[allow(dead_code)]
    DataSources,
}

impl Section {
    /// Display name for the section.
    pub fn display_name(&self) -> &'static str {
        match self {
            Section::BasicInfo => "Basic Info",
            Section::Model => "Model",
            Section::MemoryBlocks => "Memory Blocks",
            Section::ToolsAndRules => "Tools & Rules",
            Section::ContextOptions => "Context Options",
            Section::Integrations => "Integrations",
            Section::Pattern => "Coordination Pattern",
            Section::Members => "Members",
            Section::SharedMemory => "Shared Memory",
            Section::DataSources => "Data Sources",
        }
    }
}

/// Destination for saving the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveDestination {
    Database,
    File,
    Both,
    Preview,
    Cancel,
}

impl SaveDestination {
    pub fn display_name(&self) -> &'static str {
        match self {
            SaveDestination::Database => "Save to database",
            SaveDestination::File => "Export to file",
            SaveDestination::Both => "Both (database + file)",
            SaveDestination::Preview => "Preview (show TOML)",
            SaveDestination::Cancel => "Cancel",
        }
    }
}

/// Menu choice in the main builder loop.
#[derive(Debug, Clone)]
pub enum MenuChoice {
    EditSection(Section),
    Done,
    Cancel,
}

/// Path to the builder state cache file.
pub fn state_cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pattern")
        .join("builder-state.toml")
}

/// Write current state to the cache file for recovery.
pub fn write_state_cache(content: &str) -> Result<()> {
    let path = state_cache_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| miette::miette!("Failed to create cache directory: {}", e))?;
    }
    std::fs::write(&path, content)
        .map_err(|e| miette::miette!("Failed to write state cache: {}", e))?;
    Ok(())
}
