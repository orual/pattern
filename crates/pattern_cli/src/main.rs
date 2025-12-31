mod agent_ops;
mod background_tasks;
mod chat;
mod commands;
mod coordination_helpers;
mod data_source_config;
mod discord;
mod endpoints;
mod forwarding;
mod helpers;
mod message_display;
mod output;
mod permission_sink;
mod slash_commands;
mod tracing_writer;

// CLI Status - pattern_db (SQLite/sqlx) migration
//
// This CLI uses RuntimeContext from pattern_core and pattern_db for database access.
//
// Module status:
// - agent_ops.rs: Agent loading/creation via RuntimeContext - WORKING
// - chat.rs: Single agent and group chat - WORKING
// - commands/agent.rs: list/status WORKING, create/export/rules stubbed
// - commands/group.rs: All commands WORKING (list, create, add-member, status, export)
// - commands/export.rs: CAR export/import stubbed (needs format implementation)
// - commands/debug.rs: Memory listing WORKING, some commands stubbed
// - commands/db.rs: Stats/query stubbed (needs SQLite equivalents)

use clap::{Parser, Subcommand};
use miette::Result;
use owo_colors::OwoColorize;
use pattern_core::config::{self};
use std::path::PathBuf;
use tracing::info;

// TODO: Database initialization is being migrated from SurrealDB to pattern_db
// The following imports are no longer needed once migration is complete:
// use pattern_core::db_v1::{DatabaseConfig, client::{self}};
//
// New approach will use:
// use pattern_db::ConstellationDb;
// use pattern_core::runtime::RuntimeContext;

#[derive(Parser)]
#[command(name = "pattern-cli")]
#[command(about = "Pattern ADHD Support System CLI")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Configuration file path
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,

    /// Database file path (overrides config)
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Enable debug logging
    #[arg(long)]
    debug: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive chat with agents
    Chat {
        /// Agent name to chat with
        #[arg(long, default_value = "Pattern", conflicts_with = "group")]
        agent: String,

        /// Group name to chat with
        #[arg(long, conflicts_with = "agent")]
        group: Option<String>,

        /// Run as Discord bot instead of CLI chat
        #[arg(long)]
        discord: bool,
    },
    /// Agent management
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
    },
    /// Database inspection
    Db {
        #[command(subcommand)]
        cmd: DbCommands,
    },
    /// Debug tools
    Debug {
        #[command(subcommand)]
        cmd: DebugCommands,
    },
    /// Configuration management
    Config {
        #[command(subcommand)]
        cmd: ConfigCommands,
    },
    /// Agent group management
    Group {
        #[command(subcommand)]
        cmd: GroupCommands,
    },
    /// OAuth authentication
    #[cfg(feature = "oauth")]
    Auth {
        #[command(subcommand)]
        cmd: AuthCommands,
    },
    /// ATProto/Bluesky authentication
    Atproto {
        #[command(subcommand)]
        cmd: AtprotoCommands,
    },
    /// Export agents, groups, or constellations to CAR files
    Export {
        #[command(subcommand)]
        cmd: ExportCommands,
    },
    /// Import from CAR files
    Import {
        /// Path to CAR file to import
        file: PathBuf,

        /// Rename imported entity to this name
        #[arg(long)]
        rename_to: Option<String>,

        /// Preserve original IDs when importing
        #[arg(long, default_value_t = true)]
        preserve_ids: bool,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// List all agents
    List,
    /// Show agent details
    Status {
        /// Agent name
        name: String,
    },
    /// Create a new agent interactively
    Create {
        /// Load initial config from TOML file
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Edit an existing agent interactively
    Edit {
        /// Agent name to edit
        name: String,
    },
    /// Export agent configuration to TOML file
    Export {
        /// Agent name to export
        name: String,
        /// Output file path (defaults to <agent_name>.toml)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Add configuration to an agent
    Add {
        #[command(subcommand)]
        cmd: AgentAddCommands,
    },
    /// Remove configuration from an agent
    Remove {
        #[command(subcommand)]
        cmd: AgentRemoveCommands,
    },
}

#[derive(Subcommand)]
enum AgentAddCommands {
    /// Add a data source subscription (interactive or from TOML file)
    Source {
        /// Agent name
        agent: String,
        /// Source name (identifier for this subscription)
        source: String,
        /// Source type (bluesky, discord, file, custom) - prompted if not provided
        #[arg(long, short = 't')]
        source_type: Option<String>,
        /// Load configuration from a TOML file
        #[arg(long, conflicts_with = "source_type")]
        from_toml: Option<PathBuf>,
    },
    /// Add a memory block
    Memory {
        /// Agent name
        agent: String,
        /// Memory block label
        label: String,
        /// Content (inline)
        #[arg(long, conflicts_with = "path")]
        content: Option<String>,
        /// Load content from file
        #[arg(long, conflicts_with = "content")]
        path: Option<PathBuf>,
        /// Memory type (core, working, archival)
        #[arg(long, short = 't', default_value = "working")]
        memory_type: String,
        /// Permission level (read_only, append, read_write, admin)
        #[arg(long, short = 'p', default_value = "read_write")]
        permission: String,
        /// Pin the block (always in context)
        #[arg(long)]
        pinned: bool,
    },
    /// Enable a tool
    Tool {
        /// Agent name
        agent: String,
        /// Tool name to enable
        tool: String,
    },
    /// Add a workflow rule
    Rule {
        /// Agent name
        agent: String,
        /// Tool name the rule applies to
        tool: String,
        /// Rule type (start-constraint, max-calls, exit-loop, continue-loop, cooldown, requires-preceding)
        rule_type: String,
        /// Optional rule parameters (e.g., max count for max-calls, duration for cooldown)
        #[arg(short = 'p', long)]
        params: Option<String>,
        /// Optional conditions (comma-separated tool names for requires-preceding)
        #[arg(short = 'c', long)]
        conditions: Option<String>,
        /// Rule priority (1-10, higher = more important)
        #[arg(long, default_value = "5")]
        priority: u8,
    },
}

#[derive(Subcommand)]
enum AgentRemoveCommands {
    /// Remove a data source subscription
    Source {
        /// Agent name
        agent: String,
        /// Source name to remove
        source: String,
    },
    /// Remove a memory block
    Memory {
        /// Agent name
        agent: String,
        /// Memory block label to remove
        label: String,
    },
    /// Disable a tool
    Tool {
        /// Agent name
        agent: String,
        /// Tool name to disable
        tool: String,
    },
    /// Remove a workflow rule
    Rule {
        /// Agent name
        agent: String,
        /// Tool name to remove rules from
        tool: String,
        /// Optional rule type to remove (removes all for tool if not specified)
        rule_type: Option<String>,
    },
}

#[cfg(feature = "oauth")]
#[derive(Subcommand)]
enum AuthCommands {
    /// Authenticate with Anthropic OAuth
    Login {
        /// Provider to authenticate with
        #[arg(default_value = "anthropic")]
        provider: String,
    },
    /// Show current auth status
    Status,
    /// Logout (remove stored tokens)
    Logout {
        /// Provider to logout from
        #[arg(default_value = "anthropic")]
        provider: String,
    },
}

#[derive(Subcommand)]
enum DbCommands {
    /// Show database stats
    Stats,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Save current configuration to file
    Save {
        /// Path to save configuration
        #[arg(default_value = "pattern.toml")]
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum GroupCommands {
    /// List all groups
    List,
    /// Show group details and members
    Status {
        /// Group name
        name: String,
    },
    /// Create a new group interactively
    Create {
        /// Load initial config from TOML file
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Edit an existing group interactively
    Edit {
        /// Group name to edit
        name: String,
    },
    /// Export group configuration to TOML file
    Export {
        /// Group name to export
        name: String,
        /// Output file path (defaults to <group_name>_group.toml)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
    },
    /// Add configuration to a group
    Add {
        #[command(subcommand)]
        cmd: GroupAddCommands,
    },
    /// Remove configuration from a group
    Remove {
        #[command(subcommand)]
        cmd: GroupRemoveCommands,
    },
}

#[derive(Subcommand)]
enum GroupAddCommands {
    /// Add an agent member to the group
    Member {
        /// Group name
        group: String,
        /// Agent name
        agent: String,
        /// Member role (regular, supervisor, observer, specialist)
        #[arg(long, default_value = "regular")]
        role: String,
        /// Capabilities (comma-separated)
        #[arg(long)]
        capabilities: Option<String>,
    },
    /// Add a shared memory block
    Memory {
        /// Group name
        group: String,
        /// Memory block label
        label: String,
        /// Content (inline)
        #[arg(long, conflicts_with = "path")]
        content: Option<String>,
        /// Load content from file
        #[arg(long, conflicts_with = "content")]
        path: Option<PathBuf>,
    },
    /// Add a data source subscription (interactive or from TOML file)
    Source {
        /// Group name
        group: String,
        /// Source name (identifier for this subscription)
        source: String,
        /// Source type (bluesky, discord, file, custom) - prompted if not provided
        #[arg(long, short = 't')]
        source_type: Option<String>,
        /// Load configuration from a TOML file
        #[arg(long, conflicts_with = "source_type")]
        from_toml: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum GroupRemoveCommands {
    /// Remove an agent member from the group
    Member {
        /// Group name
        group: String,
        /// Agent name to remove
        agent: String,
    },
    /// Remove a shared memory block
    Memory {
        /// Group name
        group: String,
        /// Memory block label to remove
        label: String,
    },
    /// Remove a data source subscription
    Source {
        /// Group name
        group: String,
        /// Source name to remove
        source: String,
    },
}

#[derive(Subcommand)]
enum ExportCommands {
    /// Export an agent to a CAR file
    Agent {
        /// Agent name to export
        name: String,
        /// Output file path (defaults to <name>.car)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Exclude embeddings from export to reduce file size
        #[arg(long)]
        exclude_embeddings: bool,
    },
    /// Export a group with all member agents to a CAR file
    Group {
        /// Group name to export
        name: String,
        /// Output file path (defaults to <name>.car)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Exclude embeddings from export to reduce file size
        #[arg(long)]
        exclude_embeddings: bool,
    },
    /// Export entire constellation to a CAR file
    Constellation {
        /// Output file path (defaults to constellation.car)
        #[arg(short = 'o', long)]
        output: Option<PathBuf>,
        /// Exclude embeddings from export to reduce file size
        #[arg(long)]
        exclude_embeddings: bool,
    },
}

#[derive(Subcommand)]
enum AtprotoCommands {
    /// Login with app password
    Login {
        /// Your handle (e.g., alice.bsky.social) or DID
        identifier: String,
        /// App password (will prompt if not provided)
        #[arg(short = 'p', long)]
        app_password: Option<String>,
        /// Agent to link this identity to (defaults to _constellation_ for shared identity)
        #[arg(short = 'a', long, default_value = "_constellation_")]
        agent_id: String,
    },
    /// Login with OAuth
    OAuth {
        /// Your handle (e.g., alice.bsky.social) or DID
        identifier: String,
        /// Agent to link this identity to (defaults to _constellation_ for shared identity)
        #[arg(short = 'a', long, default_value = "_constellation_")]
        agent_id: String,
    },
    /// Show authentication status
    Status,
    /// Unlink an ATProto identity
    Unlink {
        /// Handle or DID to unlink
        identifier: String,
    },
    /// Test ATProto connections
    Test,
}

#[derive(Subcommand)]
enum DebugCommands {
    /// Search archival memory as if you were an agent
    SearchArchival {
        /// Agent name to search as
        #[arg(long)]
        agent: String,
        /// Search query
        query: String,
        /// Maximum number of results
        #[arg(long, default_value = "10")]
        limit: usize,
    },
    /// List all archival memories for an agent
    ListArchival {
        /// Agent name
        agent: String,
    },
    /// List all core memory blocks for an agent
    ListCore {
        /// Agent name
        agent: String,
    },
    /// List all memory blocks for an agent (core + archival)
    ListAllMemory {
        /// Agent name
        agent: String,
    },
    /// Edit a memory block by exporting to file
    EditMemory {
        /// Agent name
        agent: String,
        /// Memory block label/name
        label: String,
        /// Optional file path (defaults to memory_<label>.txt)
        #[arg(long)]
        file: Option<String>,
    },
    /// Search conversation history
    SearchConversations {
        /// Agent name to search conversations for
        agent: String,
        /// Search query (optional)
        query: Option<String>,
        /// Filter by role (user, assistant, system, tool)
        #[arg(long)]
        role: Option<String>,
        /// Start time filter (ISO 8601 format)
        #[arg(long)]
        start_time: Option<String>,
        /// End time filter (ISO 8601 format)
        #[arg(long)]
        end_time: Option<String>,
        /// Maximum number of results
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    /// Show the current context that would be passed to the LLM
    ShowContext {
        /// Agent name
        agent: String,
    },
    /// Modify memory block properties
    ModifyMemory {
        /// Agent name
        agent: String,
        /// Memory block label to modify
        label: String,
        /// New label (optional)
        #[arg(long)]
        new_label: Option<String>,
        /// New permission (core_read_write, archival_read_write, recall_read_write)
        #[arg(long)]
        permission: Option<String>,
        /// New memory type (core, archival)
        #[arg(long)]
        memory_type: Option<String>,
    },
    /// Clean up message context by removing unpaired/out-of-order messages
    ContextCleanup {
        /// Agent name
        agent: String,
        /// Interactive mode (prompt for each action)
        #[arg(short = 'i', long, default_value = "true")]
        interactive: bool,
        /// Dry run - show what would be deleted without actually deleting
        #[arg(short = 'd', long)]
        dry_run: bool,
        /// Limit to recent N messages
        #[arg(short = 'l', long)]
        limit: Option<usize>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if it exists
    let _ = dotenvy::dotenv();
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .terminal_links(true)
                .rgb_colors(miette::RgbColors::Preferred)
                .with_cause_chain()
                .with_syntax_highlighting(miette::highlighters::SyntectHighlighter::default())
                .color(true)
                .context_lines(5)
                .tab_width(2)
                .break_words(true)
                .build(),
        )
    }))?;
    miette::set_panic_hook();
    let cli = Cli::parse();

    // Initialize our custom tracing writer
    let tracing_writer = tracing_writer::init_tracing_writer();

    // Initialize tracing with file logging
    use tracing_appender::rolling;
    use tracing_subscriber::{
        EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt,
    };

    // Create log directory in user's data directory
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pattern")
        .join("logs");

    // Ensure log directory exists
    std::fs::create_dir_all(&log_dir).ok();

    // Create a rolling file appender that rotates daily
    let file_appender = rolling::daily(&log_dir, "pattern-cli.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    // Create the base subscriber with environment filter
    let env_filter = if cli.debug {
        EnvFilter::new(
            "pattern_core=debug,pattern_cli=debug,pattern_nd=debug,pattern_mcp=debug,pattern_discord=debug,pattern_main=debug,rocketman=debug,loro_internal=warn,info",
        )
    } else {
        EnvFilter::new(
            "pattern_core=info,pattern_cli=info,pattern_nd=info,pattern_mcp=info,pattern_discord=info,pattern_main=info,rocketman=info,loro_internal=warn,warning",
        )
    };

    // Create terminal layer
    let terminal_layer = if cli.debug {
        fmt::layer()
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .with_timer(fmt::time::LocalTime::rfc_3339())
            .with_writer(tracing_writer.clone())
            .pretty()
            .boxed()
    } else {
        fmt::layer()
            .with_target(false)
            .with_thread_ids(false)
            .with_thread_names(false)
            .with_writer(tracing_writer.clone())
            .compact()
            .boxed()
    };

    // Create file layer with debug logging
    let file_env_filter = EnvFilter::new(
        "pattern_core=debug,pattern_cli=debug,pattern_nd=debug,pattern_mcp=debug,pattern_discord=debug,pattern_main=debug,info",
    );

    let file_layer = fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_timer(fmt::time::LocalTime::rfc_3339())
        .with_ansi(false) // Disable ANSI colors in file output
        .with_writer(non_blocking)
        .pretty();

    // Initialize the subscriber with both layers, each with their own filter
    tracing_subscriber::registry()
        .with(terminal_layer.with_filter(env_filter))
        .with(file_layer.with_filter(file_env_filter))
        .init();

    info!(
        "Logging initialized. Logs are being written to: {:?}",
        log_dir.join("pattern-cli.log")
    );

    // Load configuration
    let config = if let Some(config_path) = &cli.config {
        info!("Loading config from: {:?}", config_path);
        config::load_config(config_path).await?
    } else {
        info!("Loading config from standard locations");
        config::load_config_from_standard_locations().await?
    };

    // TODO: Uncomment when pattern_db is integrated:
    // let db = pattern_db::ConstellationDb::new(&config.database.path).await?;
    // let model_provider = /* create from config */;
    // let embedding_provider = /* create from config */;
    // let runtime_ctx = RuntimeContext::builder()
    //     .db(db)
    //     .model_provider(model_provider)
    //     .embedding_provider(embedding_provider)
    //     .build()
    //     .await?;

    // Group initialization from config is disabled during migration
    // Previously this would:
    // 1. Iterate over config.groups
    // 2. Create or load each group
    // 3. Load or create member agents
    // 4. Set up coordination patterns

    match &cli.command {
        Commands::Chat {
            agent,
            group,
            discord,
        } => {
            let output = crate::output::Output::new();

            // Create heartbeat channel for agent(s)
            let (heartbeat_sender, heartbeat_receiver) =
                pattern_core::context::heartbeat::heartbeat_channel();

            if let Some(group_name) = group {
                // Chat with a group
                output.success("Starting group chat mode...");
                output.info("Group:", &group_name.bright_cyan().to_string());

                // Check if we have a Bluesky configuration block
                let has_bluesky_config = config.bluesky.is_some();

                if has_bluesky_config {
                    output.info("Bluesky:", "Jetstream routing enabled");
                }

                // Just route to the appropriate chat function based on mode
                if *discord {
                    tracing::info!(
                        "Main: Discord flag detected, calling run_discord_bot_with_group"
                    );
                    #[cfg(feature = "discord")]
                    {
                        discord::run_discord_bot_with_group(
                            group_name, &config, true, // enable_cli
                        )
                        .await?;
                    }
                    #[cfg(not(feature = "discord"))]
                    {
                        output.error("Discord support not compiled. Add --features discord");
                        return Ok(());
                    }
                } else if has_bluesky_config {
                    chat::chat_with_group_and_jetstream(group_name, &config).await?;
                } else {
                    chat::chat_with_group(group_name, &config).await?;
                }
            } else {
                // Chat with a single agent
                if *discord {
                    output.error("Discord mode requires a group. Use --group <name> --discord");
                    return Ok(());
                }

                output.success("Starting chat mode...");
                output.info("Agent:", &agent.bright_cyan().to_string());

                // Suppress unused variable warnings (heartbeat handled by RuntimeContext now)
                let _ = heartbeat_sender;
                let _ = heartbeat_receiver;

                chat::chat_with_single_agent(agent, &config).await?;
            }
        }
        Commands::Agent { cmd } => match cmd {
            AgentCommands::List => commands::agent::list(&config).await?,
            AgentCommands::Status { name } => commands::agent::status(name, &config).await?,
            AgentCommands::Create { from } => {
                let dbs = crate::helpers::get_dbs(&config).await?;
                let builder = if let Some(path) = from {
                    commands::builder::agent::AgentBuilder::from_file(path.clone())
                        .await?
                        .with_dbs(dbs)
                } else {
                    commands::builder::agent::AgentBuilder::new().with_dbs(dbs)
                };
                if let Some(result) = builder.run().await? {
                    result.display();
                }
            }
            AgentCommands::Edit { name } => {
                let dbs = crate::helpers::get_dbs(&config).await?;
                let builder = commands::builder::agent::AgentBuilder::from_db(dbs, name).await?;
                if let Some(result) = builder.run().await? {
                    result.display();
                }
            }
            AgentCommands::Export { name, output } => {
                commands::agent::export(name, output.as_deref()).await?
            }
            AgentCommands::Add { cmd: add_cmd } => match add_cmd {
                AgentAddCommands::Source {
                    agent,
                    source,
                    source_type,
                    from_toml,
                } => {
                    commands::agent::add_source(
                        agent,
                        source,
                        source_type.as_deref(),
                        from_toml.as_deref(),
                        &config,
                    )
                    .await?
                }
                AgentAddCommands::Memory {
                    agent,
                    label,
                    content,
                    path,
                    memory_type,
                    permission,
                    pinned,
                } => {
                    commands::agent::add_memory(
                        agent,
                        label,
                        content.as_deref(),
                        path.as_deref(),
                        memory_type,
                        permission,
                        *pinned,
                        &config,
                    )
                    .await?
                }
                AgentAddCommands::Tool { agent, tool } => {
                    commands::agent::add_tool(agent, tool, &config).await?
                }
                AgentAddCommands::Rule {
                    agent,
                    tool,
                    rule_type,
                    params,
                    conditions,
                    priority,
                } => {
                    commands::agent::add_rule(
                        agent,
                        &rule_type,
                        tool,
                        params.as_deref(),
                        conditions.as_deref(),
                        *priority,
                    )
                    .await?
                }
            },
            AgentCommands::Remove { cmd: remove_cmd } => match remove_cmd {
                AgentRemoveCommands::Source { agent, source } => {
                    commands::agent::remove_source(agent, source, &config).await?
                }
                AgentRemoveCommands::Memory { agent, label } => {
                    commands::agent::remove_memory(agent, label, &config).await?
                }
                AgentRemoveCommands::Tool { agent, tool } => {
                    commands::agent::remove_tool(agent, tool, &config).await?
                }
                AgentRemoveCommands::Rule {
                    agent,
                    tool,
                    rule_type,
                } => commands::agent::remove_rule(agent, tool, rule_type.as_deref()).await?,
            },
        },
        Commands::Db { cmd } => {
            let output = crate::output::Output::new();
            match cmd {
                DbCommands::Stats => commands::db::stats(&config, &output).await?,
            }
        }
        Commands::Debug { cmd } => match cmd {
            DebugCommands::SearchArchival {
                agent,
                query,
                limit,
            } => {
                commands::debug::search_archival_memory(agent, query, *limit).await?;
            }
            DebugCommands::ListArchival { agent } => {
                commands::debug::list_archival_memory(&agent).await?;
            }
            DebugCommands::ListCore { agent } => {
                commands::debug::list_core_memory(&agent).await?;
            }
            DebugCommands::ListAllMemory { agent } => {
                commands::debug::list_all_memory(&agent).await?;
            }
            DebugCommands::EditMemory { agent, label, file } => {
                commands::debug::edit_memory(&agent, &label, file.as_deref()).await?;
            }
            DebugCommands::SearchConversations {
                agent,
                query,
                role,
                start_time,
                end_time,
                limit,
            } => {
                commands::debug::search_conversations(
                    &agent,
                    query.as_deref(),
                    role.as_deref(),
                    start_time.as_deref(),
                    end_time.as_deref(),
                    *limit,
                )
                .await?;
            }
            DebugCommands::ShowContext { agent } => {
                commands::debug::show_context(&agent, &config).await?;
            }
            DebugCommands::ModifyMemory {
                agent,
                label,
                new_label,
                permission,
                memory_type,
            } => {
                commands::debug::modify_memory(agent, label, new_label, permission, memory_type)
                    .await?;
            }
            DebugCommands::ContextCleanup {
                agent,
                interactive,
                dry_run,
                limit,
            } => {
                commands::debug::context_cleanup(agent, *interactive, *dry_run, *limit).await?;
            }
        },
        Commands::Config { cmd } => {
            let output = crate::output::Output::new();
            match cmd {
                ConfigCommands::Show => commands::config::show(&config, &output).await?,
                ConfigCommands::Save { path } => {
                    commands::config::save(&config, path, &output).await?
                }
            }
        }
        Commands::Group { cmd } => match cmd {
            GroupCommands::List => commands::group::list(&config).await?,
            GroupCommands::Status { name } => commands::group::status(name, &config).await?,
            GroupCommands::Create { from } => {
                let dbs = crate::helpers::get_dbs(&config).await?;
                let builder = if let Some(path) = from {
                    commands::builder::group::GroupBuilder::from_file(path.clone())
                        .await?
                        .with_dbs(dbs)
                } else {
                    commands::builder::group::GroupBuilder::default().with_dbs(dbs)
                };
                if let Some(result) = builder.run().await? {
                    result.display();
                }
            }
            GroupCommands::Edit { name } => {
                let dbs = crate::helpers::get_dbs(&config).await?;
                let builder = commands::builder::group::GroupBuilder::from_db(dbs, name).await?;
                if let Some(result) = builder.run().await? {
                    result.display();
                }
            }
            GroupCommands::Export { name, output } => {
                commands::group::export(name, output.as_deref(), &config).await?
            }
            GroupCommands::Add { cmd: add_cmd } => match add_cmd {
                GroupAddCommands::Member {
                    group,
                    agent,
                    role,
                    capabilities,
                } => {
                    commands::group::add_member(
                        &group,
                        &agent,
                        &role,
                        capabilities.as_deref(),
                        &config,
                    )
                    .await?
                }
                GroupAddCommands::Memory {
                    group,
                    label,
                    content,
                    path,
                } => {
                    commands::group::add_memory(
                        &group,
                        &label,
                        content.as_deref(),
                        path.as_deref(),
                        &config,
                    )
                    .await?
                }
                GroupAddCommands::Source {
                    group,
                    source,
                    source_type,
                    from_toml,
                } => {
                    commands::group::add_source(
                        &group,
                        &source,
                        source_type.as_deref(),
                        from_toml.as_deref(),
                        &config,
                    )
                    .await?
                }
            },
            GroupCommands::Remove { cmd: remove_cmd } => match remove_cmd {
                GroupRemoveCommands::Member { group, agent } => {
                    commands::group::remove_member(&group, &agent, &config).await?
                }
                GroupRemoveCommands::Memory { group, label } => {
                    commands::group::remove_memory(&group, &label, &config).await?
                }
                GroupRemoveCommands::Source { group, source } => {
                    commands::group::remove_source(&group, &source, &config).await?
                }
            },
        },
        #[cfg(feature = "oauth")]
        Commands::Auth { cmd } => match cmd {
            AuthCommands::Login { provider } => commands::auth::login(provider, &config).await?,
            AuthCommands::Status => commands::auth::status(&config).await?,
            AuthCommands::Logout { provider } => commands::auth::logout(provider, &config).await?,
        },
        Commands::Atproto { cmd } => match cmd {
            AtprotoCommands::Login {
                identifier,
                app_password,
                agent_id,
            } => {
                commands::atproto::app_password_login(
                    identifier,
                    app_password.clone(),
                    agent_id,
                    &config,
                )
                .await?
            }
            AtprotoCommands::OAuth {
                identifier,
                agent_id,
            } => commands::atproto::oauth_login(identifier, agent_id, &config).await?,
            AtprotoCommands::Status => commands::atproto::status(&config).await?,
            AtprotoCommands::Unlink { identifier } => {
                commands::atproto::unlink(identifier, &config).await?
            }
            AtprotoCommands::Test => commands::atproto::test(&config).await?,
        },
        Commands::Export { cmd } => match cmd {
            ExportCommands::Agent {
                name,
                output,
                exclude_embeddings,
            } => {
                commands::export::export_agent(name, output.clone(), *exclude_embeddings, &config)
                    .await?
            }
            ExportCommands::Group {
                name,
                output,
                exclude_embeddings,
            } => {
                commands::export::export_group(name, output.clone(), *exclude_embeddings, &config)
                    .await?
            }
            ExportCommands::Constellation {
                output,
                exclude_embeddings,
            } => {
                commands::export::export_constellation(output.clone(), *exclude_embeddings, &config)
                    .await?
            }
        },
        Commands::Import {
            file,
            rename_to,
            preserve_ids,
        } => {
            commands::export::import(file.clone(), rename_to.clone(), *preserve_ids, &config)
                .await?
        }
    }

    // Flush any remaining logs before exit
    drop(tracing_writer);

    Ok(())
}
