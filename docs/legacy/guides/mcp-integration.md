# MCP Integration Guide

This document covers all MCP (Model Context Protocol) integration details for Pattern.

## MCP Tools Available

Claude Code has access to several MCP (Model Context Protocol) tools that enhance development workflows:

### Context7 Tools
- **`mcp__context7__resolve-library-id`**: Resolves package names to Context7 library IDs
  - Use when you need to look up documentation for external libraries
  - Returns matching libraries with trust scores and documentation coverage
- **`mcp__context7__get-library-docs`**: Fetches library documentation
  - Requires library ID from resolve-library-id
  - Can focus on specific topics within documentation
  - Default 10k token limit (configurable)

### Language Server Tools (rust-analyzer)
- **`mcp__language-server__definition`**: Jump to symbol definitions
  - Find where functions, types, constants are implemented
  - Requires qualified symbol names (e.g., `pattern::server::PatternServer`)
- **`mcp__language-server__diagnostics`**: Get file diagnostics
  - Shows errors, warnings from rust-analyzer
  - Use before/after edits to verify code health
- **`mcp__language-server__edit_file`**: Batch file edits
  - Apply multiple line-based edits efficiently
  - Alternative to standard Edit tool for complex changes
- **`mcp__language-server__hover`**: Get type info and docs
  - Position-based (file, line, column)
  - Shows types, trait implementations, documentation
- **`mcp__language-server__references`**: Find all symbol usages
  - Critical before refactoring to understand impact
  - Returns all locations where symbol appears
- **`mcp__language-server__rename_symbol`**: Safe symbol renaming
  - Updates symbol and all references automatically
  - Position-based operation

## MCP Workflow Examples

### 1. Researching External Dependencies
```
1. User asks about using a library (e.g., "How do I use tokio channels?")
2. mcp__context7__resolve-library-id("tokio") → Get library ID
3. mcp__context7__get-library-docs(id, topic="channels") → Get relevant docs
4. Implement based on official documentation
```

**Note on letta-rs documentation:**
- The letta crate may not be on Context7 yet
- Alternative sources:
  - Read source directly: `../letta-rs/src/` (we have local access)
  - Generate docs: `cd ../letta-rs && cargo doc --open`
  - Check docs.rs if published
  - Use language server tools to explore the API

### 2. Understanding Existing Code
```
1. Find a function call you don't understand
2. mcp__language-server__hover(file, line, col) → Get quick info
3. mcp__language-server__definition("module::function") → See implementation
4. mcp__language-server__references("module::function") → See usage patterns
```

### 3. Safe Refactoring
```
1. mcp__language-server__references("OldName") → Assess impact
2. mcp__language-server__rename_symbol(file, line, col, "NewName") → Rename everywhere
3. mcp__language-server__diagnostics(file) → Verify no breakage
4. cargo test → Ensure tests still pass
```

### 4. Fixing Compilation Errors
```
1. cargo check → Initial error list
2. mcp__language-server__diagnostics(file) → Detailed diagnostics with context
3. mcp__language-server__hover on error locations → Understand type mismatches
4. mcp__language-server__edit_file → Apply fixes
5. mcp__language-server__diagnostics(file) → Verify fixes
```

### 5. Implementing New Features
```
1. mcp__context7 tools → Research library APIs if using external deps
2. Glob/Grep → Find similar patterns in codebase
3. mcp__language-server__definition → Understand interfaces to implement
4. Write implementation
5. mcp__language-server__diagnostics → Catch issues early
6. cargo test → Verify functionality
```

See @LSP_EDIT_GUIDE.md for details on potential pitfalls.

## MCP Server Implementation

Using the official `rmcp` SDK from git (see [MCP_SDK_GUIDE.md](./MCP_SDK_GUIDE.md) for detailed patterns):

```rust
use rmcp::{
    handler::server::tool::Parameters,
    model::{CallToolResult, Content, Implementation, InitializeResult as ServerInfo},
    Error as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Clone)]
pub struct PatternMcpServer {
    letta_client: Arc<letta::LettaClient>,
    db: Arc<Database>,
    multi_agent_system: Arc<MultiAgentSystem>,
}

// Request structs need JsonSchema derive
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
struct ScheduleEventRequest {
    user_id: i64,
    title: String,
    start_time: String,
    description: Option<String>,
    duration_minutes: Option<u32>,
}

// Define tools using rmcp macros
#[rmcp::tool_router]
impl PatternMcpServer {
    #[rmcp::tool(description = "Schedule an event with smart time estimation")]
    async fn schedule_event(
        &self,
        params: Parameters<ScheduleEventRequest>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        // Implementation with ADHD time multipliers
        Ok(CallToolResult::success(vec![Content::text(
            "Event scheduled with ADHD-aware buffers"
        )]))
    }

    #[rmcp::tool(description = "Send message to user via Discord")]
    async fn send_message(
        &self,
        params: Parameters<SendMessageRequest>,
    ) -> Result<CallToolResult, McpError> {
        // Send via Discord bot instance
        Ok(CallToolResult::success(vec![Content::text(
            "Message sent via Discord"
        )]))
    }

    #[rmcp::tool(description = "Check activity state for interruption timing")]
    async fn check_activity_state(&self) -> Result<CallToolResult, McpError> {
        // Platform-specific activity monitoring
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json!({
                "interruptible": true,
                "current_focus": null,
                "last_activity": "idle"
            })).unwrap()
        )]))
    }
}
```

## MCP Integration Issues & Solutions

### Current Status (2025-07-03)
We discovered that Letta's Python MCP client has a critical bug with async task management. When trying to use the streamable HTTP transport:
1. Initial MCP server registration succeeds
2. But listing tools fails with `anyio.BrokenResourceError`
3. The error is: `RuntimeError: Attempted to exit cancel scope in a different task than it was entered in`
4. This is a known Python asyncio issue when cancel scopes cross task boundaries

### Solution Implemented: SSE Transport ✅

We've successfully implemented SSE (Server-Sent Events) transport as a workaround:
- SSE is deprecated in MCP spec but still works with Letta
- Added `mcp-sse` feature flag to enable SSE transport
- Configuration: set `mcp.transport = "sse"` and `mcp.port = 8081` in pattern.toml
- The server listens on `http://localhost:8081/sse` (and `/message` for POST)
- Implementation uses rmcp's `SseServer` with proper configuration

#### Alternative Option: Stdio-to-HTTP Proxy
If SSE doesn't work with Letta, we could create a small binary `pattern-mcp-stdio` that:
- Implements the MCP stdio transport (which Letta knows how to use)
- Forwards all requests to Pattern's main HTTP API
- Acts as a thin translation layer
- Benefits: Letta's stdio support is more mature than HTTP/SSE
- Can be a simple 100-line binary

### Key Learnings
1. **Path mismatch**: Initially told Letta to use `/mcp` but streamable HTTP serves at root
2. **Accept headers**: Streamable HTTP requires `Accept: application/json, text/event-stream`
3. **Persistent connections**: The protocol expects long-lived connections, not request/response
4. **Letta compatibility**: Letta's Python client works better with SSE than streamable HTTP

## Current MCP Tools Implemented

1. **chat_with_agent** - Send messages to Letta agents
2. **get_agent_memory** - Retrieve agent memory blocks
3. **update_agent_memory** - Update agent memory (using blocks API workaround)
4. **schedule_event** - Schedule events with ADHD time multipliers
5. **send_message** - Send messages via available channels
6. **check_activity_state** - Check system activity for interruption timing
7. **send_discord_message** - Send message to Discord channel
8. **send_discord_embed** - Send rich embed to Discord
9. **get_channel_info** - Get Discord channel details
10. **send_discord_dm** - Send direct message to Discord user

## Running Pattern with MCP

```bash
# Full mode (Discord + MCP + background tasks)
cargo run --features full

# Just MCP with SSE transport
cargo run --features binary,mcp,mcp-sse

# MCP with stdio transport (default)
cargo run --features binary,mcp
```

## References

- [MCP SDK Implementation Guide](./MCP_SDK_GUIDE.md) - Detailed patterns for using the official SDK
- [Official MCP Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) - Use git version only
- [MCP Specification](https://modelcontextprotocol.io/specification/2025-06-18)
- [LSP Edit Guide](./LSP_EDIT_GUIDE.md) - Language server pitfalls and patterns
