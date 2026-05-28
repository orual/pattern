# Action Items: pattern_core and pattern_cli Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Address critical and high-priority TODOs in pattern_core and pattern_cli from docs/action-items.md

**Architecture:** Fix CRDT memory editing, reimplement CLI commands using pattern_db queries, add missing permission checks and response collection in coordination patterns.

**Tech Stack:** Rust, sqlx, loro, pattern_db FTS5 search, tokio

---

## Task 1: Fix Memory Edit Operation (Critical)

**Files:**
- Modify: `crates/pattern_core/src/memory/cache.rs:1283-1295`
- Reference: `crates/pattern_core/src/tool/builtin/block_edit.rs:239-310` (pattern to follow)

**Context:**
The current `replace_in_block_with_options` does a naive string replace that loses CRDT semantics:
```rust
let current = doc.text_content();
let new_content = current.replacen(old, new, 1);
doc.set_text(&new_content, true)?;
```

The block_edit tool shows the correct pattern: use `text.splice()` with proper byte-to-Unicode position conversion.

**Step 1: Write the failing test**

Add to `crates/pattern_core/src/memory/cache.rs` tests:

```rust
#[tokio::test]
async fn test_cache_replace_preserves_crdt_operations() {
    let (_dir, dbs) = test_dbs().await;
    let cache = MemoryCache::new(dbs.clone());

    // Create test agent
    let agent = Agent {
        id: "test-agent".to_string(),
        name: "Test".to_string(),
        // ... minimal fields
    };
    pattern_db::queries::create_agent(dbs.constellation().pool(), &agent).await.unwrap();

    // Create a text block with initial content
    cache.create_block(
        "test-agent",
        "test_block",
        "Test block",
        BlockType::Working,
        BlockSchema::text(),
        2000,
    ).await.unwrap();

    cache.update_block_text("test-agent", "test_block", "Hello world, hello universe").await.unwrap();

    // Get the block and check initial version
    let doc = cache.get_block("test-agent", "test_block").await.unwrap().unwrap();
    let initial_version = doc.inner().oplog_vv();

    // Replace first "hello" with "goodbye"
    let replaced = cache.replace_in_block_with_options(
        "test-agent",
        "test_block",
        "hello",
        "goodbye",
        WriteOptions::default(),
    ).await.unwrap();

    assert!(replaced);

    // Verify content
    let content = cache.get_rendered_content("test-agent", "test_block").await.unwrap().unwrap();
    assert_eq!(content, "Hello world, goodbye universe");

    // Verify CRDT version advanced (not reset)
    let doc = cache.get_block("test-agent", "test_block").await.unwrap().unwrap();
    let new_version = doc.inner().oplog_vv();
    assert!(new_version != initial_version, "Version should have advanced via splice, not reset");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo nextest run -p pattern-core test_cache_replace_preserves_crdt_operations`
Expected: The test may pass or fail depending on current behavior, but the key is verifying CRDT semantics are preserved.

**Step 3: Implement proper CRDT replace**

Replace the TODO section in `cache.rs:1283-1295`:

```rust
// Get current text and replace using CRDT operations
let text = doc.inner().get_text("content");
let current = text.to_string();

// Find the position of old text
let Some(byte_pos) = current.find(old) else {
    return Ok(false); // Pattern not found
};

// Convert byte positions to Unicode positions for Loro
use loro::cursor::PosType;
let unicode_start = text
    .convert_pos(byte_pos, PosType::Bytes, PosType::Unicode)
    .ok_or_else(|| MemoryError::OperationFailed {
        operation: "replace".to_string(),
        cause: format!("Invalid byte position: {}", byte_pos),
    })?;
let unicode_end = text
    .convert_pos(byte_pos + old.len(), PosType::Bytes, PosType::Unicode)
    .ok_or_else(|| MemoryError::OperationFailed {
        operation: "replace".to_string(),
        cause: format!("Invalid byte position: {}", byte_pos + old.len()),
    })?;

// Splice the text (CRDT-aware operation)
text.splice(unicode_start, unicode_end - unicode_start, new)
    .map_err(|e| MemoryError::OperationFailed {
        operation: "replace".to_string(),
        cause: format!("Splice failed: {}", e),
    })?;

doc.inner().commit();
```

**Step 4: Run test to verify it passes**

Run: `cargo nextest run -p pattern-core test_cache_replace_preserves_crdt_operations`
Expected: PASS

**Step 5: Run full test suite**

Run: `cargo nextest run -p pattern-core`
Expected: All tests pass

**Step 6: Commit**

```bash
git add crates/pattern_core/src/memory/cache.rs
git commit -m "[pattern-core] fix CRDT-aware text replacement in memory cache"
```

---

## Task 2: Reimplement /list Slash Command

**Files:**
- Modify: `crates/pattern_cli/src/slash_commands.rs:136-146`

**Context:**
The `/list` command is stubbed. pattern_db provides `queries::list_agents()`.

**Step 1: Write failing test (manual verification)**

The test here is manual - run `pattern chat` and type `/list` to see the stub message.

**Step 2: Implement the command**

Replace lines 136-146:

```rust
"/list" => {
    // Get database pool from context
    let pool = context.runtime().databases().constellation().pool();

    match pattern_db::queries::list_agents(pool).await {
        Ok(agents) => {
            output.section("Agents in Database");
            if agents.is_empty() {
                output.status("No agents found in database");
            } else {
                for agent in agents {
                    let status_str = match agent.status {
                        pattern_db::models::AgentStatus::Active => "active".green(),
                        pattern_db::models::AgentStatus::Inactive => "inactive".yellow(),
                        pattern_db::models::AgentStatus::Suspended => "suspended".red(),
                    };
                    output.list_item(&format!(
                        "{} ({}) - {}",
                        agent.name.bright_cyan(),
                        agent.model_name.dimmed(),
                        status_str
                    ));
                }
            }
        }
        Err(e) => {
            output.error(&format!("Failed to list agents: {}", e));
        }
    }

    // Also show agents in current runtime context
    output.print("");
    output.section("Agents in Current Context");
    for name in context.list_agents() {
        output.list_item(&name.bright_cyan().to_string());
    }
    Ok(false)
}
```

**Step 3: Verify compilation**

Run: `cargo check -p pattern-cli`
Expected: Compiles without errors

**Step 4: Commit**

```bash
git add crates/pattern_cli/src/slash_commands.rs
git commit -m "[pattern-cli] reimplement /list command with pattern_db"
```

---

## Task 3: Reimplement /archival Slash Command

**Files:**
- Modify: `crates/pattern_cli/src/slash_commands.rs:291-298`

**Context:**
The `/archival` command needs FTS search. pattern_db provides `fts::search_archival()`.

**Step 1: Implement the command**

Replace lines 291-298:

```rust
"/archival" => {
    // Parse query from remaining parts
    let query = if parts.len() > 1 {
        parts[1..].join(" ")
    } else {
        output.error("Usage: /archival <search query>");
        return Ok(false);
    };

    // Get agent for search scope
    match context.get_agent(None).await {
        Ok(agent) => {
            let agent_id = agent.id().to_string();
            let pool = context.runtime().databases().constellation().pool();

            match pattern_db::fts::search_archival(pool, &query, Some(&agent_id), 20).await {
                Ok(results) => {
                    output.section(&format!("Archival Search: '{}'", query));
                    if results.is_empty() {
                        output.status("No results found");
                    } else {
                        for (i, result) in results.iter().enumerate() {
                            output.print(&format!(
                                "{}. [score: {:.2}] {}",
                                i + 1,
                                result.rank.abs(),
                                result.content.chars().take(100).collect::<String>()
                            ));
                        }
                    }
                }
                Err(e) => {
                    output.error(&format!("Search failed: {}", e));
                }
            }
        }
        Err(e) => {
            output.error(&format!("Failed to get agent: {}", e));
        }
    }
    Ok(false)
}
```

**Step 2: Verify compilation**

Run: `cargo check -p pattern-cli`

**Step 3: Commit**

```bash
git add crates/pattern_cli/src/slash_commands.rs
git commit -m "[pattern-cli] reimplement /archival search with FTS5"
```

---

## Task 4: Reimplement /context Slash Command

**Files:**
- Modify: `crates/pattern_cli/src/slash_commands.rs:299-306`

**Step 1: Implement the command**

Replace lines 299-306:

```rust
"/context" => {
    // Parse optional limit
    let limit: i64 = if parts.len() > 1 {
        parts[1].parse().unwrap_or(20)
    } else {
        20
    };

    match context.get_agent(None).await {
        Ok(agent) => {
            let agent_id = agent.id().to_string();
            let pool = context.runtime().databases().constellation().pool();

            match pattern_db::queries::get_message_summaries(pool, &agent_id, limit).await {
                Ok(summaries) => {
                    output.section(&format!("Recent Messages (last {})", limit));
                    if summaries.is_empty() {
                        output.status("No messages in context");
                    } else {
                        for msg in summaries.iter().rev() {
                            let role_str = match msg.role {
                                pattern_db::models::MessageRole::User => "user".green(),
                                pattern_db::models::MessageRole::Assistant => "assistant".cyan(),
                                pattern_db::models::MessageRole::Tool => "tool".yellow(),
                                pattern_db::models::MessageRole::System => "system".magenta(),
                            };
                            let preview = msg.content_preview
                                .as_deref()
                                .unwrap_or("[no preview]")
                                .chars()
                                .take(60)
                                .collect::<String>();
                            output.print(&format!("[{}] {}", role_str, preview));
                        }
                    }
                }
                Err(e) => {
                    output.error(&format!("Failed to get messages: {}", e));
                }
            }
        }
        Err(e) => {
            output.error(&format!("Failed to get agent: {}", e));
        }
    }
    Ok(false)
}
```

**Step 2: Verify and commit**

Run: `cargo check -p pattern-cli`

```bash
git add crates/pattern_cli/src/slash_commands.rs
git commit -m "[pattern-cli] reimplement /context command with message summaries"
```

---

## Task 5: Reimplement /search Slash Command

**Files:**
- Modify: `crates/pattern_cli/src/slash_commands.rs:307-313`

**Step 1: Implement the command**

Replace lines 307-313:

```rust
"/search" => {
    let query = if parts.len() > 1 {
        parts[1..].join(" ")
    } else {
        output.error("Usage: /search <query>");
        return Ok(false);
    };

    match context.get_agent(None).await {
        Ok(agent) => {
            let agent_id = agent.id().to_string();
            let pool = context.runtime().databases().constellation().pool();

            match pattern_db::fts::search_messages(pool, &query, Some(&agent_id), 20).await {
                Ok(results) => {
                    output.section(&format!("Message Search: '{}'", query));
                    if results.is_empty() {
                        output.status("No matching messages found");
                    } else {
                        for (i, result) in results.iter().enumerate() {
                            output.print(&format!(
                                "{}. [score: {:.2}] {}",
                                i + 1,
                                result.rank.abs(),
                                result.content.chars().take(80).collect::<String>()
                            ));
                        }
                    }
                }
                Err(e) => {
                    output.error(&format!("Search failed: {}", e));
                }
            }
        }
        Err(e) => {
            output.error(&format!("Failed to get agent: {}", e));
        }
    }
    Ok(false)
}
```

**Step 2: Verify and commit**

Run: `cargo check -p pattern-cli`

```bash
git add crates/pattern_cli/src/slash_commands.rs
git commit -m "[pattern-cli] reimplement /search command with FTS5"
```

---

## Task 6: Reimplement Debug show-context Command

**Files:**
- Modify: `crates/pattern_cli/src/commands/debug.rs:485-503`

**Context:**
This needs to show the full context that would be passed to the LLM. We need to access RuntimeContext or use direct DB queries.

**Step 1: Implement the command**

The command needs a RuntimeContext. For now, implement a simplified version that shows what's available via direct DB access:

```rust
pub async fn show_context(agent_name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    // Open database connection
    let db_path = config.database_path();
    let db = pattern_db::ConstellationDb::open(&db_path).await
        .context("Failed to open constellation database")?;
    let pool = db.pool();

    // Find agent by name
    let agent = pattern_db::queries::get_agent_by_name(pool, agent_name).await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent_name))?;

    output.section(&format!("Context for Agent: {}", agent.name.bright_cyan()));

    // Show system prompt
    output.subsection("System Prompt");
    output.print(&agent.system_prompt);
    output.print("");

    // Show enabled tools
    output.subsection("Enabled Tools");
    let tools: Vec<String> = serde_json::from_value(agent.enabled_tools.0.clone())
        .unwrap_or_default();
    if tools.is_empty() {
        output.status("No tools enabled");
    } else {
        for tool in tools {
            output.list_item(&tool);
        }
    }
    output.print("");

    // Show memory blocks
    output.subsection("Memory Blocks");
    let blocks = pattern_db::queries::list_blocks(pool, &agent.id).await?;
    if blocks.is_empty() {
        output.status("No memory blocks");
    } else {
        for block in blocks {
            output.list_item(&format!(
                "{} ({:?}) - {} chars",
                block.label.bright_cyan(),
                block.block_type,
                block.content_preview.as_deref().map(|s| s.len()).unwrap_or(0)
            ));
        }
    }
    output.print("");

    // Show recent messages count
    let msg_count = pattern_db::queries::count_messages(pool, &agent.id).await?;
    output.kv("Active Messages", &msg_count.to_string());

    Ok(())
}
```

**Step 2: Verify and commit**

Run: `cargo check -p pattern-cli`

```bash
git add crates/pattern_cli/src/commands/debug.rs
git commit -m "[pattern-cli] reimplement debug show-context with pattern_db"
```

---

## Task 7: Reimplement Debug edit-memory Command

**Files:**
- Modify: `crates/pattern_cli/src/commands/debug.rs:520-542`

**Step 1: Implement the command**

```rust
pub async fn edit_memory(agent_name: &str, label: &str, file_path: Option<&str>) -> Result<()> {
    let output = Output::new();

    // Open database
    let config = PatternConfig::load()?;
    let db_path = config.database_path();
    let db = pattern_db::ConstellationDb::open(&db_path).await
        .context("Failed to open constellation database")?;
    let pool = db.pool();

    // Find agent
    let agent = pattern_db::queries::get_agent_by_name(pool, agent_name).await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent_name))?;

    // Get memory block
    let block = pattern_db::queries::get_block_by_label(pool, &agent.id, label).await?
        .ok_or_else(|| anyhow::anyhow!("Block '{}' not found for agent '{}'", label, agent_name))?;

    // Reconstruct content from Loro snapshot
    let doc = loro::LoroDoc::new();
    if !block.loro_snapshot.is_empty() {
        doc.import(&block.loro_snapshot)
            .context("Failed to import Loro snapshot")?;
    }
    let content = doc.get_text("content").to_string();

    // Write to temp file or specified file
    let edit_path = if let Some(path) = file_path {
        PathBuf::from(path)
    } else {
        let mut temp = std::env::temp_dir();
        temp.push(format!("pattern_memory_{}_{}.txt", agent_name, label));
        temp
    };

    std::fs::write(&edit_path, &content)
        .context("Failed to write content to file")?;

    output.success(&format!("Exported block '{}' to: {}", label, edit_path.display()));
    output.status("Edit the file, then use /memory-import to reimport");
    output.print("");
    output.kv("Block ID", &block.id);
    output.kv("Type", &format!("{:?}", block.block_type));
    output.kv("Permission", &format!("{:?}", block.permission));
    output.kv("Content length", &content.len().to_string());

    Ok(())
}
```

**Step 2: Verify and commit**

```bash
git add crates/pattern_cli/src/commands/debug.rs
git commit -m "[pattern-cli] reimplement debug edit-memory with export to file"
```

---

## Task 8: Reimplement Debug modify-memory Command

**Files:**
- Modify: `crates/pattern_cli/src/commands/debug.rs:556-590`

**Step 1: Implement the command**

```rust
pub async fn modify_memory(
    agent: &String,
    label: &String,
    new_label: &Option<String>,
    permission: &Option<String>,
    memory_type: &Option<String>,
) -> Result<()> {
    let output = Output::new();

    // Open database
    let config = PatternConfig::load()?;
    let db_path = config.database_path();
    let db = pattern_db::ConstellationDb::open(&db_path).await
        .context("Failed to open constellation database")?;
    let pool = db.pool();

    // Find agent
    let agent_record = pattern_db::queries::get_agent_by_name(pool, agent).await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent))?;

    // Get memory block
    let block = pattern_db::queries::get_block_by_label(pool, &agent_record.id, label).await?
        .ok_or_else(|| anyhow::anyhow!("Block '{}' not found", label))?;

    let mut modified = false;

    // Update permission if specified
    if let Some(perm_str) = permission {
        let perm = match perm_str.to_lowercase().as_str() {
            "readonly" | "read_only" | "ro" => pattern_db::models::MemoryPermission::ReadOnly,
            "readwrite" | "read_write" | "rw" => pattern_db::models::MemoryPermission::ReadWrite,
            "append" => pattern_db::models::MemoryPermission::Append,
            "admin" => pattern_db::models::MemoryPermission::Admin,
            _ => return Err(anyhow::anyhow!("Unknown permission: {}", perm_str)),
        };
        pattern_db::queries::update_block_permission(pool, &block.id, perm).await?;
        output.success(&format!("Updated permission to {:?}", perm));
        modified = true;
    }

    // Update type if specified
    if let Some(type_str) = memory_type {
        let block_type = match type_str.to_lowercase().as_str() {
            "core" => pattern_db::models::MemoryBlockType::Core,
            "working" => pattern_db::models::MemoryBlockType::Working,
            "archival" => pattern_db::models::MemoryBlockType::Archival,
            _ => return Err(anyhow::anyhow!("Unknown memory type: {}", type_str)),
        };
        pattern_db::queries::update_block_type(pool, &block.id, block_type).await?;
        output.success(&format!("Updated type to {:?}", block_type));
        modified = true;
    }

    // Note: Renaming labels requires more complex handling (updating references)
    if let Some(nl) = new_label {
        output.warning(&format!(
            "Label renaming from '{}' to '{}' not yet implemented - requires updating all references",
            label, nl
        ));
    }

    if !modified && new_label.is_none() {
        output.status("No modifications specified");
    }

    Ok(())
}
```

**Step 2: Verify and commit**

```bash
git add crates/pattern_cli/src/commands/debug.rs
git commit -m "[pattern-cli] reimplement debug modify-memory with pattern_db"
```

---

## Task 9: Reimplement Debug context-cleanup Command

**Files:**
- Modify: `crates/pattern_cli/src/commands/debug.rs:606-650` (approximate)

**Step 1: Implement the command**

```rust
pub async fn context_cleanup(
    agent_name: &str,
    interactive: bool,
    dry_run: bool,
    limit: Option<usize>,
) -> Result<()> {
    let output = Output::new();

    // Open database
    let config = PatternConfig::load()?;
    let db_path = config.database_path();
    let db = pattern_db::ConstellationDb::open(&db_path).await
        .context("Failed to open constellation database")?;
    let pool = db.pool();

    // Find agent
    let agent = pattern_db::queries::get_agent_by_name(pool, agent_name).await?
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent_name))?;

    // Get messages
    let messages = pattern_db::queries::get_messages_with_archived(
        pool,
        &agent.id,
        limit.unwrap_or(1000) as i64,
    ).await?;

    output.section(&format!("Context Cleanup for: {}", agent_name.bright_cyan()));
    output.kv("Total messages", &messages.len().to_string());

    // Analyze for issues
    let mut issues: Vec<(String, String)> = Vec::new();
    let mut tool_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for msg in &messages {
        // Parse content to find tool calls and results
        if let Some(content) = &msg.content_json {
            if let Some(obj) = content.0.as_object() {
                // Check for tool_calls
                if let Some(calls) = obj.get("tool_calls").and_then(|v| v.as_array()) {
                    for call in calls {
                        if let Some(id) = call.get("id").and_then(|v| v.as_str()) {
                            tool_call_ids.insert(id.to_string());
                        }
                    }
                }
                // Check for tool_results
                if let Some(results) = obj.get("tool_responses").and_then(|v| v.as_array()) {
                    for result in results {
                        if let Some(id) = result.get("tool_call_id").and_then(|v| v.as_str()) {
                            tool_result_ids.insert(id.to_string());
                        }
                    }
                }
            }
        }
    }

    // Find unpaired tool calls (calls without results)
    for call_id in &tool_call_ids {
        if !tool_result_ids.contains(call_id) {
            issues.push((call_id.clone(), "Tool call without result".to_string()));
        }
    }

    // Find orphan results (results without calls)
    for result_id in &tool_result_ids {
        if !tool_call_ids.contains(result_id) {
            issues.push((result_id.clone(), "Tool result without call".to_string()));
        }
    }

    if issues.is_empty() {
        output.success("No issues found in message context");
        return Ok(());
    }

    output.warning(&format!("Found {} potential issues:", issues.len()));
    for (id, issue) in &issues {
        output.list_item(&format!("{}: {}", id.dimmed(), issue));
    }

    if dry_run {
        output.status("Dry run - no changes made");
        return Ok(());
    }

    if interactive {
        // Would use dialoguer here for confirmation
        output.status("Interactive mode would prompt for each deletion");
    }

    // For now, just report - actual cleanup requires careful consideration
    output.status("Automatic cleanup not yet implemented - review issues above");

    Ok(())
}
```

**Step 2: Verify and commit**

```bash
git add crates/pattern_cli/src/commands/debug.rs
git commit -m "[pattern-cli] reimplement debug context-cleanup with issue detection"
```

---

## Task 10: Add Cross-Agent Search Permission Checks

**Files:**
- Modify: `crates/pattern_core/src/runtime/mod.rs:417-451`

**Context:**
The search function logs but doesn't enforce permission checks for cross-agent searches.

**Step 1: Design permission check**

For now, implement a basic check that requires explicit permission grants:

```rust
async fn search(
    &self,
    query: &str,
    scope: SearchScope,
    options: SearchOptions,
) -> MemoryResult<Vec<MemorySearchResult>> {
    match scope {
        SearchScope::CurrentAgent => self.memory.search(&self.agent_id, query, options).await,
        SearchScope::Agent(ref id) => {
            // Check if this agent has permission to search the target agent
            if id.as_str() != self.agent_id {
                // For now, only allow if both agents are in the same group
                let has_permission = self.check_cross_agent_permission(id.as_str()).await;
                if !has_permission {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        target_agent = %id,
                        "Cross-agent search denied - no shared group membership"
                    );
                    return Ok(vec![]); // Return empty instead of error for graceful degradation
                }
            }
            self.memory.search(id.as_str(), query, options).await
        }
        SearchScope::Agents(ref ids) => {
            // Filter to only permitted agents
            let mut all_results = Vec::new();
            let mut denied_count = 0;

            for id in ids {
                if id.as_str() != self.agent_id {
                    let has_permission = self.check_cross_agent_permission(id.as_str()).await;
                    if !has_permission {
                        denied_count += 1;
                        continue;
                    }
                }
                match self.memory.search(id.as_str(), query, options.clone()).await {
                    Ok(results) => all_results.extend(results),
                    Err(e) => {
                        tracing::warn!(agent_id = %id, error = %e, "Search failed for agent");
                    }
                }
            }

            if denied_count > 0 {
                tracing::info!(
                    agent_id = %self.agent_id,
                    denied_count,
                    "Some cross-agent searches were denied"
                );
            }

            Ok(all_results)
        }
        SearchScope::Constellation => {
            // Constellation-wide search requires special permission
            // For now, allow if agent has "constellation_search" capability
            if !self.has_capability("constellation_search") {
                tracing::warn!(
                    agent_id = %self.agent_id,
                    "Constellation-wide search denied - missing capability"
                );
                return Ok(vec![]);
            }
            self.memory.search_all(query, options).await
        }
    }
}

/// Check if the current agent can search another agent's memory.
/// Returns true if they share group membership.
async fn check_cross_agent_permission(&self, target_agent_id: &str) -> bool {
    // Check if agents share a group
    if let Some(ref runtime_ctx) = self.runtime_context {
        if let Some(ctx) = runtime_ctx.upgrade() {
            // Get groups for both agents
            let pool = ctx.databases().constellation().pool();
            let self_groups = pattern_db::queries::get_agent_groups(pool, &self.agent_id).await;
            let target_groups = pattern_db::queries::get_agent_groups(pool, target_agent_id).await;

            if let (Ok(self_groups), Ok(target_groups)) = (self_groups, target_groups) {
                // Check for any shared group
                for sg in &self_groups {
                    for tg in &target_groups {
                        if sg.id == tg.id {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
```

**Step 2: Verify and commit**

```bash
git add crates/pattern_core/src/runtime/mod.rs
git commit -m "[pattern-core] add cross-agent search permission checks via group membership"
```

---

## Task 11: Fix Message Content Combination

**Files:**
- Modify: `crates/pattern_core/src/messages/mod.rs:403-409` and `468-474`

**Context:**
When combining multiple content items (Text + ToolCalls), the code just takes the first item. We need to properly merge them.

**Step 1: Create a content combination helper**

Add helper function:

```rust
/// Combine multiple MessageContent items into a single content.
/// - Multiple Text items: concatenate with newlines
/// - Text + ToolCalls: create a compound content (if supported) or prioritize ToolCalls
/// - Multiple ToolCalls: merge into single ToolCalls
fn combine_content_items(items: Vec<MessageContent>) -> MessageContent {
    if items.is_empty() {
        return MessageContent::Text("".to_string());
    }
    if items.len() == 1 {
        return items.into_iter().next().unwrap();
    }

    let mut texts = Vec::new();
    let mut tool_calls = Vec::new();

    for item in items {
        match item {
            MessageContent::Text(t) => texts.push(t),
            MessageContent::ToolCalls(calls) => tool_calls.extend(calls),
            // Other content types passed through as-is if they're the only item
            other => {
                // If we have a mix and this is something else, just keep collecting
                // For now, we'll drop these in the combination
                tracing::debug!("Dropping non-text/tool-call content in combination: {:?}", other);
            }
        }
    }

    // If we have tool calls, they take precedence (assistant is calling tools)
    if !tool_calls.is_empty() {
        // If there's also text, we could potentially include it as a preamble
        // but for now, return just the tool calls
        if !texts.is_empty() {
            tracing::debug!(
                "Combining text ({} chars) with {} tool calls - text becomes thinking",
                texts.iter().map(|s| s.len()).sum::<usize>(),
                tool_calls.len()
            );
        }
        return MessageContent::ToolCalls(tool_calls);
    }

    // Otherwise, combine texts
    MessageContent::Text(texts.join("\n"))
}
```

**Step 2: Use the helper**

Replace the TODO sections:

```rust
// Line 403-409
let combined_content = combine_content_items(current_assistant_content.clone());

// Line 468-474
let combined_content = combine_content_items(current_assistant_content.clone());
```

**Step 3: Verify and commit**

```bash
git add crates/pattern_core/src/messages/mod.rs
git commit -m "[pattern-core] properly combine Text + ToolCalls message content"
```

---

## Task 12: Fix Coordination Pattern Response Collection

**Files:**
- Modify: `crates/pattern_core/src/coordination/patterns/round_robin.rs:225`
- Modify: `crates/pattern_core/src/coordination/patterns/dynamic.rs:316,474`
- Modify: `crates/pattern_core/src/coordination/patterns/sleeptime.rs:296,539`

**Context:**
These patterns return empty vecs for `agent_responses`. We need to collect actual responses.

**Step 1: Fix round_robin.rs**

At line 225, the code has `agent_responses: vec![]`. We need to capture the response from the agent execution:

```rust
// Before the completion event, collect the response
let agent_response = AgentResponse {
    agent_id: current_agent.id().to_string(),
    agent_name: current_agent.name().to_string(),
    content: response_content, // captured from agent execution
    metadata: response_metadata,
};

// Then in the event:
let _ = tx
    .send(GroupResponseEvent::Complete {
        group_id,
        pattern: "round_robin".to_string(),
        execution_time: start_time.elapsed(),
        agent_responses: vec![agent_response], // Include the response
        state_changes: Some(new_state),
    })
    .await;
```

The specific implementation depends on how responses are captured in each pattern. This task requires reading more context from each file to implement correctly.

**Step 2: Similar fixes for dynamic.rs and sleeptime.rs**

Each pattern needs to capture agent responses during execution and include them in the Complete event.

**Step 3: Commit**

```bash
git add crates/pattern_core/src/coordination/patterns/*.rs
git commit -m "[pattern-core] collect actual agent responses in coordination patterns"
```

---

## Summary

This plan addresses:

1. **Critical (1 item):**
   - Memory cache CRDT-aware replace operation

2. **High Priority CLI (8 items):**
   - `/list`, `/archival`, `/context`, `/search` slash commands
   - `show-context`, `edit-memory`, `modify-memory`, `context-cleanup` debug commands

3. **High Priority Core (3 items):**
   - Cross-agent search permissions
   - Message content combination
   - Coordination pattern response collection

**Estimated complexity:** Medium-High
**Dependencies:** pattern_db queries and FTS must be working (they are based on the code review)

---

**Plan complete and saved to `docs/plans/2025-01-01-action-items-core-cli.md`.**

**Two execution options:**

1. **Subagent-Driven (this session)** - I dispatch fresh subagent per task, review between tasks, fast iteration

2. **Parallel Session (separate)** - Open new session with executing-plans, batch execution with checkpoints

**Which approach?**
