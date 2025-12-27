# CLAUDE.md - Pattern Discord

Discord bot integration for the Pattern system, enabling multi-agent support through Discord's chat interface.

## Current Status

### ✅ Working Features
- **Message handling**: Full processing with batching, merging, and queue management
- **Typing indicators**: Auto-refresh every 8 seconds during processing
- **Reaction handling**: Process reactions on bot messages as new inputs
- **Group integration**: Routes messages to agent groups with coordination patterns
- **Data source**: Discord messages can be ingested as data source events
- **Basic commands**: `/chat`, `/status`, `/memory`, `/help` partially implemented

### 🚧 In Progress
- **Slash commands**: Core structure exists but many commands need implementation
- **Command routing**: Natural language command detection planned

## Architecture Overview

### Key Components

1. **Bot Core** (`bot.rs`)
   - Serenity framework integration (1400+ lines of sophisticated bot code)
   - Event handling (messages, reactions, joins)
   - Message queue with smart batching
   - Connection resilience and error recovery

2. **Message Processing**
   - Smart merging of rapid messages from same user
   - Queue management with configurable delays
   - Typing indicator management
   - Reaction buffering during processing

3. **Integration Points**
   - Agent group routing based on content
   - CLI mode for terminal-based testing
   - Data source mode for event ingestion
   - Endpoint registration for agent messaging

## Discord-Specific Patterns

### Message Handling
```rust
// Always check context
match message.channel_id.to_channel(&ctx).await? {
    Channel::Private(_) => handle_dm(message),
    Channel::Guild(channel) => handle_guild_message(message, channel),
    _ => Ok(()), // Ignore other channel types
}

// Respect rate limits
if let Err(why) = message.reply(&ctx, response).await {
    if why.to_string().contains("rate limit") {
        // Queue for later or drop gracefully
    }
}
```

### User Context
```rust
pub struct DiscordContext {
    pub user_id: UserId,
    pub username: String,
    pub guild_id: Option<GuildId>,
    pub channel_id: ChannelId,
    pub is_dm: bool,
    pub recent_messages: Vec<Message>,
    pub user_timezone: Option<Tz>,
}
```

### Agent Selection
```rust
// Keywords for routing
const CRISIS_KEYWORDS: &[&str] = &["emergency", "crisis", "help", "panic"];
const PLANNING_KEYWORDS: &[&str] = &["plan", "schedule", "organize", "todo"];
const MEMORY_KEYWORDS: &[&str] = &["remember", "recall", "forgot", "memory"];

// Route based on content analysis
fn select_agent_group(content: &str) -> AgentGroup {
    let lower = content.to_lowercase();
    
    if CRISIS_KEYWORDS.iter().any(|&kw| lower.contains(kw)) {
        return AgentGroup::Crisis;
    }
    // ... other checks
    
    AgentGroup::Main // default
}
```

### Defaults and Naming
- Agent names are arbitrary; behavior is driven by group roles:
  - Supervisor: preferred default for slash commands when no agent is specified.
  - Specialist domains: `system_integrity` and `memory_management` map to specific tool availability.
- Bot self-mentions are rewritten to `@<supervisor_name>` when a supervisor is present in the current group context.

## Implementation Features

### Message Queue System
- **Smart batching**: Merges rapid messages from same user
- **Configurable delays**: `DISCORD_BATCH_DELAY_MS` (default 1500ms)
- **Max message size**: Splits responses over 2000 chars
- **Reaction buffering**: Stores reactions during processing

### Processing State Management
- Tracks current message ID and start time
- Prevents concurrent processing
- Buffers reactions during busy state
- Cleans up typing indicators on completion

### Error Handling
- Graceful degradation on API failures
- Connection resilience with auto-reconnect
- Rate limit awareness
- Comprehensive logging


## Testing

Run with Discord integration:
```bash
pattern-cli chat --discord
pattern-cli chat --discord --group main
```

## Privacy & Security

- DM content isolation from public channels
- User permission checking
- No message content logging in production
- Rate limit compliance

## Future Enhancements

- Natural language command parsing
- Ephemeral responses for sensitive data
- Voice channel support
- Embed formatting for rich responses
- Thread management
- Role-based access control
