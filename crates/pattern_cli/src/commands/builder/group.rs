//! Group-specific builder implementation.

use std::collections::HashMap;
use std::path::PathBuf;

use dialoguer::Select;
use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::{
    BlueskySourceConfig, CustomSourceConfig, DataSourceConfig, DiscordSourceConfig,
    FileSourceConfig, GroupConfig, GroupMemberConfig, GroupMemberRoleConfig, GroupPatternConfig,
    MemoryBlockConfig,
};
use pattern_core::db::ConstellationDatabases;
use pattern_core::memory::{MemoryPermission, MemoryType};

use super::display::{SummaryRenderer, format_optional};
use super::editors::{
    self, CollectionAction, CollectionItem, confirm, edit_enum, edit_text, input_optional,
    input_required, select_menu,
};
use super::save::{SaveContext, SaveResult};
use super::{ConfigSource, MenuChoice, Section, write_state_cache};
use crate::helpers::generate_id;

/// Coordination pattern types for selection.
const PATTERN_TYPES: &[&str] = &[
    "round_robin",
    "supervisor",
    "pipeline",
    "dynamic",
    "sleeptime",
];

/// Group configuration builder.
pub struct GroupBuilder {
    /// The configuration being built.
    pub config: GroupConfig,
    /// Source of the configuration.
    pub source: ConfigSource,
    /// Whether any changes have been made.
    pub modified: bool,
    /// Database connections for saves.
    dbs: Option<ConstellationDatabases>,
}

impl GroupBuilder {
    /// Create a new builder with default configuration.
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            config: GroupConfig {
                id: None,
                name: name.to_string(),
                description: description.to_string(),
                pattern: GroupPatternConfig::RoundRobin {
                    skip_unavailable: true,
                },
                members: vec![],
                shared_memory: HashMap::new(),
                data_sources: HashMap::new(),
            },
            source: ConfigSource::New,
            modified: false,
            dbs: None,
        }
    }

    /// Create a builder with default empty configuration.
    pub fn default() -> Self {
        Self::new("", "")
    }

    /// Create a builder from an existing config file.
    pub async fn from_file(path: PathBuf) -> Result<Self> {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| miette::miette!("Failed to read file {}: {}", path.display(), e))?;

        let config: GroupConfig =
            toml::from_str(&content).map_err(|e| miette::miette!("Failed to parse TOML: {}", e))?;

        Ok(Self {
            config,
            source: ConfigSource::FromFile(path),
            modified: false,
            dbs: None,
        })
    }

    /// Create a builder from an existing group in the database.
    pub async fn from_db(dbs: ConstellationDatabases, name: &str) -> Result<Self> {
        let group = pattern_db::queries::get_group_by_name(dbs.constellation.pool(), name)
            .await
            .map_err(|e| miette::miette!("Database error: {}", e))?
            .ok_or_else(|| miette::miette!("Group '{}' not found", name))?;

        // Get members
        let members = pattern_db::queries::get_group_members(dbs.constellation.pool(), &group.id)
            .await
            .map_err(|e| miette::miette!("Failed to get group members: {}", e))?;

        // Convert DB members to config members
        let mut member_configs = Vec::new();
        for member in members {
            // Look up agent name
            let agent = pattern_db::queries::get_agent(dbs.constellation.pool(), &member.agent_id)
                .await
                .map_err(|e| miette::miette!("Failed to get agent: {}", e))?;

            let agent_name = agent
                .map(|a| a.name)
                .unwrap_or_else(|| member.agent_id.clone());

            let role = match member.role.as_ref().map(|j| &j.0) {
                Some(pattern_db::models::GroupMemberRole::Supervisor) => {
                    GroupMemberRoleConfig::Supervisor
                }
                Some(pattern_db::models::GroupMemberRole::Observer) => {
                    GroupMemberRoleConfig::Observer
                }
                Some(pattern_db::models::GroupMemberRole::Specialist { domain }) => {
                    GroupMemberRoleConfig::Specialist {
                        domain: domain.clone(),
                    }
                }
                _ => GroupMemberRoleConfig::Regular,
            };

            member_configs.push(GroupMemberConfig {
                name: agent_name,
                agent_id: Some(pattern_core::id::AgentId(member.agent_id)),
                config_path: None,
                agent_config: None,
                role,
                capabilities: member.capabilities.0.clone(),
            });
        }

        // Convert DB pattern type to config pattern
        let pattern = match group.pattern_type {
            pattern_db::models::PatternType::RoundRobin => GroupPatternConfig::RoundRobin {
                skip_unavailable: true,
            },
            pattern_db::models::PatternType::Supervisor => GroupPatternConfig::Supervisor {
                leader: member_configs
                    .first()
                    .map(|m| m.name.clone())
                    .unwrap_or_default(),
            },
            pattern_db::models::PatternType::Pipeline => GroupPatternConfig::Pipeline {
                stages: member_configs.iter().map(|m| m.name.clone()).collect(),
            },
            pattern_db::models::PatternType::Dynamic => GroupPatternConfig::Dynamic {
                selector: "random".to_string(),
                selector_config: HashMap::new(),
            },
            pattern_db::models::PatternType::Voting => {
                // Voting doesn't exist in config, map to RoundRobin
                GroupPatternConfig::RoundRobin {
                    skip_unavailable: true,
                }
            }
            pattern_db::models::PatternType::Sleeptime => GroupPatternConfig::Sleeptime {
                check_interval: 60,
                triggers: vec![],
                intervention_agent: None,
            },
        };

        // TODO: Load shared_memory from DB (blocks with agent_id = '_constellation_')
        // TODO: Load data_sources from DB or pattern_config JSON
        let config = GroupConfig {
            id: Some(pattern_core::id::GroupId(group.id)),
            name: group.name,
            description: group.description.unwrap_or_default(),
            pattern,
            members: member_configs,
            shared_memory: HashMap::new(),
            data_sources: HashMap::new(),
        };

        Ok(Self {
            config,
            source: ConfigSource::FromDb(name.to_string()),
            modified: false,
            dbs: Some(dbs),
        })
    }

    /// Set the database connections for saving.
    pub fn with_dbs(mut self, dbs: ConstellationDatabases) -> Self {
        self.dbs = Some(dbs);
        self
    }

    /// Render the configuration summary.
    pub fn render_summary(&self) -> String {
        let mut r = SummaryRenderer::new(&format!("Group: {}", self.config.name));

        // Basic Info
        r.section("Basic Info");
        r.kv("Name", &self.config.name);
        r.kv(
            "Description",
            &format_optional(Some(&self.config.description)),
        );

        // Pattern
        r.section("Pattern");
        let pattern_name = match &self.config.pattern {
            GroupPatternConfig::RoundRobin { skip_unavailable } => {
                format!("Round Robin (skip unavailable: {})", skip_unavailable)
            }
            GroupPatternConfig::Supervisor { leader } => {
                format!("Supervisor (leader: {})", leader)
            }
            GroupPatternConfig::Pipeline { stages } => {
                format!("Pipeline ({} stages)", stages.len())
            }
            GroupPatternConfig::Dynamic { selector, .. } => {
                format!("Dynamic (selector: {})", selector)
            }
            GroupPatternConfig::Sleeptime {
                check_interval,
                triggers,
                ..
            } => {
                format!(
                    "Sleeptime (interval: {}s, {} triggers)",
                    check_interval,
                    triggers.len()
                )
            }
        };
        r.kv("Type", &pattern_name);

        // Members
        r.section(&format!("Members ({})", self.config.members.len()));
        if self.config.members.is_empty() {
            r.kv_dimmed("", "(none)");
        } else {
            for member in &self.config.members {
                let role_str = match &member.role {
                    GroupMemberRoleConfig::Regular => "regular",
                    GroupMemberRoleConfig::Supervisor => "supervisor",
                    GroupMemberRoleConfig::Observer => "observer",
                    GroupMemberRoleConfig::Specialist { domain } => domain,
                };
                r.list_item(&format!(
                    "{} {}",
                    member.name.cyan(),
                    format!("[{}]", role_str).dimmed()
                ));
            }
        }

        // Shared Memory
        r.section(&format!(
            "Shared Memory ({})",
            self.config.shared_memory.len()
        ));
        if self.config.shared_memory.is_empty() {
            r.kv_dimmed("", "(none)");
        } else {
            for (label, block) in &self.config.shared_memory {
                let perm = format!("[{:?}]", block.permission).to_lowercase();
                r.list_item(&format!("{} {}", label.cyan(), perm.dimmed()));
            }
        }

        // Data Sources
        r.section(&format!(
            "Data Sources ({})",
            self.config.data_sources.len()
        ));
        if self.config.data_sources.is_empty() {
            r.kv_dimmed("", "(none)");
        } else {
            for (name, source) in &self.config.data_sources {
                let source_type = match source {
                    DataSourceConfig::Bluesky(_) => "bluesky",
                    DataSourceConfig::Discord(_) => "discord",
                    DataSourceConfig::File(_) => "file",
                    DataSourceConfig::Custom(c) => &c.source_type,
                };
                r.list_item(&format!(
                    "{} {}",
                    name.cyan(),
                    format!("[{}]", source_type).dimmed()
                ));
            }
        }

        r.finish()
    }

    /// Get the sections available for this builder.
    pub fn sections() -> Vec<Section> {
        vec![
            Section::BasicInfo,
            Section::Pattern,
            Section::Members,
            Section::MemoryBlocks, // Reuse for shared memory
            Section::Integrations, // Reuse for data sources
        ]
    }

    /// Display the main menu and get user choice.
    pub fn show_menu(&self) -> Result<MenuChoice> {
        let sections = Self::sections();
        let mut options: Vec<String> = sections
            .iter()
            .map(|s| s.display_name().to_string())
            .collect();
        options.push("Done - Save".green().to_string());
        options.push("Cancel".red().to_string());

        println!();
        let selection = Select::new()
            .with_prompt("What would you like to change?")
            .items(&options)
            .default(options.len() - 2)
            .interact()
            .map_err(|e| miette::miette!("Selection error: {}", e))?;

        if selection < sections.len() {
            Ok(MenuChoice::EditSection(sections[selection]))
        } else if selection == sections.len() {
            Ok(MenuChoice::Done)
        } else {
            Ok(MenuChoice::Cancel)
        }
    }

    /// Edit a section of the configuration.
    pub async fn edit_section(&mut self, section: Section) -> Result<()> {
        match section {
            Section::BasicInfo => self.edit_basic_info()?,
            Section::Pattern => self.edit_pattern()?,
            Section::Members => self.edit_members().await?,
            Section::MemoryBlocks => self.edit_shared_memory()?,
            Section::Integrations => self.edit_data_sources()?,
            _ => {
                println!("{}", "Section not applicable for groups".yellow());
            }
        }

        // Write state cache after each edit
        if let Ok(toml) = self.to_toml() {
            let _ = write_state_cache(&toml);
        }

        Ok(())
    }

    /// Edit basic info section.
    fn edit_basic_info(&mut self) -> Result<()> {
        println!("\n{}", "─ Basic Info ─".bold());

        // Name
        if let Some(new_name) = edit_text("Name", Some(&self.config.name), false)? {
            self.config.name = new_name;
            self.modified = true;
        }

        // Description
        if let Some(new_desc) = edit_text("Description", Some(&self.config.description), true)? {
            self.config.description = new_desc;
            self.modified = true;
        }

        Ok(())
    }

    /// Edit pattern section.
    fn edit_pattern(&mut self) -> Result<()> {
        println!("\n{}", "─ Pattern ─".bold());

        let current_idx = match &self.config.pattern {
            GroupPatternConfig::RoundRobin { .. } => 0,
            GroupPatternConfig::Supervisor { .. } => 1,
            GroupPatternConfig::Pipeline { .. } => 2,
            GroupPatternConfig::Dynamic { .. } => 3,
            GroupPatternConfig::Sleeptime { .. } => 4,
        };

        let pattern_idx = edit_enum("Pattern type", PATTERN_TYPES, current_idx)?;

        self.config.pattern = match pattern_idx {
            0 => {
                let skip = confirm("Skip unavailable agents?", true)?;
                GroupPatternConfig::RoundRobin {
                    skip_unavailable: skip,
                }
            }
            1 => {
                let leader = if self.config.members.is_empty() {
                    input_required("Leader agent name")?
                } else {
                    let member_names: Vec<&str> = self
                        .config
                        .members
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect();
                    let idx = select_menu("Select leader", &member_names, 0)?;
                    member_names[idx].to_string()
                };
                GroupPatternConfig::Supervisor { leader }
            }
            2 => {
                println!("  Enter stages in order (member names):");
                let stages = if self.config.members.is_empty() {
                    // Manual entry
                    let mut stages = Vec::new();
                    loop {
                        let stage = input_optional(&format!(
                            "Stage {} (empty to finish)",
                            stages.len() + 1
                        ))?;
                        match stage {
                            Some(s) if !s.is_empty() => stages.push(s),
                            _ => break,
                        }
                    }
                    stages
                } else {
                    // Select from existing members
                    let member_names: Vec<&str> = self
                        .config
                        .members
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect();
                    let selected =
                        editors::edit_multiselect("Pipeline stages", &member_names, &[])?;
                    selected
                        .into_iter()
                        .map(|i| member_names[i].to_string())
                        .collect()
                };
                GroupPatternConfig::Pipeline { stages }
            }
            3 => {
                let selector_options = ["random", "capability", "load_balancing", "custom"];
                let sel_idx = select_menu("Selector strategy", &selector_options, 0)?;
                let selector = selector_options[sel_idx].to_string();
                GroupPatternConfig::Dynamic {
                    selector,
                    selector_config: HashMap::new(),
                }
            }
            _ => {
                let interval_str = input_required("Check interval (seconds)")?;
                let check_interval: u64 = interval_str
                    .parse()
                    .map_err(|_| miette::miette!("Invalid number"))?;

                let intervention = input_optional("Intervention agent name (optional)")?;

                GroupPatternConfig::Sleeptime {
                    check_interval,
                    triggers: vec![],
                    intervention_agent: intervention,
                }
            }
        };
        self.modified = true;

        Ok(())
    }

    /// Edit members section.
    async fn edit_members(&mut self) -> Result<()> {
        println!("\n{}", "─ Members ─".bold());

        loop {
            let items: Vec<MemberItem> = self
                .config
                .members
                .iter()
                .map(|m| MemberItem { member: m.clone() })
                .collect();

            match editors::edit_collection("Members", &items)? {
                CollectionAction::Add => {
                    self.add_member().await?;
                }
                CollectionAction::Edit(idx) => {
                    self.edit_member(idx)?;
                }
                CollectionAction::Remove(idx) => {
                    let name = self.config.members[idx].name.clone();
                    self.config.members.remove(idx);
                    self.modified = true;
                    println!("{} Removed '{}'", "✓".green(), name);
                }
                CollectionAction::EditAsToml => {
                    self.edit_members_as_toml()?;
                }
                CollectionAction::Done => break,
            }
        }

        Ok(())
    }

    async fn add_member(&mut self) -> Result<()> {
        let name = input_required("Member name (agent name)")?;

        // Check if name already exists
        if self.config.members.iter().any(|m| m.name == name) {
            return Err(miette::miette!("Member '{}' already exists", name));
        }

        // If we have a database, try to resolve agent ID
        let agent_id = if let Some(ref dbs) = self.dbs {
            match pattern_db::queries::get_agent_by_name(dbs.constellation.pool(), &name).await {
                Ok(Some(agent)) => Some(pattern_core::id::AgentId(agent.id)),
                _ => {
                    println!(
                        "{}",
                        format!("Note: Agent '{}' not found in database", name).yellow()
                    );
                    None
                }
            }
        } else {
            None
        };

        let role_options = ["regular", "supervisor", "observer", "specialist"];
        let role_idx = select_menu("Role", &role_options, 0)?;
        let role = match role_idx {
            1 => GroupMemberRoleConfig::Supervisor,
            2 => GroupMemberRoleConfig::Observer,
            3 => {
                let domain = input_required("Specialist domain")?;
                GroupMemberRoleConfig::Specialist { domain }
            }
            _ => GroupMemberRoleConfig::Regular,
        };

        self.config.members.push(GroupMemberConfig {
            name: name.clone(),
            agent_id,
            config_path: None,
            agent_config: None,
            role,
            capabilities: vec![],
        });
        self.modified = true;

        println!("{} Added member '{}'", "✓".green(), name);
        Ok(())
    }

    fn edit_member(&mut self, idx: usize) -> Result<()> {
        let member = &self.config.members[idx];
        println!("\nEditing member '{}'", member.name.cyan());

        let role_options = ["regular", "supervisor", "observer", "specialist"];
        let current_role_idx = match &member.role {
            GroupMemberRoleConfig::Regular => 0,
            GroupMemberRoleConfig::Supervisor => 1,
            GroupMemberRoleConfig::Observer => 2,
            GroupMemberRoleConfig::Specialist { .. } => 3,
        };

        let role_idx = select_menu("Role", &role_options, current_role_idx)?;
        let role = match role_idx {
            1 => GroupMemberRoleConfig::Supervisor,
            2 => GroupMemberRoleConfig::Observer,
            3 => {
                let domain = input_required("Specialist domain")?;
                GroupMemberRoleConfig::Specialist { domain }
            }
            _ => GroupMemberRoleConfig::Regular,
        };

        self.config.members[idx].role = role;
        self.modified = true;

        Ok(())
    }

    fn edit_members_as_toml(&mut self) -> Result<()> {
        println!("{}", "Members TOML:".bold());
        let toml = toml::to_string_pretty(&self.config.members)
            .map_err(|e| miette::miette!("Serialization error: {}", e))?;
        println!("{}", toml);
        println!("\n{}", "(Preview only)".dimmed());
        Ok(())
    }

    /// Edit shared memory blocks section.
    fn edit_shared_memory(&mut self) -> Result<()> {
        println!("\n{}", "─ Shared Memory ─".bold());

        loop {
            let items: Vec<SharedMemoryItem> = self
                .config
                .shared_memory
                .iter()
                .map(|(label, block)| SharedMemoryItem {
                    label: label.clone(),
                    block: block.clone(),
                })
                .collect();

            match editors::edit_collection("Shared Memory", &items)? {
                CollectionAction::Add => {
                    self.add_shared_memory_block()?;
                }
                CollectionAction::Edit(idx) => {
                    let label = items[idx].label.clone();
                    self.edit_shared_memory_block(&label)?;
                }
                CollectionAction::Remove(idx) => {
                    let label = &items[idx].label;
                    self.config.shared_memory.remove(label);
                    self.modified = true;
                    println!("{} Removed '{}'", "✓".green(), label);
                }
                CollectionAction::EditAsToml => {
                    println!("{}", "Shared memory TOML:".bold());
                    let toml = toml::to_string_pretty(&self.config.shared_memory)
                        .map_err(|e| miette::miette!("Serialization error: {}", e))?;
                    println!("{}", toml);
                    println!("\n{}", "(Preview only)".dimmed());
                }
                CollectionAction::Done => break,
            }
        }

        Ok(())
    }

    fn add_shared_memory_block(&mut self) -> Result<()> {
        let label = input_required("Block label")?;

        if self.config.shared_memory.contains_key(&label) {
            return Err(miette::miette!("Block '{}' already exists", label));
        }

        let content = input_optional("Initial content (or empty)")?;

        let permission_options = ["read_write", "read_only", "append"];
        let perm_idx = select_menu("Permission", &permission_options, 0)?;
        let permission = match perm_idx {
            0 => MemoryPermission::ReadWrite,
            1 => MemoryPermission::ReadOnly,
            _ => MemoryPermission::Append,
        };

        self.config.shared_memory.insert(
            label.clone(),
            MemoryBlockConfig {
                content,
                content_path: None,
                permission,
                memory_type: MemoryType::Core,
                description: None,
                id: None,
                shared: true, // Shared memory is always shared
            },
        );
        self.modified = true;

        println!("{} Added shared memory block '{}'", "✓".green(), label);
        Ok(())
    }

    fn edit_shared_memory_block(&mut self, label: &str) -> Result<()> {
        let block = self
            .config
            .shared_memory
            .get(label)
            .ok_or_else(|| miette::miette!("Block not found"))?
            .clone();

        println!("\nEditing shared memory block '{}'", label.cyan());

        let content = input_optional(&format!(
            "Content (current: {}, empty to keep)",
            block.content.as_deref().unwrap_or("(empty)")
        ))?;

        let permission_options = ["read_write", "read_only", "append"];
        let current_perm_idx = match block.permission {
            MemoryPermission::ReadWrite => 0,
            MemoryPermission::ReadOnly => 1,
            MemoryPermission::Append => 2,
            _ => 0,
        };
        let perm_idx = select_menu("Permission", &permission_options, current_perm_idx)?;
        let permission = match perm_idx {
            0 => MemoryPermission::ReadWrite,
            1 => MemoryPermission::ReadOnly,
            _ => MemoryPermission::Append,
        };

        self.config.shared_memory.insert(
            label.to_string(),
            MemoryBlockConfig {
                content: content.or(block.content),
                permission,
                ..block
            },
        );
        self.modified = true;

        Ok(())
    }

    /// Edit data sources section.
    fn edit_data_sources(&mut self) -> Result<()> {
        println!("\n{}", "─ Data Sources ─".bold());

        loop {
            let items: Vec<DataSourceItem> = self
                .config
                .data_sources
                .iter()
                .map(|(name, source)| DataSourceItem {
                    name: name.clone(),
                    source: source.clone(),
                })
                .collect();

            match editors::edit_collection("Data Sources", &items)? {
                CollectionAction::Add => {
                    self.add_data_source()?;
                }
                CollectionAction::Edit(idx) => {
                    let name = items[idx].name.clone();
                    self.edit_data_source(&name)?;
                }
                CollectionAction::Remove(idx) => {
                    let name = &items[idx].name;
                    self.config.data_sources.remove(name);
                    self.modified = true;
                    println!("{} Removed '{}'", "✓".green(), name);
                }
                CollectionAction::EditAsToml => {
                    println!("{}", "Data sources TOML:".bold());
                    let toml = toml::to_string_pretty(&self.config.data_sources)
                        .map_err(|e| miette::miette!("Serialization error: {}", e))?;
                    println!("{}", toml);
                    println!("\n{}", "(Preview only)".dimmed());
                }
                CollectionAction::Done => break,
            }
        }

        Ok(())
    }

    fn add_data_source(&mut self) -> Result<()> {
        let name = input_required("Source name")?;

        if self.config.data_sources.contains_key(&name) {
            return Err(miette::miette!("Data source '{}' already exists", name));
        }

        let source_types = ["bluesky", "discord", "file", "custom"];
        let type_idx = select_menu("Source type", &source_types, 0)?;

        let source = match type_idx {
            0 => {
                let handle = input_optional("Bluesky handle (optional)")?;
                let mentions_only = confirm("Mentions only?", false)?;
                DataSourceConfig::Bluesky(BlueskySourceConfig {
                    name: name.clone(),
                    handle,
                    keywords: vec![],
                    mentions_only,
                })
            }
            1 => {
                let guild_id = input_optional("Guild ID (optional, empty for all)")?;
                DataSourceConfig::Discord(DiscordSourceConfig {
                    name: name.clone(),
                    guild_id,
                    channel_ids: vec![],
                })
            }
            2 => {
                println!("  Enter paths to watch (one per line, empty line to finish):");
                let mut paths = Vec::new();
                loop {
                    let path_str =
                        input_optional(&format!("Path {} (empty to finish)", paths.len() + 1))?;
                    match path_str {
                        Some(p) if !p.is_empty() => paths.push(PathBuf::from(p)),
                        _ => break,
                    }
                }
                if paths.is_empty() {
                    return Err(miette::miette!("At least one path is required"));
                }
                let recursive = confirm("Watch recursively?", true)?;
                DataSourceConfig::File(FileSourceConfig {
                    name: name.clone(),
                    paths,
                    recursive,
                    include_patterns: vec![],
                    exclude_patterns: vec![],
                    permission_rules: vec![],
                })
            }
            _ => {
                let source_type = input_required("Custom source type")?;
                DataSourceConfig::Custom(CustomSourceConfig {
                    name: name.clone(),
                    source_type,
                    config: serde_json::Value::Object(serde_json::Map::new()),
                })
            }
        };

        self.config.data_sources.insert(name.clone(), source);
        self.modified = true;

        println!("{} Added data source '{}'", "✓".green(), name);
        Ok(())
    }

    fn edit_data_source(&mut self, name: &str) -> Result<()> {
        let source = self
            .config
            .data_sources
            .get(name)
            .ok_or_else(|| miette::miette!("Data source not found"))?
            .clone();

        println!("\nEditing data source '{}'", name.cyan());
        println!(
            "{}",
            "(Limited editing - consider Edit as TOML for complex changes)".dimmed()
        );

        // Type-specific editing
        match source {
            DataSourceConfig::Bluesky(mut cfg) => {
                let mentions = confirm("Mentions only?", cfg.mentions_only)?;
                cfg.mentions_only = mentions;
                self.config
                    .data_sources
                    .insert(name.to_string(), DataSourceConfig::Bluesky(cfg));
            }
            DataSourceConfig::Discord(cfg) => {
                // Keep as-is for now
                println!("Discord source: guild_id={:?}", cfg.guild_id);
            }
            DataSourceConfig::File(mut cfg) => {
                // Show current paths
                println!("Current paths:");
                for (i, path) in cfg.paths.iter().enumerate() {
                    println!("  {}. {}", i + 1, path.display());
                }

                let edit_options = ["Edit paths", "Toggle recursive", "Done"];
                loop {
                    let choice = select_menu("File source options", &edit_options, 2)?;
                    match choice {
                        0 => {
                            // Edit paths
                            println!("  Enter new paths (one per line, empty to finish):");
                            let mut new_paths = Vec::new();
                            loop {
                                let path_str = input_optional(&format!(
                                    "Path {} (empty to finish)",
                                    new_paths.len() + 1
                                ))?;
                                match path_str {
                                    Some(p) if !p.is_empty() => new_paths.push(PathBuf::from(p)),
                                    _ => break,
                                }
                            }
                            if !new_paths.is_empty() {
                                cfg.paths = new_paths;
                                println!("{} Updated paths", "✓".green());
                            }
                        }
                        1 => {
                            cfg.recursive = !cfg.recursive;
                            println!(
                                "{} Recursive: {}",
                                "✓".green(),
                                if cfg.recursive { "on" } else { "off" }
                            );
                        }
                        _ => break,
                    }
                }
                self.config
                    .data_sources
                    .insert(name.to_string(), DataSourceConfig::File(cfg));
            }
            DataSourceConfig::Custom(_) => {
                println!("Custom sources should be edited via TOML");
            }
        }
        self.modified = true;

        Ok(())
    }

    /// Validate the configuration before saving.
    pub fn validate(&self) -> Result<()> {
        if self.config.name.trim().is_empty() {
            return Err(miette::miette!("Group name is required"));
        }

        if self.config.description.trim().is_empty() {
            return Err(miette::miette!("Group description is required"));
        }

        // Validate pattern-specific requirements
        match &self.config.pattern {
            GroupPatternConfig::Supervisor { leader } => {
                if leader.is_empty() {
                    return Err(miette::miette!("Supervisor pattern requires a leader"));
                }
            }
            GroupPatternConfig::Pipeline { stages } => {
                if stages.is_empty() {
                    return Err(miette::miette!(
                        "Pipeline pattern requires at least one stage"
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Convert to TOML string.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(&self.config)
            .map_err(|e| miette::miette!("Serialization error: {}", e))
    }

    /// Run the main builder loop.
    pub async fn run(mut self) -> Result<Option<SaveResult>> {
        loop {
            // Display summary
            println!("\n{}", self.render_summary());

            // Show menu
            match self.show_menu()? {
                MenuChoice::EditSection(section) => {
                    self.edit_section(section).await?;
                }
                MenuChoice::Done => {
                    // Validate before save
                    self.validate()?;
                    return self.save().await;
                }
                MenuChoice::Cancel => {
                    if self.modified {
                        if confirm("Discard changes?", false)? {
                            return Ok(None);
                        }
                    } else {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// Save the configuration.
    async fn save(self) -> Result<Option<SaveResult>> {
        use pattern_core::memory::{
            BlockSchema, BlockType, MemoryCache, MemoryStore, SharedBlockManager,
        };
        use std::sync::Arc;

        let name = self.config.name.clone();
        let config = self.config.clone();
        let source = self.source.clone();
        let dbs = self.dbs.clone();

        let ctx = SaveContext::new(source);

        let to_toml = || {
            toml::to_string_pretty(&config)
                .map_err(|e| miette::miette!("Serialization error: {}", e))
        };

        let config_for_save = config.clone();
        let dbs_for_save = dbs.clone();

        let save_to_db = || {
            let config = config_for_save.clone();
            let dbs = dbs_for_save.clone();
            async move {
                let dbs = dbs.ok_or_else(|| miette::miette!("No database connection"))?;
                let pool = dbs.constellation.pool();

                let id = config
                    .id
                    .as_ref()
                    .map(|id| id.0.clone())
                    .unwrap_or_else(|| generate_id("grp"));

                // Convert pattern to DB type
                let pattern_type = match &config.pattern {
                    GroupPatternConfig::RoundRobin { .. } => {
                        pattern_db::models::PatternType::RoundRobin
                    }
                    GroupPatternConfig::Supervisor { .. } => {
                        pattern_db::models::PatternType::Supervisor
                    }
                    GroupPatternConfig::Pipeline { .. } => {
                        pattern_db::models::PatternType::Pipeline
                    }
                    GroupPatternConfig::Dynamic { .. } => pattern_db::models::PatternType::Dynamic,
                    GroupPatternConfig::Sleeptime { .. } => {
                        pattern_db::models::PatternType::Sleeptime
                    }
                };

                let group = pattern_db::models::AgentGroup {
                    id: id.clone(),
                    name: config.name.clone(),
                    description: Some(config.description.clone()),
                    pattern_type,
                    pattern_config: pattern_db::Json(
                        serde_json::to_value(&config.pattern).unwrap_or_default(),
                    ),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                };

                // Check if exists for update vs insert
                let existing = pattern_db::queries::get_group(pool, &id)
                    .await
                    .map_err(|e| miette::miette!("Database error: {}", e))?;

                // Get existing members for diffing
                let existing_members = if existing.is_some() {
                    pattern_db::queries::update_group(pool, &group)
                        .await
                        .map_err(|e| miette::miette!("Failed to update group: {}", e))?;

                    pattern_db::queries::get_group_members(pool, &id)
                        .await
                        .map_err(|e| miette::miette!("Failed to get members: {}", e))?
                } else {
                    pattern_db::queries::create_group(pool, &group)
                        .await
                        .map_err(|e| miette::miette!("Failed to create group: {}", e))?;
                    vec![]
                };

                // Build a map of existing member agent_ids for diffing
                let existing_member_ids: std::collections::HashSet<String> = existing_members
                    .iter()
                    .map(|m| m.agent_id.clone())
                    .collect();

                // Resolve all config members to agent IDs
                let mut desired_members: Vec<(String, &GroupMemberConfig)> = vec![];
                for member in &config.members {
                    let agent_id = if let Some(ref aid) = member.agent_id {
                        aid.0.clone()
                    } else {
                        match pattern_db::queries::get_agent_by_name(pool, &member.name).await {
                            Ok(Some(agent)) => agent.id,
                            _ => continue, // Skip members we can't find
                        }
                    };
                    desired_members.push((agent_id, member));
                }
                let desired_member_ids: std::collections::HashSet<String> =
                    desired_members.iter().map(|(id, _)| id.clone()).collect();

                // Remove members that are no longer in the config
                for existing in &existing_members {
                    if !desired_member_ids.contains(&existing.agent_id) {
                        pattern_db::queries::remove_group_member(pool, &id, &existing.agent_id)
                            .await
                            .map_err(|e| miette::miette!("Failed to remove member: {}", e))?;
                    }
                }

                // Add or update members
                for (agent_id, member) in &desired_members {
                    let role = match &member.role {
                        GroupMemberRoleConfig::Supervisor => Some(pattern_db::Json(
                            pattern_db::models::GroupMemberRole::Supervisor,
                        )),
                        GroupMemberRoleConfig::Regular => Some(pattern_db::Json(
                            pattern_db::models::GroupMemberRole::Regular,
                        )),
                        GroupMemberRoleConfig::Observer => Some(pattern_db::Json(
                            pattern_db::models::GroupMemberRole::Observer,
                        )),
                        GroupMemberRoleConfig::Specialist { domain } => Some(pattern_db::Json(
                            pattern_db::models::GroupMemberRole::Specialist {
                                domain: domain.clone(),
                            },
                        )),
                    };

                    if existing_member_ids.contains(agent_id) {
                        // Update existing member's role and capabilities
                        pattern_db::queries::update_group_member(
                            pool,
                            &id,
                            agent_id,
                            role.as_ref(),
                            &pattern_db::Json(member.capabilities.clone()),
                        )
                        .await
                        .map_err(|e| miette::miette!("Failed to update member: {}", e))?;
                    } else {
                        // Add new member
                        let db_member = pattern_db::models::GroupMember {
                            group_id: id.clone(),
                            agent_id: agent_id.clone(),
                            role,
                            capabilities: pattern_db::Json(member.capabilities.clone()),
                            joined_at: chrono::Utc::now(),
                        };
                        pattern_db::queries::add_group_member(pool, &db_member)
                            .await
                            .map_err(|e| miette::miette!("Failed to add member: {}", e))?;
                    }
                }

                // Handle shared memory blocks
                if !config.shared_memory.is_empty() {
                    let dbs_arc = Arc::new(dbs);
                    let cache = MemoryCache::new(dbs_arc.clone());
                    let sharing_manager = SharedBlockManager::new(dbs_arc);

                    // Get all member agent IDs for sharing
                    let member_agent_ids: Vec<String> =
                        desired_members.iter().map(|(id, _)| id.clone()).collect();

                    for (label, block_config) in &config.shared_memory {
                        // Check if block already exists using cache
                        let existing_doc = cache
                            .get_block(&id, label)
                            .await
                            .map_err(|e| miette::miette!("Failed to check block: {:?}", e))?;

                        let block_id = if let Some(doc) = existing_doc {
                            // Get block ID from metadata
                            let metadata = cache
                                .get_block_metadata(&id, label)
                                .await
                                .map_err(|e| {
                                    miette::miette!("Failed to get block metadata: {:?}", e)
                                })?
                                .ok_or_else(|| {
                                    miette::miette!("Block exists but metadata not found")
                                })?;
                            let existing_id = metadata.id.clone();

                            // Update content if provided - use the doc we already have
                            if let Some(ref content) = block_config.content {
                                doc.set_text(content, true)
                                    .map_err(|e| miette::miette!("Failed to set content: {}", e))?;
                                cache
                                    .persist_block(&id, label)
                                    .await
                                    .map_err(|e| miette::miette!("Failed to persist: {:?}", e))?;
                            }
                            existing_id
                        } else {
                            // Create new block
                            let block_type = match block_config.memory_type {
                                MemoryType::Core => BlockType::Core,
                                MemoryType::Working => BlockType::Working,
                                MemoryType::Archival => BlockType::Archival,
                            };

                            let block_id = cache
                                .create_block(
                                    &id,
                                    label,
                                    block_config
                                        .description
                                        .as_deref()
                                        .unwrap_or(&format!("Shared memory: {}", label)),
                                    block_type,
                                    BlockSchema::text(),
                                    2000,
                                )
                                .await
                                .map_err(|e| {
                                    miette::miette!("Failed to create shared block: {:?}", e)
                                })?;

                            // Set initial content using update_block_text
                            if let Some(ref content) = block_config.content {
                                cache.update_block_text(&id, label, content).await.map_err(
                                    |e| miette::miette!("Failed to set content: {:?}", e),
                                )?;
                            }

                            block_id
                        };

                        // Share block with all group members
                        let db_permission = match block_config.permission {
                            MemoryPermission::ReadOnly => {
                                pattern_db::models::MemoryPermission::ReadOnly
                            }
                            MemoryPermission::Partner => {
                                pattern_db::models::MemoryPermission::Partner
                            }
                            MemoryPermission::Human => pattern_db::models::MemoryPermission::Human,
                            MemoryPermission::Append => {
                                pattern_db::models::MemoryPermission::Append
                            }
                            MemoryPermission::ReadWrite => {
                                pattern_db::models::MemoryPermission::ReadWrite
                            }
                            MemoryPermission::Admin => pattern_db::models::MemoryPermission::Admin,
                        };

                        for agent_id in &member_agent_ids {
                            // Ignore errors if sharing already exists
                            let _ = sharing_manager
                                .share_block(&block_id, agent_id, db_permission)
                                .await;
                        }
                    }
                }

                Ok(id)
            }
        };

        super::save::save_config(&ctx, &name, to_toml, save_to_db).await
    }
}

/// Helper struct for displaying members in collection editor.
#[derive(Clone)]
struct MemberItem {
    member: GroupMemberConfig,
}

impl CollectionItem for MemberItem {
    fn display_short(&self) -> String {
        let role_str = match &self.member.role {
            GroupMemberRoleConfig::Regular => "regular",
            GroupMemberRoleConfig::Supervisor => "supervisor",
            GroupMemberRoleConfig::Observer => "observer",
            GroupMemberRoleConfig::Specialist { domain } => domain,
        };
        format!("{} [{}]", self.member.name, role_str.dimmed())
    }

    fn label(&self) -> String {
        self.member.name.clone()
    }
}

/// Helper struct for displaying shared memory blocks in collection editor.
#[derive(Clone)]
struct SharedMemoryItem {
    label: String,
    block: MemoryBlockConfig,
}

impl CollectionItem for SharedMemoryItem {
    fn display_short(&self) -> String {
        let perm = format!("[{:?}]", self.block.permission).to_lowercase();
        format!("{} {}", self.label, perm.dimmed())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

/// Helper struct for displaying data sources in collection editor.
#[derive(Clone)]
struct DataSourceItem {
    name: String,
    source: DataSourceConfig,
}

impl CollectionItem for DataSourceItem {
    fn display_short(&self) -> String {
        let source_type = match &self.source {
            DataSourceConfig::Bluesky(_) => "bluesky",
            DataSourceConfig::Discord(_) => "discord",
            DataSourceConfig::File(_) => "file",
            DataSourceConfig::Custom(c) => &c.source_type,
        };
        format!("{} {}", self.name, format!("[{}]", source_type).dimmed())
    }

    fn label(&self) -> String {
        self.name.clone()
    }
}
