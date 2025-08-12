use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use pattern_core::{
    Agent, ModelProvider,
    agent::{AgentRecord, AgentType, DatabaseAgent, ResponseEvent},
    config::PatternConfig,
    context::{
        heartbeat,
        heartbeat::{HeartbeatReceiver, HeartbeatSender},
        message_router::BlueskyEndpoint,
    },
    coordination::groups::{AgentGroup, AgentWithMembership, GroupManager, GroupResponseEvent},
    data_source::{BlueskyFilter, DataSourceBuilder},
    db::{
        client::DB,
        ops::{self, atproto::get_user_atproto_identities},
    },
    embeddings::{EmbeddingProvider, cloud::OpenAIEmbedder},
    id::{AgentId, RelationId},
    memory::{Memory, MemoryBlock},
    message::{Message, MessageContent},
    model::{GenAiClient, ResponseOptions},
    tool::{
        ToolRegistry,
        builtin::{DataSourceTool, MessageTarget},
    },
};
use std::{sync::Arc, time::Duration};
use surrealdb::RecordId;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tracing::info;

use crate::{endpoints::CliEndpoint, output::Output};

#[cfg(feature = "discord")]
use pattern_discord::{bot::DiscordBot, endpoints::DiscordEndpoint};

/// Build a ContextConfig and CompressionStrategy from an AgentConfig with optional overrides
fn build_context_config(
    agent_config: &pattern_core::config::AgentConfig,
) -> (
    pattern_core::context::ContextConfig,
    Option<pattern_core::context::CompressionStrategy>,
) {
    let mut context_config = pattern_core::context::ContextConfig::default();
    let mut compression_strategy = None;

    // Set base instructions
    if let Some(system_prompt) = &agent_config.system_prompt {
        context_config.base_instructions = system_prompt.clone();
    }

    // Apply context options if available
    if let Some(ctx_opts) = &agent_config.context {
        if let Some(max_messages) = ctx_opts.max_messages {
            context_config.max_context_messages = max_messages;
        }
        if let Some(memory_char_limit) = ctx_opts.memory_char_limit {
            context_config.memory_char_limit = memory_char_limit;
        }
        if let Some(enable_thinking) = ctx_opts.enable_thinking {
            context_config.enable_thinking = enable_thinking;
        }
        if let Some(strategy) = &ctx_opts.compression_strategy {
            compression_strategy = Some(strategy.clone());
        }
    }

    (context_config, compression_strategy)
}

/// Set up Discord endpoint for an agent if configured
#[cfg(feature = "discord")]
async fn setup_discord_endpoint(agent: &Arc<dyn Agent>, output: &Output) -> Result<()> {
    // Check if DISCORD_TOKEN is available
    let discord_token = match std::env::var("DISCORD_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            // No Discord token configured
            return Ok(());
        }
    };

    output.status("Setting up Discord endpoint...");

    // Create Discord endpoint
    let mut discord_endpoint = DiscordEndpoint::new(discord_token);

    // Check for DISCORD_CHANNEL_ID (specific channel to listen to)
    if let Ok(channel_id) = std::env::var("DISCORD_CHANNEL_ID") {
        if let Ok(channel_id) = channel_id.parse::<u64>() {
            discord_endpoint = discord_endpoint.with_default_channel(channel_id);
            output.info("Listen channel:", &format!("{}", channel_id));
        } else {
            output.warning("Invalid DISCORD_CHANNEL_ID value");
        }
    }

    // Check if there's a default channel ID (fallback for messages without target)
    if let Ok(channel_id) = std::env::var("DISCORD_DEFAULT_CHANNEL") {
        if let Ok(channel_id) = channel_id.parse::<u64>() {
            // Only set if DISCORD_CHANNEL_ID wasn't already set
            if std::env::var("DISCORD_CHANNEL_ID").is_err() {
                discord_endpoint = discord_endpoint.with_default_channel(channel_id);
                output.info("Default channel:", &format!("{}", channel_id));
            }
        } else {
            output.warning("Invalid DISCORD_DEFAULT_CHANNEL value");
        }
    }

    // Check if there's a default DM user ID for CLI mode
    if let Ok(user_id) = std::env::var("DISCORD_DEFAULT_DM_USER") {
        if let Ok(user_id) = user_id.parse::<u64>() {
            discord_endpoint = discord_endpoint.with_default_dm_user(user_id);
            output.info("Default DM user:", &format!("{}", user_id));
        } else {
            output.warning("Invalid DISCORD_DEFAULT_DM_USER value");
        }
    }

    // Display APP_ID and PUBLIC_KEY if configured (for reference)
    if let Ok(app_id) = std::env::var("APP_ID") {
        output.info("Discord App ID:", &app_id);
    }
    if let Ok(_) = std::env::var("PUBLIC_KEY") {
        output.info("Public key:", "✓ Configured");
    }

    // Register the endpoint
    agent
        .register_endpoint("discord".to_string(), Arc::new(discord_endpoint))
        .await?;

    output.success("Discord endpoint configured");

    Ok(())
}

/// Set up Bluesky endpoint for an agent if configured
async fn setup_bluesky_endpoint(
    agent: &Arc<dyn Agent>,
    config: &PatternConfig,
    output: &Output,
) -> Result<()> {
    // Check if agent has a bluesky_handle configured
    let bluesky_handle = if let Some(handle) = &config.agent.bluesky_handle {
        handle.clone()
    } else {
        // No Bluesky handle configured for this agent
        return Ok(());
    };

    output.status(&format!(
        "Checking Bluesky credentials for {}",
        bluesky_handle.bright_cyan()
    ));

    // Look up ATProto identity for this handle
    let identities = get_user_atproto_identities(&DB, &config.user.id)
        .await
        .into_diagnostic()?;

    // Find identity matching the handle
    let identity = identities
        .into_iter()
        .find(|i| i.handle == bluesky_handle || i.id.to_string() == bluesky_handle);

    if let Some(identity) = identity {
        // Get credentials
        if let Some(creds) = identity.get_auth_credentials() {
            output.status(&format!(
                "Setting up Bluesky endpoint for {}",
                identity.handle.bright_cyan()
            ));

            // Create Bluesky endpoint
            match BlueskyEndpoint::new(creds, identity.handle.clone()).await {
                Ok(endpoint) => {
                    // Register as the Bluesky endpoint for this agent
                    agent
                        .register_endpoint("bluesky".to_string(), Arc::new(endpoint))
                        .await?;
                    output.success(&format!(
                        "Bluesky endpoint configured for {}",
                        identity.handle.bright_green()
                    ));
                }
                Err(e) => {
                    output.warning(&format!("Failed to create Bluesky endpoint: {:?}", e));
                }
            }
        } else {
            output.warning(&format!(
                "No credentials available for Bluesky account {}",
                bluesky_handle
            ));
        }
    } else {
        output.warning(&format!(
            "No ATProto identity found for handle '{}'. Run 'pattern-cli atproto login' to authenticate.",
            bluesky_handle
        ));
    }

    Ok(())
}

/// Load an existing agent from the database or create a new one
pub async fn load_or_create_agent(
    name: &str,
    model_name: Option<String>,
    enable_tools: bool,
    config: &PatternConfig,
    heartbeat_sender: heartbeat::HeartbeatSender,
) -> Result<Arc<dyn Agent>> {
    let output = Output::new();

    // First, try to find an existing agent with this name
    let query = "SELECT id FROM agent WHERE name = $name LIMIT 1";
    let mut response = DB
        .query(query)
        .bind(("name", name.to_string()))
        .await
        .into_diagnostic()?;

    let agent_ids: Vec<RecordId> = response.take("id").into_diagnostic()?;

    let agent = if let Some(id_value) = agent_ids.first() {
        let agent_id = AgentId::from_record(id_value.clone());

        // Load the full agent record
        let mut existing_agent = match AgentRecord::load_with_relations(&DB, &agent_id).await {
            Ok(Some(agent)) => {
                tracing::trace!("Full AgentRecord: {:#?}", agent);
                agent
            }
            Ok(None) => return Err(miette::miette!("Agent not found after query")),
            Err(e) => return Err(miette::miette!("Failed to load agent: {}", e)),
        };

        // Load memories and messages
        load_agent_memories_and_messages(&mut existing_agent, &output).await?;

        output.kv("ID", &existing_agent.id.to_string().dimmed().to_string());
        output.kv(
            "Type",
            &format!("{:?}", existing_agent.agent_type)
                .bright_yellow()
                .to_string(),
        );
        output.kv(
            "History",
            &format!("{} messages", existing_agent.total_messages),
        );
        println!();

        // Create runtime agent from the stored record
        create_agent_from_record(
            existing_agent.clone(),
            model_name,
            enable_tools,
            config,
            heartbeat_sender,
        )
        .await?
    } else {
        output.info("+", &format!("Creating new agent '{}'", name.bright_cyan()));
        println!();

        // Create a new agent
        create_agent(name, model_name, enable_tools, config, heartbeat_sender).await?
    };

    // Set up Bluesky endpoint if configured
    let output = Output::new();
    setup_bluesky_endpoint(&agent, config, &output)
        .await
        .inspect_err(|e| {
            tracing::error!("{:?}", e);
        })?;

    Ok(agent)
}

/// Load memory blocks and messages for an AgentRecord
pub async fn load_agent_memories_and_messages(
    agent_record: &mut AgentRecord,
    output: &Output,
) -> Result<()> {
    // Manually load message history since the macro doesn't handle edge entities properly yet
    agent_record.messages = agent_record
        .load_message_history(&DB, false)
        .await
        .map_err(|e| miette::miette!("Failed to load message history: {}", e))?;

    tracing::debug!(
        "After loading message history: {} messages",
        agent_record.messages.len()
    );

    // Also manually load memory blocks using the ops function
    let memory_tuples = ops::get_agent_memories(&DB, &agent_record.id)
        .await
        .map_err(|e| miette::miette!("Failed to load memory blocks: {}", e))?;

    output.status(&format!(
        "Loaded {} memory blocks for agent {}",
        memory_tuples.len(),
        agent_record.name
    ));

    // Convert to the format expected by AgentRecord
    agent_record.memories = memory_tuples
        .into_iter()
        .map(|(memory_block, access_level)| {
            output.list_item(&format!(
                "{} ({} chars)",
                memory_block.label.bright_yellow(),
                memory_block.value.len()
            ));
            let relation = pattern_core::agent::AgentMemoryRelation {
                id: RelationId::nil(),
                in_id: agent_record.id.clone(),
                out_id: memory_block.id.clone(),
                access_level,
                created_at: chrono::Utc::now(),
            };
            (memory_block, relation)
        })
        .collect();

    tracing::debug!(
        "After loading memory blocks: {} memories",
        agent_record.memories.len()
    );

    Ok(())
}

pub async fn load_model_embedding_providers(
    model_name: Option<String>,
    config: &PatternConfig,
    record: Option<&AgentRecord>,
    enable_tools: bool,
) -> Result<(
    Arc<RwLock<GenAiClient>>,
    Option<Arc<OpenAIEmbedder>>,
    ResponseOptions,
)> {
    // Create model provider - use OAuth if available
    let model_provider = {
        #[cfg(feature = "oauth")]
        {
            use pattern_core::oauth::resolver::OAuthClientBuilder;
            let oauth_client =
                OAuthClientBuilder::new(Arc::new(DB.clone()), config.user.id.clone()).build()?;
            // Wrap in GenAiClient with all endpoints available
            let genai_client = GenAiClient::with_endpoints(
                oauth_client,
                vec![
                    genai::adapter::AdapterKind::Anthropic,
                    genai::adapter::AdapterKind::Gemini,
                    genai::adapter::AdapterKind::OpenAI,
                    genai::adapter::AdapterKind::Groq,
                    genai::adapter::AdapterKind::Cohere,
                ],
            );
            Arc::new(RwLock::new(genai_client))
        }
        #[cfg(not(feature = "oauth"))]
        {
            Arc::new(RwLock::new(GenAiClient::new().await?))
        }
    };

    // Get available models and select the one to use
    let model_info = {
        let provider = model_provider.read().await;
        let models = provider.list_models().await?;

        // If a specific model was requested, try to find it
        // Priority: CLI arg > config > stored preference > defaults
        let selected_model = if let Some(requested_model) = &model_name {
            models
                .iter()
                .find(|m| {
                    let model_lower = requested_model.to_lowercase();
                    m.id.to_lowercase().contains(&model_lower)
                        || m.name.to_lowercase().contains(&model_lower)
                })
                .cloned()
        } else if let Some(record) = record
            && let Some(stored_model) = &record.model_id
        {
            // Try to use the agent's stored model preference
            models.iter().find(|m| &m.id == stored_model).cloned()
        } else if let Some(config_model) = &config.model.model {
            // Use model from config
            models
                .iter()
                .find(|m| {
                    let model_lower = config_model.to_lowercase();
                    m.id.to_lowercase().contains(&model_lower)
                        || m.name.to_lowercase().contains(&model_lower)
                })
                .cloned()
        } else {
            // Default to Gemini models with free tier
            models
                .iter()
                .find(|m| {
                    m.provider.to_lowercase() == "gemini" && m.id.contains("gemini-2.5-flash")
                })
                .cloned()
                .or_else(|| {
                    models
                        .iter()
                        .find(|m| {
                            m.provider.to_lowercase() == "gemini" && m.id.contains("gemini-2.5-pro")
                        })
                        .cloned()
                })
                .or_else(|| models.clone().into_iter().next())
        };

        selected_model.ok_or_else(|| {
            if let Some(config_model) = &config.model.model {
                let available: Vec<_> = models
                    .iter()
                    .filter(|m| m.provider == config.model.provider.to_string())
                    .map(|m| m.id.as_str())
                    .collect();

                if available.is_empty() {
                    miette::miette!("No models available. Please set API keys in your .env file")
                } else {
                    miette::miette!(
                        "Model '{}' not found. Available: {:?}",
                        config_model,
                        available
                    )
                }
            } else {
                miette::miette!("No models available. Please set API keys in your .env file")
            }
        })?
    };

    info!("Selected model: {} ({})", model_info.name, model_info.id);

    // Create embedding provider if API key is available
    let embedding_provider = if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        Some(Arc::new(OpenAIEmbedder::new(
            "text-embedding-3-small".to_string(),
            api_key,
            None,
        )))
    } else {
        None
    };

    // Create response options with the selected model
    let mut response_options = ResponseOptions {
        model_info: model_info.clone(),
        temperature: Some(0.7),
        max_tokens: Some(pattern_core::model::defaults::calculate_max_tokens(
            &model_info,
            None,
        )),
        capture_content: Some(true),
        capture_tool_calls: Some(enable_tools),
        top_p: None,
        stop_sequences: vec![],
        capture_usage: Some(true),
        capture_reasoning_content: Some(true),
        capture_raw_body: None,
        response_format: None,
        normalize_reasoning_content: Some(true),
        reasoning_effort: Some(genai::chat::ReasoningEffort::Medium),
    };

    // Enable reasoning mode if the model supports it
    if model_info
        .capabilities
        .contains(&pattern_core::model::ModelCapability::ExtendedThinking)
    {
        response_options.capture_reasoning_content = Some(true);
        response_options.normalize_reasoning_content = Some(true);
        // Use medium effort by default
        response_options.reasoning_effort = Some(genai::chat::ReasoningEffort::Medium);
    }

    Ok((model_provider, embedding_provider, response_options))
}

/// Create a runtime agent from a stored AgentRecord
pub async fn create_agent_from_record(
    record: AgentRecord,
    model_name: Option<String>,
    enable_tools: bool,
    config: &PatternConfig,
    heartbeat_sender: heartbeat::HeartbeatSender,
) -> Result<Arc<dyn Agent>> {
    let (model_provider, embedding_provider, response_options) =
        load_model_embedding_providers(model_name, config, Some(&record), enable_tools).await?;
    // Create tool registry
    let tools = ToolRegistry::new();

    // Create agent from the record
    let agent = DatabaseAgent::from_record(
        record,
        DB.clone(),
        model_provider,
        tools.clone(),
        embedding_provider.clone(),
        heartbeat_sender,
    )
    .await?;

    // Set the chat options with our selected model
    {
        let mut options = agent.chat_options.write().await;
        *options = Some(response_options);
    }

    // Wrap in Arc before calling monitoring methods
    let agent = Arc::new(agent);
    agent.clone().start_stats_sync().await?;
    agent.clone().start_memory_sync().await?;
    agent.clone().start_message_monitoring().await?;

    // Register data sources
    register_data_sources(agent.clone(), config, tools, embedding_provider).await;

    // Convert to trait object for endpoint setup
    let agent_dyn: Arc<dyn Agent> = agent;

    // Set up Bluesky endpoint if configured
    let output = Output::new();
    setup_bluesky_endpoint(&agent_dyn, config, &output)
        .await
        .inspect_err(|e| {
            tracing::error!("Failed to setup Bluesky endpoint: {:?}", e);
        })?;

    Ok(agent_dyn)
}

/// Create a runtime agent from a stored AgentRecord with a shared constellation tracker
pub async fn create_agent_from_record_with_tracker(
    record: AgentRecord,
    model_name: Option<String>,
    enable_tools: bool,
    config: &PatternConfig,
    heartbeat_sender: heartbeat::HeartbeatSender,
    constellation_tracker: Option<
        Arc<pattern_core::constellation_memory::ConstellationActivityTracker>,
    >,
    output: &Output,
    shared_tools: Option<ToolRegistry>,
) -> Result<Arc<dyn Agent>> {
    let (model_provider, embedding_provider, response_options) =
        load_model_embedding_providers(model_name, config, Some(&record), enable_tools).await?;
    // Use shared tools if provided, otherwise create new registry
    let tools = shared_tools.unwrap_or_else(ToolRegistry::new);

    // Create agent from the record
    let agent = DatabaseAgent::from_record(
        record.clone(),
        DB.clone(),
        model_provider,
        tools.clone(),
        embedding_provider.clone(),
        heartbeat_sender,
    )
    .await?;

    // Set the chat options with our selected model
    {
        let mut options = agent.chat_options.write().await;
        *options = Some(response_options);
    }

    // If we have a constellation tracker, set it up
    if let Some(tracker) = constellation_tracker {
        // Check if agent already has a constellation_activity block
        let existing_block = agent.get_memory("constellation_activity").await?;

        let _block = if let Some(existing) = existing_block {
            // Reuse the existing block's ID to avoid duplicates
            tracing::info!(
                "Found existing constellation_activity block with id: {}",
                existing.id
            );
            let updated_block =
                pattern_core::constellation_memory::create_constellation_activity_block(
                    existing.id.clone(),
                    record.owner_id.clone(),
                    tracker.format_as_memory_content().await,
                );
            agent
                .update_memory("constellation_activity", updated_block.clone())
                .await?;

            updated_block
        } else {
            // Check database for any existing constellation_activity blocks for this agent
            tracing::info!("No constellation_activity block in memory, checking database");
            let existing_memories =
                ops::get_agent_memories(&DB, &record.id)
                    .await
                    .map_err(|e| {
                        miette::miette!("Failed to check for existing memory blocks: {}", e)
                    })?;

            let existing_constellation_block = existing_memories
                .iter()
                .find(|(mem, _)| mem.label == "constellation_activity")
                .map(|(mem, _)| mem.clone());

            if let Some(existing) = existing_constellation_block {
                // Found one in database, reuse its ID
                tracing::info!(
                    "Found existing constellation_activity block in database with id: {}",
                    existing.id
                );
                let updated_block =
                    pattern_core::constellation_memory::create_constellation_activity_block(
                        existing.id.clone(),
                        record.owner_id.clone(),
                        tracker.format_as_memory_content().await,
                    );
                agent
                    .update_memory("constellation_activity", updated_block.clone())
                    .await?;

                updated_block
            } else {
                // Really first time - create new block with the tracker's memory ID
                tracing::info!(
                    "Creating new constellation_activity block with tracker id: {}",
                    tracker.memory_id()
                );
                let activity_block =
                    pattern_core::constellation_memory::create_constellation_activity_block(
                        tracker.memory_id().clone(),
                        record.owner_id.clone(),
                        tracker.format_as_memory_content().await,
                    );
                agent
                    .update_memory("constellation_activity", activity_block.clone())
                    .await?;

                activity_block
            }
        };

        // Set the tracker on the agent's context
        agent.set_constellation_tracker(tracker);

        tracing::debug!("persisting constellation tracker memory");
        // ops::persist_agent_memory(
        //     &DB,
        //     agent.id(),
        //     &block,
        //     pattern_core::memory::MemoryPermission::ReadWrite,
        // )
        // .await?;
    }

    // Wrap in Arc before calling monitoring methods
    let agent = Arc::new(agent);

    tokio::time::sleep(Duration::from_millis(112)).await;
    agent.clone().start_stats_sync().await?;
    agent.clone().start_memory_sync().await?;
    agent.clone().start_message_monitoring().await?;

    // NOTE: We do NOT register data sources here - that's handled by the caller
    // This function should only create the agent without side effects
    //

    // Convert to trait object for endpoint setup
    let agent_dyn: Arc<dyn Agent> = agent;

    // Set up Bluesky endpoint if configured
    setup_bluesky_endpoint(&agent_dyn, config, output)
        .await
        .inspect_err(|e| {
            tracing::error!("Failed to setup Bluesky endpoint: {:?}", e);
        })?;

    // Set up Discord endpoint if configured
    #[cfg(feature = "discord")]
    {
        setup_discord_endpoint(&agent_dyn, output)
            .await
            .inspect_err(|e| {
                tracing::error!("Failed to setup Discord endpoint: {:?}", e);
            })?;
    }

    Ok(agent_dyn)
}

pub async fn register_data_sources<M, E>(
    agent: Arc<DatabaseAgent<M, E>>,
    config: &PatternConfig,
    tools: ToolRegistry,
    embedding_provider: Option<Arc<E>>,
) where
    E: EmbeddingProvider + Clone + 'static,
    M: ModelProvider + 'static,
{
    register_data_sources_with_target(agent, config, tools, embedding_provider, None).await
}

/// Create an agent from a record with data sources configured for a group
#[allow(dead_code)]
pub async fn create_agent_from_record_with_data_sources(
    record: AgentRecord,
    model_name: Option<String>,
    enable_tools: bool,
    config: &PatternConfig,
    heartbeat_sender: pattern_core::context::heartbeat::HeartbeatSender,
    constellation_tracker: Option<
        Arc<pattern_core::constellation_memory::ConstellationActivityTracker>,
    >,
    group_id: Option<pattern_core::id::GroupId>,
    _group_name: Option<String>,
) -> Result<Arc<dyn Agent>> {
    // Use the existing function to create model and embedding providers
    let (model_provider, embedding_provider, response_options) =
        load_model_embedding_providers(model_name, config, Some(&record), enable_tools).await?;

    // Create tool registry
    let tools = ToolRegistry::new();

    // Create agent from the record
    let agent = DatabaseAgent::from_record(
        record,
        DB.clone(),
        model_provider,
        tools.clone(),
        embedding_provider.clone(),
        heartbeat_sender,
    )
    .await?;

    // Set the chat options with our selected model
    {
        let mut options = agent.chat_options.write().await;
        *options = Some(response_options);
    }

    // Wrap in Arc before calling monitoring methods
    let agent = Arc::new(agent);
    agent.clone().start_stats_sync().await?;
    agent.clone().start_memory_sync().await?;
    agent.clone().start_message_monitoring().await?;

    // Set constellation tracker if provided
    if let Some(tracker) = constellation_tracker.clone() {
        agent.set_constellation_tracker(tracker);
    }

    // If group_id is provided, DON'T set up data sources yet
    // They will be started after the group endpoint is registered
    let has_group = group_id.is_some();
    if !has_group {
        // No group - use regular data sources
        register_data_sources(agent.clone(), config, tools, embedding_provider).await;
    }

    // Convert to trait object
    let agent_dyn: Arc<dyn Agent> = agent;

    // Set up endpoints - skip Bluesky endpoint since we're routing to group

    let output = Output::new();
    setup_bluesky_endpoint(&agent_dyn, config, &output).await?;

    // Only register CLI endpoint if NOT part of a group
    // When part of a group, chat_with_group will register both CLI and Group endpoints
    if !has_group {
        // Check if Discord is configured and user wants Discord as default
        let use_discord_default = std::env::var("DISCORD_DEFAULT_USER_ENDPOINT")
            .map(|v| v.to_lowercase() == "true" || v == "1")
            .unwrap_or(false);

        if use_discord_default && std::env::var("DISCORD_TOKEN").is_ok() {
            // Discord is configured and user wants it as default
            // The Discord endpoint was already registered in setup_discord_endpoint
            output.info("Default endpoint:", "Discord (DMs/channels)");
        } else {
            // Register CLI endpoint as default
            agent_dyn
                .set_default_user_endpoint(Arc::new(CliEndpoint::new(output.clone())))
                .await?;
            output.info("Default endpoint:", "CLI");
        }
    }

    Ok(agent_dyn)
}

pub async fn register_data_sources_with_target<M, E>(
    agent: Arc<DatabaseAgent<M, E>>,
    config: &PatternConfig,
    tools: ToolRegistry,
    embedding_provider: Option<Arc<E>>,
    target: Option<MessageTarget>,
) where
    E: EmbeddingProvider + Clone + 'static,
    M: ModelProvider + 'static,
{
    let config = config.clone();

    // hardcoding so that only pattern gets messages initially
    if agent.name() == "Pattern" {
        tokio::spawn(async move {
            let filter = config
                .bluesky
                .as_ref()
                .and_then(|b| b.default_filter.as_ref())
                .unwrap_or(&BlueskyFilter {
                    exclude_keywords: vec!["patternstop".to_string()],
                    ..Default::default()
                })
                .clone();
            tracing::info!("filter: {:?}", filter);
            let mut data_sources = if let Some(target) = target {
                DataSourceBuilder::new()
                    .with_bluesky_source("bluesky_jetstream".to_string(), filter, true)
                    .build_with_target(
                        agent.id(),
                        agent.name(),
                        DB.clone(),
                        embedding_provider,
                        Some(agent.handle().await),
                        get_bluesky_credentials(&config).await,
                        target,
                    )
                    .await
                    .unwrap()
            } else {
                DataSourceBuilder::new()
                    .with_bluesky_source("bluesky_jetstream".to_string(), filter, true)
                    .build(
                        agent.id(),
                        agent.name(),
                        DB.clone(),
                        embedding_provider,
                        Some(agent.handle().await),
                        get_bluesky_credentials(&config).await,
                    )
                    .await
                    .unwrap()
            };

            data_sources
                .start_monitoring("bluesky_jetstream")
                .await
                .unwrap();
            tools.register(DataSourceTool::new(Arc::new(RwLock::new(data_sources))));
        });
    }
}

pub async fn get_bluesky_credentials(
    config: &PatternConfig,
) -> Option<(
    pattern_core::atproto_identity::AtprotoAuthCredentials,
    String,
)> {
    // Check if agent has a bluesky_handle configured
    let bluesky_handle = if let Some(handle) = &config.agent.bluesky_handle {
        handle.clone()
    } else {
        // No Bluesky handle configured for this agent
        return None;
    };
    // Look up ATProto identity for this handle
    let identities = get_user_atproto_identities(&DB, &config.user.id)
        .await
        .ok()
        .unwrap_or_default();

    // Find identity matching the handle
    let identity = identities
        .into_iter()
        .find(|i| i.handle == bluesky_handle || i.id.to_string() == bluesky_handle);

    if let Some(identity) = identity {
        // Get credentials
        if let Some(creds) = identity.get_auth_credentials() {
            return Some((creds, bluesky_handle));
        }
    }
    None
}

/// Create an agent with the specified configuration
pub async fn create_agent(
    name: &str,
    model_name: Option<String>,
    enable_tools: bool,
    config: &PatternConfig,
    heartbeat_sender: heartbeat::HeartbeatSender,
) -> Result<Arc<dyn Agent>> {
    let output = Output::new();

    let (model_provider, embedding_provider, response_options) =
        load_model_embedding_providers(model_name, config, None, enable_tools).await?;

    // Create memory with the configured user as owner
    let memory = Memory::with_owner(&config.user.id);

    // Create tool registry
    let tools = ToolRegistry::new();

    // Use IDs from config or generate new ones
    let agent_id = config.agent.id.clone().unwrap_or_else(AgentId::generate);
    let user_id = config.user.id.clone();

    // Build context config from agent config
    let (context_config, compression_strategy) = build_context_config(&config.agent);

    // Load tool rules from configuration
    let tool_rules = config.agent.get_tool_rules().unwrap_or_else(|e| {
        output.warning(&format!("Failed to load tool rules from config: {}", e));
        output.status("Agent will run without tool rules");
        vec![]
    });

    if !tool_rules.is_empty() {
        output.success(&format!(
            "Loaded {} tool rules from configuration",
            tool_rules.len()
        ));
    }

    // Create agent
    let agent = DatabaseAgent::new(
        agent_id.clone(),
        user_id,
        AgentType::Generic,
        name.to_string(),
        context_config.base_instructions.clone(),
        memory,
        DB.clone(),
        model_provider,
        tools.clone(),
        embedding_provider.clone(),
        heartbeat_sender,
        tool_rules,
    );

    // Update the agent with the full context config
    agent
        .update_context_config(context_config, compression_strategy)
        .await?;

    // Set the chat options with our selected model
    {
        let mut options = agent.chat_options.write().await;
        *options = Some(response_options);
    }

    // Store the agent in the database
    match agent.store().await {
        Ok(_) => {
            output.success(&format!(
                "Saved new agent '{}' to database",
                name.bright_cyan()
            ));
            println!();
        }
        Err(e) => {
            output.warning(&format!("Failed to save agent to database: {}", e));
            output.status("Agent will work for this session but won't persist");
            println!();
        }
    }

    // Wrap in Arc before calling monitoring methods
    let agent = Arc::new(agent);
    agent.clone().start_stats_sync().await?;
    agent.clone().start_memory_sync().await?;
    agent.clone().start_message_monitoring().await?;

    // Update memory blocks from config only if they don't exist or have changed
    // First check persona
    if let Some(persona) = &config.agent.persona {
        match agent.get_memory("persona").await {
            Ok(Some(mut existing)) => {
                // Update existing block preserving its ID
                if existing.value != *persona {
                    output.status("Updating persona in agent's core memory...");
                    existing.value = persona.clone();
                    existing.description = Some("Agent's persona and identity".to_string());
                    existing.permission = pattern_core::memory::MemoryPermission::Append;

                    if let Err(e) = agent.update_memory("persona", existing).await {
                        output.warning(&format!("Failed to update persona memory: {}", e));
                    }
                }
            }
            Ok(None) | Err(_) => {
                // Create new block
                output.status("Adding persona to agent's core memory...");
                let persona_block =
                    MemoryBlock::owned(config.user.id.clone(), "persona", persona.clone())
                        .with_description("Agent's persona and identity")
                        .with_permission(pattern_core::memory::MemoryPermission::Append);

                if let Err(e) = agent.update_memory("persona", persona_block).await {
                    output.warning(&format!("Failed to add persona memory: {}", e));
                }
            }
        }
    }

    // Check and update other configured memory blocks
    for (label, block_config) in &config.agent.memory {
        // Load content from either inline or file
        let content = match block_config.load_content().await {
            Ok(content) => content,
            Err(e) => {
                output.warning(&format!(
                    "Failed to load content for memory block '{}': {}",
                    label, e
                ));
                continue;
            }
        };

        match agent.get_memory(label).await {
            Ok(Some(mut existing)) => {
                // Update existing block preserving its ID
                if existing.value != content {
                    output.info("Updating memory block", &label.bright_yellow().to_string());
                    existing.value = content;
                    existing.memory_type = block_config.memory_type;
                    existing.permission = block_config.permission;
                    if let Some(desc) = &block_config.description {
                        existing.description = Some(desc.clone());
                    }

                    if let Err(e) = agent.update_memory(label, existing).await {
                        output
                            .warning(&format!("Failed to update memory block '{}': {}", label, e));
                    }
                }
            }
            Ok(None) | Err(_) => {
                // Check if this is a shared memory that already exists
                if block_config.shared {
                    // Look for existing shared memory by owner and label
                    match pattern_core::db::ops::find_memory_by_owner_and_label(
                        &DB,
                        &config.user.id,
                        label,
                    )
                    .await
                    {
                        Ok(Some(existing_memory)) => {
                            output.info(
                                "Linking to shared memory",
                                &label.bright_yellow().to_string(),
                            );

                            // Check if we need to update the permission to be more permissive
                            if block_config.permission > existing_memory.permission {
                                output.info(
                                    "Updating shared memory permission",
                                    &format!(
                                        "{:?} -> {:?}",
                                        existing_memory.permission, block_config.permission
                                    ),
                                );
                                // Update the memory's permission
                                let mut updated_memory = existing_memory.clone();
                                updated_memory.permission = block_config.permission;
                                updated_memory.value = content; // Also update content if different
                                if let Some(desc) = &block_config.description {
                                    updated_memory.description = Some(desc.clone());
                                }

                                // Update the memory block itself
                                if let Err(e) = pattern_core::db::ops::update_memory_content(
                                    &DB,
                                    updated_memory.id.clone(),
                                    updated_memory.value.clone(),
                                    embedding_provider.as_ref().map(|p| p.as_ref()),
                                )
                                .await
                                {
                                    output.warning(&format!(
                                        "Failed to update shared memory '{}': {}",
                                        label, e
                                    ));
                                }
                            }

                            // Attach the memory to this agent with their requested permission
                            if let Err(e) = pattern_core::db::ops::attach_memory_to_agent(
                                &DB,
                                &agent.id(),
                                &existing_memory.id,
                                block_config.permission,
                            )
                            .await
                            {
                                output.warning(&format!(
                                    "Failed to attach shared memory '{}': {}",
                                    label, e
                                ));
                            }
                        }
                        Ok(None) => {
                            // No existing shared memory, create it
                            output.info(
                                "Creating new shared memory",
                                &label.bright_yellow().to_string(),
                            );
                            let memory_block =
                                MemoryBlock::owned(config.user.id.clone(), label.clone(), content)
                                    .with_memory_type(block_config.memory_type)
                                    .with_permission(block_config.permission);

                            let memory_block = if let Some(desc) = &block_config.description {
                                memory_block.with_description(desc.clone())
                            } else {
                                memory_block
                            };

                            if let Err(e) = agent.update_memory(label, memory_block).await {
                                output.warning(&format!(
                                    "Failed to add memory block '{}': {}",
                                    label, e
                                ));
                            }
                        }
                        Err(e) => {
                            output.warning(&format!(
                                "Failed to check for shared memory '{}': {}",
                                label, e
                            ));
                            // Fall back to creating a new memory
                            let memory_block =
                                MemoryBlock::owned(config.user.id.clone(), label.clone(), content)
                                    .with_memory_type(block_config.memory_type)
                                    .with_permission(block_config.permission);

                            let memory_block = if let Some(desc) = &block_config.description {
                                memory_block.with_description(desc.clone())
                            } else {
                                memory_block
                            };

                            if let Err(e) = agent.update_memory(label, memory_block).await {
                                output.warning(&format!(
                                    "Failed to add memory block '{}': {}",
                                    label, e
                                ));
                            }
                        }
                    }
                } else {
                    // Not shared, create normally
                    output.info("Adding memory block", &label.bright_yellow().to_string());
                    let memory_block =
                        MemoryBlock::owned(config.user.id.clone(), label.clone(), content)
                            .with_memory_type(block_config.memory_type)
                            .with_permission(block_config.permission);

                    let memory_block = if let Some(desc) = &block_config.description {
                        memory_block.with_description(desc.clone())
                    } else {
                        memory_block
                    };

                    if let Err(e) = agent.update_memory(label, memory_block).await {
                        output.warning(&format!("Failed to add memory block '{}': {}", label, e));
                    }
                }
            }
        }
    }

    // Register data sources
    register_data_sources(agent.clone(), config, tools, embedding_provider).await;

    // Convert to trait object
    let agent_dyn: Arc<dyn Agent> = agent;

    Ok(agent_dyn)
}

/// Handle slash commands in chat
async fn handle_slash_command(
    command: &str,
    current_agent: &Arc<dyn Agent>,
    output: &Output,
) -> Result<bool> {
    let parts: Vec<&str> = command.trim().split_whitespace().collect();
    if parts.is_empty() {
        return Ok(false);
    }

    match parts[0] {
        "/help" | "/?" => {
            output.section("Available Commands");
            output.list_item("/help or /? - Show this help message");
            output.list_item("/exit or /quit - Exit the chat");
            output.list_item("/send <agent> <message> - Send a message to another agent");
            output.list_item("/list - List all agents");
            output.list_item("/status - Show current agent status");
            println!();
            Ok(false)
        }
        "/exit" | "/quit" => Ok(true),
        "/send" => {
            if parts.len() < 3 {
                output.error("Usage: /send <agent_name> <message>");
                return Ok(false);
            }

            let target_agent = parts[1];
            let message = parts[2..].join(" ");

            // Use the send_message tool through the agent
            let _tool_input = serde_json::json!({
                "target": {
                    "target_type": "agent",
                    "target_id": target_agent
                },
                "content": message,
                "request_heartbeat": false
            });

            output.status(&format!("Sending message to agent '{}'...", target_agent));

            // Create a system message asking the agent to send the message
            let system_msg = Message::system(format!(
                "Please use the send_message tool to send this message to agent '{}': {}",
                target_agent, message
            ));

            match current_agent.clone().process_message(system_msg).await {
                Ok(response) => {
                    // Check if the tool was used
                    let mut tool_used = false;
                    for content in &response.content {
                        if let MessageContent::ToolCalls(calls) = content {
                            for call in calls {
                                if call.fn_name == "send_message" {
                                    tool_used = true;
                                    output.success(&format!(
                                        "Message sent to agent '{}'",
                                        target_agent
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                    if !tool_used {
                        output.warning("Agent did not use send_message tool. Try enabling tools.");
                    }
                }
                Err(e) => {
                    output.error(&format!("Failed to send message: {}", e));
                }
            }

            Ok(false)
        }
        "/list" => {
            output.status("Fetching agent list...");
            match ops::list_entities::<AgentRecord, _>(&DB).await {
                Ok(agents) => {
                    if agents.is_empty() {
                        output.status("No agents found");
                    } else {
                        output.section("Available Agents");
                        for agent in agents {
                            output.list_item(&format!(
                                "{} ({})",
                                agent.name.bright_cyan(),
                                agent.id.to_string().dimmed()
                            ));
                        }
                    }
                }
                Err(e) => {
                    output.error(&format!("Failed to list agents: {}", e));
                }
            }
            println!();
            Ok(false)
        }
        "/status" => {
            output.section("Current Agent Status");
            output.kv("Name", &current_agent.name().bright_cyan().to_string());
            output.kv("ID", &current_agent.id().to_string().dimmed().to_string());

            // Try to get memory stats
            match current_agent.list_memory_keys().await {
                Ok(memory_blocks) => {
                    output.kv("Memory blocks", &memory_blocks.len().to_string());
                }
                Err(_) => {
                    output.kv("Memory blocks", "error loading");
                }
            }

            println!();
            Ok(false)
        }
        _ => {
            output.error(&format!("Unknown command: {}", parts[0]));
            output.status("Type /help for available commands");
            Ok(false)
        }
    }
}

/// Chat with an agent
pub async fn chat_with_agent(
    agent: Arc<dyn Agent>,
    mut heartbeat_receiver: heartbeat::HeartbeatReceiver,
) -> Result<()> {
    use rustyline_async::{Readline, ReadlineEvent};
    let output = Output::new();

    output.status("Type 'quit' or 'exit' to leave the chat");
    output.status("Use Ctrl+D for multiline input, Enter to send");

    let (mut rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;

    // Update the global tracing writer to use the SharedWriter
    crate::tracing_writer::set_shared_writer(writer.clone());

    // Create output with SharedWriter for proper concurrent output
    let output = output.with_writer(writer.clone());

    // Set up default user endpoint
    let use_discord_default = std::env::var("DISCORD_DEFAULT_USER_ENDPOINT")
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(false);

    if use_discord_default && std::env::var("DISCORD_TOKEN").is_ok() {
        // Discord is configured and user wants it as default
        // The Discord endpoint should already be registered
        output.info("Default endpoint:", "Discord (DMs/channels)");
    } else {
        // Register CLI endpoint as default
        let cli_endpoint = Arc::new(CliEndpoint::new(output.clone()));
        agent.set_default_user_endpoint(cli_endpoint).await?;
        output.info("Default endpoint:", "CLI");
    }

    // Spawn heartbeat monitor task
    let agent_clone = agent.clone();
    let output_clone = output.clone();
    tokio::spawn(async move {
        while let Some(heartbeat) = heartbeat_receiver.recv().await {
            tracing::debug!(
                "💓 Received heartbeat request from agent {}: tool {} (call_id: {})",
                heartbeat.agent_id,
                heartbeat.tool_name,
                heartbeat.tool_call_id
            );

            // Clone for the task
            let agent = agent_clone.clone();
            let output = output_clone.clone();
            // Spawn task to handle this heartbeat
            tokio::spawn(async move {
                tracing::info!("💓 Processing heartbeat from tool: {}", heartbeat.tool_name);

                // Create a system message to trigger another turn
                let heartbeat_message = Message::system(format!(
                    "[Heartbeat continuation from tool: {}]",
                    heartbeat.tool_name
                ));

                match agent.process_message_stream(heartbeat_message).await {
                    Ok(mut response_stream) => {
                        while let Some(event) = response_stream.next().await {
                            // Display heartbeat response
                            output.status("💓 Heartbeat continuation:");

                            print_response_event(event, &output);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error processing heartbeat: {}", e);
                    }
                }
            });
        }
        tracing::debug!("Heartbeat monitor task exiting");
    });

    loop {
        // Handle user input
        let event = rl.readline().await;
        match event {
            Ok(ReadlineEvent::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }

                // Check for slash commands
                if line.trim().starts_with('/') {
                    match handle_slash_command(&line, &agent, &output).await {
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

                // Create a message using the actual Message structure
                let message = Message {
                    content: MessageContent::Text(line.clone()),
                    word_count: line.split_whitespace().count() as u32,
                    ..Default::default()
                };

                let r_agent = agent.clone();
                let output = output.clone();
                tokio::spawn(async move {
                    // Process message with streaming
                    output.status("Thinking...");

                    use tokio_stream::StreamExt;

                    match r_agent.clone().process_message_stream(message).await {
                        Ok(mut stream) => {
                            while let Some(event) = stream.next().await {
                                print_response_event(event, &output);
                            }
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
            output.status(&format!("💭 Reasoning: {}", text));
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

/// Load or create an agent from a group member configuration
pub async fn load_or_create_agent_from_member(
    member: &pattern_core::config::GroupMemberConfig,
    user_id: &pattern_core::id::UserId,
    model_name: Option<String>,
    enable_tools: bool,
    heartbeat_sender: heartbeat::HeartbeatSender,
    main_config: Option<&PatternConfig>,
) -> Result<Arc<dyn Agent>> {
    let output = Output::new();

    // If member has an agent_id, load that agent
    if let Some(agent_id) = &member.agent_id {
        output.status(&format!(
            "Loading existing agent {} for group member",
            agent_id.to_string().dimmed()
        ));

        // Load the existing agent
        let mut agent_record = match AgentRecord::load_with_relations(&DB, agent_id).await {
            Ok(Some(agent)) => agent,
            Ok(None) => return Err(miette::miette!("Agent {} not found", agent_id)),
            Err(e) => return Err(miette::miette!("Failed to load agent {}: {}", agent_id, e)),
        };

        // Load memories and messages
        load_agent_memories_and_messages(&mut agent_record, &output).await?;

        // Create runtime agent from record (reusing existing function)
        // Build a config that preserves the bluesky_handle from main_config if available
        let bluesky_handle = main_config.and_then(|cfg| cfg.agent.bluesky_handle.clone());

        let config = PatternConfig {
            user: pattern_core::config::UserConfig {
                id: user_id.clone(),
                name: None,
                settings: Default::default(),
            },
            agent: pattern_core::config::AgentConfig {
                id: Some(agent_id.clone()),
                name: agent_record.name.clone(),
                system_prompt: None,
                persona: None,
                instructions: None,
                memory: Default::default(),
                bluesky_handle,
                tool_rules: Vec::new(),
                tools: Vec::new(),
                model: None,
                context: None,
            },
            model: pattern_core::config::ModelConfig {
                provider: "Gemini".to_string(),
                model: model_name,
                temperature: None,
                settings: Default::default(),
            },
            database: Default::default(),
            groups: vec![],
            bluesky: None,
        };

        return create_agent_from_record(
            agent_record,
            None,
            enable_tools,
            &config,
            heartbeat_sender,
        )
        .await;
    }

    // If member has a config_path, load the agent config from file
    if let Some(config_path) = &member.config_path {
        output.status(&format!(
            "Loading agent config from {}",
            config_path.display().bright_cyan()
        ));

        let agent_config = pattern_core::config::AgentConfig::load_from_file(config_path).await?;

        // Build full config with loaded agent config
        // Use agent's model config if available, otherwise fall back to main config, then default
        let model_config = if let Some(agent_model) = &agent_config.model {
            agent_model.clone()
        } else if let Some(main_cfg) = main_config {
            main_cfg.model.clone()
        } else {
            pattern_core::config::ModelConfig {
                provider: "Gemini".to_string(),
                model: model_name,
                temperature: None,
                settings: Default::default(),
            }
        };

        let config = PatternConfig {
            user: pattern_core::config::UserConfig {
                id: user_id.clone(),
                name: None,
                settings: Default::default(),
            },
            agent: agent_config,
            model: model_config,
            database: Default::default(),
            groups: vec![],
            bluesky: None,
        };

        // Use the agent name from config, or fall back to member name
        let agent_name = if !config.agent.name.is_empty() {
            config.agent.name.clone()
        } else {
            member.name.clone()
        };

        // Extract the model name from the agent's config to pass explicitly
        let agent_model_name = config.agent.model.as_ref().and_then(|m| m.model.clone());

        // Load or create the agent with this config
        return load_or_create_agent(
            &agent_name,
            agent_model_name,
            enable_tools,
            &config,
            heartbeat_sender,
        )
        .await;
    }

    // Check if member has an inline agent_config
    if let Some(inline_config) = &member.agent_config {
        output.status(&format!(
            "Creating agent '{}' from inline config",
            member.name.bright_cyan()
        ));

        // Build full config with inline agent config
        // Use agent's model config if available, otherwise fall back to main config, then default
        let model_config = if let Some(agent_model) = &inline_config.model {
            agent_model.clone()
        } else if let Some(main_cfg) = main_config {
            main_cfg.model.clone()
        } else {
            pattern_core::config::ModelConfig {
                provider: "Gemini".to_string(),
                model: model_name,
                temperature: None,
                settings: Default::default(),
            }
        };

        // Preserve the bluesky_handle from inline config or main config
        let mut agent_config = inline_config.clone();
        if agent_config.bluesky_handle.is_none() {
            if let Some(main_cfg) = main_config {
                agent_config.bluesky_handle = main_cfg.agent.bluesky_handle.clone();
            }
        }

        let config = PatternConfig {
            user: pattern_core::config::UserConfig {
                id: user_id.clone(),
                name: None,
                settings: Default::default(),
            },
            agent: agent_config,
            model: model_config,
            database: Default::default(),
            groups: vec![],
            bluesky: None,
        };

        // Use the agent name from config, or fall back to member name
        let agent_name = if !config.agent.name.is_empty() {
            config.agent.name.clone()
        } else {
            member.name.clone()
        };

        // Extract the model name from the agent's config to pass explicitly
        let agent_model_name = config.agent.model.as_ref().and_then(|m| m.model.clone());

        // Load or create the agent with this config
        return load_or_create_agent(
            &agent_name,
            agent_model_name,
            enable_tools,
            &config,
            heartbeat_sender,
        )
        .await;
    }

    // Otherwise create a basic agent with just the member name
    output.info(
        "+",
        &format!("Creating basic agent '{}'", member.name.bright_cyan()),
    );

    let config = PatternConfig {
        user: pattern_core::config::UserConfig {
            id: user_id.clone(),
            name: None,
            settings: Default::default(),
        },
        agent: pattern_core::config::AgentConfig {
            id: None,
            name: member.name.clone(),
            system_prompt: None,
            persona: None,
            instructions: None,
            memory: Default::default(),
            bluesky_handle: main_config.and_then(|cfg| cfg.agent.bluesky_handle.clone()),
            tool_rules: Vec::new(),
            tools: Vec::new(),
            model: None,
            context: None,
        },
        model: pattern_core::config::ModelConfig {
            provider: "Gemini".to_string(),
            model: model_name,
            temperature: None,
            settings: Default::default(),
        },
        database: Default::default(),
        groups: vec![],
        bluesky: None,
    };

    create_agent(&member.name, None, enable_tools, &config, heartbeat_sender).await
}

/// Chat with a group of agents
pub async fn chat_with_group<M: GroupManager + Clone + 'static>(
    group: AgentGroup,
    agents: Vec<Arc<dyn Agent>>,
    pattern_manager: M,
    heartbeat_receiver: heartbeat::HeartbeatReceiver,
) -> Result<()> {
    use rustyline_async::Readline;

    let (rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;

    // Update the global tracing writer to use the SharedWriter
    crate::tracing_writer::set_shared_writer(writer.clone());

    // Create output with SharedWriter for proper concurrent output
    let output = Output::new().with_writer(writer.clone());

    // Initialize group chat
    let agents_with_membership = init_group_chat(&group, agents, &pattern_manager, &output).await?;

    // Run the chat loop
    run_group_chat_loop(
        group,
        agents_with_membership,
        pattern_manager,
        heartbeat_receiver,
        output,
        rl,
    )
    .await
}

/// Shared setup for group agents including loading, memory setup, and tool registration
pub struct GroupSetup {
    pub group: AgentGroup,
    pub agents_with_membership: Vec<AgentWithMembership<Arc<dyn Agent>>>,
    pub pattern_agent: Option<Arc<dyn Agent>>,
    pub shared_tools: ToolRegistry,
    #[allow(dead_code)] // Used during agent creation, kept for potential future use
    pub constellation_tracker:
        Arc<pattern_core::constellation_memory::ConstellationActivityTracker>,
    #[allow(dead_code)] // Used during agent creation, kept for potential future use
    pub heartbeat_sender: HeartbeatSender,
    pub heartbeat_receiver: HeartbeatReceiver,
}

/// Set up a group with all necessary components
pub async fn setup_group<M: GroupManager + Clone + 'static>(
    group_name: &str,
    pattern_manager: &M,
    model: Option<String>,
    no_tools: bool,
    config: &PatternConfig,
    output: &Output,
) -> Result<GroupSetup> {
    // Load the group from database
    let group = ops::get_group_by_name(&DB, &config.user.id, group_name).await?;
    let group = match group {
        Some(g) => g,
        None => {
            output.error(&format!("Group '{}' not found", group_name));
            return Err(miette::miette!("Group not found"));
        }
    };

    // Create heartbeat channel for agents
    let (heartbeat_sender, heartbeat_receiver) = heartbeat::heartbeat_channel();

    // Create a shared constellation activity tracker for the group
    let group_id_str = group.id.to_string();
    let tracker_memory_id = pattern_core::MemoryId(group_id_str);
    let constellation_tracker = Arc::new(
        pattern_core::constellation_memory::ConstellationActivityTracker::with_memory_id(
            tracker_memory_id,
            100,
        ),
    );

    // Create a shared ToolRegistry for all agents in the group
    let shared_tools = ToolRegistry::new();

    // Load all agents in the group
    tracing::info!("Group has {} members to load", group.members.len());
    let mut agents = Vec::new();
    let mut pattern_agent_index = None;

    // First pass: create all agents WITHOUT data sources
    for (i, (mut agent_record, _membership)) in group.members.clone().into_iter().enumerate() {
        // Load memories and messages for the agent
        load_agent_memories_and_messages(&mut agent_record, output).await?;

        // Check if this is the Pattern agent
        if agent_record.name == "Pattern" {
            pattern_agent_index = Some(i);
        }

        // Create agent - Anchor gets its own tool registry with SystemIntegrityTool
        let agent = if agent_record.name == "Anchor" && !no_tools {
            // Deep clone shared tools for Anchor to have its own registry
            let anchor_tools = shared_tools.deep_clone();

            // Create Anchor agent with its own tools
            let agent = create_agent_from_record_with_tracker(
                agent_record.clone(),
                model.clone(),
                !no_tools,
                config,
                heartbeat_sender.clone(),
                Some(constellation_tracker.clone()),
                output,
                Some(anchor_tools.clone()),
            )
            .await?;

            // Add SystemIntegrityTool only to Anchor's registry
            use pattern_core::tool::builtin::SystemIntegrityTool;
            let handle = agent.handle().await;
            let integrity_tool = SystemIntegrityTool::new(handle);
            anchor_tools.register(integrity_tool);
            output.success(&format!(
                "Emergency halt tool registered for {} agent",
                "Anchor".bright_red()
            ));

            agent
        } else {
            // All other agents use the shared tools
            create_agent_from_record_with_tracker(
                agent_record.clone(),
                model.clone(),
                !no_tools,
                config,
                heartbeat_sender.clone(),
                Some(constellation_tracker.clone()),
                output,
                Some(shared_tools.clone()),
            )
            .await?
        };

        agents.push(agent);
    }

    if agents.is_empty() {
        output.error("No agents in group");
        output.info(
            "Hint:",
            "Add agents with: pattern-cli group add-member <group> <agent>",
        );
        return Err(miette::miette!("No agents in group"));
    }

    if pattern_agent_index.is_none() {
        output.warning("Pattern agent not found in group - Jetstream routing unavailable");
        output.info(
            "Hint:",
            "Add Pattern agent with: pattern-cli group add-member <group> Pattern",
        );
    }

    // Save pattern agent reference
    let pattern_agent = pattern_agent_index.map(|idx| agents[idx].clone());

    // Initialize group chat (registers CLI and Group endpoints)
    let agents_with_membership =
        init_group_chat(&group, agents.clone(), pattern_manager, output).await?;

    Ok(GroupSetup {
        group,
        agents_with_membership,
        pattern_agent,
        shared_tools,
        constellation_tracker,
        heartbeat_sender,
        heartbeat_receiver,
    })
}

/// Initialize group chat and return the agents with membership data
async fn init_group_chat<M: GroupManager + Clone + 'static>(
    group: &AgentGroup,
    agents: Vec<Arc<dyn Agent>>,
    pattern_manager: &M,
    output: &Output,
) -> Result<Vec<AgentWithMembership<Arc<dyn Agent>>>> {
    use pattern_core::coordination::groups::AgentWithMembership;

    // Register CLI endpoint now that we have the output with SharedWriter
    let cli_endpoint = Arc::new(CliEndpoint::new(output.clone()));
    for agent in &agents {
        agent
            .set_default_user_endpoint(cli_endpoint.clone())
            .await?;
    }

    output.status(&format!(
        "Chatting with group '{}'",
        group.name.bright_cyan()
    ));
    output.info("Pattern:", &format!("{:?}", group.coordination_pattern));
    output.info("Members:", &format!("{} agents", agents.len()));
    output.status("Type 'quit' or 'exit' to leave the chat");
    output.status("Use Ctrl+D for multiline input, Enter to send");

    // Wrap agents with their membership data
    let agents_with_membership: Vec<AgentWithMembership<Arc<dyn Agent>>> = agents
        .into_iter()
        .zip(group.members.iter())
        .map(|(agent, (_, membership))| AgentWithMembership {
            agent,
            membership: membership.clone(),
        })
        .collect();

    // Register GroupCliEndpoint for routing group messages
    let group_endpoint = Arc::new(crate::endpoints::GroupCliEndpoint {
        group: group.clone(),
        agents: agents_with_membership.clone(),
        manager: Arc::new(pattern_manager.clone()),
        output: output.clone(),
    });

    // Register the endpoint with each agent's message router
    tracing::info!(
        "Registering group endpoint for {} agents",
        agents_with_membership.len()
    );
    for awm in &agents_with_membership {
        tracing::debug!("Registering group endpoint for agent: {}", awm.agent.name());
        awm.agent
            .register_endpoint("group".to_string(), group_endpoint.clone())
            .await?;
    }
    tracing::info!("✓ Group endpoint registered for all agents");

    Ok(agents_with_membership)
}

/// Run the group chat loop with initialized agents
async fn run_group_chat_loop<M: GroupManager + Clone + 'static>(
    group: AgentGroup,
    agents_with_membership: Vec<AgentWithMembership<Arc<dyn Agent>>>,
    pattern_manager: M,
    mut heartbeat_receiver: heartbeat::HeartbeatReceiver,
    output: Output,
    mut rl: rustyline_async::Readline,
) -> Result<()> {
    use rustyline_async::ReadlineEvent;

    // Clone agents for heartbeat handler
    let agents_for_heartbeat: Vec<Arc<dyn Agent>> = agents_with_membership
        .iter()
        .map(|awm| awm.agent.clone())
        .collect();

    // Spawn heartbeat monitor task
    let output_clone = output.clone();
    tokio::spawn(async move {
        tracing::info!(
            "💓 Heartbeat monitor task started with {} agents",
            agents_for_heartbeat.len()
        );
        for agent in &agents_for_heartbeat {
            tracing::info!(
                "  - Agent available for heartbeat: {} ({})",
                agent.name(),
                agent.id()
            );
        }

        while let Some(heartbeat) = heartbeat_receiver.recv().await {
            tracing::info!(
                "💓 Received heartbeat request from agent {}: tool {} (call_id: {})",
                heartbeat.agent_id,
                heartbeat.tool_name,
                heartbeat.tool_call_id
            );

            // Find the agent that sent the heartbeat
            if let Some(agent) = agents_for_heartbeat
                .iter()
                .find(|a| a.id() == heartbeat.agent_id)
            {
                tracing::info!("✅ Found agent {} for heartbeat", agent.name());
                let agent = agent.clone();
                let output = output_clone.clone();

                // Spawn task to handle this heartbeat
                tokio::spawn(async move {
                    tracing::info!("💓 Processing heartbeat from tool: {}", heartbeat.tool_name);

                    // Create a system message to trigger another turn
                    let heartbeat_message = Message::user(format!(
                        "[System message: Heartbeat continuation from tool: {}]",
                        heartbeat.tool_name
                    ));

                    match agent.process_message_stream(heartbeat_message).await {
                        Ok(mut response_stream) => {
                            while let Some(event) = response_stream.next().await {
                                // Display heartbeat response
                                output.status("💓 Heartbeat continuation:");
                                print_response_event(event, &output);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error processing heartbeat: {}", e);
                        }
                    }
                });
            } else {
                tracing::warn!(
                    "Received heartbeat from unknown agent {}",
                    heartbeat.agent_id
                );
            }
        }
        tracing::debug!("Heartbeat monitor task exiting");
    });

    loop {
        let event = rl.readline().await;
        match event {
            Ok(ReadlineEvent::Line(line)) => {
                if line.trim().is_empty() {
                    continue;
                }

                // Check for slash commands
                if line.trim().starts_with('/') {
                    // Skip for group chat - we don't have slash commands here yet
                    output.error("Slash commands not yet available in group chat");
                    continue;
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
                        Ok(mut stream) => {
                            use tokio_stream::StreamExt;

                            // Process the stream of events
                            while let Some(event) = stream.next().await {
                                print_group_response_event(event, &output, &agents_with_membership)
                                    .await;
                            }
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

pub async fn print_group_response_event(
    event: GroupResponseEvent,
    output: &Output,
    agents_with_membership: &Vec<AgentWithMembership<Arc<dyn Agent>>>,
) {
    match event {
        GroupResponseEvent::Started {
            pattern,
            agent_count,
            ..
        } => {
            output.status(&format!(
                "Starting {} pattern with {} agents",
                pattern, agent_count
            ));
        }
        GroupResponseEvent::AgentStarted {
            agent_name, role, ..
        } => {
            output.status(&format!("Agent {} ({:?}) processing...", agent_name, role));
        }
        GroupResponseEvent::TextChunk {
            agent_id,
            text,
            is_final,
        } => {
            if is_final || !text.is_empty() {
                // Find agent name
                let agent_name = agents_with_membership
                    .iter()
                    .find(|a| a.agent.id() == agent_id)
                    .map(|a| a.agent.name())
                    .unwrap_or("Unknown Agent".to_string());

                output.agent_message(&agent_name, &text);
            }
        }
        GroupResponseEvent::ReasoningChunk {
            agent_id,
            text,
            is_final,
        } => {
            if is_final || !text.is_empty() {
                // Find agent name
                let agent_name = agents_with_membership
                    .iter()
                    .find(|a| a.agent.id() == agent_id)
                    .map(|a| a.agent.name())
                    .unwrap_or("Unknown Agent".to_string());

                output.info(&format!("{} reasoning:", agent_name), &text);
            }
        }
        GroupResponseEvent::ToolCallStarted {
            agent_id,
            fn_name,
            args,
            ..
        } => {
            tracing::debug!(
                "CLI: Received ToolCallStarted {} from agent {}",
                fn_name,
                agent_id
            );

            // Debug: log all agent IDs
            for awm in agents_with_membership {
                tracing::debug!(
                    "Available agent: {} (ID: {})",
                    awm.agent.name(),
                    awm.agent.id()
                );
            }

            let agent_name = agents_with_membership
                .iter()
                .find(|a| a.agent.id() == agent_id)
                .map(|a| a.agent.name())
                .unwrap_or_else(|| {
                    tracing::warn!(
                        "Could not find agent with ID {} in agents_with_membership",
                        agent_id
                    );
                    "Unknown Agent".to_string()
                });

            output.status(&format!("{} calling tool: {}", agent_name, fn_name));
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
        GroupResponseEvent::ToolCallCompleted {
            agent_id: _,
            call_id,
            result,
        } => match result {
            Ok(result) => {
                output.tool_result(&result);
            }
            Err(error) => {
                output.error(&format!("Tool error (call {}): {}", call_id, error));
            }
        },
        GroupResponseEvent::AgentCompleted { agent_name, .. } => {
            output.status(&format!("{} completed", agent_name));
        }
        GroupResponseEvent::Complete { execution_time, .. } => {
            output.info("Execution time:", &format!("{:?}", execution_time));
        }
        GroupResponseEvent::Error {
            agent_id,
            message,
            recoverable,
        } => {
            let prefix = if let Some(agent_id) = agent_id {
                let agent_name = agents_with_membership
                    .iter()
                    .find(|a| a.agent.id() == agent_id)
                    .map(|a| a.agent.name())
                    .unwrap_or("Unknown Agent".to_string());
                format!("{} error", agent_name)
            } else {
                "Group error".to_string()
            };

            if recoverable {
                output.warning(&format!("{}: {}", prefix, message));
            } else {
                output.error(&format!("{}: {}", prefix, message));
            }
        }
    }
}

/// Set up a group chat with Jetstream data routing to the group
/// This creates agents for the group, registers data sources to route to the group,
/// and starts the chat interface
pub async fn chat_with_group_and_jetstream<M: GroupManager + Clone + 'static>(
    group_name: &str,
    pattern_manager: M,
    model: Option<String>,
    no_tools: bool,
    config: &PatternConfig,
) -> Result<()> {
    use rustyline_async::Readline;

    // Create readline and output ONCE here
    let (rl, writer) = Readline::new(format!("{} ", ">".bright_blue())).into_diagnostic()?;

    // Update the global tracing writer to use the SharedWriter
    crate::tracing_writer::set_shared_writer(writer.clone());

    // Create output with SharedWriter for proper concurrent output
    let output = Output::new().with_writer(writer.clone());

    // Use shared setup function
    let group_setup = setup_group(
        group_name,
        &pattern_manager,
        model,
        no_tools,
        config,
        &output,
    )
    .await?;

    let GroupSetup {
        group,
        agents_with_membership,
        pattern_agent,
        shared_tools,
        constellation_tracker: _,
        heartbeat_sender: _,
        heartbeat_receiver,
    } = group_setup;

    // Now that group endpoint is registered, set up data sources if we have Pattern agent
    if let Some(pattern_agent) = pattern_agent {
        if config.bluesky.is_some() {
            output.info("Jetstream:", "Setting up data source routing to group...");

            // Set up data sources with group as target
            let group_target = pattern_core::tool::builtin::MessageTarget {
                target_type: pattern_core::tool::builtin::TargetType::Group,
                target_id: Some(group.id.to_string()),
            };

            // Get the embedding provider
            let embedding_provider = if let Ok((_, embedding_provider, _)) =
                load_model_embedding_providers(None, config, None, true).await
            {
                embedding_provider
            } else {
                None
            };

            // Register data sources NOW (not in a spawn) since group endpoint is ready
            if let Some(embedding_provider) = embedding_provider {
                tracing::info!(
                    "Registering data sources with group target: {:?}",
                    group_target
                );

                // Set up data sources synchronously (not in a spawn)
                let filter = config
                    .bluesky
                    .as_ref()
                    .and_then(|b| b.default_filter.as_ref())
                    .unwrap_or(&BlueskyFilter {
                        exclude_keywords: vec!["patternstop".to_string()],
                        ..Default::default()
                    })
                    .clone();

                tracing::info!("filter: {:?}", filter);

                let mut data_sources = DataSourceBuilder::new()
                    .with_bluesky_source("bluesky_jetstream".to_string(), filter, true)
                    .build_with_target(
                        pattern_agent.id(),
                        pattern_agent.name(),
                        DB.clone(),
                        Some(embedding_provider),
                        Some(pattern_agent.handle().await),
                        get_bluesky_credentials(&config).await,
                        group_target,
                    )
                    .await
                    .map_err(|e| miette::miette!("Failed to build data sources: {}", e))?;

                // Note: We'll register endpoints on the data source's router after creating it

                tracing::info!("Starting Jetstream monitoring...");
                data_sources
                    .start_monitoring("bluesky_jetstream")
                    .await
                    .map_err(|e| miette::miette!("Failed to start monitoring: {}", e))?;

                tracing::info!("✓ Jetstream monitoring started successfully");

                // Register endpoints on the data source's router
                let data_sources_router = data_sources.router();

                // Register the CLI endpoint as default user endpoint
                data_sources_router
                    .register_endpoint(
                        "user".to_string(),
                        Arc::new(CliEndpoint::new(output.clone())),
                    )
                    .await;

                // Register the group endpoint so it can route to the group
                let group_endpoint = Arc::new(crate::endpoints::GroupCliEndpoint {
                    group: group.clone(),
                    agents: agents_with_membership.clone(),
                    manager: Arc::new(pattern_manager.clone()),
                    output: output.clone(),
                });
                data_sources_router
                    .register_endpoint("group".to_string(), group_endpoint)
                    .await;

                shared_tools.register(DataSourceTool::new(Arc::new(RwLock::new(data_sources))));

                output.success("Jetstream routing configured for group with data source tool");
            }
        }
    }

    // Now run the chat loop
    run_group_chat_loop(
        group.clone(),
        agents_with_membership,
        pattern_manager.clone(),
        heartbeat_receiver,
        output.clone(),
        rl,
    )
    .await
}

/// Internal helper that sets up group chat without starting data sources
#[allow(dead_code)]
async fn chat_with_group_internal<M: GroupManager + Clone + 'static>(
    group: AgentGroup,
    agents: Vec<Arc<dyn Agent>>,
    pattern_manager: M,
    heartbeat_receiver: heartbeat::HeartbeatReceiver,
    output: Output,
    rl: rustyline_async::Readline,
) -> Result<()> {
    // Initialize group chat
    let agents_with_membership = init_group_chat(&group, agents, &pattern_manager, &output).await?;

    // Run the chat loop
    run_group_chat_loop(
        group,
        agents_with_membership,
        pattern_manager,
        heartbeat_receiver,
        output,
        rl,
    )
    .await
}

/// Run a Discord bot for group chat with optional concurrent CLI interface
#[cfg(feature = "discord")]
pub async fn run_discord_bot_with_group<M: GroupManager + Clone + 'static>(
    group_name: &str,
    pattern_manager: M,
    model: Option<String>,
    no_tools: bool,
    config: &PatternConfig,
    enable_cli: bool,
) -> Result<()> {
    use pattern_discord::serenity::{Client, all::GatewayIntents};
    use rustyline_async::Readline;

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

    // Check if Discord token is available
    let discord_token = match std::env::var("DISCORD_TOKEN") {
        Ok(token) => token,
        Err(_) => {
            output.error("DISCORD_TOKEN environment variable not set");
            return Err(miette::miette!("Discord token required to run bot"));
        }
    };

    // Use shared setup function
    let group_setup = setup_group(
        group_name,
        &pattern_manager,
        model,
        no_tools,
        config,
        &output,
    )
    .await?;

    let GroupSetup {
        group,
        agents_with_membership,
        pattern_agent,
        shared_tools,
        constellation_tracker: _,
        heartbeat_sender: _,
        mut heartbeat_receiver,
    } = group_setup;

    output.success("Starting Discord bot with group chat...");
    output.info("Group:", &group.name.bright_cyan().to_string());
    output.info("Pattern:", &format!("{:?}", group.coordination_pattern));

    // Set up data sources if we have Pattern agent (similar to jetstream)
    if let Some(ref pattern_agent) = pattern_agent {
        if config.bluesky.is_some() {
            output.info("Bluesky:", "Setting up data source routing to group...");

            // Set up data sources with group as target
            let group_target = pattern_core::tool::builtin::MessageTarget {
                target_type: pattern_core::tool::builtin::TargetType::Group,
                target_id: Some(group.id.to_string()),
            };

            // Get the embedding provider
            let embedding_provider = if let Ok((_, embedding_provider, _)) =
                load_model_embedding_providers(None, config, None, true).await
            {
                embedding_provider
            } else {
                None
            };

            // Register data sources NOW (not in a spawn) since group endpoint is ready
            if let Some(embedding_provider) = embedding_provider {
                // Set up data sources synchronously (not in a spawn)
                let filter = config
                    .bluesky
                    .as_ref()
                    .and_then(|b| b.default_filter.as_ref())
                    .unwrap_or(&BlueskyFilter {
                        exclude_keywords: vec!["patternstop".to_string()],
                        ..Default::default()
                    })
                    .clone();

                let mut data_sources = DataSourceBuilder::new()
                    .with_bluesky_source("bluesky_jetstream".to_string(), filter, true)
                    .build_with_target(
                        pattern_agent.id(),
                        pattern_agent.name(),
                        DB.clone(),
                        Some(embedding_provider),
                        Some(pattern_agent.handle().await),
                        get_bluesky_credentials(&config).await,
                        group_target,
                    )
                    .await
                    .map_err(|e| miette::miette!("Failed to build data sources: {}", e))?;

                data_sources
                    .start_monitoring("bluesky_jetstream")
                    .await
                    .map_err(|e| miette::miette!("Failed to start monitoring: {}", e))?;

                output.success("Data sources configured for group");

                // Register endpoints on the data source's router
                let data_sources_router = data_sources.router();

                // Register the CLI endpoint as default user endpoint
                data_sources_router
                    .register_endpoint(
                        "user".to_string(),
                        Arc::new(CliEndpoint::new(output.clone())),
                    )
                    .await;

                // Register the group endpoint so it can route to the group
                let group_endpoint = Arc::new(crate::endpoints::GroupCliEndpoint {
                    group: group.clone(),
                    agents: agents_with_membership.clone(),
                    manager: Arc::new(pattern_manager.clone()),
                    output: output.clone(),
                });
                data_sources_router
                    .register_endpoint("group".to_string(), group_endpoint)
                    .await;

                shared_tools.register(DataSourceTool::new(Arc::new(RwLock::new(data_sources))));
            }
        }
    }

    // Register Discord endpoint on all agents for Discord responses
    #[cfg(feature = "discord")]
    {
        output.status("Registering Discord endpoint on agents...");

        // Create Discord endpoint with token
        let mut discord_endpoint =
            pattern_discord::endpoints::DiscordEndpoint::new(discord_token.clone());

        // Configure default channel if set
        if let Ok(channel_id) = std::env::var("DISCORD_CHANNEL_ID") {
            if let Ok(channel_id) = channel_id.parse::<u64>() {
                discord_endpoint = discord_endpoint.with_default_channel(channel_id);
                output.info("Discord channel:", &format!("{}", channel_id));
            }
        }

        // Configure default DM user if set
        if let Ok(user_id) = std::env::var("DISCORD_DEFAULT_DM_USER") {
            if let Ok(user_id) = user_id.parse::<u64>() {
                discord_endpoint = discord_endpoint.with_default_dm_user(user_id);
                output.info("Discord DM user:", &format!("{}", user_id));
            }
        }

        let discord_endpoint = Arc::new(discord_endpoint);

        // Register on all agents in the group
        for awm in &agents_with_membership {
            awm.agent
                .register_endpoint("discord".to_string(), discord_endpoint.clone())
                .await?;
        }
        output.success("✓ Discord endpoint registered on all agents");
    }

    // Create the Discord bot in CLI mode
    let bot = DiscordBot::new_cli_mode(
        agents_with_membership.clone(),
        group.clone(),
        Arc::new(pattern_manager.clone()),
    );

    // Build Discord client
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let mut client_builder = Client::builder(&discord_token, intents).event_handler(bot);

    // Set application ID if provided
    if let Ok(app_id) = std::env::var("APP_ID") {
        if let Ok(app_id_u64) = app_id.parse::<u64>() {
            client_builder = client_builder.application_id(app_id_u64.into());
            output.info("App ID:", &format!("Set to {}", app_id));
        } else {
            output.warning(&format!("Invalid APP_ID format: {}", app_id));
        }
    }

    let mut client = client_builder.await.into_diagnostic()?;

    // If CLI is enabled, spawn Discord bot in background and run CLI in foreground
    if enable_cli {
        output.status("Starting Discord bot in background...");
        output.status("CLI interface available. Type 'quit' or 'exit' to stop both.");

        // Spawn Discord bot in background
        let discord_handle = tokio::spawn(async move {
            if let Err(why) = client.start().await {
                tracing::error!("Discord bot error: {:?}", why);
            }
        });

        // Run CLI chat loop in foreground
        if let Some(rl) = rl {
            run_group_chat_loop(
                group.clone(),
                agents_with_membership,
                pattern_manager.clone(),
                heartbeat_receiver,
                output.clone(),
                rl,
            )
            .await?;
        }

        // When CLI exits, also stop Discord bot
        discord_handle.abort();
    } else {
        // Run Discord bot in foreground (blocking)
        output.status("Discord bot starting... Press Ctrl+C to stop.");

        // Spawn a simple heartbeat consumer for Discord-only mode
        // The heartbeats are already processed internally by agents,
        // we just need to consume them to prevent the channel from blocking
        let heartbeat_handle = tokio::spawn(async move {
            while let Some(heartbeat) = heartbeat_receiver.recv().await {
                tracing::debug!(
                    "💓 Consumed heartbeat in Discord-only mode from agent {}: tool {} (call_id: {})",
                    heartbeat.agent_id,
                    heartbeat.tool_name,
                    heartbeat.tool_call_id
                );
                // Heartbeats are processed internally by the agents during their tool execution
                // We just consume them here to keep the channel flowing
            }
            tracing::debug!("Heartbeat receiver closed in Discord-only mode");
        });

        // Run Discord bot
        if let Err(why) = client.start().await {
            output.error(&format!("Discord bot error: {:?}", why));
            heartbeat_handle.abort(); // Clean up heartbeat task
            return Err(miette::miette!("Failed to run Discord bot"));
        }

        // Clean up heartbeat task when Discord bot exits
        heartbeat_handle.abort();
    }

    Ok(())
}
