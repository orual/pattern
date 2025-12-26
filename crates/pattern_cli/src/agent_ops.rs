use miette::{IntoDiagnostic, Result};
use owo_colors::OwoColorize;
use pattern_core::{
    Agent, ModelProvider,
    agent::{AgentRecord, AgentType, DatabaseAgent},
    config::PatternConfig,
    context::heartbeat,
    data_source::{BlueskyFilter, DataSourceBuilder},
    db::{
        client::DB,
        ops::{self},
    },
    embeddings::{EmbeddingProvider, cloud::GeminiEmbedder},
    id::{AgentId, RelationId},
    memory::{Memory, MemoryBlock},
    model::{GenAiClient, ResponseOptions},
    tool::{
        ToolRegistry,
        builtin::{DataSourceTool, MessageTarget},
    },
};
use std::sync::Arc;
use surrealdb::RecordId;
use tokio::sync::RwLock;
use tracing::info;

use crate::{
    data_sources::get_bluesky_credentials, endpoints::setup_bluesky_endpoint, output::Output,
};

/// Build a ContextConfig and CompressionStrategy from an AgentConfig with optional overrides
fn build_context_config(
    agent_config: &pattern_core::config::AgentConfig,
    response_options: Option<&ResponseOptions>,
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

    // If we have model ResponseOptions, set max_context_tokens to the model's
    // input budget: context window minus the model's max output tokens.
    if let Some(opts) = response_options {
        let model = &opts.model_info;
        // Use the response max_tokens if provided, else derive defaults for the model.
        // Guard: ensure we always reserve at least 8192 tokens for output (matching core from_record()).
        let mut reserve_output = opts.max_tokens.map(|t| t as usize).unwrap_or_else(|| {
            pattern_core::model::defaults::calculate_max_tokens(model, None) as usize
        });
        reserve_output = reserve_output.max(8192);

        let input_budget = model.context_window.saturating_sub(reserve_output);
        context_config.max_context_tokens = Some(input_budget);
    }

    (context_config, compression_strategy)
}

/// Load an existing agent from the database or create a new one
pub async fn load_or_create_agent(
    name: &str,
    model_name: Option<String>,
    enable_tools: bool,
    config: &PatternConfig,
    heartbeat_sender: heartbeat::HeartbeatSender,
    output: &crate::output::Output,
) -> Result<Arc<dyn Agent>> {
    let output = output.clone();

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
            Ok(_) => return Err(miette::miette!("Agent not found after query")),
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
        output.print("");

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
        output.print("");

        // Create a new agent
        create_agent(name, model_name, enable_tools, config, heartbeat_sender).await?
    };

    // Set up Bluesky endpoint if configured
    let output = output.clone();
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
    // Load message history and memory blocks in parallel
    let (messages_result, memories_result) = tokio::join!(
        agent_record.load_message_history(&DB, false),
        ops::get_agent_memories(&DB, &agent_record.id)
    );

    // Handle message history result
    agent_record.messages =
        messages_result.map_err(|e| miette::miette!("Failed to load message history: {}", e))?;

    tracing::debug!(
        "After loading message history: {} messages",
        agent_record.messages.len()
    );

    // Handle memory blocks result
    let memory_tuples =
        memories_result.map_err(|e| miette::miette!("Failed to load memory blocks: {}", e))?;

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
    Option<Arc<GeminiEmbedder>>,
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
            let mut endpoints = vec![
                genai::adapter::AdapterKind::Anthropic,
                genai::adapter::AdapterKind::Gemini,
                genai::adapter::AdapterKind::OpenAI,
                genai::adapter::AdapterKind::Groq,
                genai::adapter::AdapterKind::Cohere,
            ];
            if std::env::var("OPENROUTER_API_KEY").is_ok() {
                endpoints.push(genai::adapter::AdapterKind::OpenRouter);
            }
            let genai_client = GenAiClient::with_endpoints(oauth_client, endpoints);
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
        } else if let Some(config_model) = &config.model.model {
            // Try config file model first (so it can override database)
            models
                .iter()
                .find(|m| {
                    let model_lower = config_model.to_lowercase();
                    m.id.to_lowercase().contains(&model_lower)
                        || m.name.to_lowercase().contains(&model_lower)
                })
                .cloned()
        } else if let Some(record) = record
            && let Some(stored_model) = &record.model_id
        {
            // Fall back to the agent's stored model preference
            models.iter().find(|m| &m.id == stored_model).cloned()
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
            // Build a more helpful message depending on auth mode and what's available
            let provider_name = config.model.provider.to_lowercase();
            let available_for_provider: Vec<_> = models
                .iter()
                .filter(|m| m.provider.to_lowercase() == provider_name)
                .map(|m| m.id.as_str())
                .collect();

            #[cfg(feature = "oauth")]
            {
                // If OAuth is enabled and no models are available for Anthropic,
                // suggest logging in instead of only env keys.
                if provider_name == "anthropic" && available_for_provider.is_empty() {
                    return miette::miette!(
                        "No Anthropic models available. Run 'pattern-cli auth login' to authenticate, or set ANTHROPIC_API_KEY in your environment."
                    );
                }
            }

            if let Some(config_model) = &config.model.model {
                if available_for_provider.is_empty() {
                    miette::miette!(
                        "No models available for provider '{}'. Ensure correct authentication (OAuth login or API key).",
                        config.model.provider
                    )
                } else {
                    miette::miette!(
                        "Model '{}' not found for provider '{}'. Available: {:?}",
                        config_model,
                        config.model.provider,
                        available_for_provider
                    )
                }
            } else if available_for_provider.is_empty() {
                miette::miette!(
                    "No models available. Configure OAuth (e.g., 'pattern-cli auth login' for Anthropic) or set provider API keys in .env."
                )
            } else {
                miette::miette!(
                    "No suitable model selected. Available for '{}': {:?}",
                    config.model.provider,
                    available_for_provider
                )
            }
        })?
    };

    info!("Selected model: {} ({})", model_info.name, model_info.id);

    // Create embedding provider if API key is available
    let embedding_provider = if let Ok(api_key) = std::env::var("GEMINI_API_KEY") {
        Some(Arc::new(GeminiEmbedder::new(
            "gemini-embedding-001".to_string(),
            api_key,
            Some(1536),
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
        custom_headers: None,
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

    // Set the chat options with our selected model (do not overwrite max_context_tokens here;
    // DatabaseAgent::from_record already set an appropriate value from provider data)
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

    // Apply context config from the passed PatternConfig
    let (context_config, compression_strategy) =
        build_context_config(&config.agent, Some(&response_options));

    // Log the config being applied
    output.info(
        "Context",
        &format!(
            "max_messages={}, compression={:?}",
            config
                .agent
                .context
                .as_ref()
                .and_then(|c| c.max_messages)
                .unwrap_or(100),
            compression_strategy
                .as_ref()
                .map(|s| format!("{:?}", s))
                .unwrap_or_else(|| "default".to_string())
        ),
    );

    // Update the agent with config values
    agent
        .update_context_config(context_config, compression_strategy)
        .await?;

    // Store the updated config back to the database
    if let Err(e) = agent.store().await {
        output.warning(&format!(
            "Failed to persist config updates to database: {:?}",
            e
        ));
    } else {
        output.success(&format!(
            "Updated {} with config from file and saved to database",
            record.name
        ));
    }

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
            // Block already exists in memory and database - just use it as-is
            tracing::info!(
                "Found existing constellation_activity block with id: {}, skipping update to avoid index conflicts",
                existing.id
            );

            // OLD CODE - causes "Key already present" panic in SurrealDB index:
            // let updated_block =
            //     pattern_core::constellation_memory::create_constellation_activity_block(
            //         existing.id.clone(),
            //         record.owner_id.clone(),
            //         tracker.format_as_memory_content().await,
            //     );
            // agent
            //     .update_memory("constellation_activity", updated_block.clone())
            //     .await?;

            existing
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
                // Found one in database - just use it as-is
                tracing::info!(
                    "Found existing constellation_activity block in database with id: {}, skipping update",
                    existing.id
                );

                // OLD CODE - causes "Key already present" panic:
                // let updated_block =
                //     pattern_core::constellation_memory::create_constellation_activity_block(
                //         existing.id.clone(),
                //         record.owner_id.clone(),
                //         tracker.format_as_memory_content().await,
                //     );
                // agent
                //     .update_memory("constellation_activity", updated_block.clone())
                //     .await?;

                existing
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

    // Load persona from config if present
    output.info("🔍", &format!(
        "Checking for persona in config - agent.persona is_some: {}, agent.persona_path is_some: {}",
        config.agent.persona.is_some(),
        config.agent.persona_path.is_some()
    ));
    if let Some(persona) = &config.agent.persona {
        output.info(
            "📝",
            &format!("Found persona in config: {} chars", persona.len()),
        );
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
                    } else {
                        output.success("✅ Persona updated in memory");
                    }
                } else {
                    output.info("✓", "Persona already up to date in memory");
                }
            }
            Ok(None) | Err(_) => {
                // Create new block
                output.status("Adding persona to agent's core memory...");
                let persona_block = pattern_core::memory::MemoryBlock::owned(
                    config.user.id.clone(),
                    "persona",
                    persona.clone(),
                )
                .with_description("Agent's persona and identity")
                .with_permission(pattern_core::memory::MemoryPermission::Append);

                if let Err(e) = agent.update_memory("persona", persona_block).await {
                    output.warning(&format!("Failed to add persona memory: {}", e));
                } else {
                    output.success("✅ Persona added to memory");
                }
            }
        }
    } else {
        output.info("ℹ", "No persona found in config");
    }

    // Wrap in Arc before calling monitoring methods
    let agent = Arc::new(agent);

    // Start all monitoring tasks in parallel
    let agent_stats = agent.clone();
    let agent_memory = agent.clone();
    let agent_message = agent.clone();

    let (stats_result, memory_result, message_result) = tokio::join!(
        agent_stats.start_stats_sync(),
        agent_memory.start_memory_sync(),
        agent_message.start_message_monitoring()
    );

    stats_result?;
    memory_result?;
    message_result?;

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
        crate::discord::setup_discord_endpoint(&agent_dyn, config, output)
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

    // Only register Bluesky monitoring for the supervisor role agent
    if agent_is_supervisor(&agent, &config).await {
        tracing::info!("Setting up Bluesky monitoring for supervisor agent");
        tokio::spawn(async move {
            tracing::info!("Inside Bluesky setup spawn for supervisor agent");
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
            let data_sources = if let Some(target) = target {
                DataSourceBuilder::new()
                    .with_custom_bluesky_source(
                        "bluesky_jetstream".to_string(),
                        config
                            .bluesky
                            .as_ref()
                            .map(|b| b.jetstream_endpoint.clone())
                            .unwrap_or(
                                "wss://jetstream1.us-east.fire.hose.cam/subscribe".to_string(),
                            ),
                        filter,
                        true,
                    )
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
                    .with_custom_bluesky_source(
                        "bluesky_jetstream".to_string(),
                        config
                            .bluesky
                            .as_ref()
                            .map(|b| b.jetstream_endpoint.clone())
                            .unwrap_or(
                                "wss://jetstream1.us-east.fire.hose.cam/subscribe".to_string(),
                            ),
                        filter,
                        true,
                    )
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

            tracing::info!("About to start monitoring bluesky_jetstream");
            data_sources
                .start_monitoring("bluesky_jetstream")
                .await
                .unwrap();
            tracing::info!("Successfully started monitoring bluesky_jetstream");
            tools.register(DataSourceTool::new(Arc::new(data_sources)));
        });
    }
}

/// Determine if this agent should act as the group's supervisor based on
/// database group memberships or, as a fallback, on the config file.
async fn agent_is_supervisor<M, E>(agent: &Arc<DatabaseAgent<M, E>>, config: &PatternConfig) -> bool
where
    E: EmbeddingProvider + Clone + 'static,
    M: ModelProvider + 'static,
{
    // First, check DB memberships for Supervisor role
    match pattern_core::db::ops::get_agent_memberships(&DB, &agent.id()).await {
        Ok(memberships) => {
            if memberships.iter().any(|m| {
                matches!(
                    m.role,
                    pattern_core::coordination::types::GroupMemberRole::Supervisor
                )
            }) {
                return true;
            }
        }
        Err(e) => {
            tracing::warn!("Failed to load memberships for agent {}: {}", agent.id(), e);
        }
    }

    // Fallback: Check config groups for a supervisor assignment referencing this agent
    let agent_id = agent.id();
    let agent_name = agent.name();
    for group in &config.groups {
        for member in &group.members {
            let id_matches = member
                .agent_id
                .as_ref()
                .map(|id| id == &agent_id)
                .unwrap_or(false);
            let name_matches = member.name == agent_name;
            if (id_matches || name_matches)
                && matches!(
                    member.role,
                    pattern_core::config::GroupMemberRoleConfig::Supervisor
                )
            {
                return true;
            }
        }
    }

    false
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
    let (context_config, compression_strategy) =
        build_context_config(&config.agent, Some(&response_options));

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
            output.print("");
        }
        Err(e) => {
            output.warning(&format!("Failed to save agent to database: {}", e));
            output.status("Agent will work for this session but won't persist");
            output.print("");
        }
    }

    // Wrap in Arc before calling monitoring methods
    let agent = Arc::new(agent);
    agent.clone().start_stats_sync().await?;
    agent.clone().start_memory_sync().await?;
    agent.clone().start_message_monitoring().await?;

    // Update memory blocks from config only if they don't exist
    // First check persona
    if let Some(persona) = &config.agent.persona {
        output.info(
            "📝",
            &format!("Found persona in config: {} chars", persona.len()),
        );
        match agent.get_memory("persona").await {
            Ok(Some(_existing)) => {
                // Persona already exists - DO NOT overwrite from config
                // Config values are only for initial setup, not updates
                output.info(
                    "✓",
                    "Persona already exists in memory, preserving database values",
                );
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
                } else {
                    output.success("✅ Persona added to memory");
                }
            }
        }
    } else {
        output.info("ℹ", "No persona found in config");
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
                // Memory already exists - preserve content but update permissions/type from config
                let mut needs_update = false;

                if existing.memory_type != block_config.memory_type {
                    output.info(
                        "Updating memory type",
                        &format!(
                            "{:?} -> {:?}",
                            existing.memory_type, block_config.memory_type
                        ),
                    );
                    existing.memory_type = block_config.memory_type;
                    needs_update = true;
                }

                if existing.permission != block_config.permission {
                    output.info(
                        "Updating permission",
                        &format!("{:?} -> {:?}", existing.permission, block_config.permission),
                    );
                    existing.permission = block_config.permission;
                    needs_update = true;
                }

                if let Some(desc) = &block_config.description {
                    if existing.description.as_ref() != Some(desc) {
                        existing.description = Some(desc.clone());
                        needs_update = true;
                    }
                }

                if needs_update {
                    if let Err(e) = agent.update_memory(label, existing).await {
                        output
                            .warning(&format!("Failed to update memory block '{}': {}", label, e));
                    } else {
                        output.success(&format!("✅ Updated permissions/type for '{}'", label));
                    }
                } else {
                    output.info("✓", &format!("Memory block '{}' is up to date", label));
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

                            // Update permission and type but preserve content
                            let mut needs_update = false;
                            let mut updated_memory = existing_memory.clone();

                            if existing_memory.memory_type != block_config.memory_type {
                                output.info(
                                    "Updating shared memory type",
                                    &format!(
                                        "{:?} -> {:?}",
                                        existing_memory.memory_type, block_config.memory_type
                                    ),
                                );
                                updated_memory.memory_type = block_config.memory_type;
                                needs_update = true;
                            }

                            if existing_memory.permission != block_config.permission {
                                output.info(
                                    "Updating shared memory permission",
                                    &format!(
                                        "{:?} -> {:?}",
                                        existing_memory.permission, block_config.permission
                                    ),
                                );
                                updated_memory.permission = block_config.permission;
                                needs_update = true;
                            }

                            if let Some(desc) = &block_config.description {
                                if updated_memory.description.as_ref() != Some(desc) {
                                    updated_memory.description = Some(desc.clone());
                                    needs_update = true;
                                }
                            }

                            // Update the memory block itself if needed (preserving content)
                            if needs_update {
                                // Preserve existing content, don't overwrite with config content
                                if let Err(e) = pattern_core::db::ops::update_memory_content(
                                    &DB,
                                    updated_memory.id.clone(),
                                    updated_memory.value.clone(), // Keep existing content
                                    embedding_provider.as_ref().map(|p| p.as_ref()),
                                )
                                .await
                                {
                                    output.warning(&format!(
                                        "Failed to update shared memory '{}': {}",
                                        label, e
                                    ));
                                } else {
                                    output.success(&format!(
                                        "✅ Updated permissions/type for shared '{}'",
                                        label
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

/// Load or create an agent from a group member configuration
pub async fn load_or_create_agent_from_member(
    member: &pattern_core::config::GroupMemberConfig,
    user_id: &pattern_core::id::UserId,
    model_name: Option<String>,
    enable_tools: bool,
    heartbeat_sender: heartbeat::HeartbeatSender,
    main_config: Option<&PatternConfig>,
    out: &crate::output::Output,
) -> Result<Arc<dyn Agent>> {
    let output = out.clone();

    // If member has an agent_id, load that agent; if missing, create it using provided config
    if let Some(orig_agent_id) = &member.agent_id {
        // Normalize potential "agent:<uuid>" strings from configs into raw key form
        let mut agent_id = orig_agent_id.clone();
        if let Some((prefix, key)) = agent_id.0.split_once(':') {
            if prefix != "agent" {
                output.warning(&format!(
                    "Agent ID prefix '{}' unexpected; expected 'agent'. Using key '{}'",
                    prefix, key
                ));
            }
            agent_id.0 = key.to_string();
        }
        output.status(&format!(
            "Loading existing agent {} for group member",
            agent_id.to_string().dimmed()
        ));

        // Load the existing agent
        let agent_record = match AgentRecord::load_with_relations(&DB, &agent_id).await {
            Ok(Some(agent)) => Some(agent),
            Ok(None) => {
                output.warning(&format!(
                    "Agent {} not found. Creating it from config...",
                    agent_id
                ));

                // Choose a base agent config: prefer config_path, then inline, otherwise minimal
                let mut agent_cfg = if let Some(config_path) = &member.config_path {
                    match pattern_core::config::AgentConfig::load_from_file(config_path).await {
                        Ok(cfg) => cfg,
                        Err(e) => {
                            output.warning(&format!(
                                "Failed to load config from {}: {}. Using minimal config.",
                                config_path.display(),
                                e
                            ));
                            pattern_core::config::AgentConfig {
                                id: None,
                                name: member.name.clone(),
                                system_prompt: None,
                                system_prompt_path: None,
                                persona: None,
                                persona_path: None,
                                instructions: None,
                                memory: Default::default(),
                                bluesky_handle: main_config
                                    .and_then(|cfg| cfg.agent.bluesky_handle.clone()),
                                tool_rules: Vec::new(),
                                tools: Vec::new(),
                                model: None,
                                context: None,
                            }
                        }
                    }
                } else if let Some(inline) = &member.agent_config {
                    let mut cfg = inline.clone();
                    if cfg.bluesky_handle.is_none() {
                        if let Some(main_cfg) = main_config {
                            cfg.bluesky_handle = main_cfg.agent.bluesky_handle.clone();
                        }
                    }
                    cfg
                } else {
                    pattern_core::config::AgentConfig {
                        id: None,
                        name: member.name.clone(),
                        system_prompt: None,
                        system_prompt_path: None,
                        persona: None,
                        persona_path: None,
                        instructions: None,
                        memory: Default::default(),
                        bluesky_handle: main_config
                            .and_then(|cfg| cfg.agent.bluesky_handle.clone()),
                        tool_rules: Vec::new(),
                        tools: Vec::new(),
                        model: None,
                        context: None,
                    }
                };

                // Force the desired ID into the agent config so creation uses it
                agent_cfg.id = Some(agent_id.clone());

                // Pick a model config: inline > main > default
                let model_cfg = if let Some(m) = &agent_cfg.model {
                    m.clone()
                } else if let Some(main_cfg) = main_config {
                    main_cfg.model.clone()
                } else {
                    pattern_core::config::ModelConfig {
                        provider: "Gemini".to_string(),
                        model: model_name.clone(),
                        temperature: None,
                        settings: Default::default(),
                    }
                };

                // Build a temporary PatternConfig for creation
                let tmp_cfg = PatternConfig {
                    user: pattern_core::config::UserConfig {
                        id: user_id.clone(),
                        name: None,
                        settings: Default::default(),
                    },
                    agent: agent_cfg,
                    model: model_cfg,
                    database: Default::default(),
                    groups: vec![],
                    bluesky: None,
                    discord: None,
                };

                // Create the agent with the specified ID
                // Create and note if an agent with the same name exists under a different id
                let check_name = tmp_cfg.agent.name.clone();
                if let Ok(mut resp) = DB
                    .query("SELECT id FROM agent WHERE name = $name LIMIT 1")
                    .bind(("name", check_name))
                    .await
                    .into_diagnostic()
                {
                    if let Ok(existing_ids) = resp
                        .take::<Vec<surrealdb::RecordId>>("id")
                        .into_diagnostic()
                    {
                        if let Some(existing) = existing_ids.first() {
                            let existing_id = AgentId::from_record(existing.clone());
                            if existing_id != agent_id {
                                output.warning(&format!(
                                    "An agent named '{}' already exists with a different id ({}). Creating new agent with specified id ({}).",
                                    tmp_cfg.agent.name,
                                    existing_id,
                                    agent_id
                                ));
                            }
                        }
                    }
                }

                let _created = load_or_create_agent(
                    &tmp_cfg.agent.name,
                    tmp_cfg.agent.model.as_ref().and_then(|m| m.model.clone()),
                    enable_tools,
                    &tmp_cfg,
                    heartbeat_sender.clone(),
                    &output,
                )
                .await?;

                // Load the freshly created AgentRecord for downstream init
                match AgentRecord::load_with_relations(&DB, &agent_id).await {
                    Ok(Some(agent)) => Some(agent),
                    other => {
                        return Err(miette::miette!(
                            "Failed to load newly created agent {}: {:?}",
                            agent_id,
                            other
                        ));
                    }
                }
            }
            Err(e) => {
                output.warning(&format!(
                    "Failed to load agent {}: {}. Creating it from config...",
                    agent_id, e
                ));

                // Choose a base agent config: prefer config_path, then inline, otherwise minimal
                let mut agent_cfg = if let Some(config_path) = &member.config_path {
                    match pattern_core::config::AgentConfig::load_from_file(config_path).await {
                        Ok(cfg) => cfg,
                        Err(e) => {
                            output.warning(&format!(
                                "Failed to load config from {}: {}. Using minimal config.",
                                config_path.display(),
                                e
                            ));
                            pattern_core::config::AgentConfig {
                                id: None,
                                name: member.name.clone(),
                                system_prompt: None,
                                system_prompt_path: None,
                                persona: None,
                                persona_path: None,
                                instructions: None,
                                memory: Default::default(),
                                bluesky_handle: main_config
                                    .and_then(|cfg| cfg.agent.bluesky_handle.clone()),
                                tool_rules: Vec::new(),
                                tools: Vec::new(),
                                model: None,
                                context: None,
                            }
                        }
                    }
                } else if let Some(inline) = &member.agent_config {
                    let mut cfg = inline.clone();
                    if cfg.bluesky_handle.is_none() {
                        if let Some(main_cfg) = main_config {
                            cfg.bluesky_handle = main_cfg.agent.bluesky_handle.clone();
                        }
                    }
                    cfg
                } else {
                    pattern_core::config::AgentConfig {
                        id: None,
                        name: member.name.clone(),
                        system_prompt: None,
                        system_prompt_path: None,
                        persona: None,
                        persona_path: None,
                        instructions: None,
                        memory: Default::default(),
                        bluesky_handle: main_config
                            .and_then(|cfg| cfg.agent.bluesky_handle.clone()),
                        tool_rules: Vec::new(),
                        tools: Vec::new(),
                        model: None,
                        context: None,
                    }
                };

                // Force the desired ID into the agent config so creation uses it
                agent_cfg.id = Some(agent_id.clone());

                // Pick a model config: inline > main > default
                let model_cfg = if let Some(m) = &agent_cfg.model {
                    m.clone()
                } else if let Some(main_cfg) = main_config {
                    main_cfg.model.clone()
                } else {
                    pattern_core::config::ModelConfig {
                        provider: "Gemini".to_string(),
                        model: model_name.clone(),
                        temperature: None,
                        settings: Default::default(),
                    }
                };

                // Build a temporary PatternConfig for creation
                let tmp_cfg = PatternConfig {
                    user: pattern_core::config::UserConfig {
                        id: user_id.clone(),
                        name: None,
                        settings: Default::default(),
                    },
                    agent: agent_cfg,
                    model: model_cfg,
                    database: Default::default(),
                    groups: vec![],
                    bluesky: None,
                    discord: None,
                };

                // Create the agent with the specified ID
                let _created = load_or_create_agent(
                    &tmp_cfg.agent.name,
                    tmp_cfg.agent.model.as_ref().and_then(|m| m.model.clone()),
                    enable_tools,
                    &tmp_cfg,
                    heartbeat_sender.clone(),
                    &output,
                )
                .await?;

                // Load the freshly created AgentRecord for downstream init
                match AgentRecord::load_with_relations(&DB, &agent_id).await {
                    Ok(Some(agent)) => Some(agent),
                    other => {
                        return Err(miette::miette!(
                            "Failed to load newly created agent {}: {:?}",
                            agent_id,
                            other
                        ));
                    }
                }
            }
        };

        let mut agent_record = match agent_record {
            Some(a) => a,
            None => return Err(miette::miette!("Agent {} could not be created", agent_id)),
        };

        // Load memories and messages
        load_agent_memories_and_messages(&mut agent_record, &output).await?;

        // Create runtime agent from record
        // Build a config, but also check if we have a config_path to load missing blocks from
        let bluesky_handle = main_config.and_then(|cfg| cfg.agent.bluesky_handle.clone());

        // If member also has a config_path, load it for any missing blocks (like persona)
        let agent_config = if let Some(config_path) = &member.config_path {
            output.status(&format!(
                "Loading config from {} for missing memory blocks",
                config_path.display().bright_cyan()
            ));
            match pattern_core::config::AgentConfig::load_from_file(config_path).await {
                Ok(cfg) => cfg,
                Err(e) => {
                    output.warning(&format!(
                        "Failed to load config from {}: {}",
                        config_path.display(),
                        e
                    ));
                    // Fall back to minimal config
                    pattern_core::config::AgentConfig {
                        id: Some(agent_id.clone()),
                        name: agent_record.name.clone(),
                        system_prompt: None,
                        system_prompt_path: None,
                        persona: None,
                        persona_path: None,
                        instructions: None,
                        memory: Default::default(),
                        bluesky_handle: bluesky_handle.clone(),
                        tool_rules: Vec::new(),
                        tools: Vec::new(),
                        model: None,
                        context: None,
                    }
                }
            }
        } else {
            // No config_path, use minimal config
            pattern_core::config::AgentConfig {
                id: Some(agent_id.clone()),
                name: agent_record.name.clone(),
                system_prompt: None,
                system_prompt_path: None,
                persona: None,
                persona_path: None,
                instructions: None,
                memory: Default::default(),
                bluesky_handle: bluesky_handle.clone(),
                tool_rules: Vec::new(),
                tools: Vec::new(),
                model: None,
                context: None,
            }
        };

        let config = PatternConfig {
            user: pattern_core::config::UserConfig {
                id: user_id.clone(),
                name: None,
                settings: Default::default(),
            },
            agent: agent_config,
            model: pattern_core::config::ModelConfig {
                provider: "Gemini".to_string(),
                model: model_name,
                temperature: None,
                settings: Default::default(),
            },
            database: Default::default(),
            groups: vec![],
            bluesky: None,
            discord: None,
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
            discord: None,
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
            &output,
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
            discord: None,
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
            &output,
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
            system_prompt_path: None,
            persona: None,
            persona_path: None,
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
        discord: None,
    };

    create_agent(&member.name, None, enable_tools, &config, heartbeat_sender).await
}
