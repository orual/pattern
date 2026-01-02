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

use crate::helpers::{
    create_runtime_context, get_agent_by_name, get_db, get_dbs, load_config, require_agent_by_name,
};
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
// Show Context
// =============================================================================

/// Show the current context that would be passed to the LLM.
///
/// Uses `prepare_request()` with empty messages to build the actual context,
/// showing what would be sent to the model.
pub async fn show_context(agent_name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    // Load agent via RuntimeContext
    let ctx = create_runtime_context(config).await?;
    let dbs = get_dbs(config).await?;

    let db_agent = match get_agent_by_name(&dbs.constellation, agent_name).await? {
        Some(a) => a,
        None => {
            output.error(&format!("Agent '{}' not found", agent_name));
            return Ok(());
        }
    };

    let agent = ctx
        .load_agent(&db_agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to load agent '{}': {}", agent_name, e))?;

    output.section(&format!("Context for: {}", agent.name().bright_cyan()));

    // Build the ACTUAL context using prepare_request with empty messages
    let runtime = agent.runtime();
    match runtime
        .prepare_request(
            Vec::<pattern_core::messages::Message>::new(),
            None,
            None,
            None,
            Some(&db_agent.system_prompt),
        )
        .await
    {
        Ok(request) => {
            // System prompt section
            output.print("");
            if let Some(ref system_parts) = request.system {
                let total_chars: usize = system_parts.iter().map(|s| s.len()).sum();
                output.kv(
                    "System Prompt",
                    &format!(
                        "({} parts, {} chars total)",
                        system_parts.len(),
                        total_chars
                    ),
                );

                for (i, part) in system_parts.iter().enumerate() {
                    let part_type = if i == 0 {
                        "base"
                    } else if part.contains("<block:") {
                        "block"
                    } else if part.contains("# Tool Execution Rules") {
                        "rules"
                    } else {
                        "section"
                    };

                    output.list_item(&format!("[{}] {} chars", part_type.dimmed(), part.len()));
                }

                // Option to show full content
                output.print("");
                output.status("Full system prompt parts:");
                for (i, part) in system_parts.iter().enumerate() {
                    output.print(&format!("\n--- Part {} ---", i + 1));
                    // Truncate very long parts for display
                    if part.len() > 2000 {
                        output.print(&format!("{}...", &part[..2000]));
                        output.status(&format!("(truncated, {} chars total)", part.len()));
                    } else {
                        output.print(part);
                    }
                }
            } else {
                output.kv("System Prompt", "(none)");
            }

            // Tools section
            output.print("");
            if let Some(ref tools) = request.tools {
                output.kv("Tools", &format!("({})", tools.len()));
                for tool in tools {
                    output.list_item(&format!(
                        "{}: {}",
                        tool.name.bright_cyan(),
                        tool.description.as_deref().unwrap_or("(no description)")
                    ));
                }
            } else {
                output.kv("Tools", "(none)");
            }

            // Messages section
            output.print("");
            output.kv("Messages in context", &request.messages.len().to_string());
        }
        Err(e) => {
            output.error(&format!("Failed to build context: {}", e));
        }
    }

    Ok(())
}

// =============================================================================
// Edit Memory
// =============================================================================

/// Edit a memory block by exporting to file and reimporting after edits.
///
/// Supports all schema types:
/// - Text: exports as plain text
/// - Map/List/Log/Composite: exports as TOML
pub async fn edit_memory(agent_name: &str, label: &str, file_path: Option<&str>) -> Result<()> {
    use std::io::Write;

    let output = Output::new();
    let config = load_config().await?;

    // Load agent via RuntimeContext
    let ctx = create_runtime_context(&config).await?;
    let dbs = get_dbs(&config).await?;

    let db_agent = match get_agent_by_name(&dbs.constellation, agent_name).await? {
        Some(a) => a,
        None => {
            output.error(&format!("Agent '{}' not found", agent_name));
            return Ok(());
        }
    };

    let agent = ctx
        .load_agent(&db_agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to load agent '{}': {}", agent_name, e))?;

    let runtime = agent.runtime();
    let memory = runtime.memory();
    let agent_id = agent.id().to_string();

    // Get the memory block
    let doc = match memory.get_block(&agent_id, label).await {
        Ok(Some(doc)) => doc,
        Ok(None) => {
            output.error(&format!("Memory block '{}' not found", label));
            return Ok(());
        }
        Err(e) => {
            output.error(&format!("Failed to get memory block: {}", e));
            return Ok(());
        }
    };

    // Determine schema type for file extension
    let is_text = matches!(doc.schema(), pattern_core::memory::BlockSchema::Text { .. });
    let extension = if is_text { "txt" } else { "toml" };

    // Export content for editing
    let content = doc.export_for_editing();

    // Create temp file or use provided path
    let edit_path = if let Some(path) = file_path {
        std::path::PathBuf::from(path)
    } else {
        let mut temp = std::env::temp_dir();
        temp.push(format!(
            "pattern_memory_{}_{}.{}",
            agent_name, label, extension
        ));
        temp
    };

    // Write content to file
    let mut file = std::fs::File::create(&edit_path)
        .map_err(|e| miette::miette!("Failed to create temp file: {}", e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| miette::miette!("Failed to write temp file: {}", e))?;

    output.section(&format!("Editing memory block: {}", label.bright_cyan()));
    output.info("Schema:", &format!("{:?}", doc.schema()));
    output.info("File:", &edit_path.display().to_string());
    output.print("");
    output.status("Edit the file and save it. Press Enter when done, or 'q' to cancel.");

    // Wait for user input
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| miette::miette!("Failed to read input: {}", e))?;

    if input.trim().eq_ignore_ascii_case("q") {
        output.warning("Edit cancelled");
        // Clean up temp file if we created it
        if file_path.is_none() {
            let _ = std::fs::remove_file(&edit_path);
        }
        return Ok(());
    }

    // Read edited content
    let edited_content = std::fs::read_to_string(&edit_path)
        .map_err(|e| miette::miette!("Failed to read edited file: {}", e))?;

    // Apply changes based on schema
    if is_text {
        // For text, just set the content directly
        match memory.get_block(&agent_id, label).await {
            Ok(Some(doc)) => {
                if let Err(e) = doc.set_text(&edited_content, true) {
                    output.error(&format!("Failed to update memory block: {}", e));
                } else if let Err(e) = memory.persist_block(&agent_id, label).await {
                    output.error(&format!("Failed to persist memory block: {}", e));
                } else {
                    output.success(&format!("Updated memory block '{}'", label));
                }
            }
            Ok(None) => {
                output.error(&format!("Memory block '{}' not found", label));
            }
            Err(e) => {
                output.error(&format!("Failed to get memory block: {}", e));
            }
        }
    } else {
        // For structured schemas, parse TOML and import via document
        // Strip the comment header if present
        let toml_content = edited_content
            .lines()
            .skip_while(|line| line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");

        match toml::from_str::<serde_json::Value>(&toml_content) {
            Ok(value) => {
                // Import JSON into the document (Loro doc is Arc-shared with cache)
                if let Err(e) = doc.import_from_json(&value) {
                    output.error(&format!("Failed to import changes: {}", e));
                    return Ok(());
                }

                // Mark dirty and persist
                memory.mark_dirty(&agent_id, label);
                if let Err(e) = memory.persist_block(&agent_id, label).await {
                    output.error(&format!("Failed to persist changes: {}", e));
                    return Ok(());
                }

                output.success(&format!("Updated memory block '{}'", label));
            }
            Err(e) => {
                output.error(&format!("Failed to parse edited TOML: {}", e));
            }
        }
    }

    // Clean up temp file if we created it
    if file_path.is_none() {
        let _ = std::fs::remove_file(&edit_path);
    }

    Ok(())
}

// =============================================================================
// Modify Memory
// =============================================================================

/// Modify memory block metadata (label, permission, type).
pub async fn modify_memory(
    agent_name: &String,
    label: &String,
    new_label: &Option<String>,
    permission: &Option<String>,
    memory_type: &Option<String>,
) -> Result<()> {
    let output = Output::new();
    let config = load_config().await?;
    let db = get_db(&config).await?;

    // Find agent and block
    let db_agent = require_agent_by_name(&db, agent_name).await?;

    output.section(&format!("Modifying memory block: {}", label.bright_cyan()));

    let mut modified = false;

    // Update label if specified
    if let Some(nl) = new_label {
        // Check if target label already exists
        match pattern_db::queries::get_block_by_label(db.pool(), &db_agent.id, nl).await {
            Ok(Some(_)) => {
                output.error(&format!("Block with label '{}' already exists", nl));
                return Ok(());
            }
            Ok(None) => {} // Good, no conflict
            Err(e) => {
                output.error(&format!("Failed to check for existing block: {}", e));
                return Ok(());
            }
        }

        // Get current block
        match pattern_db::queries::get_block_by_label(db.pool(), &db_agent.id, label).await {
            Ok(Some(block)) => {
                match pattern_db::queries::update_block_label(db.pool(), &block.id, nl).await {
                    Ok(()) => {
                        output.success(&format!("Renamed block '{}' to '{}'", label, nl));
                        modified = true;
                    }
                    Err(e) => {
                        output.error(&format!("Failed to rename block: {}", e));
                    }
                }
            }
            Ok(None) => {
                output.error(&format!("Memory block '{}' not found", label));
                return Ok(());
            }
            Err(e) => {
                output.error(&format!("Failed to get memory block: {}", e));
                return Ok(());
            }
        }
    }

    // Update permission if specified
    if let Some(perm_str) = permission {
        let perm = match perm_str.to_lowercase().as_str() {
            "readonly" | "ro" => pattern_db::MemoryPermission::ReadOnly,
            "readwrite" | "rw" => pattern_db::MemoryPermission::ReadWrite,
            "append" | "a" => pattern_db::MemoryPermission::Append,
            "admin" => pattern_db::MemoryPermission::Admin,
            "partner" => pattern_db::MemoryPermission::Partner,
            "human" => pattern_db::MemoryPermission::Human,
            _ => {
                output.error(&format!("Invalid permission: {}", perm_str));
                output.status("Valid: readonly, readwrite, append, admin, partner, human");
                return Ok(());
            }
        };

        // Get block ID from database
        match pattern_db::queries::get_block_by_label(db.pool(), &db_agent.id, label).await {
            Ok(Some(block)) => {
                match pattern_db::queries::update_block_permission(db.pool(), &block.id, perm).await
                {
                    Ok(()) => {
                        output.success(&format!("Updated permission to: {}", perm_str));
                        modified = true;
                    }
                    Err(e) => {
                        output.error(&format!("Failed to update permission: {}", e));
                    }
                }
            }
            Ok(None) => {
                output.error(&format!("Memory block '{}' not found", label));
                return Ok(());
            }
            Err(e) => {
                output.error(&format!("Failed to get memory block: {}", e));
                return Ok(());
            }
        }
    }

    // Update memory type if specified
    if let Some(type_str) = memory_type {
        let mem_type = match type_str.to_lowercase().as_str() {
            "core" => pattern_db::MemoryBlockType::Core,
            "working" => pattern_db::MemoryBlockType::Working,
            _ => {
                output.error(&format!("Invalid memory type: {}", type_str));
                output.status("Valid: core, working");
                return Ok(());
            }
        };

        match pattern_db::queries::get_block_by_label(db.pool(), &db_agent.id, label).await {
            Ok(Some(block)) => {
                match pattern_db::queries::update_block_type(db.pool(), &block.id, mem_type).await {
                    Ok(()) => {
                        output.success(&format!("Updated memory type to: {}", type_str));
                        modified = true;
                    }
                    Err(e) => {
                        output.error(&format!("Failed to update memory type: {}", e));
                    }
                }
            }
            Ok(None) => {
                output.error(&format!("Memory block '{}' not found", label));
                return Ok(());
            }
            Err(e) => {
                output.error(&format!("Failed to get memory block: {}", e));
                return Ok(());
            }
        }
    }

    if !modified {
        output.warning("No modifications specified");
        output.status("Use --permission or --memory-type to modify the block");
    }

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
