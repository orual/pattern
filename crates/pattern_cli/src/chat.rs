//! Chat interaction loops for agents and groups
//!
//! This module contains the main chat loop functionality for both single agents
//! and agent groups, including message handling and response processing.

use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use pattern_core::{
    Agent, PermissionRule,
    agent::ResponseEvent,
    config::PatternConfig,
    data_source::FileSource,
    messages::{Message, MessageContent},
    tool::builtin::FileTool,
};
use std::sync::Arc;

use crate::{
    endpoints::{CliEndpoint, build_group_cli_endpoint},
    helpers::{
        create_runtime_context_with_dbs, get_agent_by_name, get_dbs, require_agent_by_name,
        require_group_by_name,
    },
    output::Output,
    slash_commands::handle_slash_command,
};
use pattern_core::runtime::router::MessageEndpoint;

pub fn print_response_event(event: ResponseEvent, output: &Output) {
    match event {
        ResponseEvent::ToolCallStarted {
            call_id: _,
            fn_name,
            args,
        } => {
            // For send_message, hide the content arg since it's displayed below
            let args_display = if fn_name == "send_message" {
                let mut display_args = args.clone();
                if let Some(args_obj) = display_args.as_object_mut() {
                    if args_obj.contains_key("content") {
                        args_obj.insert("content".to_string(), serde_json::json!("[shown below]"));
                    }
                }
                serde_json::to_string(&display_args).unwrap_or_else(|_| display_args.to_string())
            } else {
                serde_json::to_string_pretty(&args).unwrap_or_else(|_| args.to_string())
            };

            output.tool_call(&fn_name, &args_display);
        }
        ResponseEvent::ToolCallCompleted { call_id, result } => match result {
            Ok(content) => {
                output.tool_result(&content);
            }
            Err(error) => {
                output.error(&format!("Tool error ({}): {}", call_id, error));
            }
        },
        ResponseEvent::TextChunk { text, .. } => {
            // Display agent's response text
            output.agent_message("Agent", &text);
        }
        ResponseEvent::ReasoningChunk { text, is_final: _ } => {
            output.status(&format!("Reasoning: {}", text));
        }
        ResponseEvent::ToolCalls { .. } => {
            // Skip - we handle individual ToolCallStarted events instead
        }
        ResponseEvent::ToolResponses { .. } => {
            // Skip - we handle individual ToolCallCompleted events instead
        }
        ResponseEvent::Complete {
            message_id,
            metadata,
        } => {
            // Could display metadata if desired
            tracing::debug!("Message {} complete: {:?}", message_id, metadata);
        }
        ResponseEvent::Error {
            message,
            recoverable,
        } => {
            if recoverable {
                output.warning(&format!("Recoverable error: {}", message));
            } else {
                output.error(&format!("Error: {}", message));
            }
        }
    }
}

// =============================================================================
// Single Agent Chat (with RuntimeContext)
// =============================================================================

/// Chat with a single agent by name
///
/// Loads the agent from the database via RuntimeContext and runs an interactive
/// chat loop. This is the primary entry point for single-agent CLI chat.
pub async fn chat_with_single_agent(agent_name: &str, config: &PatternConfig) -> Result<()> {
    use rustyline_async::{Readline, ReadlineEvent};
    let output = Output::new();

    // Open databases and find agent using shared helpers
    let dbs = get_dbs(config).await?;
    let (agent, ctx) =
        if let Ok(Some(db_agent)) = get_agent_by_name(&dbs.constellation, agent_name).await {
            output.status(&format!("Loading agent '{}'...", agent_name.bright_cyan()));

            // Create RuntimeContext using shared helper
            let ctx = create_runtime_context_with_dbs(dbs).await?;
            // Load the agent
            let agent = ctx
                .load_agent(&db_agent.id)
                .await
                .map_err(|e| miette::miette!("Failed to load agent '{}': {}", agent_name, e))?;

            output.info("  Loaded:", &agent.name().bright_cyan().to_string());
            output.info(
                "  Model:",
                &format!("{}/{}", db_agent.model_provider, db_agent.model_name),
            );

            (agent, ctx)
        } else {
            output.status(&format!("Creating agent '{}'...", agent_name.bright_cyan()));

            let ctx = create_runtime_context_with_dbs(dbs).await?;
            let agent = ctx.create_agent(&config.agent).await?;
            (agent, ctx)
        };

    let file_source = Arc::new(FileSource::with_rules(
        "docs",
        "./docs",
        vec![PermissionRule {
            pattern: "**.txt".into(),
            permission: pattern_core::memory::MemoryPermission::ReadWrite,
            operations_requiring_escalation: vec![],
        }],
    ));
    let file_tool = FileTool::new(agent.runtime().clone(), file_source.clone());
    ctx.register_block_source(file_source.clone());
    agent.runtime().tools().register(file_tool);

    // Set up readline
    let (mut rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;
    crate::tracing_writer::set_shared_writer(writer.clone());
    let output = output.with_writer(writer.clone());

    // Register CLI endpoint as default
    let cli_endpoint = Arc::new(CliEndpoint::new(output.clone()));
    agent
        .runtime()
        .router()
        .set_default_user_endpoint(cli_endpoint)
        .await;
    output.info("Default endpoint:", "CLI");

    // Start heartbeat processor via RuntimeContext
    let output_clone = output.clone();
    ctx.start_heartbeat_processor(move |event, _agent_id, agent_name| {
        let output = output_clone.clone();
        async move {
            output.status(&format!("Heartbeat continuation from {}:", agent_name));
            print_response_event(event, &output);
        }
    })
    .await
    .map_err(|e| miette::miette!("Failed to start heartbeat processor: {}", e))?;

    // Spawn CLI permission listener
    let _perm_task = crate::permission_sink::spawn_cli_permission_listener(output.clone());

    output.status("");
    output.status(&format!("Agent '{}' ready", agent_name.bright_cyan()));
    output.status("Type 'quit' or 'exit' to leave, Enter to send");
    output.status("");

    // Chat loop
    loop {
        let event = rl.readline().await;
        match event {
            Ok(ReadlineEvent::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }

                // Check for slash commands
                if line.trim().starts_with('/') {
                    match handle_slash_command(
                        &line,
                        crate::slash_commands::CommandContext::SingleAgent(&agent),
                        &output,
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

                rl.add_history_entry(line.clone());

                // Create message
                let message = Message {
                    content: MessageContent::Text(line.clone()),
                    word_count: line.split_whitespace().count() as u32,
                    ..Default::default()
                };

                // Process message with streaming
                let r_agent = agent.clone();
                let output = output.clone();
                tokio::spawn(async move {
                    output.status("Thinking...");

                    use tokio_stream::StreamExt;

                    match r_agent.clone().process(message).await {
                        Ok(stream) => {
                            // Tee the agent stream to CLI printer and optional file
                            let sinks = crate::forwarding::build_cli_agent_sinks(&output).await;
                            let ctx = pattern_core::realtime::AgentEventContext {
                                source_tag: Some("CLI".to_string()),
                                agent_name: Some(r_agent.name().to_string()),
                            };
                            let mut stream =
                                pattern_core::realtime::tap_agent_stream(stream, sinks, ctx);
                            // Drain without direct printing; sinks handle display
                            while let Some(_event) = stream.next().await {}
                        }
                        Err(e) => {
                            output.error(&format!("Error: {}", e));
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

// =============================================================================
// Group Chat Functions
// =============================================================================

/// Chat with a group of agents
///
/// Loads the group from the database, instantiates agents via RuntimeContext,
/// and runs an interactive chat loop with the group.
pub async fn chat_with_group(group_name: &str, config: &PatternConfig) -> Result<()> {
    use rustyline_async::{Readline, ReadlineEvent};
    let output = Output::new();

    // Open databases and find group using shared helpers
    let dbs = get_dbs(config).await?;
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

    // Create RuntimeContext using shared helper
    let ctx = create_runtime_context_with_dbs(dbs).await?;

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

    // Set up readline
    let (mut rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;
    crate::tracing_writer::set_shared_writer(writer.clone());
    let output = output.with_writer(writer.clone());

    // Build the group CLI endpoint
    let group_endpoint = build_group_cli_endpoint(&db_group, &db_members, agents, output.clone())
        .await
        .map_err(|e| miette::miette!("Failed to build group endpoint: {}", e))?;

    output.status("");
    output.status(&format!(
        "Group '{}' ready ({:?} pattern)",
        group_name.bright_cyan(),
        db_group.pattern_type
    ));
    output.status("Type 'quit' or 'exit' to leave, Enter to send");
    output.status("");

    // Chat loop
    loop {
        let event = rl.readline().await;
        match event {
            Ok(ReadlineEvent::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }

                if line.trim() == "quit" || line.trim() == "exit" {
                    output.status("Goodbye!");
                    break;
                }

                rl.add_history_entry(line.clone());

                // Create message
                let message = Message {
                    content: MessageContent::Text(line.clone()),
                    word_count: line.split_whitespace().count() as u32,
                    ..Default::default()
                };

                // Route through group endpoint
                if let Err(e) = group_endpoint.send(message, None, None).await {
                    output.error(&format!("Group routing error: {}", e));
                }
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

/// Chat with a group and Jetstream data routing
///
/// This is an enhanced version that also subscribes to Jetstream events
/// and routes them through the group.
pub async fn chat_with_group_and_jetstream(group_name: &str, config: &PatternConfig) -> Result<()> {
    let output = Output::new();

    // For now, just call the regular chat_with_group
    // Jetstream integration requires additional work to set up the firehose consumer
    output.warning("Jetstream routing not yet integrated with pattern_db groups");
    output.info("Fallback:", "Using standard group chat");

    chat_with_group(group_name, config).await
}
