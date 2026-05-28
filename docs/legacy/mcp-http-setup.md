# Pattern MCP HTTP Transport Setup

This guide explains how to configure Pattern to use the HTTP transport for MCP (Model Context Protocol) instead of stdio.

## Why HTTP Transport?

- **Persistent connections**: Pattern runs as a long-lived service, making HTTP more suitable than stdio
- **Integration with Letta**: Letta can connect to Pattern's MCP tools via HTTP
- **Multiple clients**: HTTP allows multiple Letta agents to use Pattern's tools simultaneously

## Configuration

### 1. Environment Variables

Copy `.env.http.example` to `.env`:

```bash
cp .env.http.example .env
```

Key settings:
```bash
MCP_ENABLED=true
MCP_TRANSPORT=http
MCP_PORT=8080  # Default port
```

### 2. Running Pattern with HTTP MCP

```bash
# With all features
cargo run --features full

# Or just MCP + Discord
cargo run --features binary,mcp,discord
```

### 3. Verify MCP Server is Running

You should see in the logs:
```
INFO pattern: Starting MCP server on http
INFO pattern::server: Starting MCP server on HTTP transport at port 8080
INFO pattern::server: MCP HTTP server listening on http://0.0.0.0:8080
INFO pattern::server: Configure Letta to use: http://localhost:8080/mcp
INFO pattern::agents: MCP server configured with 10 tools
```

### 4. Letta Configuration

Pattern automatically configures Letta to use the MCP server when HTTP transport is enabled. The configuration happens in `MultiAgentSystem::configure_mcp_server()`.

Letta will be configured with:
- Server name: `pattern-discord`
- Server URL: `http://localhost:8080/mcp`
- Transport: Streamable HTTP

### 5. Available MCP Tools

When the server starts, these tools are available to Letta agents:

- `chat_with_agent` - Send messages to Pattern agents
- `get_agent_memory` - Retrieve agent memory
- `update_agent_memory` - Update agent memory blocks
- `schedule_event` - Schedule events with ADHD-aware time estimation
- `send_message` - Send messages via Discord
- `check_activity_state` - Check user activity for interruption timing
- `send_discord_message` - Send messages to Discord channels
- `send_discord_embed` - Send rich embeds to Discord
- `get_discord_channel_info` - Get Discord channel information
- `send_discord_dm` - Send direct messages to Discord users

### 6. Testing the Setup

1. Start Pattern with HTTP MCP enabled
2. Create a Letta agent (Discord or via API)
3. The agent should automatically have access to Pattern's MCP tools
4. Try sending a message that uses Discord tools:
   ```
   "Send a message to #general saying hello from Pattern!"
   ```

### 7. Troubleshooting

- **Port already in use**: Change `MCP_PORT` to a different value
- **Connection refused**: Ensure Pattern is running and MCP is enabled
- **Tools not available**: Check that Letta successfully connected to the MCP server in the logs
- **Discord tools failing**: Ensure Discord bot is also running (`DISCORD_TOKEN` is set)
- **"MCP server already exists" error**: Run `./cleanup_mcp.sh` before restarting Pattern, or the error is harmless if Pattern continues running
- **"Internal Server Error" when listing tools**: This happens if Letta tries to connect before the HTTP server is ready. Pattern now waits 500ms before configuring Letta to avoid this.

### 9. Understanding the Startup Sequence

When Pattern starts with HTTP MCP transport:

1. Pattern starts the HTTP MCP server on the configured port
2. After a 3-second delay, Pattern attempts to register itself with Letta
3. Pattern will retry registration up to 3 times with increasing delays (5s, 7s)
4. If registration fails, the MCP server continues running and can be registered manually

You can verify the MCP server is running by checking:
- Pattern logs show: `MCP HTTP server listening on http://0.0.0.0:8080`
- Letta ADE shows the "pattern-discord" MCP server in settings

### 10. Known Issues with Letta Performance

Letta can be slow to respond, especially when:
- Processing large agent memories
- Handling multiple concurrent requests
- Building up a backlog of operations

This can cause:
- "Internal Server Error" when listing MCP tools
- Connection timeouts or "IncompleteMessage" errors
- Long delays before tools become available

**Workarounds:**
1. Use `./register_mcp_manually.sh` to register the MCP server when Letta is ready
2. Wait a few minutes after registration before trying to use tools
3. Check Letta's logs for processing delays
4. Consider restarting Letta if it becomes unresponsive

### 8. Advanced Configuration

You can also use a TOML config file:

```toml
[mcp]
enabled = true
transport = "http"
port = 8080
```

Environment variables override TOML settings.