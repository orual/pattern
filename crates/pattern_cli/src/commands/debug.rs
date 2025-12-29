//! Debug commands for agent inspection and memory management
//!
//! This module provides debugging utilities for inspecting agent state,
//! memory blocks, message history, and context.
//!
//! Uses pattern_db::queries for direct database access via shared helpers.

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::PatternConfig;
use pattern_db::search::ContentFilter;

use crate::helpers::{get_db, load_config, require_agent_by_name};
use crate::output::Output;

// =============================================================================
// Search Conversations
// =============================================================================

/// Search conversation history for an agent using FTS.
pub async fn search_conversations(
    agent_name: &str,
    query: Option<&str>,
    _role: Option<&str>,
    _start_time: Option<&str>,
    _end_time: Option<&str>,
    limit: usize,
) -> Result<()> {
    let output = Output::new();
    let config = load_config().await?;
    let db = get_db(&config).await?;

    // Find agent
    let agent = require_agent_by_name(&db, agent_name).await?;

    let query_text = match query {
        Some(q) => q,
        None => {
            output.warning("No search query provided. Use --query to search.");
            return Ok(());
        }
    };

    output.status(&format!(
        "Searching conversations for '{}': \"{}\"",
        agent_name.bright_cyan(),
        query_text
    ));
    output.status("");

    // Use pattern_db's hybrid search
    let results = pattern_db::search::search(db.pool())
        .text(query_text)
        .filter(ContentFilter::messages(Some(&agent.id)))
        .limit(limit as i64)
        .execute()
        .await
        .map_err(|e| miette::miette!("Search failed: {}", e))?;

    if results.is_empty() {
        output.info("No results found", "");
        return Ok(());
    }

    output.status(&format!("Found {} result(s):", results.len()));
    output.status("");

    for result in results {
        // Display result
        let score_str = format!("{:.3}", result.score);
        output.info(
            &format!("  [{}]", score_str.dimmed()),
            &result.id.bright_yellow().to_string(),
        );

        if let Some(content) = &result.content {
            let preview = if content.len() > 200 {
                format!("{}...", &content[..200])
            } else {
                content.clone()
            };
            output.status(&format!("    {}", preview.dimmed()));
        }
        output.status("");
    }

    Ok(())
}

// =============================================================================
// Search Archival Memory
// =============================================================================

/// Search archival memory using FTS.
pub async fn search_archival_memory(agent_name: &str, query: &str, limit: usize) -> Result<()> {
    let output = Output::new();
    let config = load_config().await?;
    let db = get_db(&config).await?;

    // Find agent
    let agent = require_agent_by_name(&db, agent_name).await?;

    output.status(&format!(
        "Searching archival memory for '{}': \"{}\"",
        agent_name.bright_cyan(),
        query
    ));
    output.status("");

    // Use pattern_db's hybrid search for archival entries
    let results = pattern_db::search::search(db.pool())
        .text(query)
        .filter(ContentFilter::archival(Some(&agent.id)))
        .limit(limit as i64)
        .execute()
        .await
        .map_err(|e| miette::miette!("Search failed: {}", e))?;

    if results.is_empty() {
        output.info("No results found", "");
        return Ok(());
    }

    output.status(&format!("Found {} result(s):", results.len()));
    output.status("");

    for result in results {
        let score_str = format!("{:.3}", result.score);
        output.info(
            &format!("  [{}]", score_str.dimmed()),
            &result.id.bright_yellow().to_string(),
        );

        if let Some(content) = &result.content {
            let preview = if content.len() > 200 {
                format!("{}...", &content[..200])
            } else {
                content.clone()
            };
            output.status(&format!("    {}", preview.dimmed()));
        }
        output.status("");
    }

    Ok(())
}

// =============================================================================
// List Archival Memory
// =============================================================================

/// List all archival memory entries for an agent.
///
/// Uses pattern_db::queries::list_archival_entries() to fetch entries.
pub async fn list_archival_memory(agent_name: &str) -> Result<()> {
    let output = Output::new();
    let config = load_config().await?;
    let db = get_db(&config).await?;

    // Find agent by name using shared helper
    let agent = require_agent_by_name(&db, agent_name).await?;

    // Get archival entries (use large limit to get all)
    let entries = pattern_db::queries::list_archival_entries(db.pool(), &agent.id, 1000, 0)
        .await
        .map_err(|e| miette::miette!("Failed to list archival entries: {}", e))?;

    output.status(&format!(
        "Archival Memory for '{}' ({} entries):",
        agent_name.bright_cyan(),
        entries.len()
    ));
    output.status("");

    if entries.is_empty() {
        output.info("(no archival entries)", "");
        return Ok(());
    }

    for entry in entries {
        // Show entry ID and creation time
        output.info("Entry:", &entry.id.bright_yellow().to_string());
        output.kv("  Created", &entry.created_at.to_string());

        // Show content (truncated if long)
        let content_preview = if entry.content.len() > 200 {
            format!("{}...", &entry.content[..200])
        } else {
            entry.content.clone()
        };
        output.kv("  Content", &content_preview.dimmed().to_string());

        // Show metadata if present
        if let Some(meta) = &entry.metadata {
            let meta_str =
                serde_json::to_string(&meta.0).unwrap_or_else(|_| "(invalid json)".to_string());
            if meta_str != "{}" && meta_str != "null" {
                output.kv("  Metadata", &meta_str.dimmed().to_string());
            }
        }

        // Show chunk info if this is part of a larger entry
        if entry.chunk_index > 0 || entry.parent_entry_id.is_some() {
            output.kv("  Chunk", &format!("#{}", entry.chunk_index));
            if let Some(parent) = &entry.parent_entry_id {
                output.kv("  Parent", parent);
            }
        }

        output.status("");
    }

    Ok(())
}

// =============================================================================
// List Core Memory
// =============================================================================

/// List all core/working memory blocks for an agent.
///
/// Uses pattern_db::queries::list_blocks() to fetch memory blocks,
/// then filters for Core and Working types.
pub async fn list_core_memory(agent_name: &str) -> Result<()> {
    let output = Output::new();
    let config = load_config().await?;
    let db = get_db(&config).await?;

    // Find agent by name using shared helper
    let agent = require_agent_by_name(&db, agent_name).await?;

    // Get all memory blocks for this agent
    let blocks = pattern_db::queries::list_blocks(db.pool(), &agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to list memory blocks: {}", e))?;

    // Filter for Core and Working types
    let core_blocks: Vec<_> = blocks
        .into_iter()
        .filter(|b| {
            matches!(
                b.block_type,
                pattern_db::models::MemoryBlockType::Core
                    | pattern_db::models::MemoryBlockType::Working
            )
        })
        .collect();

    output.status(&format!(
        "Core Memory for '{}' ({} blocks):",
        agent_name.bright_cyan(),
        core_blocks.len()
    ));
    output.status("");

    if core_blocks.is_empty() {
        output.info("(no core memory blocks)", "");
        return Ok(());
    }

    for block in core_blocks {
        // Block header with label and type
        let type_str = format!("{:?}", block.block_type).to_lowercase();
        output.info(
            &format!("{} ({})", block.label.bright_yellow(), type_str),
            "",
        );

        // Metadata
        output.kv("  ID", &block.id);
        output.kv("  Permission", &format!("{}", block.permission));
        output.kv("  Char Limit", &block.char_limit.to_string());
        if block.pinned {
            output.kv("  Pinned", "yes");
        }

        // Description
        if !block.description.is_empty() {
            output.kv("  Description", &block.description.dimmed().to_string());
        }

        // Content preview (core blocks are usually small enough to show fully)
        if let Some(preview) = &block.content_preview {
            output.status("  Content:");
            // Indent the content
            for line in preview.lines() {
                output.status(&format!("    {}", line.dimmed()));
            }
        } else {
            output.kv("  Content", "(empty)");
        }

        output.status("");
    }

    Ok(())
}

// =============================================================================
// List All Memory
// =============================================================================

/// List all memory for an agent (blocks and archival entries).
///
/// Combines list_blocks() for memory blocks and list_archival_entries()
/// for archival memory, grouped by type.
pub async fn list_all_memory(agent_name: &str) -> Result<()> {
    let output = Output::new();
    let config = load_config().await?;
    let db = get_db(&config).await?;

    // Find agent by name using shared helper
    let agent = require_agent_by_name(&db, agent_name).await?;

    // Get all memory blocks
    let blocks = pattern_db::queries::list_blocks(db.pool(), &agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to list memory blocks: {}", e))?;

    // Get archival entry count
    let archival_count = pattern_db::queries::count_archival_entries(db.pool(), &agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to count archival entries: {}", e))?;

    output.status(&format!("All Memory for '{}':", agent_name.bright_cyan()));
    output.status("");

    // Group blocks by type
    let mut core_blocks = Vec::new();
    let mut working_blocks = Vec::new();
    let mut log_blocks = Vec::new();
    let mut archival_blocks = Vec::new();

    for block in blocks {
        match block.block_type {
            pattern_db::models::MemoryBlockType::Core => core_blocks.push(block),
            pattern_db::models::MemoryBlockType::Working => working_blocks.push(block),
            pattern_db::models::MemoryBlockType::Log => log_blocks.push(block),
            pattern_db::models::MemoryBlockType::Archival => archival_blocks.push(block),
        }
    }

    // Core Memory Section
    output.section(&format!("Core Memory ({} blocks)", core_blocks.len()));
    if core_blocks.is_empty() {
        output.info("  (none)", "");
    } else {
        for block in &core_blocks {
            let preview = block
                .content_preview
                .as_deref()
                .map(|s| {
                    if s.len() > 60 {
                        format!("{}...", &s[..60])
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "(empty)".to_string());
            output.info(
                &format!("  {}", block.label.bright_yellow()),
                &preview.dimmed().to_string(),
            );
        }
    }
    output.status("");

    // Working Memory Section
    output.section(&format!("Working Memory ({} blocks)", working_blocks.len()));
    if working_blocks.is_empty() {
        output.info("  (none)", "");
    } else {
        for block in &working_blocks {
            let preview = block
                .content_preview
                .as_deref()
                .map(|s| {
                    if s.len() > 60 {
                        format!("{}...", &s[..60])
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "(empty)".to_string());
            output.info(
                &format!("  {}", block.label.bright_yellow()),
                &preview.dimmed().to_string(),
            );
        }
    }
    output.status("");

    // Log Memory Section
    if !log_blocks.is_empty() {
        output.section(&format!("Log Memory ({} blocks)", log_blocks.len()));
        for block in &log_blocks {
            let preview = block
                .content_preview
                .as_deref()
                .map(|s| {
                    if s.len() > 60 {
                        format!("{}...", &s[..60])
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "(empty)".to_string());
            output.info(
                &format!("  {}", block.label.bright_yellow()),
                &preview.dimmed().to_string(),
            );
        }
        output.status("");
    }

    // Archival Memory Section (entries, not blocks)
    output.section(&format!("Archival Memory ({} entries)", archival_count));
    if archival_count == 0 {
        output.info("  (none)", "");
    } else {
        output.info(
            "  ",
            &format!("Use 'debug list-archival {}' to view entries", agent_name)
                .dimmed()
                .to_string(),
        );
    }

    // Also show archival blocks if any
    if !archival_blocks.is_empty() {
        output.status("");
        output.section(&format!(
            "Archival Blocks ({} blocks)",
            archival_blocks.len()
        ));
        for block in &archival_blocks {
            let preview = block
                .content_preview
                .as_deref()
                .map(|s| {
                    if s.len() > 60 {
                        format!("{}...", &s[..60])
                    } else {
                        s.to_string()
                    }
                })
                .unwrap_or_else(|| "(empty)".to_string());
            output.info(
                &format!("  {}", block.label.bright_yellow()),
                &preview.dimmed().to_string(),
            );
        }
    }

    // Summary
    output.status("");
    let total_blocks =
        core_blocks.len() + working_blocks.len() + log_blocks.len() + archival_blocks.len();
    output.kv(
        "Total",
        &format!(
            "{} blocks, {} archival entries",
            total_blocks, archival_count
        ),
    );

    Ok(())
}

// =============================================================================
// Show Context - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Found agent by name via raw SurrealDB query
// 2. Loaded full agent via load_or_create_agent
// 3. Called agent.system_prompt() to get current prompt
// 4. Called agent.available_tools() to list tools
// 5. Displayed formatted context
//
// Needs: RuntimeContext for agent loading

/// Show the current context that would be passed to the LLM
///
/// NOTE: Currently STUBBED. Needs RuntimeContext for agent loading.
pub async fn show_context(agent_name: &str, _config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Context display for '{}' temporarily disabled during database migration",
        agent_name.bright_cyan()
    ));

    output.info("Reason:", "Needs RuntimeContext for agent loading");
    output.status("Previous functionality:");
    output.list_item("Full system prompt as passed to LLM");
    output.list_item("Available tools list with descriptions");
    output.list_item("Memory block values embedded in prompt");

    Ok(())
}

// =============================================================================
// Edit Memory - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Found agent by name via raw SurrealDB query
// 2. Found memory block by label via agent_memories relation
// 3. Exported content to temp file
// 4. Waited for user to edit file
// 5. Read edited content and updated database
//
// Needs: pattern_db::queries::{get_memory_block, update_memory_block}

/// Edit a memory block by exporting to file and reimporting after edits
///
/// NOTE: Currently STUBBED. Needs pattern_db memory queries.
pub async fn edit_memory(agent_name: &str, label: &str, file_path: Option<&str>) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Memory editing for '{}' temporarily disabled during database migration",
        agent_name.bright_cyan()
    ));

    output.info("Block label:", label);
    if let Some(path) = file_path {
        output.info("File path:", path);
    }
    output.info("Reason:", "Needs pattern_db memory update queries");
    output.status("Previous functionality:");
    output.list_item("Export memory block to temp file");
    output.list_item("Wait for user to edit");
    output.list_item("Import edited content back to database");

    Ok(())
}

// =============================================================================
// Modify Memory - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Found agent by name via raw SurrealDB query
// 2. Found memory block by label
// 3. Updated specified fields (label, permission, memory_type)
//
// Needs: pattern_db::queries::update_memory_block()

/// Modify memory block metadata
///
/// NOTE: Currently STUBBED. Needs pattern_db memory update queries.
pub async fn modify_memory(
    agent: &String,
    label: &String,
    new_label: &Option<String>,
    permission: &Option<String>,
    memory_type: &Option<String>,
) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Memory modification for '{}' temporarily disabled during database migration",
        agent.bright_cyan()
    ));

    output.info("Current label:", label);
    if let Some(nl) = new_label {
        output.info("New label:", nl);
    }
    if let Some(p) = permission {
        output.info("Permission:", p);
    }
    if let Some(mt) = memory_type {
        output.info("Memory type:", mt);
    }
    output.info(
        "Reason:",
        "Needs pattern_db::queries::update_memory_block()",
    );

    Ok(())
}

// =============================================================================
// Context Cleanup - STUBBED
// =============================================================================

// TODO: Reimplement for pattern_db (SQLite/sqlx)
//
// Previous implementation:
// 1. Found agent by name via raw SurrealDB query
// 2. Created AgentHandle and searched all messages
// 3. Analyzed for unpaired tool calls/results
// 4. Detected out-of-order tool call/result pairs
// 5. Optionally deleted problematic messages
//
// Needs: pattern_db::queries::{get_agent_messages, delete_message}

/// Clean up message context by removing unpaired/out-of-order messages
///
/// NOTE: Currently STUBBED. Needs pattern_db message queries.
pub async fn context_cleanup(
    agent_name: &str,
    interactive: bool,
    dry_run: bool,
    limit: Option<usize>,
) -> Result<()> {
    let output = Output::new();

    output.warning(&format!(
        "Context cleanup for '{}' temporarily disabled during database migration",
        agent_name.bright_cyan()
    ));

    output.info("Interactive:", if interactive { "yes" } else { "no" });
    output.info("Dry run:", if dry_run { "yes" } else { "no" });
    if let Some(l) = limit {
        output.info("Limit:", &l.to_string());
    }
    output.info("Reason:", "Needs pattern_db message queries");
    output.status("Previous functionality:");
    output.list_item("Detect unpaired tool calls (no matching results)");
    output.list_item("Detect unpaired tool results (no matching calls)");
    output.list_item("Detect out-of-order tool call/result pairs");
    output.list_item("Interactive confirmation for deletion");
    output.list_item("Dry run mode to preview changes");

    Ok(())
}
