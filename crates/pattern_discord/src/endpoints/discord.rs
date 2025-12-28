use crate::bot::DiscordBot;
use serde_json::Value;
use serenity::http::Http;
use serenity::model::id::{ChannelId, UserId as DiscordUserId};
use std::sync::Arc;
use tracing::{debug, info, warn};

use pattern_core::Result;
use pattern_core::config::DiscordAppConfig;
use pattern_core::messages::{ContentPart, Message, MessageContent};
use pattern_core::runtime::router::{MessageEndpoint, MessageOrigin};

/// Discord endpoint for sending messages through the Pattern message router
#[derive(Clone)]
pub struct DiscordEndpoint {
    /// Serenity HTTP client for Discord API
    http: Arc<Http>,
    /// Reference to the Discord bot for context
    bot: Option<Arc<DiscordBot>>,
    /// Optional default channel for broadcasts
    default_channel: Option<ChannelId>,
    /// Optional default DM user for CLI mode
    default_dm_user: Option<DiscordUserId>,
}

impl DiscordEndpoint {
    /// Create a new Discord endpoint with the bot token
    pub fn new(token: String) -> Self {
        let http = Arc::new(Http::new(&token));
        Self {
            http,
            bot: None,
            default_channel: None,
            default_dm_user: None,
        }
    }

    /// For DMs, prefix the content with an agent/facet tag when available
    fn dm_tagged_content(content: &str, origin: Option<&MessageOrigin>) -> String {
        if let Some(MessageOrigin::Agent { name, .. }) = origin {
            // Subtle Markdown tag so recipients know which facet is speaking
            format!("*[{}]*\n{}", name, content)
        } else {
            content.to_string()
        }
    }

    /// Create a new Discord endpoint with token and optional config
    pub fn with_config(token: String, config: Option<&DiscordAppConfig>) -> Self {
        let mut endpoint = Self::new(token);

        // Apply config if provided
        if let Some(cfg) = config {
            // Set default channel from first allowed channel
            if let Some(channels) = &cfg.allowed_channels {
                if let Some(first) = channels.first() {
                    if let Ok(channel_id) = first.parse::<u64>() {
                        endpoint.default_channel = Some(ChannelId::new(channel_id));
                    }
                }
            }

            // Set default DM user from first admin user
            if let Some(admins) = &cfg.admin_users {
                if let Some(first) = admins.first() {
                    if let Ok(user_id) = first.parse::<u64>() {
                        endpoint.default_dm_user = Some(DiscordUserId::new(user_id));
                    }
                }
            }
        }

        endpoint
    }

    /// Set the bot reference for context access
    pub fn with_bot(mut self, bot: Arc<DiscordBot>) -> Self {
        self.bot = Some(bot);
        self
    }

    /// Try to resolve a channel name to a channel ID
    /// Supports formats: "#channel-name", "channel-name", or numeric ID
    async fn resolve_channel_id(&self, target_id: &str) -> Option<ChannelId> {
        info!("resolve_channel_id called with target_id: '{}'", target_id);

        // Strip leading # if present
        let channel_ref = target_id.trim_start_matches('#');
        // First, try parsing as a numeric ID
        match channel_ref.parse::<u64>() {
            Ok(id) => {
                info!(
                    "Successfully parsed '{}' as numeric ID: {}",
                    channel_ref, id
                );
                return Some(ChannelId::new(id));
            }
            Err(e) => {
                info!("Failed to parse '{}' as numeric ID: {:?}", channel_ref, e);
            }
        }

        // Try to resolve by channel name using Discord API
        // Guild IDs must come from bot config (loaded at startup)
        let guild_ids: Vec<u64> = if let Some(bot) = &self.bot {
            bot.config()
                .allowed_guilds
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| s.parse::<u64>().ok())
                .collect()
        } else {
            Vec::new()
        };

        for guild_id_u64 in guild_ids {
            let guild_id = serenity::model::id::GuildId::new(guild_id_u64);

            info!(
                "Fetching channels for guild {} to resolve name '{}'",
                guild_id, channel_ref
            );

            // Try to get guild channels via HTTP API
            match self.http.get_channels(guild_id).await {
                Ok(channels) => {
                    info!(
                        "Retrieved {} channels from guild {}",
                        channels.len(),
                        guild_id
                    );

                    // Search for exact channel name match
                    for channel in &channels {
                        if channel.name == channel_ref {
                            info!(
                                "Found exact match channel '{}' with ID: {}",
                                channel_ref, channel.id
                            );
                            return Some(channel.id);
                        }
                    }

                    // If no exact match, try partial matching
                    for channel in &channels {
                        if channel.name.contains(channel_ref) {
                            info!(
                                "Found partial match channel '{}' -> '{}' with ID: {}",
                                channel_ref, channel.name, channel.id
                            );
                            return Some(channel.id);
                        }
                    }

                    info!(
                        "No channel found matching name '{}' in {}",
                        channel_ref, guild_id
                    );
                }
                Err(e) => {
                    info!("Failed to fetch guild channels for {}: {}", guild_id, e);
                }
            }
        }

        info!("Could not resolve channel name '{}' to ID", channel_ref);
        None
    }

    /// Try to resolve a Discord username or display name to a user ID
    /// Supports formats: "@username", "username", "Display Name", or numeric ID
    async fn resolve_user_id(&self, target_id: &str) -> Option<serenity::model::id::UserId> {
        info!("resolve_user_id called with target_id: '{}'", target_id);

        // Strip leading @ if present
        let user_ref = target_id.trim_start_matches('@');
        info!("After stripping @: '{}'", user_ref);

        // First, try parsing as a numeric ID
        match user_ref.parse::<u64>() {
            Ok(id) => {
                info!(
                    "Successfully parsed '{}' as numeric user ID: {}",
                    user_ref, id
                );
                return Some(serenity::model::id::UserId::new(id));
            }
            Err(e) => {
                info!("Failed to parse '{}' as numeric user ID: {:?}", user_ref, e);
            }
        }

        // Try to resolve by username/display name using Discord API
        // Guild IDs must come from bot config (loaded at startup)
        let guild_ids: Vec<u64> = if let Some(bot) = &self.bot {
            bot.config()
                .allowed_guilds
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|s| s.parse::<u64>().ok())
                .collect()
        } else {
            Vec::new()
        };

        for guild_id_u64 in guild_ids {
            let guild_id = serenity::model::id::GuildId::new(guild_id_u64);

            info!(
                "Fetching guild members from guild {} to resolve user '{}'",
                guild_id, user_ref
            );

            // Try to get guild members via HTTP API
            // Note: This requires proper bot permissions (GUILD_MEMBERS intent)
            match self
                .http
                .get_guild_members(guild_id, Some(1000), None)
                .await
            {
                Ok(members) => {
                    info!(
                        "Retrieved {} members from guild {}",
                        members.len(),
                        guild_id
                    );

                    // Search for exact username match (case insensitive)
                    for member in &members {
                        if member.user.name.to_lowercase() == user_ref.to_lowercase() {
                            info!(
                                "Found exact username match '{}' -> {} (ID: {})",
                                user_ref, member.user.name, member.user.id
                            );
                            return Some(member.user.id);
                        }
                    }

                    // Search for exact display name match (case insensitive)
                    for member in &members {
                        if let Some(ref display_name) = member.user.global_name {
                            if display_name.to_lowercase() == user_ref.to_lowercase() {
                                info!(
                                    "Found exact display name match '{}' -> {} (ID: {})",
                                    user_ref, display_name, member.user.id
                                );
                                return Some(member.user.id);
                            }
                        }
                    }

                    // Search for exact nickname match (case insensitive)
                    for member in &members {
                        if let Some(ref nick) = member.nick {
                            if nick.to_lowercase() == user_ref.to_lowercase() {
                                info!(
                                    "Found exact nickname match '{}' -> {} (ID: {})",
                                    user_ref, nick, member.user.id
                                );
                                return Some(member.user.id);
                            }
                        }
                    }

                    // If no exact match, try partial matching on username
                    for member in &members {
                        if member
                            .user
                            .name
                            .to_lowercase()
                            .contains(&user_ref.to_lowercase())
                        {
                            info!(
                                "Found partial username match '{}' -> {} (ID: {})",
                                user_ref, member.user.name, member.user.id
                            );
                            return Some(member.user.id);
                        }
                    }

                    // Try partial matching on display name
                    for member in &members {
                        if let Some(ref display_name) = member.user.global_name {
                            if display_name
                                .to_lowercase()
                                .contains(&user_ref.to_lowercase())
                            {
                                info!(
                                    "Found partial display name match '{}' -> {} (ID: {})",
                                    user_ref, display_name, member.user.id
                                );
                                return Some(member.user.id);
                            }
                        }
                    }

                    info!("No user found matching '{}'", user_ref);
                }
                Err(e) => {
                    info!("Failed to fetch guild members for {}: {}", guild_id, e);
                }
            }
        }

        info!("Could not resolve username '{}' to user ID", user_ref);
        None
    }

    /// Set a default channel for messages without specific targets
    pub fn with_default_channel(mut self, channel_id: u64) -> Self {
        self.default_channel = Some(ChannelId::new(channel_id));
        self
    }

    /// Set a default DM user for messages without specific targets
    pub fn with_default_dm_user(mut self, user_id: u64) -> Self {
        self.default_dm_user = Some(DiscordUserId::new(user_id));
        self
    }

    /// Extract text content from a Pattern message
    fn extract_text(message: &Message) -> String {
        match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => "[Non-text content]".to_string(),
        }
    }

    /// Check if a string is likely a Discord reaction
    fn is_discord_reaction(text: &str) -> bool {
        let trimmed = text.trim();

        // Log what we're checking
        debug!(
            "Checking if '{}' (len {}, chars {}) is a reaction",
            trimmed,
            trimmed.len(),
            trimmed.chars().count()
        );

        // Check for standard Discord emoji format :name:
        if trimmed.starts_with(':') && trimmed.ends_with(':') && trimmed.len() > 2 {
            debug!("Detected :name: format emoji");
            return true;
        }

        // Check for custom emoji format <:name:id> or <a:name:id>
        if (trimmed.starts_with("<:") || trimmed.starts_with("<a:")) && trimmed.ends_with('>') {
            debug!("Detected custom emoji format");
            return true;
        }

        // Check for unicode emoji (single character or with variation selectors)
        // Allow up to 4 chars for variation selectors and zero-width joiners
        if trimmed.chars().count() <= 4 {
            for ch in trimmed.chars() {
                // Basic emoji ranges
                if (ch >= '\u{1F300}' && ch <= '\u{1F9FF}') || // Misc symbols & pictographs
                   (ch >= '\u{2600}' && ch <= '\u{26FF}') ||   // Misc symbols
                   (ch >= '\u{2700}' && ch <= '\u{27BF}') ||   // Dingbats
                   (ch >= '\u{1F600}' && ch <= '\u{1F64F}') ||  // Emoticons
                   (ch >= '\u{1F900}' && ch <= '\u{1F9FF}') ||  // Supplemental symbols
                   (ch >= '\u{2000}' && ch <= '\u{206F}')
                // General punctuation (includes some emoji)
                {
                    debug!("Detected unicode emoji");
                    return true;
                }
            }
        }

        debug!("Not detected as emoji");
        false
    }

    /// Parse a Discord emoji string into a ReactionType
    fn parse_discord_emoji(emoji_str: &str) -> serenity::model::channel::ReactionType {
        let trimmed = emoji_str.trim();

        // Check for custom emoji format :name:id or <:name:id>
        if trimmed.starts_with("<:") && trimmed.ends_with('>') {
            // Parse custom emoji <:name:id>
            let inner = &trimmed[2..trimmed.len() - 1];
            if let Some(colon_pos) = inner.rfind(':') {
                if let Ok(id) = inner[colon_pos + 1..].parse::<u64>() {
                    let name = inner[..colon_pos].to_string();
                    return serenity::model::channel::ReactionType::Custom {
                        animated: false,
                        id: serenity::model::id::EmojiId::new(id),
                        name: Some(name),
                    };
                }
            }
        }

        // Check for animated custom emoji <a:name:id>
        if trimmed.starts_with("<a:") && trimmed.ends_with('>') {
            let inner = &trimmed[3..trimmed.len() - 1];
            if let Some(colon_pos) = inner.rfind(':') {
                if let Ok(id) = inner[colon_pos + 1..].parse::<u64>() {
                    let name = inner[..colon_pos].to_string();
                    return serenity::model::channel::ReactionType::Custom {
                        animated: true,
                        id: serenity::model::id::EmojiId::new(id),
                        name: Some(name),
                    };
                }
            }
        }

        // Check for simple :name: format (needs special handling)
        if trimmed.starts_with(':') && trimmed.ends_with(':') && trimmed.len() > 2 {
            // Keep the colons for Discord to interpret
            // Discord will handle converting :thumbsup: to 👍 or finding the custom emoji
            return serenity::model::channel::ReactionType::Unicode(trimmed.to_string());
        }

        // Otherwise treat as unicode emoji
        serenity::model::channel::ReactionType::Unicode(trimmed.to_string())
    }

    /// Check if a channel is a DM and validate against admin_users if applicable
    async fn validate_channel_access(&self, channel_id: ChannelId) -> Result<()> {
        if let Some(ref bot) = self.bot {
            // Try to get channel info to determine type
            match self.http.get_channel(channel_id).await {
                Ok(channel) => {
                    use serenity::model::channel::Channel;
                    match channel {
                        Channel::Private(private_channel) => {
                            // This is a DM channel - validate against admin_users
                            if let Some(admin_users) = &bot.config().admin_users {
                                // Get the recipient user ID
                                let recipient_id = private_channel.recipient.id.to_string();

                                // Check if recipient is in admin_users
                                if !admin_users.contains(&recipient_id) {
                                    return Err(pattern_core::CoreError::ToolExecutionFailed {
                                        tool_name: "discord_endpoint".to_string(),
                                        cause: format!(
                                            "DM channel {} with recipient {} not in admin_users; delivery blocked",
                                            channel_id, recipient_id
                                        ),
                                        parameters: serde_json::json!({
                                            "channel_id": channel_id.get(),
                                            "recipient_id": recipient_id,
                                        }),
                                    });
                                }
                            }
                            // DM channel with no admin_users configured - allow
                        }
                        Channel::Guild(_) | _ => {
                            // This is a guild channel or other non-DM channel type - validate against allowed_channels
                            if let Some(allowed) = &bot.config().allowed_channels {
                                let ok = allowed.iter().any(|s| s == &channel_id.to_string());
                                if !ok {
                                    return Err(pattern_core::CoreError::ToolExecutionFailed {
                                        tool_name: "discord_endpoint".to_string(),
                                        cause: format!(
                                            "Channel {} not in allowed_channels; delivery blocked",
                                            channel_id
                                        ),
                                        parameters: serde_json::json!({ "channel_id": channel_id.get() }),
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    // If we can't get channel info, be conservative and check allowed_channels
                    warn!("Failed to get channel info for {}: {}", channel_id, e);
                    if let Some(allowed) = &bot.config().allowed_channels {
                        let ok = allowed.iter().any(|s| s == &channel_id.to_string());
                        if !ok {
                            return Err(pattern_core::CoreError::ToolExecutionFailed {
                                tool_name: "discord_endpoint".to_string(),
                                cause: format!(
                                    "Channel {} not validated; couldn't get channel info: {}",
                                    channel_id, e
                                ),
                                parameters: serde_json::json!({ "channel_id": channel_id.get() }),
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Send a message to a specific Discord channel
    async fn send_to_channel(
        &self,
        channel_id: ChannelId,
        mut content: String,
        origin: Option<&MessageOrigin>,
    ) -> Result<()> {
        info!(
            "send_to_channel called with content: '{}', is_reaction: {}",
            content,
            Self::is_discord_reaction(&content)
        );

        // Check if this is just an emoji - if so, try to add it as a reaction to the last message
        if Self::is_discord_reaction(&content) {
            // Try to get the last message in the channel (excluding our own)
            match channel_id
                .messages(&self.http, serenity::builder::GetMessages::new().limit(10))
                .await
            {
                Ok(messages) => {
                    // Find the first message that's not from us
                    if let Ok(current_user) = self.http.get_current_user().await {
                        for msg in messages {
                            if msg.author.id != current_user.id {
                                // For :name: format, try to find the custom emoji in the guild
                                let mut reaction_type = Self::parse_discord_emoji(&content);

                                // If it's :name: format, try to resolve it to a custom emoji
                                if content.trim().starts_with(':') && content.trim().ends_with(':')
                                {
                                    let emoji_name = content
                                        .trim()
                                        .trim_start_matches(':')
                                        .trim_end_matches(':');

                                    // Get guild ID from the message
                                    if let Some(guild_id) = msg.guild_id {
                                        // Try to get guild emojis
                                        if let Ok(emojis) = self.http.get_emojis(guild_id).await {
                                            // Find emoji by name
                                            if let Some(emoji) =
                                                emojis.iter().find(|e| e.name == emoji_name)
                                            {
                                                info!(
                                                    "Found custom emoji {} with ID {}",
                                                    emoji_name, emoji.id
                                                );
                                                reaction_type = serenity::model::channel::ReactionType::Custom {
                                                    animated: emoji.animated,
                                                    id: emoji.id,
                                                    name: Some(emoji.name.clone()),
                                                };
                                            } else {
                                                debug!(
                                                    "Custom emoji '{}' not found in guild",
                                                    emoji_name
                                                );
                                            }
                                        }
                                    }
                                }

                                info!(
                                    "Adding reaction {:?} to message {} in channel {}",
                                    reaction_type, msg.id, channel_id
                                );

                                // Try to add the reaction
                                match msg.react(&self.http, reaction_type).await {
                                    Ok(_) => {
                                        info!("Successfully added reaction");
                                        return Ok(());
                                    }
                                    Err(e) => {
                                        warn!("Failed to add reaction: {}", e);
                                        // Continue to try as regular message
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("Couldn't fetch messages to react to: {}", e);
                }
            }
        }

        // If this is a DM channel, add facet tag for clarity
        if let Ok(channel) = channel_id.to_channel(&self.http).await {
            if matches!(channel, serenity::model::channel::Channel::Private(_)) {
                content = Self::dm_tagged_content(&content, origin);
            }
        }

        // Fall back to sending as regular message with timeout
        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            channel_id.say(&self.http, &content),
        )
        .await
        {
            Ok(Ok(_)) => {
                info!("Successfully sent message to channel {}", channel_id);
            }
            Ok(Err(e)) => {
                tracing::error!("Discord API error for channel {}: {}", channel_id, e);
                return Err(pattern_core::CoreError::ToolExecutionFailed {
                    tool_name: "discord_endpoint".to_string(),
                    cause: format!("Failed to send message to channel: {}", e),
                    parameters: serde_json::json!({
                        "channel_id": channel_id.get(),
                        "content_length": content.len(),
                        "error": e.to_string()
                    }),
                });
            }
            Err(_) => {
                tracing::error!(
                    "Discord API TIMEOUT for channel {} after 10 seconds",
                    channel_id
                );
                return Err(pattern_core::CoreError::ToolExecutionFailed {
                    tool_name: "discord_endpoint".to_string(),
                    cause: format!(
                        "Discord API call timed out after 10 seconds for channel {}",
                        channel_id
                    ),
                    parameters: serde_json::json!({
                        "channel_id": channel_id.get(),
                        "content_length": content.len(),
                        "timeout": "10s"
                    }),
                });
            }
        }

        info!("Sent message to Discord channel {}", channel_id);
        Ok(())
    }

    /// Send a DM to a specific Discord user
    async fn send_dm(&self, user_id: DiscordUserId, content: String) -> Result<()> {
        // Create a DM channel with the user
        let dm_channel = user_id.create_dm_channel(&self.http).await.map_err(|e| {
            pattern_core::CoreError::ToolExecutionFailed {
                tool_name: "discord_endpoint".to_string(),
                cause: format!("Failed to create DM channel: {}", e),
                parameters: serde_json::json!({
                    "user_id": user_id.get(),
                }),
            }
        })?;

        if content.len() >= 8192 {
            let messages = crate::bot::split_message(&content, 8192);
            for message in messages {
                // Send the message
                dm_channel.say(&self.http, &message).await.map_err(|e| {
                    pattern_core::CoreError::ToolExecutionFailed {
                        tool_name: "discord_endpoint".to_string(),
                        cause: format!("Failed to send DM: {}", e),
                        parameters: serde_json::json!({
                            "user_id": user_id.get(),
                            "content_length": content.len()
                        }),
                    }
                })?;
            }
        } else {
            // Send the message
            dm_channel.say(&self.http, &content).await.map_err(|e| {
                pattern_core::CoreError::ToolExecutionFailed {
                    tool_name: "discord_endpoint".to_string(),
                    cause: format!("Failed to send DM: {}", e),
                    parameters: serde_json::json!({
                        "user_id": user_id.get(),
                        "content_length": content.len()
                    }),
                }
            })?;
        }

        info!("Sent DM to Discord user {}", user_id);
        Ok(())
    }
}

#[async_trait::async_trait]
impl MessageEndpoint for DiscordEndpoint {
    async fn send(
        &self,
        message: Message,
        metadata: Option<Value>,
        origin: Option<&MessageOrigin>,
    ) -> Result<Option<String>> {
        let content = Self::extract_text(&message);

        info!(
            "Discord endpoint send() called with metadata: {:?}",
            metadata
        );

        // Check metadata for routing information
        if let Some(ref meta) = metadata {
            // Check if we should reply to a specific message (for delayed responses)
            let reply_to_id = meta
                .get("discord_message_id")
                .or_else(|| meta.get("custom").and_then(|v| v.get("discord_message_id")))
                .and_then(|v| v.as_u64());

            // First check for explicit channel_id (highest priority)
            if let Some(channel_id) = meta.get("target_id").and_then(|v| v.as_u64()) {
                let channel = ChannelId::new(channel_id);

                // Validate channel access (handles both DM and guild channels)
                self.validate_channel_access(channel).await?;

                // If we have a message to reply to and response is delayed, use reply
                if let Some(msg_id) = reply_to_id {
                    // Check if this is a delayed response
                    // First check metadata (for batched messages)
                    let mut should_reply = meta
                        .get("response_delay_ms")
                        .and_then(|v| v.as_u64())
                        .map(|delay| delay > 30000)
                        .unwrap_or(false);

                    // If not in metadata, check bot's current processing time
                    if !should_reply {
                        if let Some(ref bot) = self.bot {
                            if let Some(duration) = bot.get_current_processing_time().await {
                                let elapsed = duration.as_millis() as u64;
                                should_reply = elapsed > 30000;
                                if should_reply {
                                    debug!(
                                        "Using reply threading: message processing took {}ms",
                                        elapsed
                                    );
                                }
                            }
                        }
                    }

                    if should_reply {
                        // Use reply for delayed responses
                        if let Ok(original_msg) = self
                            .http
                            .get_message(channel, serenity::model::id::MessageId::new(msg_id))
                            .await
                        {
                            if let Err(e) = original_msg.reply(&self.http, &content).await {
                                warn!(
                                    "Failed to reply to message: {}, falling back to channel send",
                                    e
                                );
                                self.send_to_channel(channel, content, origin).await?;
                            } else {
                                info!("Replied to message {} in channel {}", msg_id, channel_id);
                                return Ok(Some(format!("reply:{}:{}", channel_id, msg_id)));
                            }
                        } else {
                            // Can't find original message, just send to channel
                            self.send_to_channel(channel, content, origin).await?;
                        }
                    } else {
                        self.send_to_channel(channel, content, origin).await?;
                    }
                } else {
                    self.send_to_channel(channel, content, origin).await?;
                }
                return Ok(Some(format!("channel:{}", channel_id)));
            }

            // Check if target_id contains a channel name to resolve
            if let Some(target_id) = meta.get("target_id").and_then(|v| v.as_str()) {
                if let Some(channel_id) = self.resolve_channel_id(target_id).await {
                    // Enforce allowed_channels whitelist if configured on bot
                    if let Some(ref bot) = self.bot {
                        if let Some(allowed) = &bot.config().allowed_channels {
                            let ok = allowed.iter().any(|s| s == &channel_id.to_string());
                            if !ok {
                                return Err(pattern_core::CoreError::ToolExecutionFailed {
                                    tool_name: "discord_endpoint".to_string(),
                                    cause: format!(
                                        "Channel {} not in allowed_channels; delivery blocked",
                                        channel_id
                                    ),
                                    parameters: serde_json::json!({ "channel_id": channel_id }),
                                });
                            }
                        }
                    }
                    self.send_to_channel(channel_id, content, origin).await?;
                    return Ok(Some(format!("channel:{}", channel_id)));
                }

                // If channel resolution failed, try user resolution for DMs
                if let Some(user_id) = self.resolve_user_id(target_id).await {
                    let tagged = Self::dm_tagged_content(&content, origin);
                    self.send_dm(user_id, tagged).await?;
                    return Ok(Some(format!("dm:{}", user_id)));
                }
            }

            // Then check custom metadata (from incoming discord message)
            if let Some(custom) = meta.get("custom").and_then(|v| v.as_object()) {
                if let Some(channel_id) = custom.get("discord_channel_id").and_then(|v| v.as_u64())
                {
                    // Enforce allowed_channels whitelist if configured on bot
                    if let Some(ref bot) = self.bot {
                        if let Some(allowed) = &bot.config().allowed_channels {
                            let ok = allowed.iter().any(|s| s == &channel_id.to_string());
                            if !ok {
                                return Err(pattern_core::CoreError::ToolExecutionFailed {
                                    tool_name: "discord_endpoint".to_string(),
                                    cause: format!(
                                        "Channel {} not in allowed_channels; delivery blocked",
                                        channel_id
                                    ),
                                    parameters: serde_json::json!({ "channel_id": channel_id }),
                                });
                            }
                        }
                    }
                    self.send_to_channel(ChannelId::new(channel_id), content, origin)
                        .await?;
                    return Ok(Some(format!("channel:{}", channel_id)));
                }
            }

            // Finally check for user_id to send DM (lowest priority)
            if let Some(user_id) = meta.get("discord_user_id").and_then(|v| v.as_u64()) {
                let tagged = Self::dm_tagged_content(&content, origin);
                self.send_dm(DiscordUserId::new(user_id), tagged).await?;
                return Ok(Some(format!("dm:{}", user_id)));
            }

            // Check for reply context
            if let Some(reply_to) = meta.get("reply_to_message_id").and_then(|v| v.as_u64()) {
                debug!(
                    "Reply context present but not implemented yet: {}",
                    reply_to
                );
            }
        }

        // Check if origin provides Discord context
        if let Some(MessageOrigin::Discord {
            channel_id,
            user_id,
            ..
        }) = origin
        {
            // Prefer channel if both are present (came from a channel message)
            if let Ok(chan_id) = channel_id.parse::<u64>() {
                self.send_to_channel(ChannelId::new(chan_id), content, origin)
                    .await?;
                return Ok(Some(format!("channel:{}", chan_id)));
            } else if let Ok(usr_id) = user_id.parse::<u64>() {
                let tagged = Self::dm_tagged_content(&content, origin);
                self.send_dm(DiscordUserId::new(usr_id), tagged).await?;
                return Ok(Some(format!("dm:{}", usr_id)));
            }
        }

        // Fall back to default DM user if configured
        if let Some(user) = self.default_dm_user {
            let tagged = Self::dm_tagged_content(&content, origin);
            self.send_dm(user, tagged).await?;
            return Ok(Some(format!("default_dm:{}", user)));
        }

        // Fall back to default channel if configured
        if let Some(channel) = self.default_channel {
            self.send_to_channel(channel, content, origin).await?;
            return Ok(Some(format!("default_channel:{}", channel)));
        }

        warn!("No Discord destination found in metadata or origin");
        Err(pattern_core::CoreError::ToolExecutionFailed {
            tool_name: "discord_endpoint".to_string(),
            cause: "No Discord destination specified".to_string(),
            parameters: serde_json::json!({
                "has_metadata": metadata.is_some(),
                "has_origin": origin.is_some(),
                "has_default_dm": self.default_dm_user.is_some(),
                "has_default_channel": self.default_channel.is_some(),
            }),
        })
    }

    fn endpoint_type(&self) -> &'static str {
        "discord"
    }
}
