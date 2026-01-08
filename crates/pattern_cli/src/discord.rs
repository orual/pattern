//! Discord bot integration
//!
//! This module handles Discord bot setup and integration with agent groups,
//! including endpoint configuration and bot lifecycle management.

use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use pattern_core::{Agent, config::PatternConfig};
use std::sync::Arc;

use crate::output::Output;

/// Run a Discord bot for group chat with optional concurrent CLI interface
///
/// This function:
/// 1. Loads Discord config (database first, env fallback, persist if from env)
/// 2. Loads group from database
/// 3. Creates RuntimeContext and loads agents
/// 4. Sets up CLI endpoint on agents if enabled
/// 5. Builds Discord bot with group coordination
/// 6. Runs the Discord bot event loop (with optional CLI readline)
#[cfg(feature = "discord")]
pub async fn run_discord_bot_with_group(
    group_name: &str,
    config: &PatternConfig,
    enable_cli: bool,
) -> Result<()> {
    use pattern_core::id::AgentId;
    use pattern_discord::endpoints::DiscordEndpoint;
    use pattern_discord::serenity::all::GatewayIntents;
    use pattern_discord::{DiscordBot, DiscordBotConfig, DiscordEventHandler};
    use rustyline_async::Readline;

    use crate::chat::print_response_event;
    use crate::coordination_helpers;
    use crate::endpoints::{CliEndpoint, create_group_manager};
    use crate::forwarding::CliAgentPrinterSink;
    use crate::helpers::{create_runtime_context_with_dbs, get_dbs, require_group_by_name};

    // Create readline and output for concurrent CLI/Discord
    let (rl, writer) = if enable_cli {
        let (rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;
        // Update the global tracing writer to use the SharedWriter
        crate::tracing_writer::set_shared_writer(writer.clone());
        (Some(rl), Some(writer))
    } else {
        (None, None)
    };

    // Create output with SharedWriter if CLI is enabled
    let output = if let Some(writer) = writer.clone() {
        Output::new().with_writer(writer)
    } else {
        Output::new()
    };

    output.status(&format!(
        "Starting Discord bot with group '{}'...",
        group_name.bright_cyan()
    ));

    // Open databases
    let dbs = get_dbs(config).await?;

    // Load config: database first, env fallback, persist if from env
    let discord_config = match dbs.auth.get_discord_bot_config().await {
        Ok(Some(config)) => {
            output.info("Discord config:", "loaded from database");
            config
        }
        _ => match DiscordBotConfig::from_env() {
            Some(config) => {
                output.info("Discord config:", "loaded from environment");
                // Persist to database
                if let Err(e) = dbs.auth.set_discord_bot_config(&config).await {
                    output.warning(&format!("Could not persist config to database: {}", e));
                }
                config
            }
            None => {
                return Err(miette::miette!(
                    "No Discord configuration found. Set DISCORD_TOKEN or configure in database."
                ));
            }
        },
    };

    // Find group in database
    let db_group = require_group_by_name(&dbs.constellation, group_name).await?;

    // Get group members
    let db_members = pattern_db::queries::get_group_members(dbs.constellation.pool(), &db_group.id)
        .await
        .map_err(|e| miette::miette!("Failed to get group members: {}", e))?;

    if db_members.is_empty() {
        output.error(&format!("Group '{}' has no members", group_name));
        output.info(
            "Add members with:",
            "pattern-cli group add-member <group> <agent>",
        );
        return Ok(());
    }

    output.status(&format!(
        "Loading group '{}' with {} members...",
        group_name.bright_cyan(),
        db_members.len()
    ));

    // Create RuntimeContext
    let ctx = create_runtime_context_with_dbs(dbs.clone()).await?;

    // ctx.add_event_sink(Arc::new(CliAgentPrinterSink::new(output.clone())))
    //     .await;

    // Load agents for each member
    let mut agents: Vec<Arc<dyn Agent>> = Vec::new();
    for member in &db_members {
        match ctx.load_agent(&member.agent_id).await {
            Ok(agent) => {
                output.info("  Loaded:", &agent.name().bright_cyan().to_string());
                agents.push(agent);
            }
            Err(e) => {
                output.warning(&format!(
                    "  Could not load agent {}: {}",
                    member.agent_id, e
                ));
            }
        }
    }

    if agents.is_empty() {
        output.error("No agents could be loaded for this group");
        return Err(miette::miette!("Group has no loadable agents"));
    }

    // Set up CLI endpoint on agents if enabled
    if enable_cli {
        let cli_output = output.clone();
        let cli_endpoint = Arc::new(CliEndpoint::new(cli_output));
        for agent in &agents {
            agent
                .runtime()
                .router()
                .register_endpoint("cli".to_string(), cli_endpoint.clone())
                .await;
        }
        output.info("CLI endpoint:", "enabled");
    }

    // Get the first agent's ID for patterns that need a leader
    let first_agent_id = agents
        .first()
        .map(|a| a.id())
        .unwrap_or_else(AgentId::generate);

    // Build core AgentGroup using shared helpers
    let group = coordination_helpers::build_agent_group(&db_group, first_agent_id);

    // Build agents with membership using shared helpers
    let agents_with_membership = coordination_helpers::build_agents_with_membership(
        agents.clone(),
        &db_group.id,
        &db_members,
    );

    // Create the group manager for the pattern type
    let group_manager = create_group_manager(db_group.pattern_type);

    // Create restart channel for the bot
    let (restart_tx, mut restart_rx) = tokio::sync::mpsc::channel::<()>(1);

    // Build forward sinks so CLI can mirror Discord stream
    let sinks =
        crate::forwarding::build_discord_group_sinks(&output, &agents_with_membership).await;

    // Build the Discord bot
    let bot = Arc::new(DiscordBot::new_cli_mode(
        discord_config.clone(),
        agents_with_membership.clone(),
        group.clone(),
        group_manager.clone(),
        Some(sinks),
        restart_tx.clone(),
        Some(Arc::new(dbs.clone())),
    ));

    let endpoint = Arc::new(
        DiscordEndpoint::with_config(discord_config.bot_token.clone(), config.discord.as_ref())
            .with_bot(bot.clone()),
    );

    for agent in &agents {
        agent
            .runtime()
            .router()
            .set_default_user_endpoint(endpoint.clone())
            .await;

        agent
            .runtime()
            .router()
            .register_endpoint("channel".to_string(), endpoint.clone())
            .await;
    }

    // Create event handler
    let event_handler = DiscordEventHandler::new(bot.clone());

    // Build serenity client with appropriate intents
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGE_REACTIONS
        | GatewayIntents::DIRECT_MESSAGE_REACTIONS
        | GatewayIntents::GUILD_MEMBERS;

    output.status("Building Discord client...");

    let client_builder =
        pattern_discord::serenity::Client::builder(&discord_config.bot_token, intents)
            .event_handler(event_handler);

    // Log configuration
    if let Some(channels) = &discord_config.allowed_channels {
        output.info("Allowed channels:", &channels.join(", "));
    }
    if let Some(guilds) = &discord_config.allowed_guilds {
        output.info("Allowed guilds:", &guilds.join(", "));
    }

    output.success(&format!(
        "Discord bot ready with group '{}' ({:?} pattern)",
        group_name.bright_cyan(),
        db_group.pattern_type
    ));

    // Spawn permission listener for CLI feedback
    let _perm_task = crate::permission_sink::spawn_cli_permission_listener(output.clone());

    // If CLI is enabled, spawn Discord bot in background and run CLI in foreground
    if enable_cli {
        output.status("Starting Discord bot in background...");
        output.status("CLI interface available. Type 'quit' or 'exit' to stop both.");

        // Spawn Discord bot in background
        let discord_handle = tokio::spawn(async move {
            let mut client = client_builder.await.unwrap();
            if let Err(why) = client.start().await {
                tracing::error!("Discord bot error: {:?}", why);
            }
        });

        // Spawn restart handler
        tokio::spawn(async move {
            restart_rx.recv().await;
            tracing::info!("restart signal received");
            let _ = crossterm::terminal::disable_raw_mode();

            let exe = std::env::current_exe().unwrap();
            let args: Vec<String> = std::env::args().collect();

            use std::os::unix::process::CommandExt;
            let _ = std::process::Command::new(exe).args(&args[1..]).exec();

            std::process::exit(0);
        });

        // Start heartbeat processor via RuntimeContext
        let output_clone = output.clone();
        ctx.start_heartbeat_processor(move |event, _agent_id, agent_name| {
            let output = output_clone.clone();
            async move {
                output.status("Heartbeat continuation");
                print_response_event(&agent_name, event, &output);
            }
        })
        .await
        .map_err(|e| miette::miette!("Failed to start heartbeat processor: {}", e))?;

        ctx.add_event_sink(Arc::new(CliAgentPrinterSink::new(output.clone())))
            .await;
        let _queue_handle = ctx.start_queue_processor().await;

        // Run CLI chat loop in foreground
        if let Some(rl) = rl {
            run_group_chat_loop(
                group,
                agents_with_membership,
                group_manager,
                output.clone(),
                rl,
                ctx.constellation_db(),
            )
            .await?;
        }

        // When CLI exits, also stop Discord bot
        discord_handle.abort();

        // Force exit the entire process to ensure all spawned tasks are killed
        std::process::exit(0);
    } else {
        // Discord-only mode (no CLI)
        let mut client = client_builder
            .await
            .map_err(|e| miette::miette!("Failed to create Discord client: {}", e))?;

        output.status("Discord bot starting... Press Ctrl+C to stop.");

        // Start heartbeat processor for Discord-only mode via RuntimeContext
        let output_clone = output.clone();
        ctx.start_heartbeat_processor(move |event, _agent_id, agent_name| {
            let output = output_clone.clone();
            async move {
                output.status("Heartbeat continuation");
                crate::chat::print_response_event(&agent_name, event, &output);
            }
        })
        .await
        .map_err(|e| miette::miette!("Failed to start heartbeat processor: {}", e))?;

        let _queue_handle = ctx.start_queue_processor().await;

        // Run the bot with restart handling
        loop {
            tokio::select! {
                result = client.start() => {
                    match result {
                        Ok(()) => {
                            output.status("Discord client stopped normally");
                            break;
                        }
                        Err(e) => {
                            output.error(&format!("Discord client error: {}", e));
                            break;
                        }
                    }
                }
                _ = restart_rx.recv() => {
                    output.status("Received restart signal, restarting Discord client...");
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Run the CLI chat loop for group interaction (used when enable_cli is true)
#[cfg(feature = "discord")]
async fn run_group_chat_loop(
    group: pattern_core::coordination::groups::AgentGroup,
    agents_with_membership: Vec<
        pattern_core::coordination::groups::AgentWithMembership<Arc<dyn Agent>>,
    >,
    pattern_manager: Arc<dyn pattern_core::coordination::groups::GroupManager>,
    output: Output,
    mut rl: rustyline_async::Readline,
    db: &pattern_db::ConstellationDb,
) -> Result<()> {
    use pattern_core::messages::{Message, MessageContent};
    use rustyline_async::ReadlineEvent;
    use tokio_stream::StreamExt;

    loop {
        let event = rl.readline().await;
        match event {
            Ok(ReadlineEvent::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }

                // Check for slash commands
                if line.trim().starts_with('/') {
                    // Get the default agent (first agent in group for now)
                    let default_agent = agents_with_membership.first().map(|awm| &awm.agent);

                    match crate::slash_commands::handle_slash_command(
                        &line,
                        crate::slash_commands::CommandContext::Group {
                            group: &group,
                            agents: &agents_with_membership,
                            default_agent,
                        },
                        &output,
                        db,
                    )
                    .await
                    {
                        Ok(should_exit) => {
                            if should_exit {
                                output.status("Goodbye!");
                                break;
                            }
                            continue;
                        }
                        Err(e) => {
                            output.error(&format!("Command error: {}", e));
                            continue;
                        }
                    }
                }

                if line.trim() == "quit" || line.trim() == "exit" {
                    output.status("Goodbye!");
                    break;
                }

                // Add to history
                rl.add_history_entry(line.clone());

                // Create a message
                let message = Message {
                    content: MessageContent::Text(line.clone()),
                    word_count: line.split_whitespace().count() as u32,
                    ..Default::default()
                };

                // Route through the group
                output.status("Routing message through group...");
                let output = output.clone();
                let agents_with_membership = agents_with_membership.clone();
                let group = group.clone();
                let pattern_manager = pattern_manager.clone();
                tokio::spawn(async move {
                    match pattern_manager
                        .route_message(&group, &agents_with_membership, message)
                        .await
                    {
                        Ok(stream) => {
                            // Tee to CLI printer + optional file; sinks handle printing
                            let sinks = crate::forwarding::build_cli_group_sinks(
                                &output,
                                &agents_with_membership,
                            )
                            .await;
                            let ctx = pattern_core::realtime::GroupEventContext {
                                source_tag: Some("CLI".to_string()),
                                group_name: Some(group.name.clone()),
                            };
                            let mut stream =
                                pattern_core::realtime::tap_group_stream(stream, sinks, ctx);

                            // Drain without direct printing
                            while let Some(_event) = stream.next().await {}
                        }
                        Err(e) => {
                            output.error(&format!("Error routing message: {}", e));
                        }
                    }
                });
            }
            Ok(ReadlineEvent::Interrupted) => {
                output.status("CTRL-C");
                continue;
            }
            Ok(ReadlineEvent::Eof) => {
                output.status("CTRL-D");
                break;
            }
            Err(err) => {
                output.error(&format!("Error: {:?}", err));
                break;
            }
        }
    }

    Ok(())
}

/// Run a Discord bot for single-agent chat with optional concurrent CLI interface
///
/// This function wraps a single agent in a synthetic group for Discord integration.
/// The group uses RoundRobin pattern with one member, which effectively gives
/// single-agent behavior while reusing the group infrastructure.
#[cfg(feature = "discord")]
pub async fn run_discord_bot_with_agent(
    agent_name: &str,
    config: &PatternConfig,
    enable_cli: bool,
) -> Result<()> {
    use chrono::Utc;
    use pattern_core::coordination::groups::{AgentGroup, AgentWithMembership, GroupMembership};
    use pattern_core::coordination::types::{CoordinationPattern, GroupMemberRole, GroupState};
    use pattern_core::id::GroupId;
    use pattern_db::models::PatternType;
    use pattern_discord::endpoints::DiscordEndpoint;
    use pattern_discord::serenity::all::GatewayIntents;
    use pattern_discord::{DiscordBot, DiscordBotConfig, DiscordEventHandler};
    use rustyline_async::Readline;

    use crate::chat::print_response_event;
    use crate::endpoints::{CliEndpoint, create_group_manager};
    use crate::forwarding::CliAgentPrinterSink;
    use crate::helpers::{create_runtime_context_with_dbs, get_agent_by_name, get_dbs};

    // Create readline and output for concurrent CLI/Discord
    let (rl, writer) = if enable_cli {
        let (rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;
        crate::tracing_writer::set_shared_writer(writer.clone());
        (Some(rl), Some(writer))
    } else {
        (None, None)
    };

    let output = if let Some(writer) = writer.clone() {
        Output::new().with_writer(writer)
    } else {
        Output::new()
    };

    output.status(&format!(
        "Starting Discord bot with agent '{}'...",
        agent_name.bright_cyan()
    ));

    // Open databases
    let dbs = get_dbs(config).await?;

    // Load config: database first, env fallback, persist if from env
    let discord_config = match dbs.auth.get_discord_bot_config().await {
        Ok(Some(config)) => {
            output.info("Discord config:", "loaded from database");
            config
        }
        _ => match DiscordBotConfig::from_env() {
            Some(config) => {
                output.info("Discord config:", "loaded from environment");
                if let Err(e) = dbs.auth.set_discord_bot_config(&config).await {
                    output.warning(&format!("Could not persist config to database: {}", e));
                }
                config
            }
            None => {
                return Err(miette::miette!(
                    "No Discord configuration found. Set DISCORD_TOKEN or configure in database."
                ));
            }
        },
    };

    // Find agent in database
    let db_agent = get_agent_by_name(&dbs.constellation, agent_name)
        .await?
        .ok_or_else(|| {
            miette::miette!(
                "Agent '{}' not found in database.\n\nCreate it with: pattern agent create",
                agent_name
            )
        })?;

    output.status(&format!("Loading agent '{}'...", agent_name.bright_cyan()));

    // Create RuntimeContext
    let ctx = create_runtime_context_with_dbs(dbs.clone()).await?;

    // Load the agent
    let agent = ctx
        .load_agent(&db_agent.id)
        .await
        .map_err(|e| miette::miette!("Failed to load agent '{}': {}", agent_name, e))?;

    output.info("  Loaded:", &agent.name().bright_cyan().to_string());

    // Set up CLI endpoint on agent if enabled
    if enable_cli {
        let cli_output = output.clone();
        let cli_endpoint = Arc::new(CliEndpoint::new(cli_output));
        agent
            .runtime()
            .router()
            .register_endpoint("cli".to_string(), cli_endpoint.clone())
            .await;
        output.info("CLI endpoint:", "enabled");
    }

    // Create a synthetic group wrapping this single agent
    let synthetic_group_id = format!("_discord_solo_{}", agent.id().as_str());
    let now = Utc::now();

    let group = AgentGroup {
        id: GroupId(synthetic_group_id.clone()),
        name: format!("{} (Discord)", agent.name()),
        description: format!(
            "Synthetic group for single-agent Discord mode: {}",
            agent.name()
        ),
        coordination_pattern: CoordinationPattern::RoundRobin {
            current_index: 0,
            skip_unavailable: true,
        },
        created_at: now,
        updated_at: now,
        is_active: true,
        state: GroupState::RoundRobin {
            current_index: 0,
            last_rotation: now,
        },
        members: vec![],
    };

    // Wrap agent in membership
    let membership = GroupMembership {
        agent_id: agent.id(),
        group_id: GroupId(synthetic_group_id.clone()),
        joined_at: now,
        role: GroupMemberRole::Regular,
        is_active: true,
        capabilities: vec![],
    };

    let agents_with_membership = vec![AgentWithMembership {
        agent: agent.clone(),
        membership,
    }];

    // Create the group manager (RoundRobin for single agent)
    let group_manager = create_group_manager(PatternType::RoundRobin);

    // Create restart channel for the bot
    let (restart_tx, mut restart_rx) = tokio::sync::mpsc::channel::<()>(1);

    // Build forward sinks so CLI can mirror Discord stream
    let sinks =
        crate::forwarding::build_discord_group_sinks(&output, &agents_with_membership).await;

    // Build the Discord bot
    let bot = Arc::new(DiscordBot::new_cli_mode(
        discord_config.clone(),
        agents_with_membership.clone(),
        group.clone(),
        group_manager.clone(),
        Some(sinks),
        restart_tx.clone(),
        Some(Arc::new(dbs.clone())),
    ));

    let endpoint = Arc::new(
        DiscordEndpoint::with_config(discord_config.bot_token.clone(), config.discord.as_ref())
            .with_bot(bot.clone()),
    );

    // agent
    //     .runtime()
    //     .router()
    //     .set_default_user_endpoint(endpoint.clone())
    //     .await;

    agent
        .runtime()
        .router()
        .register_endpoint("channel".to_string(), endpoint)
        .await;

    // Create event handler
    let event_handler = DiscordEventHandler::new(bot.clone());

    // Build serenity client with appropriate intents
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGE_REACTIONS
        | GatewayIntents::DIRECT_MESSAGE_REACTIONS
        | GatewayIntents::GUILD_MEMBERS;

    output.status("Building Discord client...");

    let client_builder =
        pattern_discord::serenity::Client::builder(&discord_config.bot_token, intents)
            .event_handler(event_handler);

    if let Some(channels) = &discord_config.allowed_channels {
        output.info("Allowed channels:", &channels.join(", "));
    }
    if let Some(guilds) = &discord_config.allowed_guilds {
        output.info("Allowed guilds:", &guilds.join(", "));
    }

    output.success(&format!(
        "Discord bot ready with agent '{}'",
        agent_name.bright_cyan()
    ));

    // Spawn permission listener for CLI feedback
    let _perm_task = crate::permission_sink::spawn_cli_permission_listener(output.clone());
    // If CLI is enabled, spawn Discord bot in background and run CLI in foreground
    if enable_cli {
        output.status("Starting Discord bot in background...");
        output.status("CLI interface available. Type 'quit' or 'exit' to stop both.");

        let discord_handle = tokio::spawn(async move {
            let mut client = client_builder.await.unwrap();
            if let Err(why) = client.start().await {
                tracing::error!("Discord bot error: {:?}", why);
            }
        });

        // Spawn restart handler
        tokio::spawn(async move {
            restart_rx.recv().await;
            tracing::info!("restart signal received");
            let _ = crossterm::terminal::disable_raw_mode();

            let exe = std::env::current_exe().unwrap();
            let args: Vec<String> = std::env::args().collect();

            use std::os::unix::process::CommandExt;
            let _ = std::process::Command::new(exe).args(&args[1..]).exec();

            std::process::exit(0);
        });

        // Start heartbeat processor via RuntimeContext
        let output_clone = output.clone();
        let agent_name_clone = agent.name().to_string();
        ctx.start_heartbeat_processor(move |event, _agent_id, _name| {
            let output = output_clone.clone();
            let name = agent_name_clone.clone();
            async move {
                output.status("Heartbeat continuation");
                print_response_event(&name, event, &output);
            }
        })
        .await
        .map_err(|e| miette::miette!("Failed to start heartbeat processor: {}", e))?;

        let _queue_handle = ctx.start_queue_processor().await;

        // Run CLI chat loop in foreground (reuse group chat loop)
        if let Some(rl) = rl {
            run_group_chat_loop(
                group,
                agents_with_membership,
                group_manager,
                output.clone(),
                rl,
                ctx.constellation_db(),
            )
            .await?;
        }

        discord_handle.abort();
        std::process::exit(0);
    } else {
        // Discord-only mode (no CLI)
        let mut client = client_builder
            .await
            .map_err(|e| miette::miette!("Failed to create Discord client: {}", e))?;

        output.status("Discord bot starting... Press Ctrl+C to stop.");

        let output_clone = output.clone();
        ctx.start_heartbeat_processor(move |event, _agent_id, name| {
            let output = output_clone.clone();
            async move {
                output.status("Heartbeat continuation");
                print_response_event(&name, event, &output);
            }
        })
        .await
        .map_err(|e| miette::miette!("Failed to start heartbeat processor: {}", e))?;

        ctx.add_event_sink(Arc::new(CliAgentPrinterSink::new(output.clone())))
            .await;
        let _queue_handle = ctx.start_queue_processor().await;

        loop {
            tokio::select! {
                result = client.start() => {
                    match result {
                        Ok(()) => {
                            output.status("Discord client stopped normally");
                            break;
                        }
                        Err(e) => {
                            output.error(&format!("Discord client error: {}", e));
                            break;
                        }
                    }
                }
                _ = restart_rx.recv() => {
                    output.status("Received restart signal, restarting Discord client...");
                    break;
                }
            }
        }
    }

    Ok(())
}

#[cfg(not(feature = "discord"))]
/// Placeholder when Discord feature is not enabled
pub fn discord_not_available() {
    let output = crate::output::Output::new();
    output.error("Discord support not compiled in. Enable the 'discord' feature.");
}
