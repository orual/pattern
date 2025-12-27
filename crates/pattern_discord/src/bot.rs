use serenity::{
    async_trait,
    client::{Context, EventHandler},
    model::{
        application::{Command, Interaction},
        channel::Message,
        gateway::Ready,
        id::ChannelId,
    },
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use futures::StreamExt;
use pattern_core::db::ConstellationDatabases;
use pattern_core::message::Message as PatternMessage;
use pattern_core::realtime::{GroupEventContext, GroupEventSink, tap_group_stream};
use pattern_core::{
    Agent, AgentGroup,
    coordination::groups::{AgentWithMembership, GroupManager},
};
use serenity::all::MessageId;
use serenity::builder::GetMessages;

use std::collections::{HashMap, VecDeque};
use tokio::sync::Mutex;

/// Buffered reaction for batch processing
#[derive(Debug, Clone)]
struct BufferedReaction {
    emoji: String,
    user_name: String,
    message_preview: String,
    #[allow(dead_code)]
    channel_id: u64,
    timestamp: std::time::Instant,
}

/// Queued message for processing
#[derive(Debug, Clone)]
struct QueuedMessage {
    msg_id: u64,
    channel_id: u64,
    author_name: String,
    content: String,
    timestamp: std::time::Instant,
}

/// The main Discord bot that handles all Discord interactions
#[derive(Clone)]
pub struct DiscordBot {
    /// Whether we're in CLI mode (single user, no database)
    cli_mode: bool,
    /// Agents with membership data for CLI mode
    agents_with_membership: Option<Vec<AgentWithMembership<Arc<dyn Agent>>>>,
    /// Group for CLI mode
    group: Option<AgentGroup>,
    /// Group manager for CLI mode
    group_manager: Option<Arc<dyn GroupManager>>,

    /// Bot configuration
    config: DiscordBotConfig,

    /// Database connections for constellation data access
    dbs: Option<Arc<ConstellationDatabases>>,

    /// Buffer for reactions to batch process
    reaction_buffer: Arc<Mutex<VecDeque<BufferedReaction>>>,
    /// Whether we're currently processing a message
    is_processing: Arc<Mutex<bool>>,
    /// Last message time for debouncing
    last_message_time: Arc<Mutex<std::time::Instant>>,
    /// Queue of messages to process
    message_queue: Arc<Mutex<VecDeque<QueuedMessage>>>,
    /// Currently processing message ID (for reply attachment)
    current_message_id: Arc<Mutex<Option<u64>>>,
    /// When we started processing the current message
    current_message_start: Arc<Mutex<Option<std::time::Instant>>>,
    /// Handle for typing indicator task
    typing_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Track status reactions we've added (message_id -> reaction)
    status_reactions: Arc<Mutex<HashMap<u64, char>>>,
    /// Debounced queue flush task (reset when new messages arrive while busy)
    queue_flush_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Per-channel recent activity timestamps to decide when to include history
    recent_activity_by_channel: Arc<Mutex<HashMap<u64, std::time::Instant>>>,

    /// Optional sinks to mirror group events (e.g., CLI printer, file)
    group_event_sinks: Option<Vec<Arc<dyn GroupEventSink>>>,

    /// Cached bot user ID (set on Ready)
    bot_user_id: Arc<Mutex<Option<u64>>>,

    /// restart channel sender
    restart_ch: tokio::sync::mpsc::Sender<()>,
}

/// Re-export DiscordBotConfig from pattern_auth.
/// This is the canonical configuration type for the Discord bot.
/// Use `DiscordBotConfig::from_env()` to load from environment variables,
/// or `AuthDb::get_discord_bot_config()` to load from the database.
pub use pattern_auth::DiscordBotConfig;

impl DiscordBot {
    /// Expose read-only access to bot configuration
    pub fn config(&self) -> &DiscordBotConfig {
        &self.config
    }
    /// Create a new Discord bot for CLI mode
    pub fn new_cli_mode(
        config: DiscordBotConfig,
        agents_with_membership: Vec<AgentWithMembership<Arc<dyn Agent>>>,
        group: AgentGroup,
        group_manager: Arc<dyn GroupManager>,
        group_event_sinks: Option<Vec<Arc<dyn GroupEventSink>>>,
        restart_ch: tokio::sync::mpsc::Sender<()>,
        dbs: Option<Arc<ConstellationDatabases>>,
    ) -> Self {
        Self {
            cli_mode: true,
            agents_with_membership: Some(agents_with_membership),
            group: Some(group),
            group_manager: Some(group_manager),
            config,
            dbs,
            reaction_buffer: Arc::new(Mutex::new(VecDeque::new())),
            is_processing: Arc::new(Mutex::new(false)),
            last_message_time: Arc::new(Mutex::new(std::time::Instant::now())),
            message_queue: Arc::new(Mutex::new(VecDeque::new())),
            current_message_id: Arc::new(Mutex::new(None)),
            current_message_start: Arc::new(Mutex::new(None)),
            typing_handle: Arc::new(Mutex::new(None)),
            status_reactions: Arc::new(Mutex::new(HashMap::new())),
            queue_flush_task: Arc::new(Mutex::new(None)),
            recent_activity_by_channel: Arc::new(Mutex::new(HashMap::new())),
            group_event_sinks,
            bot_user_id: Arc::new(Mutex::new(None)),
            restart_ch,
        }
    }

    /// Create a new Discord bot for full mode (with database)
    pub fn new_full_mode(
        config: DiscordBotConfig,
        restart_ch: tokio::sync::mpsc::Sender<()>,
        dbs: Arc<ConstellationDatabases>,
    ) -> Self {
        Self {
            cli_mode: false,
            agents_with_membership: None,
            group: None,
            group_manager: None,
            config,
            dbs: Some(dbs),
            reaction_buffer: Arc::new(Mutex::new(VecDeque::new())),
            is_processing: Arc::new(Mutex::new(false)),
            last_message_time: Arc::new(Mutex::new(std::time::Instant::now())),
            message_queue: Arc::new(Mutex::new(VecDeque::new())),
            current_message_id: Arc::new(Mutex::new(None)),
            current_message_start: Arc::new(Mutex::new(None)),
            typing_handle: Arc::new(Mutex::new(None)),
            status_reactions: Arc::new(Mutex::new(HashMap::new())),
            queue_flush_task: Arc::new(Mutex::new(None)),
            recent_activity_by_channel: Arc::new(Mutex::new(HashMap::new())),
            group_event_sinks: None,
            bot_user_id: Arc::new(Mutex::new(None)),
            restart_ch,
        }
    }
}

/// Event handler wrapper that holds a reference to the bot
pub struct DiscordEventHandler {
    bot: Arc<DiscordBot>,
}

impl DiscordEventHandler {
    pub fn new(bot: Arc<DiscordBot>) -> Self {
        Self { bot }
    }
}

// Safe Unicode-aware preview helper
fn unicode_preview(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let preview: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        format!("{}...", preview)
    } else {
        preview
    }
}

#[async_trait]
impl EventHandler for DiscordEventHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        debug!("{} is connected!", ready.user.name);
        debug!("Bot user ID: {}", ready.user.id);

        // Cache our bot user ID for later mention resolution
        self.bot
            .bot_user_id
            .lock()
            .await
            .replace(ready.user.id.get());

        let commands = crate::slash_commands::create_commands();

        for command in commands {
            match Command::create_global_command(&ctx.http, command).await {
                Ok(cmd) => {
                    debug!("Registered command: {}", cmd.name);
                }
                Err(e) => {
                    error!("Failed to register command: {}", e);
                }
            }
        }

        // Spawn permission request announcer (DM admin(s) and/or post in configured channel(s))
        let http = ctx.http.clone();
        let cfg = self.bot.config.clone();
        tokio::spawn(async move {
            use pattern_core::permission::broker;
            use serenity::all::{ChannelId, UserId};
            let mut rx = broker().subscribe();
            // Resolve recipients from config
            let admin_ids: Vec<u64> = cfg
                .admin_users
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| s.parse::<u64>().ok())
                .collect();
            let channel_ids: Vec<u64> = cfg
                .allowed_channels
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| s.parse::<u64>().ok())
                .collect();

            while let Ok(req) = rx.recv().await {
                let title = format!("🔐 Permission Needed: {}", req.tool_name);
                let scope = format!("scope: {:?}", req.scope);
                let tip = format!(
                    "Use /permit {} [once|always|ttl=600] or /deny {}",
                    req.id, req.id
                );
                let body = if let Some(reason) = req.reason.clone() {
                    format!("{}\n{}\nreason: {}", title, scope, reason)
                } else {
                    format!("{}\n{}", title, scope)
                };
                let content = format!("{}\n{}", body, tip);

                // Prefer request-scoped discord_channel_id if present
                let mut sent = false;
                if let Some(meta) = &req.metadata {
                    if let Some(cid) = meta.get("discord_channel_id").and_then(|v| v.as_u64()) {
                        let _ = ChannelId::new(cid).say(&http, content.clone()).await.ok();
                        sent = true;
                    }
                }
                if !sent {
                    // Try DM to each configured admin
                    for uid in &admin_ids {
                        if let Ok(channel) = UserId::new(*uid).create_dm_channel(&http).await {
                            let _ = channel.say(&http, content.clone()).await;
                            sent = true; // consider success if at least one DM succeeds
                        }
                    }
                }
                if !sent {
                    // Post to all configured channels
                    for cid in &channel_ids {
                        if !sent {
                            let _ = ChannelId::new(*cid).say(&http, content.clone()).await.ok();
                            sent = true;
                        }
                    }
                }
            }
        });
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Ignore bot's own messages
        if msg.author.bot {
            return;
        }

        // Log message context for debugging
        info!(
            "Received message - Guild: {:?}, Channel: {}, Author: {} ({}), Content length: {}",
            msg.guild_id,
            msg.channel_id,
            msg.author.name,
            msg.author.id,
            msg.content.len()
        );

        // Check if this is a thread and log it
        if let Ok(channel) = msg.channel_id.to_channel(&ctx).await {
            match channel {
                serenity::model::channel::Channel::Guild(guild_channel) => {
                    if guild_channel.thread_metadata.is_some() {
                        info!(
                            "Message is in a thread: {} (parent: {:?})",
                            guild_channel.name, guild_channel.parent_id
                        );
                    }
                }
                _ => {}
            }
        }

        // Check if we should respond
        let should_respond = {
            let is_dm = msg.guild_id.is_none();
            let is_mention = msg.mentions_me(&ctx.http).await.unwrap_or(false);

            // If allowed guilds are configured, restrict responses to those guilds (DMs unaffected)
            let guild_ok = if let (Some(gid), Some(list)) =
                (msg.guild_id, self.bot.config.allowed_guilds.as_ref())
            {
                list.contains(&gid.get().to_string()) || is_dm
            } else {
                true
            };

            // In CLI mode with a configured channel, respond to all messages in that channel
            if self.bot.cli_mode {
                if let Some(ref allowed) = self.bot.config.allowed_channels {
                    if allowed.contains(&msg.channel_id.get().to_string()) && guild_ok {
                        true
                    } else {
                        guild_ok && (is_dm || is_mention)
                    }
                } else {
                    guild_ok && (is_dm || is_mention)
                }
            } else {
                // Otherwise respond to DMs and mentions
                guild_ok && (is_dm || is_mention)
            }
        };

        if !should_respond {
            return;
        }

        // Check if we're currently processing a message
        let is_busy = *self.bot.is_processing.lock().await;

        if is_busy {
            // Simplified queueing: keep at most one pending entry per channel, replacing with newest
            let mut queue = self.bot.message_queue.lock().await;

            if let Some(existing) = queue
                .iter_mut()
                .find(|q| q.channel_id == msg.channel_id.get())
            {
                info!(
                    "Updating existing queued entry for channel {} with latest message from {}",
                    msg.channel_id, msg.author.name
                );
                existing.msg_id = msg.id.get();
                existing.author_name = msg.author.name.clone();
                existing.content = msg.content.clone();
                existing.timestamp = std::time::Instant::now();
            } else {
                info!(
                    "Queueing single pending message for channel {} from {}",
                    msg.channel_id, msg.author.name
                );
                queue.push_back(QueuedMessage {
                    msg_id: msg.id.get(),
                    channel_id: msg.channel_id.get(),
                    author_name: msg.author.name.clone(),
                    content: msg.content.clone(),
                    timestamp: std::time::Instant::now(),
                });
            }

            // Simple queued indicator
            if msg.react(&ctx.http, '📥').await.is_ok() {
                let mut reactions = self.bot.status_reactions.lock().await;
                reactions.insert(msg.id.get(), '📥');
            }

            // Debounced flush: reset a single 5s timer for the whole queue
            {
                let mut task = self.bot.queue_flush_task.lock().await;
                if let Some(handle) = task.take() {
                    handle.abort();
                }
                let bot = self.bot.clone();
                let ctx_clone = ctx.clone();
                *task = Some(tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    // On timer fire, attempt to flush queued messages
                    bot.process_message_queue(&ctx_clone).await;
                }));
            }
            return;
        }

        // Show typing indicator
        let _ = msg.channel_id.broadcast_typing(&ctx.http).await;

        // Process the message
        if let Err(e) = self.bot.process_message(&ctx, &msg).await {
            error!("Error processing message: {}", e);
            let _ = msg
                .channel_id
                .say(
                    &ctx.http,
                    "Sorry, I encountered an error processing your message.",
                )
                .await;
        }
    }

    async fn reaction_add(&self, ctx: Context, reaction: serenity::model::channel::Reaction) {
        // Skip bot's own reactions
        if let Some(user_id) = reaction.user_id {
            let cached = { *self.bot.bot_user_id.lock().await };
            if let Some(bot_id) = cached {
                if user_id.get() == bot_id {
                    return;
                }
            } else if let Ok(current_user) = ctx.http.get_current_user().await {
                // Cache for future events
                *self.bot.bot_user_id.lock().await = Some(current_user.id.get());
                if user_id == current_user.id {
                    return;
                }
            }
        }

        // Log reaction for debugging
        debug!(
            "Reaction added: {} on message {} by user {:?}",
            reaction.emoji, reaction.message_id, reaction.user_id
        );

        // Get the original message to see if it was from our bot
        if let Ok(msg) = ctx
            .http
            .get_message(reaction.channel_id, reaction.message_id)
            .await
        {
            debug!(
                "Retrieved message for reaction - author: {}, bot check starting",
                msg.author.name
            );

            // Check if the message was from our bot
            if let Ok(current_user) = ctx.http.get_current_user().await {
                debug!(
                    "Current bot user: {}, message author: {}",
                    current_user.name, msg.author.name
                );

                if msg.author.id == current_user.id {
                    // Check if we should process reactions from this channel
                    let should_process = if self.bot.cli_mode {
                        if let Some(ref allowed) = self.bot.config.allowed_channels {
                            // Only process reactions in the configured channels or DMs
                            allowed.contains(&reaction.channel_id.get().to_string())
                                || msg.guild_id.is_none()
                        } else {
                            // No channels configured, only process DMs
                            msg.guild_id.is_none()
                        }
                    } else {
                        // In non-CLI mode, only process DMs
                        msg.guild_id.is_none()
                    };

                    if !should_process {
                        info!(
                            "Ignoring reaction from channel {} (not in allowed channels)",
                            reaction.channel_id
                        );
                        return;
                    }

                    info!("Reaction is on bot's message in allowed channel - processing");
                    // Someone reacted to our bot's message
                    // Get the user who reacted
                    if let Some(user_id) = reaction.user_id {
                        if let Ok(user) = ctx.http.get_user(user_id).await {
                            // Check if we're currently processing
                            let is_busy = *self.bot.is_processing.lock().await;

                            if is_busy {
                                // Buffer the reaction for later
                                let mut buffer = self.bot.reaction_buffer.lock().await;
                                buffer.push_back(BufferedReaction {
                                    emoji: reaction.emoji.to_string(),
                                    user_name: user.name.clone(),
                                    message_preview: msg
                                        .content
                                        .chars()
                                        .take(100)
                                        .collect::<String>(),
                                    channel_id: reaction.channel_id.get(),
                                    timestamp: std::time::Instant::now(),
                                });

                                // Keep buffer size reasonable
                                if buffer.len() > 20 {
                                    buffer.pop_front();
                                }

                                info!(
                                    "Buffered reaction from {} (currently processing)",
                                    user.name
                                );
                            } else {
                                // Process immediately
                                let notification = format!(
                                    "discord reaction from '{}'\n\
                                    emoji: {}\n\
                                    on your message: {}\n\n\
                                    you may acknowledge this with a reaction (or message) of your own if appropriate.\n\
                                    to react, send a message with just an emoji to channel {} and it will attach to the most recent message",
                                    user.name,
                                    reaction.emoji,
                                    msg.content.chars().take(100).collect::<String>(),
                                    reaction.channel_id.get()
                                );

                                // Route this as a Pattern message to the agents
                                if self.bot.cli_mode {
                                    let mut pattern_msg = PatternMessage::user(notification);
                                    pattern_msg.metadata.custom = serde_json::json!({
                                        "discord_channel_id": reaction.channel_id.get(),
                                        "discord_message_id": reaction.message_id.get(),
                                        "is_reaction": true,
                                    });

                                    // Route through the group
                                    if let (
                                        Some(group),
                                        Some(agents_with_membership),
                                        Some(group_manager),
                                    ) = (
                                        &self.bot.group,
                                        &self.bot.agents_with_membership,
                                        &self.bot.group_manager,
                                    ) {
                                        info!(
                                            "Routing reaction notification through {} group",
                                            group.name
                                        );

                                        // Create a simple task to route the message
                                        let group_clone = group.clone();
                                        let agents_clone = agents_with_membership.clone();
                                        let manager_clone = group_manager.clone();
                                        let pattern_msg_clone = pattern_msg.clone();

                                        // Clone what we need for the async block
                                        let ctx_clone = ctx.clone();
                                        let channel_id = reaction.channel_id;

                                        // Spawn task to handle reaction routing without blocking
                                        tokio::spawn(async move {
                                            match manager_clone
                                                .route_message(
                                                    &group_clone,
                                                    &agents_clone,
                                                    pattern_msg_clone,
                                                )
                                                .await
                                            {
                                                Ok(mut stream) => {
                                                    use futures::StreamExt;

                                                    while let Some(event) = stream.next().await {
                                                        match event {
                                                            pattern_core::coordination::groups::GroupResponseEvent::TextChunk { .. } => {

                                                            }
                                                            pattern_core::coordination::groups::GroupResponseEvent::ToolCallStarted { fn_name, .. } => {
                                                                // Show tool activity for reactions too
                                                                let tool_msg = match fn_name.as_str() {
                                                                    "context" => "💭 Processing reaction context...".to_string(),
                                                                    "recall" => "🔍 Searching reaction history...".to_string(),
                                                                    "send_message" => {continue;},
                                                                    _ => format!("🔧 Processing with {}...", fn_name)
                                                                };
                                                                if let Err(e) = channel_id.say(&ctx_clone.http, tool_msg).await {
                                                                    debug!("Failed to send tool activity: {}", e);
                                                                }
                                                            }
                                                            pattern_core::coordination::groups::GroupResponseEvent::Error { message, .. } => {
                                                                warn!("Error processing reaction: {}", message);
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    warn!(
                                                        "Failed to route reaction notification: {}",
                                                        e
                                                    );
                                                }
                                            }
                                        });
                                    } else {
                                        info!("No group configured to handle reactions");
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            info!(
                "Received slash command: {} from {}",
                command.data.name, command.user.name
            );

            // Get agents and group for slash command handlers
            let agents = self.bot.agents_with_membership.as_deref();
            let group = self.bot.group.as_ref();
            let restart_ch = &self.bot.restart_ch;

            let result = match command.data.name.as_str() {
                "help" => crate::slash_commands::handle_help_command(&ctx, &command, agents).await,
                "status" => {
                    crate::slash_commands::handle_status_command(&ctx, &command, agents, group)
                        .await
                }
                "memory" | "archival" | "context" | "search" | "restart" => {
                    // Check user authorization for sensitive commands
                    if let Some(ref admin_users) = self.bot.config.admin_users {
                        let user_id_str = command.user.id.get().to_string();
                        if !admin_users.contains(&user_id_str) {
                            let response_result = command
                                .create_response(
                                    &ctx.http,
                                    serenity::builder::CreateInteractionResponse::Message(
                                        serenity::builder::CreateInteractionResponseMessage::new()
                                            .content("🚫 This command is not available to you.")
                                            .ephemeral(true),
                                    ),
                                )
                                .await;
                            if let Err(e) = response_result {
                                error!("Failed to send unauthorized response: {}", e);
                            }
                            return;
                        }
                    }

                    // User is authorized, execute the command
                    match command.data.name.as_str() {
                        "memory" => {
                            crate::slash_commands::handle_memory_command(&ctx, &command, agents)
                                .await
                        }
                        "archival" => {
                            crate::slash_commands::handle_archival_command(&ctx, &command, agents)
                                .await
                        }
                        "context" => {
                            crate::slash_commands::handle_context_command(&ctx, &command, agents)
                                .await
                        }
                        "search" => {
                            crate::slash_commands::handle_search_command(&ctx, &command, agents)
                                .await
                        }
                        "restart" => {
                            let admin_users = self.bot.config.admin_users.as_deref();
                            crate::slash_commands::handle_restart_command(
                                &ctx,
                                &command,
                                restart_ch,
                                admin_users,
                            )
                            .await
                        }
                        _ => unreachable!(),
                    }
                }
                "list" => {
                    crate::slash_commands::handle_list_command(
                        &ctx,
                        &command,
                        agents,
                        self.bot.dbs.as_deref(),
                    )
                    .await
                }
                "permit" => {
                    let admin_users = self.bot.config.admin_users.as_deref();
                    if let Err(e) =
                        crate::slash_commands::handle_permit(&ctx, &command, admin_users).await
                    {
                        warn!("Failed to handle permit: {}", e);
                    }
                    Ok(())
                }
                "deny" => {
                    let admin_users = self.bot.config.admin_users.as_deref();
                    if let Err(e) =
                        crate::slash_commands::handle_deny(&ctx, &command, admin_users).await
                    {
                        warn!("Failed to handle deny: {}", e);
                    }
                    Ok(())
                }
                "permits" => {
                    let admin_users = self.bot.config.admin_users.as_deref();
                    if let Err(e) =
                        crate::slash_commands::handle_permits(&ctx, &command, admin_users).await
                    {
                        warn!("Failed to handle permits: {}", e);
                    }
                    Ok(())
                }
                _ => {
                    warn!("Unknown command: {}", command.data.name);
                    Ok(())
                }
            };

            if let Err(e) = result {
                error!(
                    "Failed to handle slash command '{}': {}",
                    command.data.name, e
                );
            }
        }
    }
}

impl DiscordBot {
    /// Check if a channel is stale (no recent messages within threshold) and update last-seen time
    async fn channel_is_stale_and_touch(&self, channel_id: u64, threshold: Duration) -> bool {
        let mut map = self.recent_activity_by_channel.lock().await;
        let now = std::time::Instant::now();
        let stale = match map.get(&channel_id) {
            Some(last) => last.elapsed() > threshold,
            None => true,
        };
        map.insert(channel_id, now);
        stale
    }
    /// Get the elapsed time since we started processing the current message
    pub async fn get_current_processing_time(&self) -> Option<std::time::Duration> {
        let start_time = self.current_message_start.lock().await;
        start_time.as_ref().map(|start| start.elapsed())
    }

    /// Get the current message ID being processed
    pub async fn get_current_message_id(&self) -> Option<u64> {
        let current = self.current_message_id.lock().await;
        *current
    }

    /// Process queued messages (without recursion)
    async fn process_message_queue(&self, ctx: &Context) {
        // Wait a bit before processing queue
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Get ALL queued messages at once
        let queued_messages = {
            let mut queue = self.message_queue.lock().await;
            // Drain all messages from queue
            queue.drain(..).collect::<Vec<_>>()
        };

        if queued_messages.is_empty() {
            return;
        }

        info!(
            "Processing {} queued messages as batch",
            queued_messages.len()
        );

        // Mark as processing
        {
            let mut processing = self.is_processing.lock().await;
            *processing = true;

            // Track the first message ID for replies
            let mut current = self.current_message_id.lock().await;
            *current = Some(queued_messages[0].msg_id);
        }

        // Show typing in the channel
        if let Some(first_msg) = queued_messages.first() {
            let _ = ChannelId::new(first_msg.channel_id)
                .broadcast_typing(&ctx.http)
                .await;
        }

        // Get channel info for the messages
        let channel_id = queued_messages[0].channel_id;

        // Build concatenated message with special framing
        let mut combined_content =
            String::from("=== Multiple Discord messages arrived while you were busy ===\n\n");

        // Store message IDs for reference
        let mut message_ids = Vec::new();

        for (i, msg) in queued_messages.iter().enumerate() {
            let delay_secs = msg.timestamp.elapsed().as_secs();
            message_ids.push(msg.msg_id);

            // Check if this is already a merged message (contains separators or [Also from...])
            let is_pre_merged = msg.content.contains("---") || msg.content.contains("[Also from");

            if is_pre_merged {
                // This is already a batch, format it differently
                combined_content.push_str(&format!(
                    "[Batch {} - started by '{}' - {}s ago]:\n{}\n\n",
                    i + 1,
                    msg.author_name,
                    delay_secs,
                    msg.content
                ));
            } else {
                // Single message
                combined_content.push_str(&format!(
                    "[Message {} from '{}' - {}s ago]:\n{}\n\n",
                    i + 1,
                    msg.author_name,
                    delay_secs,
                    msg.content
                ));
            }
        }

        // Get channel name for context
        let channel_name = if let Ok(channel) = ChannelId::new(channel_id).to_channel(&ctx).await {
            match channel {
                serenity::model::channel::Channel::Guild(gc) => format!("#{}", gc.name),
                _ => format!("channel {}", channel_id),
            }
        } else {
            format!("channel {}", channel_id)
        };

        // Add extended recent context only if the batch is "stale" (oldest queued > threshold)
        let oldest_age = queued_messages
            .iter()
            .map(|m| m.timestamp.elapsed())
            .max()
            .unwrap_or_else(|| std::time::Duration::from_secs(0));
        if oldest_age > std::time::Duration::from_secs(180) {
            let extra = queued_messages.len().min(12) as u8; // cap extra to prevent bloat
            let base_limit: u8 = 4;
            let fetch_limit = base_limit.saturating_add(extra);
            let fut = ChannelId::new(channel_id)
                .messages(&ctx.http, GetMessages::new().limit(fetch_limit));
            if let Ok(Ok(mut msgs)) = tokio::time::timeout(Duration::from_secs(5), fut).await {
                msgs.reverse();
                let lines: Vec<String> = msgs
                    .into_iter()
                    .map(|m| {
                        let author = if let Some(ref gn) = m.author.global_name {
                            format!("{} [{}]", gn, m.author.name)
                        } else {
                            m.author.name.clone()
                        };
                        let text = if !m.content.is_empty() {
                            let trimmed = m.content.trim();
                            unicode_preview(trimmed, 180)
                        } else if !m.attachments.is_empty() {
                            let first = &m.attachments[0];
                            format!("<attachment: {}>", first.filename)
                        } else {
                            String::from("<non-text message>")
                        };
                        format!("- {}: {}", author, text)
                    })
                    .collect();
                if !lines.is_empty() {
                    combined_content.push_str("Recent context (latest first):\n");
                    combined_content.push_str(&lines.join("\n"));
                    combined_content.push_str("\n\n");
                }
            }
        }

        combined_content.push_str(&format!(
            "You can respond to these messages as a batch. Use send_message with target_type: \"channel\" \
            and target_id: \"{}\" (or the channel name {}) to reply. Since these messages are delayed, your response will be sent as a reply to the last message.\n\n",
            channel_id,
            channel_name
        ));

        // Create Pattern message
        let mut pattern_msg = PatternMessage::user(combined_content);
        // Use the last message ID for replies (most recent message to reply to)
        let last_msg_id = queued_messages
            .last()
            .map(|m| m.msg_id)
            .unwrap_or(queued_messages[0].msg_id);
        pattern_msg.metadata.custom = serde_json::json!({
            "discord_channel_id": channel_id,
            "discord_message_id": last_msg_id,  // Reply to the last message in batch
            "is_batch": true,
            "batch_size": queued_messages.len(),
            "response_delay_ms": queued_messages[0].timestamp.elapsed().as_millis(),
        });

        // Route through agents
        if self.cli_mode {
            if let (Some(group), Some(agents_with_membership), Some(group_manager)) = (
                &self.group,
                &self.agents_with_membership,
                &self.group_manager,
            ) {
                match group_manager
                    .route_message(group, agents_with_membership, pattern_msg)
                    .await
                {
                    Ok(stream) => {
                        use futures::StreamExt;
                        // Tee to sinks if configured
                        let mut stream = if let Some(sinks) = &self.group_event_sinks {
                            let ctx = GroupEventContext {
                                source_tag: Some("Discord".to_string()),
                                group_name: Some(group.name.clone()),
                            };
                            tap_group_stream(stream, sinks.clone(), ctx)
                        } else {
                            stream
                        };
                        let mut has_response = false;

                        let ctx_clone = ctx.clone();
                        while let Some(event) = stream.next().await {
                            has_response = true;
                            match event {
                                pattern_core::coordination::groups::GroupResponseEvent::ToolCallStarted { fn_name, .. } => {
                                    // // Show tool activity for reactions too
                                    // let tool_msg = match fn_name.as_str() {
                                    //     "context" => "💭 Processing reaction context...".to_string(),
                                    //     "recall" => "🔍 Searching reaction history...".to_string(),
                                    //     "send_message" => {continue;},
                                    //     _ => format!("🔧 Processing with {}...", fn_name)
                                    // };
                                    // if let Err(e) = channel_id.say(&ctx_clone.http, tool_msg).await {
                                    //     debug!("Failed to send tool activity: {}", e);
                                    // }
                                }
                                _ => {} // Ignore other events for batch processing
                            }
                        }

                        if !has_response {
                            // No response to batch, send indicator
                            let _ = ChannelId::new(channel_id).say(&ctx.http, "💭 ...").await;
                        }
                    }
                    Err(e) => {
                        error!("Failed to process message batch: {}", e);
                        let _ = ChannelId::new(channel_id).say(&ctx.http, "💭 ...").await;
                    }
                }
            }
        }

        // Mark as done
        {
            let mut processing = self.is_processing.lock().await;
            *processing = false;

            let mut current = self.current_message_id.lock().await;
            *current = None;
        }

        // Remove status reactions from processed messages
        {
            let mut reactions = self.status_reactions.lock().await;
            for msg_id in &message_ids {
                if let Some(emoji) = reactions.remove(msg_id) {
                    // Try to remove the reaction
                    let reaction_type = serenity::all::ReactionType::Unicode(emoji.to_string());
                    if let Ok(current_user) = ctx.http.get_current_user().await {
                        let _ = ctx
                            .http
                            .delete_reaction(
                                ChannelId::new(channel_id),
                                serenity::all::MessageId::new(*msg_id),
                                current_user.id,
                                &reaction_type,
                            )
                            .await;
                    }
                }
            }
        }

        // Process any buffered reactions
        self.flush_reaction_buffer(ctx).await;
    }

    /// Flush buffered reactions as a batch
    async fn flush_reaction_buffer(&self, _ctx: &Context) {
        let reactions = {
            let mut buffer = self.reaction_buffer.lock().await;
            if buffer.is_empty() {
                return;
            }
            buffer.drain(..).collect::<Vec<_>>()
        };

        if reactions.is_empty() {
            return;
        }

        // Format batch notification with more context
        let mut notification = String::from("=== Batched Discord Reactions ===\n\n");
        for reaction in &reactions {
            let age_secs = reaction.timestamp.elapsed().as_secs();
            notification.push_str(&format!(
                "• {} from {} ({}s ago)\n  On message: {}\n\n",
                reaction.emoji,
                reaction.user_name,
                age_secs,
                if reaction.message_preview.len() > 50 {
                    format!("{}...", &reaction.message_preview[..50])
                } else {
                    reaction.message_preview.clone()
                }
            ));
        }

        notification.push_str("These reactions arrived while you were processing. You may acknowledge if appropriate.");

        // Send batch to agents
        if self.cli_mode {
            if let (Some(group), Some(agents_with_membership), Some(group_manager)) = (
                &self.group,
                &self.agents_with_membership,
                &self.group_manager,
            ) {
                let pattern_msg = PatternMessage::user(notification);

                // Fire and forget - don't wait for response
                let group_clone = group.clone();
                let agents_clone = agents_with_membership.clone();
                let manager_clone = group_manager.clone();

                tokio::spawn(async move {
                    let _ = manager_clone
                        .route_message(&group_clone, &agents_clone, pattern_msg)
                        .await;
                });

                info!("Flushed {} buffered reactions to agents", reactions.len());
            }
        }
    }

    /// Process a Discord message and route it to Pattern agents
    async fn process_message(&self, ctx: &Context, msg: &Message) -> Result<(), String> {
        // Debounce rapid messages - wait a bit if last message was very recent
        {
            let mut last_time = self.last_message_time.lock().await;
            let now = std::time::Instant::now();
            let time_since_last = now.duration_since(*last_time);

            // If less than 500ms since last message, wait a bit
            if time_since_last < std::time::Duration::from_millis(500) {
                let wait_time = std::time::Duration::from_millis(500) - time_since_last;
                info!("Debouncing message - waiting {}ms", wait_time.as_millis());
                tokio::time::sleep(wait_time).await;
            }

            *last_time = std::time::Instant::now();
        }

        // Mark as processing and track current message and timing
        {
            let mut processing = self.is_processing.lock().await;
            *processing = true;

            let mut current = self.current_message_id.lock().await;
            *current = Some(msg.id.get());

            let mut start_time = self.current_message_start.lock().await;
            *start_time = Some(std::time::Instant::now());

            info!("Processing message {} from {}", msg.id, msg.author.name);
        }

        // Start typing indicator that refreshes every 8 seconds
        {
            let mut typing_handle = self.typing_handle.lock().await;

            // Cancel any existing typing task
            if let Some(handle) = typing_handle.take() {
                handle.abort();
            }

            let channel_id = msg.channel_id;
            let http = ctx.http.clone();

            // Spawn task to keep typing indicator alive
            let handle = tokio::spawn(async move {
                loop {
                    // Send typing indicator
                    let _ = channel_id.broadcast_typing(&http).await;

                    // Wait 8 seconds (typing lasts 10 seconds, so refresh at 8)
                    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                }
            });

            *typing_handle = Some(handle);
        }

        // Ensure we mark as not processing when done
        let result = self.process_message_inner(ctx, msg).await;

        // Mark as done and clear current message and timing
        {
            let mut processing = self.is_processing.lock().await;
            *processing = false;

            let mut current = self.current_message_id.lock().await;
            *current = None;

            let mut start_time = self.current_message_start.lock().await;
            *start_time = None;

            // Stop typing indicator
            let mut typing_handle = self.typing_handle.lock().await;
            if let Some(handle) = typing_handle.take() {
                handle.abort();
            }
        }

        // Process any buffered reactions
        self.flush_reaction_buffer(ctx).await;

        // Process any queued messages
        self.process_message_queue(ctx).await;

        result
    }

    /// Inner message processing logic
    async fn process_message_inner(&self, ctx: &Context, msg: &Message) -> Result<(), String> {
        // Track when we started processing for delay calculation
        let processing_start = std::time::Instant::now();

        if self.cli_mode {
            // Create message with Discord metadata for group routing
            let discord_channel_id = msg.channel_id.get();

            // Resolve mentions to usernames
            let mut resolved_content = msg.content.clone();
            for user in &msg.mentions {
                let mention_pattern = format!("<@{}>", user.id);
                let alt_mention_pattern = format!("<@!{}>", user.id); // Nickname mentions
                resolved_content = resolved_content
                    .replace(&mention_pattern, &format!("@{}", user.name))
                    .replace(&alt_mention_pattern, &format!("@{}", user.name));
            }

            // Get current bot user for self-mentions, map to the supervisor agent name when available
            let mut cached_bot_id = { *self.bot_user_id.lock().await };
            if cached_bot_id.is_none() {
                if let Ok(current_user) = ctx.http.get_current_user().await {
                    cached_bot_id = Some(current_user.id.get());
                    *self.bot_user_id.lock().await = cached_bot_id;
                }
            }
            if let Some(bot_id) = cached_bot_id {
                let bot_mention = format!("<@{}>", bot_id);
                let bot_alt_mention = format!("<@!{}>", bot_id);

                // Determine supervisor agent name if we have group context
                let supervisor_name = self.agents_with_membership.as_ref().and_then(|agents| {
                    agents
                        .iter()
                        .find(|a| {
                            matches!(
                                a.membership.role,
                                pattern_core::coordination::types::GroupMemberRole::Supervisor
                            )
                        })
                        .map(|a| a.agent.name())
                });

                if let Some(name) = supervisor_name {
                    let replacement = format!("@{}", name);
                    resolved_content = resolved_content
                        .replace(&bot_mention, &replacement)
                        .replace(&bot_alt_mention, &replacement);
                }
            }

            // Get channel name if possible (moved outside to be accessible)
            let channel_name = if let Ok(channel) = msg.channel_id.to_channel(&ctx).await {
                match channel {
                    serenity::model::channel::Channel::Guild(gc) => format!("#{}", gc.name),
                    _ => format!("channel {}", msg.channel_id),
                }
            } else {
                format!("channel {}", msg.channel_id)
            };

            // Get display name hierarchy: server nickname > global display name > username
            // Always show username in brackets for clarity
            let display_name_with_username = if let Some(guild_id) = msg.guild_id {
                // Try to get member to access server nickname
                match ctx.http.get_member(guild_id, msg.author.id).await {
                    Ok(member) => {
                        if let Some(nick) = member.nick {
                            // Server nickname available
                            format!("{} [{}]", nick, msg.author.name)
                        } else if let Some(ref global_name) = msg.author.global_name {
                            // No nickname, use global display name
                            format!("{} [{}]", global_name, msg.author.name)
                        } else {
                            // Just username
                            msg.author.name.clone()
                        }
                    }
                    Err(e) => {
                        debug!("Failed to get member for nickname: {}", e);
                        // Fall back to global display name or username
                        if let Some(ref global_name) = msg.author.global_name {
                            format!("{} [{}]", global_name, msg.author.name)
                        } else {
                            msg.author.name.clone()
                        }
                    }
                }
            } else {
                // DM - use global display name if available, always show username
                if let Some(ref global_name) = msg.author.global_name {
                    format!("{} [{}]", global_name, msg.author.name)
                } else {
                    msg.author.name.clone()
                }
            };

            // Build context string with better framing
            let discord_context = if msg.guild_id.is_none() {
                format!(
                    "Direct message from Discord user '{}'",
                    display_name_with_username
                )
            } else {
                format!(
                    "Message from '{}' in Discord {}",
                    display_name_with_username, channel_name
                )
            };

            // Build lightweight reply/thread context
            let mut reply_context = String::new();
            if let Some(referenced) = &msg.referenced_message {
                // Identify author display
                let ref_author = referenced.author.name.clone();
                let ref_preview = unicode_preview(referenced.content.as_str(), 180);
                reply_context.push_str(&format!(
                    "\n[Replying to {}]: \"{}\"\n",
                    ref_author, ref_preview
                ));
            }

            // Provide recent in-channel context only if channel looks stale (no activity for ~3 minutes)
            let mut recent_context = String::new();
            let include_recent_context = self
                .channel_is_stale_and_touch(msg.channel_id.get(), Duration::from_secs(180))
                .await;
            if include_recent_context {
                if let Ok(mut msgs) = msg
                    .channel_id
                    .messages(
                        &ctx.http,
                        GetMessages::new()
                            .before(MessageId::new(msg.id.get()))
                            .limit(4),
                    )
                    .await
                {
                    // Newest first -> reverse for chronological
                    msgs.reverse();
                    // Summarize last few lines with author and snippet
                    let mut lines = Vec::new();
                    for m in msgs.into_iter() {
                        // Skip pure bot-system noise unless it's from us
                        if m.author.bot && m.author.id != msg.author.id {
                            continue;
                        }
                        let author = if let Some(ref gn) = m.author.global_name {
                            format!("{} [{}]", gn, m.author.name)
                        } else {
                            m.author.name.clone()
                        };
                        let text = if !m.content.is_empty() {
                            let trimmed = m.content.trim();
                            unicode_preview(trimmed, 160)
                        } else if !m.attachments.is_empty() {
                            let first = &m.attachments[0];
                            format!("<attachment: {}>", first.filename)
                        } else {
                            String::from("<non-text message>")
                        };
                        lines.push(format!("- {}: {}", author, text));
                    }
                    if !lines.is_empty() {
                        recent_context.push_str("\nRecent context:\n");
                        recent_context.push_str(&lines.join("\n"));
                        recent_context.push_str("\n");
                    }
                }
            }

            // Process attachments if any
            let mut attachment_content = String::new();
            let mut unique_image_urls = std::collections::HashSet::new();
            // Build a small-timeout HTTP client for fetching small text attachments
            let http_client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .ok();
            if !msg.attachments.is_empty() {
                for attachment in &msg.attachments {
                    // Check if it's an image file
                    let is_image = attachment.filename.ends_with(".png")
                        || attachment.filename.ends_with(".jpg")
                        || attachment.filename.ends_with(".jpeg")
                        || attachment.filename.ends_with(".gif")
                        || attachment.filename.ends_with(".webp")
                        || attachment
                            .content_type
                            .as_ref()
                            .map_or(false, |ct| ct.starts_with("image/"));

                    if is_image {
                        // Add unique image URL for multimodal processing
                        unique_image_urls.insert(attachment.url.clone());
                        attachment_content.push_str(&format!(
                            "\n\n[Image attachment: {} ({} bytes)]",
                            attachment.filename, attachment.size
                        ));
                    } else if attachment.size < 20_000 {
                        // Only process text files under 20KB
                        let is_text = attachment.filename.ends_with(".txt")
                            || attachment.filename.ends_with(".md")
                            || attachment.filename.ends_with(".json")
                            || attachment.filename.ends_with(".yaml")
                            || attachment.filename.ends_with(".yml")
                            || attachment.filename.ends_with(".log")
                            || attachment
                                .content_type
                                .as_ref()
                                .map_or(false, |ct| ct.starts_with("text/"));

                        if is_text {
                            if let Some(client) = &http_client {
                                match client.get(&attachment.url).send().await {
                                    Ok(resp) => match resp.text().await {
                                        Ok(text) => {
                                            attachment_content.push_str(&format!(
                                                "\n\nAttachment '{}' ({} bytes): \n```\n{}\n```",
                                                attachment.filename, attachment.size, text
                                            ));
                                        }
                                        Err(e) => {
                                            debug!(
                                                "Failed to read attachment text {}: {}",
                                                attachment.filename, e
                                            );
                                        }
                                    },
                                    Err(e) => {
                                        debug!(
                                            "Failed to fetch attachment {}: {}",
                                            attachment.filename, e
                                        );
                                    }
                                }
                            } else {
                                debug!(
                                    "HTTP client unavailable; skipping fetch for attachment {}",
                                    attachment.filename
                                );
                            }
                        }
                    }
                }
            }

            // Convert to vec and take only last 4 images to avoid token bloat
            let all_images: Vec<String> = unique_image_urls.into_iter().collect();
            let selected_images: Vec<_> = all_images.iter().rev().take(4).rev().cloned().collect();

            // Append image markers to attachment content
            for image_url in &selected_images {
                attachment_content.push_str(&format!("\n[IMAGE: {}]", image_url));
            }

            // Create framing prompt that makes responding optional
            let framed_message = format!(
                "{}{}{}\n\
                Message: {}{}\n\n\
                you can respond if you have something to add, or if you're directly mentioned.
                if you do, use send_message with target_type: \"channel\" and target_id: \"{}\" (or the channel name {})",
                discord_context,
                reply_context,
                recent_context,
                resolved_content,
                attachment_content,
                discord_channel_id,
                channel_name
            );

            let mut pattern_msg = PatternMessage::user(framed_message);

            // Add Discord context to metadata so send_message knows where to reply
            pattern_msg.metadata.custom = serde_json::json!({
                "discord_channel_id": msg.channel_id.get(),
                "discord_guild_id": msg.guild_id.map(|g| g.get()),
                "discord_user_id": msg.author.id.get(),
                "discord_username": msg.author.name.clone(),
                "discord_message_id": msg.id.get(),  // Track the original message for replies
                "is_dm": msg.guild_id.is_none(),
                "processing_start_ms": processing_start.elapsed().as_millis(),  // Track when we started
            });

            // Check if we have a group setup
            if let (Some(group), Some(agents_with_membership), Some(group_manager)) = (
                &self.group,
                &self.agents_with_membership,
                &self.group_manager,
            ) {
                // Log which coordination pattern we're using
                info!(
                    "Routing message using {:?} coordination pattern",
                    group.coordination_pattern
                );

                // Route through group manager using the real agents with membership
                let response_stream = group_manager
                    .route_message(group, agents_with_membership, pattern_msg)
                    .await
                    .map_err(|e| format!("Failed to route message: {}", e))?;

                // Tee to optional sinks (e.g., CLI printer, file) so CLI can mirror Discord output
                let mut response_stream = if let Some(sinks) = &self.group_event_sinks {
                    let ctx = GroupEventContext {
                        source_tag: Some("Discord".to_string()),
                        group_name: Some(group.name.clone()),
                    };
                    tap_group_stream(response_stream, sinks.clone(), ctx)
                } else {
                    response_stream
                };

                // Set up idle timeout - resets on any activity
                let idle_timeout = Duration::from_secs(600); // 10 minutes of inactivity
                let mut last_activity = tokio::time::Instant::now();

                // Track state
                let current_message = String::new();
                let mut has_sent_initial_response = false;
                let mut active_agents: usize = 0;
                let mut completed_agents = 0;

                // First-event watchdog: post a small indicator after 20s if no events
                let started_flag = Arc::new(AtomicBool::new(false));
                let flag = started_flag.clone();
                let http = ctx.http.clone();
                let ch = msg.channel_id;
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    if !flag.load(Ordering::SeqCst) {
                        let _ = ch.say(&http, "💭 thinking…").await;
                    }
                });

                // Process stream with idle timeout
                loop {
                    match tokio::time::timeout_at(
                        last_activity + idle_timeout,
                        response_stream.next(),
                    )
                    .await
                    {
                        Ok(Some(event)) => {
                            // Reset idle timer on any event
                            last_activity = tokio::time::Instant::now();
                            has_sent_initial_response = true; // ANY activity counts as a response
                            started_flag.store(true, Ordering::SeqCst);

                            match event {
                                pattern_core::coordination::groups::GroupResponseEvent::TextChunk { ..} => {},
                                pattern_core::coordination::groups::GroupResponseEvent::ToolCallStarted { agent_id: _, call_id:_, fn_name, args: _ } => {
                                    //info!("Tool call started: {} ({})", fn_name, call_id);

                                    // Don't intercept send_message tool calls - let them go through the agent's router
                                    // This ensures proper routing based on the target specified in the tool call
                                    if fn_name != "send_message" {
                                        // Show tool activity if we haven't sent anything yet
                                        let tool_msg = match fn_name.as_str() {
                                            "context" => "💭 Agent is accessing memory...".to_string(),
                                            "recall" => "🔍 Agent is accessing recall memory...".to_string(),
                                            "search" => "🔎 Agent is searching memory/history...".to_string(),
                                            _ => format!("🔧 Agent is using {}", fn_name)
                                        };
                                        // Use channel.say() to respond in the same channel instead of DM
                                        if let Err(e) = msg.channel_id.say(&ctx.http, tool_msg).await {
                                            debug!("Failed to send tool activity: {}", e);
                                        }
                                        has_sent_initial_response = true;
                                        started_flag.store(true, Ordering::SeqCst);
                                    }
                                },
                                pattern_core::coordination::groups::GroupResponseEvent::ToolCallCompleted { agent_id: _, call_id:_, result } => {
                                    //info!("Tool call completed: {} - {:?}", call_id, result);

                                    // Check if this was a send_message tool that succeeded
                                    if let Ok(result_str) = &result {
                                        if result_str.contains("Message sent successfully") || result_str.contains("channel:") {
                                            info!("send_message tool completed successfully");
                                            has_sent_initial_response = true; // Mark as having responded
                                            started_flag.store(true, Ordering::SeqCst);
                                        }
                                    }
                                },
                                pattern_core::coordination::groups::GroupResponseEvent::Error { agent_id: _, message, recoverable } => {
                                    warn!("Agent error: {} (recoverable: {})", message, recoverable);
                                    // Don't send error details to Discord, just log them
                                    if !recoverable {
                                        // For non-recoverable errors, maybe send a generic message
                                        let _ = msg.channel_id.say(&ctx.http, "💭 ... !").await;
                                        break; // Stop processing on non-recoverable errors
                                    }
                                    // For recoverable errors, just continue silently
                                },
                                pattern_core::coordination::groups::GroupResponseEvent::AgentStarted { agent_name, .. } => {
                                    debug!("Agent {} started processing", agent_name);
                                    active_agents += 1;

                                    // Start typing indicator when agent starts thinking
                                    let _ = msg.channel_id.broadcast_typing(&ctx.http).await;
                                    started_flag.store(true, Ordering::SeqCst);
                                },
                                pattern_core::coordination::groups::GroupResponseEvent::AgentCompleted { agent_name, .. } => {
                                    debug!("Agent {} completed processing", agent_name);
                                    completed_agents += 1;
                                    active_agents = active_agents.saturating_sub(1);

                                    // If all agents have completed, we can exit
                                    if active_agents == 0 && completed_agents > 0 {
                                        break;
                                    }
                                },
                                _ => {} // Ignore other events for now
                            }
                        }
                        Ok(None) => {
                            // Stream ended normally
                            break;
                        }
                        Err(_) => {
                            // Idle timeout
                            let timeout_msg = if has_sent_initial_response {
                                "⏱️ (No further activity for 10 minutes - entities may still be processing)"
                            } else {
                                "⏱️ Request timed out after 10 minutes of inactivity. No entities responded."
                            };
                            if let Err(e) = msg.channel_id.say(&ctx.http, timeout_msg).await {
                                warn!("Failed to send timeout message: {}", e);
                            }
                            break;
                        }
                    }
                }

                // Send any remaining buffered content
                if !current_message.trim().is_empty() {
                    for chunk in split_message(&current_message, 2000) {
                        if let Err(e) = msg.channel_id.say(&ctx.http, chunk).await {
                            warn!("Failed to send final response chunk: {}", e);
                        }
                    }
                }

                // If we never sent anything, send a status message
                if !has_sent_initial_response {
                    let status_msg = if active_agents > 0 {
                        "The entities started processing but produced no response."
                    } else {
                        "No entities were available to process your message."
                    };
                    if let Err(e) = msg.channel_id.say(&ctx.http, status_msg).await {
                        warn!("Failed to send status message: {}", e);
                    }
                }
            } else {
                // Fallback for single agent mode
                if let Some(agents_with_membership) = &self.agents_with_membership {
                    if let Some(awm) = agents_with_membership.first() {
                        let agent = &awm.agent;
                        // Direct agent call
                        let mut stream = agent
                            .clone()
                            .process(pattern_msg)
                            .await
                            .map_err(|e| format!("Failed to process message: {}", e))?;

                        let mut response = String::new();
                        while let Some(event) = stream.next().await {
                            match event {
                                pattern_core::agent::ResponseEvent::TextChunk { text, .. } => {
                                    response.push_str(&text);
                                }
                                _ => {} // Ignore other events
                            }
                        }

                        if response.is_empty() {
                            response = "No response from entity.".to_string();
                        }

                        msg.channel_id
                            .say(&ctx.http, response)
                            .await
                            .map_err(|e| format!("Failed to send reply: {}", e))?;
                    }
                }
            }
        } else {
            // TODO: Implement full database mode with user lookup
            msg.channel_id
                .say(&ctx.http, "Full mode not yet implemented")
                .await
                .map_err(|e| format!("Failed to reply: {}", e))?;
        }
        Ok(())
    }
}

/// Split a message into chunks that fit Discord's message length limit
pub fn split_message(content: &str, max_length: usize) -> Vec<String> {
    if content.len() <= max_length {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in content.lines() {
        if current.len() + line.len() + 1 > max_length {
            if !current.is_empty() {
                chunks.push(current.trim().to_string());
                current = String::new();
            }

            // If a single line is too long, split it
            if line.len() > max_length {
                for chunk in line.chars().collect::<Vec<_>>().chunks(max_length) {
                    chunks.push(chunk.iter().collect());
                }
            } else {
                current = line.to_string();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        chunks.push(current.trim().to_string());
    }

    chunks
}
