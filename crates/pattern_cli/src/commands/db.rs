//! Database inspection commands
//!
//! Provides database statistics and inspection for debugging purposes.

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::PatternConfig;

use crate::helpers::get_db;
use crate::output::Output;

/// Show database statistics
pub async fn stats(config: &PatternConfig, output: &Output) -> Result<()> {
    let db = get_db(config).await?;

    output.success("Database Statistics");
    output.status("");

    // Get overall stats
    let stats = pattern_db::queries::get_stats(db.pool())
        .await
        .map_err(|e| miette::miette!("Failed to get stats: {}", e))?;

    // Display counts
    output.section("Entity Counts");
    output.kv("Agents", &stats.agent_count.to_string());
    output.kv("Groups", &stats.group_count.to_string());
    output.kv("Messages", &stats.message_count.to_string());
    output.kv("Memory Blocks", &stats.memory_block_count.to_string());
    output.kv("Archival Entries", &stats.archival_entry_count.to_string());

    // Database file info
    output.status("");
    output.section("Database Info");
    output.kv("Path", &config.database.path.display().to_string());

    // Try to get file size
    if let Ok(metadata) = std::fs::metadata(&config.database.path) {
        let size = metadata.len();
        let size_str = if size < 1024 {
            format!("{} B", size)
        } else if size < 1024 * 1024 {
            format!("{:.1} KB", size as f64 / 1024.0)
        } else {
            format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
        };
        output.kv("Size", &size_str);
    }

    // Most active agents by message count
    output.status("");
    output.section("Most Active Agents");

    let active_agents = pattern_db::queries::get_most_active_agents(db.pool(), 5)
        .await
        .map_err(|e| miette::miette!("Failed to get active agents: {}", e))?;

    if active_agents.is_empty() {
        output.info("  (no agents)", "");
    } else {
        for agent in active_agents {
            output.info(
                &format!("  {}", agent.name.bright_cyan()),
                &format!("{} messages", agent.message_count),
            );
        }
    }

    Ok(())
}
