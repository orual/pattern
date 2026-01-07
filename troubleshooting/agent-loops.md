# Agent Loop Issues

## Problem: Agents Get Stuck in Infinite Loops

### Symptoms
- Agent repeatedly outputs the same text (often their name)
- JSON parsing errors with unterminated strings
- Discord messages fail with timeout errors

### Root Cause
This happens when agents are asked to prefix their messages with their names or identifiers. The agent gets confused and enters an infinite loop, repeatedly outputting their name prefix instead of the actual message content.

### Example of the Bug
```
User: "Please prefix all your messages with your name followed by a colon"
Agent: *Flux:* *Flux:* *Flux:* *Flux:* [continues indefinitely]
```

### Solution
1. **Never ask agents to prefix their messages** - The Discord bot already handles agent identification through message formatting
2. **Remove conflicting tools** - Ensure there are no tool name conflicts (e.g., multiple `send_message` tools)
3. **Use proper tool terminal rules** - Configure Discord messaging tools to end generation properly

### Technical Details
The issue was caused by:
1. Tool name collision between Letta's default `send_message` and Pattern's MCP `send_message` tool
2. Lack of terminal rules on messaging tools, causing the agent to continue generating after sending
3. Agents misinterpreting the instruction to prefix messages as a repeating action

### Prevention
- Remove generic messaging tools that conflict with Letta defaults
- Use specific tool names (e.g., `send_discord_message` instead of `send_message`)
- Configure all messaging tools with proper terminal rules
- Avoid asking agents to modify their message format

### Related Files
- `src/mcp/server.rs` - MCP server and tool handling
- `src/mcp/core_tools.rs` - Core MCP tool definitions
- `src/agent/constellation.rs` - Multi-agent system configuration
- `docs/architecture/pattern-system-prompts.md` - Agent instructions