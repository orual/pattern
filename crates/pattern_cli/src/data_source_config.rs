//! Reusable data source configuration builders.
//!
//! This module provides interactive builders and file loaders for data source
//! configuration. Used by both CLI commands and the agent/group builders.

use std::path::{Path, PathBuf};

use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::{
    BlueskySourceConfig, CustomSourceConfig, DataSourceConfig, DiscordSourceConfig,
    FilePermissionRuleConfig, FileSourceConfig, ShellSourceConfig,
};
use pattern_core::data_source::DefaultCommandValidator;
use pattern_core::memory::MemoryPermission;

use crate::commands::builder::editors::{confirm, input_optional, input_required, select_menu};

// =============================================================================
// Top-Level Dispatchers
// =============================================================================

/// Build a new data source interactively based on type.
pub fn build_source_interactive(name: &str, source_type: &str) -> Result<DataSourceConfig> {
    match source_type.to_lowercase().as_str() {
        "bluesky" => Ok(DataSourceConfig::Bluesky(build_bluesky_interactive(
            name, None,
        )?)),
        "discord" => Ok(DataSourceConfig::Discord(build_discord_interactive(
            name, None,
        )?)),
        "file" => Ok(DataSourceConfig::File(build_file_interactive(name, None)?)),
        "shell" => Ok(DataSourceConfig::Shell(build_shell_interactive(
            name, None,
        )?)),
        "custom" => Ok(DataSourceConfig::Custom(build_custom_interactive(
            name, None,
        )?)),
        other => Err(miette::miette!(
            "Unknown source type '{}'. Valid types: bluesky, discord, file, custom",
            other
        )),
    }
}

/// Edit an existing data source interactively.
pub fn edit_source_interactive(
    name: &str,
    existing: &DataSourceConfig,
) -> Result<DataSourceConfig> {
    match existing {
        DataSourceConfig::Bluesky(cfg) => Ok(DataSourceConfig::Bluesky(build_bluesky_interactive(
            name,
            Some(cfg),
        )?)),
        DataSourceConfig::Discord(cfg) => Ok(DataSourceConfig::Discord(build_discord_interactive(
            name,
            Some(cfg),
        )?)),
        DataSourceConfig::File(cfg) => Ok(DataSourceConfig::File(build_file_interactive(
            name,
            Some(cfg),
        )?)),
        DataSourceConfig::Shell(cfg) => Ok(DataSourceConfig::Shell(build_shell_interactive(
            name,
            Some(cfg),
        )?)),
        DataSourceConfig::Custom(cfg) => Ok(DataSourceConfig::Custom(build_custom_interactive(
            name,
            Some(cfg),
        )?)),
    }
}

// =============================================================================
// File Loaders
// =============================================================================

/// Load a data source configuration from a TOML file.
pub fn load_source_from_toml(path: &Path) -> Result<DataSourceConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| miette::miette!("Failed to read {}: {}", path.display(), e))?;

    let config: DataSourceConfig =
        toml::from_str(&content).map_err(|e| miette::miette!("Failed to parse TOML: {}", e))?;

    Ok(config)
}

/// Load arbitrary JSON configuration from a file.
pub fn load_json_config(path: &Path) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| miette::miette!("Failed to read {}: {}", path.display(), e))?;

    let config: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| miette::miette!("Failed to parse JSON: {}", e))?;

    Ok(config)
}

// =============================================================================
// Bluesky Source Builder
// =============================================================================

/// Build or edit a Bluesky source configuration interactively.
pub fn build_bluesky_interactive(
    name: &str,
    existing: Option<&BlueskySourceConfig>,
) -> Result<BlueskySourceConfig> {
    println!("\n{}", "─ Bluesky Source Configuration ─".bold());
    let dids_current = existing.map(|e| &e.dids[..]).unwrap_or(&[]);
    let mentions_current = existing.map(|e| &e.mentions[..]).unwrap_or(&[]);

    let dids = edit_string_list("DIDs to watch", dids_current)?;
    let mentions = edit_string_list("Mentions to watch for", mentions_current)?;

    // Keywords
    let current_keywords = existing.map(|e| &e.keywords[..]).unwrap_or(&[]);
    let keywords = edit_string_list("Keywords to include", current_keywords)?;

    let current_exclude = existing.map(|e| &e.exclude_keywords[..]).unwrap_or(&[]);
    let exclude_keywords = edit_string_list("Keywords to exclude", current_exclude)?;

    let current_allow = existing
        .map(|e| e.require_agent_participation)
        .unwrap_or(true);
    let allow_any_mentions = confirm(
        &format!("Allow any mentions? (current: {})", current_allow),
        current_allow,
    )?;

    let current_participation = existing
        .map(|e| e.require_agent_participation)
        .unwrap_or(true);
    let require_agent_participation = confirm(
        &format!("Mentions only? (current: {})", current_participation),
        current_participation,
    )?;

    let current_languages = existing.map(|e| &e.languages[..]).unwrap_or(&[]);
    let languages = edit_string_list("Languages to filter to", current_languages)?;

    let nsids_current = existing.map(|e| &e.nsids[..]).unwrap_or(&[]);
    let nsids = edit_string_list("NSIDS to show", nsids_current)?;

    let current_friends = existing.map(|e| &e.friends[..]).unwrap_or(&[]);
    let friends = edit_string_list("Friends, dids to always show posts from", current_friends)?;

    let current_ex_dids = existing.map(|e| &e.exclude_dids[..]).unwrap_or(&[]);
    let exclude_dids =
        edit_string_list("DIDs to exclude from any thread context", current_ex_dids)?;

    Ok(BlueskySourceConfig {
        name: name.to_string(),
        keywords,
        require_agent_participation,
        allow_any_mentions,
        exclude_keywords,
        nsids,
        dids,
        languages,
        mentions,
        friends,
        exclude_dids,
        ..Default::default()
    })
}

// =============================================================================
// Discord Source Builder
// =============================================================================

/// Build or edit a Discord source configuration interactively.
pub fn build_discord_interactive(
    name: &str,
    existing: Option<&DiscordSourceConfig>,
) -> Result<DiscordSourceConfig> {
    println!("\n{}", "─ Discord Source Configuration ─".bold());

    // Guild ID
    let current_guild = existing.and_then(|e| e.guild_id.as_deref());
    let guild_id = input_optional(&format!(
        "Guild ID (empty for all guilds){}",
        format_current_optional(current_guild)
    ))?;
    let guild_id = guild_id.or_else(|| current_guild.map(String::from));

    // Channel IDs
    let current_channels = existing.map(|e| &e.channel_ids[..]).unwrap_or(&[]);
    let channel_ids = edit_string_list("Channel IDs to monitor (empty for all)", current_channels)?;

    Ok(DiscordSourceConfig {
        name: name.to_string(),
        guild_id,
        channel_ids,
    })
}

// =============================================================================
// File Source Builder
// =============================================================================

/// Build or edit a File source configuration interactively.
pub fn build_file_interactive(
    name: &str,
    existing: Option<&FileSourceConfig>,
) -> Result<FileSourceConfig> {
    println!("\n{}", "─ File Source Configuration ─".bold());

    // Paths
    let current_paths: Vec<String> = existing
        .map(|e| e.paths.iter().map(|p| p.display().to_string()).collect())
        .unwrap_or_default();
    let path_strings = edit_string_list_required("Paths to watch", &current_paths)?;
    let paths: Vec<PathBuf> = path_strings.into_iter().map(PathBuf::from).collect();

    // Recursive
    let current_recursive = existing.map(|e| e.recursive).unwrap_or(true);
    let recursive = confirm(
        &format!("Watch recursively? (current: {})", current_recursive),
        current_recursive,
    )?;

    // Include patterns
    let current_include = existing.map(|e| &e.include_patterns[..]).unwrap_or(&[]);
    let include_patterns =
        edit_string_list("Include glob patterns (empty for all)", current_include)?;

    // Exclude patterns
    let current_exclude = existing.map(|e| &e.exclude_patterns[..]).unwrap_or(&[]);
    let exclude_patterns = edit_string_list("Exclude glob patterns", current_exclude)?;

    // Permission rules
    let current_rules = existing.map(|e| &e.permission_rules[..]).unwrap_or(&[]);
    let permission_rules = edit_permission_rules(current_rules)?;

    Ok(FileSourceConfig {
        name: name.to_string(),
        paths,
        recursive,
        include_patterns,
        exclude_patterns,
        permission_rules,
    })
}

pub fn build_shell_interactive(
    name: &str,
    existing: Option<&ShellSourceConfig>,
) -> Result<ShellSourceConfig> {
    use pattern_core::data_source::ShellPermission;

    println!("\n{}", "─ Shell Configuration ─".bold());
    println!(
        "{}",
        "Configure shell access permissions and security restrictions.".dimmed()
    );

    // Permission level
    let permission_options = ["read_only", "read_write", "admin"];
    let current_perm_idx = existing
        .map(|e| match e.validator.permission {
            ShellPermission::ReadOnly => 0,
            ShellPermission::ReadWrite => 1,
            ShellPermission::Admin => 2,
        })
        .unwrap_or(0);

    let perm_idx = select_menu(
        "Permission level (determines what commands are allowed)",
        &permission_options,
        current_perm_idx,
    )?;
    let permission = match perm_idx {
        1 => ShellPermission::ReadWrite,
        2 => ShellPermission::Admin,
        _ => ShellPermission::ReadOnly,
    };

    // Allowed paths
    let current_paths: Vec<String> = existing
        .map(|e| {
            e.validator
                .allowed_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect()
        })
        .unwrap_or_else(|| vec!["./".to_string()]);

    println!(
        "\n{}",
        "Allowed paths restrict file operations to specific directories.".dimmed()
    );
    let path_strings = edit_string_list("Allowed paths", &current_paths)?;
    let allowed_paths: Vec<std::path::PathBuf> = path_strings
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect();

    // Strict path enforcement
    let current_strict = existing
        .map(|e| e.validator.strict_path_enforcement)
        .unwrap_or(false);
    let strict = confirm(
        "Enable strict path enforcement? (blocks commands accessing paths outside allowed list)",
        current_strict,
    )?;

    // Custom denied patterns
    let current_denied: Vec<String> = existing
        .map(|e| e.validator.custom_denied_patterns.clone())
        .unwrap_or_default();
    println!(
        "\n{}",
        "Custom denied patterns block commands containing these substrings.".dimmed()
    );
    let custom_denied = edit_string_list("Custom denied patterns", &current_denied)?;

    // Build the validator
    let mut validator = DefaultCommandValidator::new(permission);
    for path in allowed_paths {
        validator = validator.allow_path(path);
    }
    if strict {
        validator = validator.strict();
    }
    for pattern in custom_denied {
        validator = validator.deny_pattern(pattern);
    }

    Ok(ShellSourceConfig {
        name: name.to_string(),
        validator,
    })
}

// =============================================================================
// Custom Source Builder
// =============================================================================

/// Build or edit a Custom source configuration interactively.
pub fn build_custom_interactive(
    name: &str,
    existing: Option<&CustomSourceConfig>,
) -> Result<CustomSourceConfig> {
    println!("\n{}", "─ Custom Source Configuration ─".bold());

    // Source type
    let current_type = existing.map(|e| e.source_type.as_str());
    let source_type = if let Some(ct) = current_type {
        let new_type = input_optional(&format!("Source type (current: {})", ct))?;
        new_type.unwrap_or_else(|| ct.to_string())
    } else {
        input_required("Source type")?
    };

    // Config JSON
    println!(
        "{}",
        "Config can be entered inline or loaded from a JSON file.".dimmed()
    );

    let config_options = ["Keep current", "Enter inline JSON", "Load from file"];
    let has_existing = existing.is_some();
    let default_idx = if has_existing { 0 } else { 1 };

    let choice = if has_existing {
        select_menu("Config source", &config_options, default_idx)?
    } else {
        select_menu("Config source", &config_options[1..], 0)? + 1
    };

    let config = match choice {
        0 => {
            // Keep current
            existing
                .map(|e| e.config.clone())
                .unwrap_or_else(|| serde_json::json!({}))
        }
        1 => {
            // Enter inline
            let json_str = input_optional("JSON config (or empty for {})")?;
            match json_str {
                Some(s) if !s.is_empty() => {
                    serde_json::from_str(&s).map_err(|e| miette::miette!("Invalid JSON: {}", e))?
                }
                _ => serde_json::json!({}),
            }
        }
        _ => {
            // Load from file
            let path_str = input_required("JSON file path")?;
            load_json_config(Path::new(&path_str))?
        }
    };

    Ok(CustomSourceConfig {
        name: name.to_string(),
        source_type,
        config,
    })
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Format current value hint for optional fields.
fn format_current_optional(current: Option<&str>) -> String {
    match current {
        Some(v) => format!(" (current: {})", v.dimmed()),
        None => String::new(),
    }
}

/// Edit a list of strings interactively.
fn edit_string_list(prompt: &str, current: &[String]) -> Result<Vec<String>> {
    println!("\n{}", prompt.bold());

    if !current.is_empty() {
        println!("Current values:");
        for (i, item) in current.iter().enumerate() {
            println!("  {}. {}", i + 1, item);
        }
    }

    let mut items: Vec<String> = current.to_vec();

    loop {
        let options = if items.is_empty() {
            vec!["Add item", "Done"]
        } else {
            vec!["Add item", "Remove item", "Clear all", "Done"]
        };

        let choice = select_menu("Action", &options, options.len() - 1)?;

        match options[choice] {
            "Add item" => {
                if let Some(item) = input_optional("New item")? {
                    if !item.is_empty() {
                        items.push(item.clone());
                        println!("{} Added '{}'", "✓".green(), item);
                    }
                }
            }
            "Remove item" => {
                if items.is_empty() {
                    println!("{}", "No items to remove".yellow());
                } else {
                    let item_strs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
                    let idx = select_menu("Select item to remove", &item_strs, 0)?;
                    let removed = items.remove(idx);
                    println!("{} Removed '{}'", "✓".green(), removed);
                }
            }
            "Clear all" => {
                items.clear();
                println!("{} Cleared all items", "✓".green());
            }
            "Done" | _ => break,
        }
    }

    Ok(items)
}

/// Edit a list of strings, requiring at least one item.
fn edit_string_list_required(prompt: &str, current: &[String]) -> Result<Vec<String>> {
    println!("\n{}", prompt.bold());

    if !current.is_empty() {
        println!("Current values:");
        for (i, item) in current.iter().enumerate() {
            println!("  {}. {}", i + 1, item);
        }
    }

    let mut items: Vec<String> = current.to_vec();

    // If empty, force at least one item
    if items.is_empty() {
        let first = input_required("First item (required)")?;
        items.push(first);
        println!("{} Added first item", "✓".green());
    }

    loop {
        let options = vec!["Add item", "Remove item", "Clear and restart", "Done"];
        let choice = select_menu("Action", &options, options.len() - 1)?;

        match options[choice] {
            "Add item" => {
                if let Some(item) = input_optional("New item")? {
                    if !item.is_empty() {
                        items.push(item.clone());
                        println!("{} Added '{}'", "✓".green(), item);
                    }
                }
            }
            "Remove item" => {
                if items.len() <= 1 {
                    println!("{}", "Must have at least one item".yellow());
                } else {
                    let item_strs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
                    let idx = select_menu("Select item to remove", &item_strs, 0)?;
                    let removed = items.remove(idx);
                    println!("{} Removed '{}'", "✓".green(), removed);
                }
            }
            "Clear and restart" => {
                items.clear();
                let first = input_required("First item (required)")?;
                items.push(first);
                println!("{} Restarted with new item", "✓".green());
            }
            "Done" | _ => break,
        }
    }

    Ok(items)
}

/// Edit permission rules interactively.
fn edit_permission_rules(
    current: &[FilePermissionRuleConfig],
) -> Result<Vec<FilePermissionRuleConfig>> {
    println!("\n{}", "Permission Rules".bold());
    println!(
        "{}",
        "Rules map glob patterns to permission levels.".dimmed()
    );

    if !current.is_empty() {
        println!("Current rules:");
        for (i, rule) in current.iter().enumerate() {
            println!("  {}. {} -> {:?}", i + 1, rule.pattern, rule.permission);
        }
    }

    let mut rules: Vec<FilePermissionRuleConfig> = current.to_vec();

    loop {
        let options = if rules.is_empty() {
            vec!["Add rule", "Done (default: read_write for all)"]
        } else {
            vec!["Add rule", "Remove rule", "Clear all", "Done"]
        };

        let choice = select_menu("Action", &options, options.len() - 1)?;

        match choice {
            0 => {
                // Add rule
                let pattern =
                    input_required("Glob pattern (e.g., '*.config.toml', 'src/**/*.rs')")?;

                let perm_options = ["read_only", "read_write", "append"];
                let perm_idx = select_menu("Permission level", &perm_options, 1)?;
                let permission = match perm_idx {
                    0 => MemoryPermission::ReadOnly,
                    2 => MemoryPermission::Append,
                    _ => MemoryPermission::ReadWrite,
                };

                rules.push(FilePermissionRuleConfig {
                    pattern: pattern.clone(),
                    permission,
                });
                println!(
                    "{} Added rule: {} -> {:?}",
                    "✓".green(),
                    pattern,
                    permission
                );
            }
            1 if !rules.is_empty() => {
                // Remove rule
                let rule_strs: Vec<String> = rules
                    .iter()
                    .map(|r| format!("{} -> {:?}", r.pattern, r.permission))
                    .collect();
                let rule_refs: Vec<&str> = rule_strs.iter().map(|s| s.as_str()).collect();
                let idx = select_menu("Select rule to remove", &rule_refs, 0)?;
                let removed = rules.remove(idx);
                println!("{} Removed rule: {}", "✓".green(), removed.pattern);
            }
            2 if !rules.is_empty() => {
                // Clear all
                rules.clear();
                println!("{} Cleared all rules", "✓".green());
            }
            _ => break,
        }
    }

    Ok(rules)
}

// =============================================================================
// Display Helpers
// =============================================================================

/// Render a data source configuration as a summary string.
pub fn render_source_summary(name: &str, source: &DataSourceConfig) -> String {
    let mut lines = Vec::new();

    match source {
        DataSourceConfig::Bluesky(cfg) => {
            lines.push(format!("{} [bluesky]", name.cyan()));
            if !cfg.keywords.is_empty() {
                lines.push(format!("  Keywords included: {}", cfg.keywords.join(", ")));
            }
            if !cfg.exclude_keywords.is_empty() {
                lines.push(format!(
                    "  Keywords excluded: {}",
                    cfg.exclude_keywords.join(", ")
                ));
            }
            if !cfg.dids.is_empty() {
                lines.push(format!("  DIDs watched: {}", cfg.dids.join(", ")));
            } else {
                lines.push("  DIDs watched: (all)".to_string());
            }
            if !cfg.friends.is_empty() {
                lines.push(format!("  Friends watched: {}", cfg.friends.join(", ")));
            }
            if !cfg.languages.is_empty() {
                lines.push(format!("  Languages watched: {}", cfg.languages.join(", ")));
            } else {
                lines.push("  Languages watched: (all)".to_string());
            }
            if !cfg.mentions.is_empty() {
                lines.push(format!(
                    "  Mentions watched for: {}",
                    cfg.mentions.join(", ")
                ));
            }
            if !cfg.nsids.is_empty() {
                lines.push(format!("  NSIDs watched: {}", cfg.nsids.join(", ")));
            } else {
                lines.push("  NSIDs watched: (all)".to_string());
            }
            if !cfg.exclude_dids.is_empty() {
                lines.push(format!("  Exclude DIDs: {}", cfg.exclude_dids.join(", ")));
            }
            if cfg.allow_any_mentions {
                lines.push("  All mentions may be shown".to_string());
            } else {
                lines.push("  Only mentions from DID list + friends may be shown".to_string());
            }
            if cfg.require_agent_participation {
                lines.push("  Agent will not see threads it was not invited into".to_string());
            } else {
                lines
                    .push("  Agent may see content adjacent to previous participation".to_string());
            }
        }
        DataSourceConfig::Discord(cfg) => {
            lines.push(format!("{} [discord]", name.cyan()));
            if let Some(guild) = &cfg.guild_id {
                lines.push(format!("  Guild: {}", guild));
            } else {
                lines.push("  Guild: (all)".to_string());
            }
            if !cfg.channel_ids.is_empty() {
                lines.push(format!("  Channels: {}", cfg.channel_ids.join(", ")));
            } else {
                lines.push("  Channels: (all)".to_string());
            }
        }
        DataSourceConfig::File(cfg) => {
            lines.push(format!("{} [file]", name.cyan()));
            lines.push(format!("  Paths: {}", cfg.paths.len()));
            for path in &cfg.paths {
                lines.push(format!("    - {}", path.display()));
            }
            lines.push(format!("  Recursive: {}", cfg.recursive));
            if !cfg.include_patterns.is_empty() {
                lines.push(format!("  Include: {}", cfg.include_patterns.join(", ")));
            }
            if !cfg.exclude_patterns.is_empty() {
                lines.push(format!("  Exclude: {}", cfg.exclude_patterns.join(", ")));
            }
            if !cfg.permission_rules.is_empty() {
                lines.push(format!(
                    "  Permission rules: {}",
                    cfg.permission_rules.len()
                ));
                for rule in &cfg.permission_rules {
                    lines.push(format!("    {} -> {:?}", rule.pattern, rule.permission));
                }
            }
        }
        DataSourceConfig::Shell(cfg) => {
            lines.push(format!("{} [shell]", name.cyan()));
            let config_preview = serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".to_string());
            if config_preview.len() > 50 {
                lines.push(format!("  Config: {}...", &config_preview[..50]));
            } else {
                lines.push(format!("  Config: {}", config_preview));
            }
        }
        DataSourceConfig::Custom(cfg) => {
            lines.push(format!("{} [{}]", name.cyan(), cfg.source_type));
            let config_preview =
                serde_json::to_string(&cfg.config).unwrap_or_else(|_| "{}".to_string());
            if config_preview.len() > 50 {
                lines.push(format!("  Config: {}...", &config_preview[..50]));
            } else {
                lines.push(format!("  Config: {}", config_preview));
            }
        }
    }

    lines.join("\n")
}
