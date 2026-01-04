//! Agent-specific builder implementation.

use std::collections::HashMap;
use std::path::PathBuf;

use dialoguer::Select;
use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::{
    AgentConfig, ContextConfigOptions, DataSourceConfig, MemoryBlockConfig, ModelConfig,
    ToolRuleConfig,
};
use pattern_core::db::ConstellationDatabases;
use pattern_core::memory::MemoryType;

use super::display::{SummaryRenderer, format_optional, format_path, truncate};
use super::editors::{
    self, CollectionAction, CollectionItem, TextOrPath, edit_enum, edit_text, edit_text_or_file,
    edit_tools_multiselect, input_required, select_menu,
};
use super::save::{SaveContext, SaveResult};
use super::{ConfigSource, MenuChoice, Section, write_state_cache};
use crate::data_source_config;
use crate::helpers::generate_id;

/// Known model providers for selection.
const MODEL_PROVIDERS: &[&str] = &["anthropic", "openai", "gemini", "ollama"];

/// Agent configuration builder.
pub struct AgentBuilder {
    /// The configuration being built.
    pub config: AgentConfig,
    /// Source of the configuration.
    pub source: ConfigSource,
    /// Whether any changes have been made.
    pub modified: bool,
    /// Database connections for saves.
    dbs: Option<ConstellationDatabases>,
}

impl AgentBuilder {
    /// Create a new builder with default configuration.
    pub fn new() -> Self {
        Self {
            config: AgentConfig::default(),
            source: ConfigSource::New,
            modified: false,
            dbs: None,
        }
    }

    /// Create a builder from an existing config file.
    pub async fn from_file(path: PathBuf) -> Result<Self> {
        let config = AgentConfig::load_from_file(&path)
            .await
            .map_err(|e| miette::miette!("Failed to load config from {}: {}", path.display(), e))?;

        Ok(Self {
            config,
            source: ConfigSource::FromFile(path),
            modified: false,
            dbs: None,
        })
    }

    /// Create a builder from an existing agent in the database.
    pub async fn from_db(dbs: ConstellationDatabases, name: &str) -> Result<Self> {
        let agent = pattern_db::queries::get_agent_by_name(dbs.constellation.pool(), name)
            .await
            .map_err(|e| miette::miette!("Database error: {}", e))?
            .ok_or_else(|| miette::miette!("Agent '{}' not found", name))?;

        // Start from JSON config if parseable, otherwise default
        let mut config: AgentConfig =
            serde_json::from_value(agent.config.0.clone()).unwrap_or_default();

        // Always merge authoritative fields from DB columns (JSON may be stale/incomplete)
        config.id = Some(pattern_core::id::AgentId(agent.id.clone()));
        config.name = agent.name.clone();

        // Use DB system_prompt if config's is missing/empty
        if config.system_prompt.is_none()
            || config.system_prompt.as_ref().is_some_and(|s| s.is_empty())
        {
            if !agent.system_prompt.is_empty() {
                config.system_prompt = Some(agent.system_prompt.clone());
            }
        }

        // Use DB model info if config's is missing
        if config.model.is_none() {
            config.model = Some(ModelConfig {
                provider: agent.model_provider.clone(),
                model: Some(agent.model_name.clone()),
                temperature: None,
                settings: HashMap::new(),
            });
        }

        // Use DB tools if config's is missing/empty
        if config.tools.is_empty() && !agent.enabled_tools.0.is_empty() {
            config.tools = agent.enabled_tools.0.clone();
        }

        // Use DB tool_rules if config's is missing
        if config.tool_rules.is_empty() {
            if let Some(ref rules_json) = agent.tool_rules {
                if let Ok(rules) = serde_json::from_value(rules_json.0.clone()) {
                    config.tool_rules = rules;
                }
            }
        }

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
        let mut r = SummaryRenderer::new(&format!("Agent: {}", self.config.name));

        // Basic Info
        r.section("Basic Info");
        r.kv("Name", &self.config.name);

        if let Some(ref path) = self.config.system_prompt_path {
            r.kv("System", &format_path(path));
        } else {
            r.kv(
                "System",
                &format_optional(self.config.system_prompt.as_deref()),
            );
        }

        if let Some(ref path) = self.config.persona_path {
            r.kv("Persona", &format_path(path));
        } else {
            r.kv("Persona", &format_optional(self.config.persona.as_deref()));
        }

        r.kv(
            "Instructions",
            &format_optional(self.config.instructions.as_deref()),
        );

        // Model
        r.section("Model");
        if let Some(ref model) = self.config.model {
            r.kv("Provider", &model.provider);
            r.kv("Model", model.model.as_deref().unwrap_or("(default)"));
            if let Some(temp) = model.temperature {
                r.kv("Temperature", &format!("{:.2}", temp));
            } else {
                r.kv_dimmed("Temperature", "(default)");
            }
        } else {
            r.kv_dimmed("Provider", "(not set)");
            r.kv_dimmed("Model", "(not set)");
        }

        // Memory Blocks
        r.section(&format!("Memory Blocks ({})", self.config.memory.len()));
        if self.config.memory.is_empty() {
            r.kv_dimmed("", "(none)");
        } else {
            for (label, block) in &self.config.memory {
                let perm = format!("[{:?}]", block.permission).to_lowercase();
                let preview = block
                    .content
                    .as_ref()
                    .map(|c| truncate(c, 30))
                    .unwrap_or_else(|| "(empty)".to_string());
                r.list_item(&format!(
                    "{} {} {}",
                    label.cyan(),
                    perm.dimmed(),
                    preview.dimmed()
                ));
            }
        }

        // Tools
        r.section(&format!("Tools ({})", self.config.tools.len()));
        r.inline_list(&self.config.tools);

        // Tool Rules
        r.section(&format!("Tool Rules ({})", self.config.tool_rules.len()));
        if self.config.tool_rules.is_empty() {
            r.kv_dimmed("", "(none)");
        } else {
            for rule in &self.config.tool_rules {
                let rule_type = format!("{:?}", rule.rule_type)
                    .split('{')
                    .next()
                    .unwrap_or("Unknown")
                    .to_lowercase();
                r.list_item(&format!("{}: {}", rule.tool_name.cyan(), rule_type));
            }
        }

        // Context Options
        r.section("Context");
        if let Some(ref ctx) = self.config.context {
            if let Some(max) = ctx.max_messages {
                r.kv("Max messages", &max.to_string());
            }
            if let Some(ref strat) = ctx.compression_strategy {
                let strat_name = format!("{:?}", strat)
                    .split('{')
                    .next()
                    .unwrap_or("Unknown")
                    .to_lowercase();
                r.kv("Compression", &strat_name);
            }
        } else {
            r.kv_dimmed("", "(using defaults)");
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
                    DataSourceConfig::Shell(_) => "shell",
                    DataSourceConfig::Custom(c) => &c.source_type,
                };
                r.list_item(&format!(
                    "{} {}",
                    name.cyan(),
                    format!("[{}]", source_type).dimmed()
                ));
            }
        }

        // Integrations
        r.section("Integrations");
        r.kv(
            "Bluesky",
            &format_optional(self.config.bluesky_handle.as_deref()),
        );

        r.finish()
    }

    /// Get the sections available for this builder.
    pub fn sections() -> Vec<Section> {
        vec![
            Section::BasicInfo,
            Section::Model,
            Section::MemoryBlocks,
            Section::ToolsAndRules,
            Section::ContextOptions,
            Section::DataSources,
            Section::Integrations,
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
            .default(options.len() - 2) // Default to Done
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
    pub fn edit_section(&mut self, section: Section) -> Result<()> {
        match section {
            Section::BasicInfo => self.edit_basic_info()?,
            Section::Model => self.edit_model()?,
            Section::MemoryBlocks => self.edit_memory_blocks()?,
            Section::ToolsAndRules => self.edit_tools_and_rules()?,
            Section::ContextOptions => self.edit_context_options()?,
            Section::DataSources => self.edit_data_sources()?,
            Section::Integrations => self.edit_integrations()?,
            _ => {
                println!("{}", "Section not applicable for agents".yellow());
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

        // Name (required)
        if let Some(new_name) = edit_text("Name", Some(&self.config.name), false)? {
            self.config.name = new_name;
            self.modified = true;
        }

        // System prompt
        match edit_text_or_file(
            "System prompt",
            self.config.system_prompt.as_deref(),
            self.config.system_prompt_path.as_deref(),
        )? {
            TextOrPath::Keep => {}
            TextOrPath::Text(text) => {
                self.config.system_prompt = Some(text);
                self.config.system_prompt_path = None;
                self.modified = true;
            }
            TextOrPath::Path(path) => {
                self.config.system_prompt_path = Some(path);
                self.config.system_prompt = None;
                self.modified = true;
            }
        }

        // Persona
        match edit_text_or_file(
            "Persona",
            self.config.persona.as_deref(),
            self.config.persona_path.as_deref(),
        )? {
            TextOrPath::Keep => {}
            TextOrPath::Text(text) => {
                self.config.persona = Some(text);
                self.config.persona_path = None;
                self.modified = true;
            }
            TextOrPath::Path(path) => {
                self.config.persona_path = Some(path);
                self.config.persona = None;
                self.modified = true;
            }
        }

        // Instructions
        if let Some(new_instructions) =
            edit_text("Instructions", self.config.instructions.as_deref(), true)?
        {
            self.config.instructions = Some(new_instructions);
            self.modified = true;
        }

        Ok(())
    }

    /// Edit model section.
    fn edit_model(&mut self) -> Result<()> {
        println!("\n{}", "─ Model ─".bold());

        let current_provider = self
            .config
            .model
            .as_ref()
            .map(|m| m.provider.as_str())
            .unwrap_or("anthropic");

        let current_idx = MODEL_PROVIDERS
            .iter()
            .position(|&p| p == current_provider)
            .unwrap_or(0);

        let provider_idx = edit_enum("Provider", MODEL_PROVIDERS, current_idx)?;
        let provider = MODEL_PROVIDERS[provider_idx].to_string();

        let current_model = self.config.model.as_ref().and_then(|m| m.model.as_deref());

        let model = edit_text("Model name", current_model, true)?;

        let current_temp = self
            .config
            .model
            .as_ref()
            .and_then(|m| m.temperature)
            .map(|t| t.to_string());

        let temp_str = edit_text("Temperature", current_temp.as_deref(), true)?;
        let temperature = temp_str.and_then(|s| s.parse::<f32>().ok());

        self.config.model = Some(ModelConfig {
            provider,
            model,
            temperature,
            settings: self
                .config
                .model
                .as_ref()
                .map(|m| m.settings.clone())
                .unwrap_or_default(),
        });
        self.modified = true;

        Ok(())
    }

    /// Edit memory blocks section.
    fn edit_memory_blocks(&mut self) -> Result<()> {
        println!("\n{}", "─ Memory Blocks ─".bold());

        loop {
            let items: Vec<MemoryBlockItem> = self
                .config
                .memory
                .iter()
                .map(|(label, block)| MemoryBlockItem {
                    label: label.clone(),
                    block: block.clone(),
                })
                .collect();

            match editors::edit_collection("Memory Blocks", &items)? {
                CollectionAction::Add => {
                    self.add_memory_block()?;
                }
                CollectionAction::Edit(idx) => {
                    let label = items[idx].label.clone();
                    self.edit_memory_block(&label)?;
                }
                CollectionAction::Remove(idx) => {
                    let label = &items[idx].label;
                    self.config.memory.remove(label);
                    self.modified = true;
                    println!("{} Removed '{}'", "✓".green(), label);
                }
                CollectionAction::EditAsToml => {
                    self.edit_memory_blocks_as_toml()?;
                }
                CollectionAction::Done => break,
            }
        }

        Ok(())
    }

    fn add_memory_block(&mut self) -> Result<()> {
        let label = editors::input_required("Block label")?;

        if self.config.memory.contains_key(&label) {
            return Err(miette::miette!("Block '{}' already exists", label));
        }

        let content = edit_text_or_file("Content", None, None)?;

        let (content, content_path) = match content {
            TextOrPath::Keep => (None, None),
            TextOrPath::Text(s) if s.is_empty() => (None, None),
            TextOrPath::Text(s) => (Some(s), None),
            TextOrPath::Path(p) => (None, Some(p)),
        };

        let permission_options = [
            "read_write",
            "read_only",
            "append",
            "partner",
            "human",
            "admin",
        ];
        let perm_idx = editors::select_menu("Permission", &permission_options, 0)?;
        let permission = match perm_idx {
            0 => pattern_core::memory::MemoryPermission::ReadWrite,
            1 => pattern_core::memory::MemoryPermission::ReadOnly,
            2 => pattern_core::memory::MemoryPermission::Append,
            3 => pattern_core::memory::MemoryPermission::Partner,
            4 => pattern_core::memory::MemoryPermission::Human,
            _ => pattern_core::memory::MemoryPermission::Admin,
        };

        let type_options = ["core", "working", "archival"];
        let type_idx = editors::select_menu("Memory type", &type_options, 0)?;
        let memory_type = match type_idx {
            0 => pattern_core::memory::MemoryType::Core,
            1 => pattern_core::memory::MemoryType::Working,
            _ => pattern_core::memory::MemoryType::Archival,
        };

        let shared = editors::confirm("Shared with other agents?", false)?;

        self.config.memory.insert(
            label.clone(),
            MemoryBlockConfig {
                content,
                content_path,
                permission,
                memory_type,
                description: None,
                id: None,
                shared,
            },
        );
        self.modified = true;

        println!("{} Added memory block '{}'", "✓".green(), label);
        Ok(())
    }

    fn edit_memory_block(&mut self, label: &str) -> Result<()> {
        let block = self
            .config
            .memory
            .get(label)
            .ok_or_else(|| miette::miette!("Block not found"))?
            .clone();

        let content = edit_text_or_file(
            "Content",
            block.content.as_deref(),
            block.content_path.as_deref(),
        )?;

        let (new_content, new_path) = match content {
            TextOrPath::Keep => (block.content.clone(), block.content_path.clone()),
            TextOrPath::Text(s) => (Some(s), None),
            TextOrPath::Path(p) => (None, Some(p)),
        };

        let permission_options = [
            "read_write",
            "read_only",
            "append",
            "partner",
            "human",
            "admin",
        ];
        let current_perm_idx = match block.permission {
            pattern_core::memory::MemoryPermission::ReadWrite => 0,
            pattern_core::memory::MemoryPermission::ReadOnly => 1,
            pattern_core::memory::MemoryPermission::Append => 2,
            pattern_core::memory::MemoryPermission::Partner => 3,
            pattern_core::memory::MemoryPermission::Human => 4,
            pattern_core::memory::MemoryPermission::Admin => 5,
        };
        let perm_idx = editors::select_menu("Permission", &permission_options, current_perm_idx)?;
        let permission = match perm_idx {
            0 => pattern_core::memory::MemoryPermission::ReadWrite,
            1 => pattern_core::memory::MemoryPermission::ReadOnly,
            2 => pattern_core::memory::MemoryPermission::Append,
            3 => pattern_core::memory::MemoryPermission::Partner,
            4 => pattern_core::memory::MemoryPermission::Human,
            _ => pattern_core::memory::MemoryPermission::Admin,
        };

        self.config.memory.insert(
            label.to_string(),
            MemoryBlockConfig {
                content: new_content,
                content_path: new_path,
                permission,
                ..block
            },
        );
        self.modified = true;

        Ok(())
    }

    fn edit_memory_blocks_as_toml(&mut self) -> Result<()> {
        println!("{}", "Edit memory blocks as TOML:".bold());
        let toml = toml::to_string_pretty(&self.config.memory)
            .map_err(|e| miette::miette!("Serialization error: {}", e))?;
        println!("{}", toml);
        println!(
            "\n{}",
            "(Copy, edit, and paste back. This is a preview only for now.)".dimmed()
        );
        // TODO: Actually implement temp file editing if desired
        Ok(())
    }

    /// Edit tools and rules section.
    fn edit_tools_and_rules(&mut self) -> Result<()> {
        println!("\n{}", "─ Tools & Rules ─".bold());

        let options = ["Edit tools list", "Edit tool rules", "Done"];
        loop {
            let selection = editors::select_menu("What to edit?", &options, 2)?;
            match selection {
                0 => {
                    let new_tools = edit_tools_multiselect(&self.config.tools)?;
                    self.config.tools = new_tools;
                    self.modified = true;
                }
                1 => {
                    self.edit_tool_rules()?;
                }
                _ => break,
            }
        }

        Ok(())
    }

    fn edit_tool_rules(&mut self) -> Result<()> {
        loop {
            let items: Vec<ToolRuleItem> = self
                .config
                .tool_rules
                .iter()
                .map(|r| ToolRuleItem { rule: r.clone() })
                .collect();

            match editors::edit_collection("Tool Rules", &items)? {
                CollectionAction::Add => {
                    self.add_tool_rule()?;
                }
                CollectionAction::Edit(idx) => {
                    self.edit_tool_rule(idx)?;
                }
                CollectionAction::Remove(idx) => {
                    self.config.tool_rules.remove(idx);
                    self.modified = true;
                    println!("{} Removed rule", "✓".green());
                }
                CollectionAction::EditAsToml => {
                    println!("{}", "Tool rules TOML:".bold());
                    let toml = toml::to_string_pretty(&self.config.tool_rules)
                        .map_err(|e| miette::miette!("Serialization error: {}", e))?;
                    println!("{}", toml);
                    println!("\n{}", "(Preview only)".dimmed());
                }
                CollectionAction::Done => break,
            }
        }
        Ok(())
    }

    fn add_tool_rule(&mut self) -> Result<()> {
        let tool_name = editors::input_required("Tool name")?;

        let rule_types = [
            "ContinueLoop",
            "ExitLoop",
            "StartConstraint",
            "MaxCalls",
            "Cooldown",
            "RequiresPrecedingTools",
        ];
        let type_idx = editors::select_menu("Rule type", &rule_types, 0)?;

        let rule_type = match type_idx {
            0 => pattern_core::config::ToolRuleTypeConfig::ContinueLoop,
            1 => pattern_core::config::ToolRuleTypeConfig::ExitLoop,
            2 => pattern_core::config::ToolRuleTypeConfig::StartConstraint,
            3 => {
                let max_str = editors::input_required("Max calls")?;
                let max: u32 = max_str
                    .parse()
                    .map_err(|_| miette::miette!("Invalid number"))?;
                pattern_core::config::ToolRuleTypeConfig::MaxCalls(max)
            }
            4 => {
                let secs_str = editors::input_required("Cooldown (seconds)")?;
                let secs: u64 = secs_str
                    .parse()
                    .map_err(|_| miette::miette!("Invalid number"))?;
                pattern_core::config::ToolRuleTypeConfig::Cooldown(secs)
            }
            _ => pattern_core::config::ToolRuleTypeConfig::RequiresPrecedingTools,
        };

        let priority_str = editors::input_optional("Priority (1-10, default 5)")?;
        let priority: u8 = priority_str.and_then(|s| s.parse().ok()).unwrap_or(5);

        self.config.tool_rules.push(ToolRuleConfig {
            tool_name,
            rule_type,
            conditions: vec![],
            priority,
            metadata: None,
        });
        self.modified = true;

        println!("{} Added tool rule", "✓".green());
        Ok(())
    }

    fn edit_tool_rule(&mut self, idx: usize) -> Result<()> {
        let rule = &self.config.tool_rules[idx];
        println!("\nEditing rule for '{}'", rule.tool_name.cyan());
        println!(
            "{}",
            "(Rule editing is limited - consider Edit as TOML for complex changes)".dimmed()
        );

        // For now, just allow changing priority
        let priority_str = editors::input_optional(&format!(
            "Priority (current: {}, empty to keep)",
            rule.priority
        ))?;
        if let Some(s) = priority_str {
            if let Ok(p) = s.parse::<u8>() {
                self.config.tool_rules[idx].priority = p;
                self.modified = true;
            }
        }

        Ok(())
    }

    /// Edit context options section.
    fn edit_context_options(&mut self) -> Result<()> {
        println!("\n{}", "─ Context Options ─".bold());

        let ctx = self
            .config
            .context
            .get_or_insert_with(ContextConfigOptions::default);

        let max_msgs_str = edit_text(
            "Max messages",
            ctx.max_messages.map(|n| n.to_string()).as_deref(),
            true,
        )?;
        if let Some(s) = max_msgs_str {
            ctx.max_messages = s.parse().ok();
            self.modified = true;
        }

        let thinking = editors::confirm(
            "Enable thinking/reasoning?",
            ctx.enable_thinking.unwrap_or(false),
        )?;
        ctx.enable_thinking = Some(thinking);
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

        let source_types = ["bluesky", "discord", "file", "shell", "custom"];
        let type_idx = select_menu("Source type", &source_types, 0)?;
        let source_type = source_types[type_idx];

        let source = data_source_config::build_source_interactive(&name, source_type)?;

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

        let updated = data_source_config::edit_source_interactive(name, &source)?;
        self.config.data_sources.insert(name.to_string(), updated);
        self.modified = true;

        println!("{} Updated data source '{}'", "✓".green(), name);
        Ok(())
    }

    fn edit_integrations(&mut self) -> Result<()> {
        println!("\n{}", "─ Integrations ─".bold());

        if let Some(handle) = edit_text(
            "Bluesky handle",
            self.config.bluesky_handle.as_deref(),
            true,
        )? {
            self.config.bluesky_handle = Some(handle);
            self.modified = true;
        }

        Ok(())
    }

    /// Validate the configuration before saving.
    pub fn validate(&self) -> Result<()> {
        if self.config.name.trim().is_empty() {
            return Err(miette::miette!("Agent name is required"));
        }

        if self.config.model.is_none() {
            return Err(miette::miette!("Model configuration is required"));
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
                    self.edit_section(section)?;
                }
                MenuChoice::Done => {
                    // Validate before save
                    self.validate()?;
                    return self.save().await;
                }
                MenuChoice::Cancel => {
                    if self.modified {
                        if editors::confirm("Discard changes?", false)? {
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

                // Create RuntimeContext first - this handles DB access and memory operations
                let runtime_ctx = crate::helpers::create_runtime_context_with_dbs(dbs)
                    .await
                    .map_err(|e| miette::miette!("Failed to create runtime context: {}", e))?;

                let pool = runtime_ctx.dbs().constellation.pool();

                let id = config
                    .id
                    .as_ref()
                    .map(|id| id.0.clone())
                    .unwrap_or_else(|| generate_id("agt"));

                // Check if agent already exists
                let existing = pattern_db::queries::get_agent(pool, &id)
                    .await
                    .map_err(|e| miette::miette!("Database error: {}", e))?;

                if existing.is_some() {
                    // Update existing agent DB record
                    let db_agent = config.to_db_agent(&id);
                    pattern_db::queries::update_agent(pool, &db_agent)
                        .await
                        .map_err(|e| miette::miette!("Failed to update agent: {}", e))?;

                    // Load agent and use its runtime's memory for updates
                    let loaded_agent = runtime_ctx
                        .load_agent(&id)
                        .await
                        .map_err(|e| miette::miette!("Failed to load agent: {}", e))?;

                    let agent_runtime = loaded_agent.runtime();
                    let memory = agent_runtime.memory();

                    // Sync memory blocks from config
                    for (label, block_config) in &config.memory {
                        let content = block_config
                            .load_content()
                            .await
                            .map_err(|e| miette::miette!("Failed to load content: {}", e))?;

                        // Check if block exists - use the doc directly if it does
                        if let Some(doc) = memory
                            .get_block(&id, label)
                            .await
                            .map_err(|e| miette::miette!("Failed to get block: {:?}", e))?
                        {
                            // Update existing block content
                            if !content.is_empty() {
                                doc.set_text(&content, true)
                                    .map_err(|e| miette::miette!("Failed to set content: {}", e))?;
                                memory
                                    .persist_block(&id, label)
                                    .await
                                    .map_err(|e| miette::miette!("Failed to persist: {:?}", e))?;
                            }
                        } else {
                            // Create new block
                            use pattern_core::memory::{BlockSchema, BlockType};
                            let block_type = match block_config.memory_type {
                                MemoryType::Core => BlockType::Core,
                                MemoryType::Working => BlockType::Working,
                                MemoryType::Archival => BlockType::Archival,
                            };

                            let doc = memory
                                .create_block(
                                    &id,
                                    label,
                                    block_config
                                        .description
                                        .as_deref()
                                        .unwrap_or(&format!("{} memory block", label)),
                                    block_type,
                                    BlockSchema::text(),
                                    0, // Use default char limit
                                )
                                .await
                                .map_err(|e| miette::miette!("Failed to create block: {:?}", e))?;

                            if !content.is_empty() {
                                doc.set_text(&content, true).map_err(|e| {
                                    miette::miette!("Failed to set content: {:?}", e)
                                })?;
                                memory.persist_block(&id, label).await.map_err(|e| {
                                    miette::miette!("Failed to persist block: {:?}", e)
                                })?;
                            }
                        }
                    }
                } else {
                    // Create new agent - RuntimeContext::create_agent handles memory blocks
                    let mut config_with_id = config.clone();
                    config_with_id.id = Some(pattern_core::id::AgentId(id.clone()));

                    runtime_ctx
                        .create_agent(&config_with_id)
                        .await
                        .map_err(|e| miette::miette!("Failed to create agent: {}", e))?;
                }

                Ok(id)
            }
        };

        super::save::save_config(&ctx, &name, to_toml, save_to_db).await
    }
}

/// Helper struct for displaying memory blocks in collection editor.
#[derive(Clone)]
struct MemoryBlockItem {
    label: String,
    block: MemoryBlockConfig,
}

impl CollectionItem for MemoryBlockItem {
    fn display_short(&self) -> String {
        let perm = format!("[{:?}]", self.block.permission).to_lowercase();
        format!("{} {}", self.label, perm.dimmed())
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

/// Helper struct for displaying tool rules in collection editor.
#[derive(Clone)]
struct ToolRuleItem {
    rule: ToolRuleConfig,
}

impl CollectionItem for ToolRuleItem {
    fn display_short(&self) -> String {
        let rule_type = format!("{:?}", self.rule.rule_type)
            .split('{')
            .next()
            .unwrap_or("Unknown")
            .to_lowercase();
        format!("{}: {}", self.rule.tool_name, rule_type)
    }

    fn label(&self) -> String {
        self.rule.tool_name.clone()
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
            DataSourceConfig::Shell(_) => "shell",
            DataSourceConfig::Custom(c) => &c.source_type,
        };
        format!("{} {}", self.name, format!("[{}]", source_type).dimmed())
    }

    fn label(&self) -> String {
        self.name.clone()
    }
}
