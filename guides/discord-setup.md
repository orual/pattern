# Pattern Discord Bot

The Pattern Discord bot provides a natural language interface to the multi-agent system through Discord.

## Setup

1. Create a Discord application and bot at https://discord.com/developers/applications
2. Copy `.env.discord` to `.env` and add your bot token
3. Invite the bot to your server with appropriate permissions (Send Messages, Read Messages, Use Slash Commands)


## Usage

### Slash Commands

- `/chat [message]` - Chat with the agents
- `/vibe` - Check current system state/vibe
- `/tasks` - List current tasks (coming soon)
- `/memory` - Show shared memory state

### Natural Language Routing

The bot supports flexible agent routing in DMs and channels:

#### Default Agents (configurable)
- `pattern` - Orchestrator/sleeptime agent
- `entropy` - Task management
- `flux` - Time management
- `archive` - Memory/search
- `momentum` - Energy tracking
- `anchor` - Habits

#### Routing Formats

1. **@ mentions**: `@entropy help me break down this task`
2. **Colon prefix**: `flux: when should I schedule this?`
3. **Slash agent**: `/agent momentum how's my energy?`
4. **Default**: Messages without routing go to the orchestrator

### Customizing Agents

You can customize which agents are available by modifying the `MultiAgentSystemBuilder` in `src/bin/discord_bot.rs`:

```rust
// Add custom agents
builder.add_agent(
    "chaos".to_string(),     // Internal ID (used for routing)
    "Chaos".to_string(),     // Display name
    "Task breaker".to_string(), // Description
    "You are Chaos, the task breakdown specialist.".to_string(),
    false, // Not a sleeptime agent
);
```

Agent names are automatically detected for routing - no hardcoded lists!

## Configuration

### Environment Variables

- `DISCORD_TOKEN` (required) - Bot token
- `DISCORD_CHANNEL_ID` (optional) - Limit to specific channel(s). Comma-separated list supported.
- `DISCORD_GUILD_IDS` (optional) - Restrict responses to specific guild(s). Comma-separated list supported. (`DISCORD_GUILD_ID` for a single ID also works)
- `LETTA_BASE_URL` - Letta server URL (default: http://localhost:8000)
- `LETTA_API_KEY` - For Letta cloud instead of local server
- `PATTERN_DB_PATH` - Database location (default: pattern.db)

### Config File

Create `pattern.toml` for persistent configuration (non-sensitive options like channel and admin lists can live here):

```toml
[discord]
# Non-sensitive options (token stays in environment)
# Channels the bot may proactively post to when routing prompts or announcements
allowed_channels = ["1390442382654181477", "1310716219527135363"] # or: "1390442382654181477,1310716219527135363"
# Admin users allowed to use /permit, /deny, /permits
admin_users = ["592429922052472840", "123456789012345678"]     # or: "592429922052472840,123456789012345678"

# Behavior flags (optional)
respond_to_dms = true
respond_to_mentions = true

[letta]
base_url = "http://localhost:8000"
# Or for cloud:
# api_key = "your_api_key"

[database]
path = "pattern.db"
```

## Security Notes

- Never commit `.env` or `pattern.toml` with real tokens
- Use environment variables for sensitive data in production
- The bot requires MESSAGE_CONTENT intent for natural language routing

### Env/Config precedence

- Token (`DISCORD_TOKEN`): environment only (required).
- allowed_channels/admin_users: read from pattern.toml if present; otherwise from env.
  - Environment overrides: `DISCORD_CHANNEL_ID` and `DISCORD_ADMIN_USERS` accept comma-separated lists.
