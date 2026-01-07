# Discord Integration Issues & Solutions

This document tracks known issues with Discord integration and their solutions.

## Inter-Agent Communication Issues & Fixes

**Critical Issues Discovered (2025-07-03)**

We discovered several critical issues with inter-agent communication that can cause infinite loops and confusing behavior:

### Problems Identified

1. **System Message Routing Issues**
   - System messages from tools appear as user messages in the recipient agent's context
   - Agents can't distinguish between user requests and system-generated messages
   - Example: When Pattern sends a message via tool, Archive sees it as a user request

2. **Broadcast Message Loops**
   - Messages broadcast to all agents (e.g., "Message sent to all agents") trigger responses from everyone
   - Responding agents may invoke tools that broadcast again, creating loops
   - No filtering mechanism to prevent agents from responding to system notifications

3. **Self-Echo Problem**
   - Agents receive their own tool invocation results as new messages
   - Tool results like "Message successfully sent" appear as new user input
   - Agents may respond to their own success confirmations

4. **The Pattern Emoji Explosion Incident**
   - Pattern was asked to use emojis to express itself
   - It used `send_discord_message` tool with emoji content
   - The tool's success message ("Message sent to channel") appeared as new user input
   - Pattern interpreted this as encouragement and sent more emojis
   - Created an infinite loop of increasingly enthusiastic emoji messages
   - Only stopped when the user explicitly said "STOP"

### Proposed Solutions

1. **Message Tagging System**
   ```rust
   enum MessageSource {
       User(UserId),
       System,
       Agent(AgentId),
       Tool(ToolName),
   }
   
   struct TaggedMessage {
       content: String,
       source: MessageSource,
       is_broadcast: bool,
       requires_response: bool,
   }
   ```

2. **Tool Response Filtering**
   - Tool results should be marked as non-conversational
   - Agents should ignore messages marked as `MessageSource::Tool`
   - Success/failure notifications should not appear in conversation history

3. **Broadcast Handling Rules**
   - Broadcast messages should be marked with `requires_response: false`
   - Only the orchestrating agent (Pattern) should handle broadcast responses
   - Other agents should acknowledge receipt without generating responses

4. **Agent Communication Protocol**
   ```rust
   // Clear rules for when agents should respond
   impl Agent {
       fn should_respond(&self, msg: &TaggedMessage) -> bool {
           match msg.source {
               MessageSource::User(_) => true,
               MessageSource::Agent(id) if id != self.id => msg.requires_response,
               MessageSource::System => false,
               MessageSource::Tool(_) => false,
               _ => false,
           }
       }
   }
   ```

### Implementation Status
- Issue identified and documented
- Root cause: Letta's message handling doesn't distinguish message sources
- Temporary workaround: Careful prompt engineering to prevent tool loops
- Long-term fix: Implement message tagging system in Pattern's agent layer

**Note**: Inter-agent communication is a desired feature for coordination, but needs proper implementation to prevent chaos. Until fixed, limit direct agent-to-agent messaging and rely on shared memory for coordination.

## Discord Bot Issues & Solutions (2025-07-03)

### Fixed Discord Timeout & Visibility Issues

#### Problems Encountered
1. **Messages were ephemeral by default** - /chat command responses were only visible to the user
2. **Timeouts after first few messages** - Agent responses would timeout after ~60 seconds
3. **Duplicate agent creation** - Same agents being created multiple times for same user
4. **No feedback during long operations** - Users left wondering if bot was working
5. **Missing logs** - Some Discord interactions weren't showing up in logs at all

#### Solutions Implemented

1. **Fixed Ephemeral Messages**
   - Chat messages are now public by default (visible to everyone)
   - Added optional `private` parameter for users who want private conversations
   - Implementation: `CreateInteractionResponseMessage::new().ephemeral(is_private)`

2. **Improved Timeout Handling**
   - Reduced timeout from 60s to 30s (more reasonable for Discord's 15-minute limit)
   - Added progress update after 5 seconds: "🤔 Still thinking... (Letta can be slow sometimes)"
   - Proper timeout error messages instead of generic errors
   - Code uses `tokio::time::timeout` with progress task that gets aborted on completion

3. **Fixed Duplicate Agent Creation**
   - Changed from checking database to checking Letta directly for existing agents
   - Lists all agents and checks by name before creating
   - Prevents race condition where `get_agent_for_user` only returned one agent

4. **Better Logging & Event Handling**
   - Added info logs when slash commands are received
   - Fixed message handler to only respond to DMs and mentions (not all messages)
   - This prevents conflicts and unnecessary processing

5. **Proper User Initialization**
   - Chat command now calls `initialize_user` before sending messages
   - Ensures all agents are created before trying to use them
   - Handles initialization errors gracefully

#### Key Code Changes
```rust
// Discord response visibility
CreateInteractionResponseMessage::new().ephemeral(is_private)

// Timeout with progress updates
let progress_task = tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(5)).await;
    ctx.http.edit_interaction_response(/*...*/)
        .content("🤔 Still thinking...").await;
});

// Check Letta for existing agents
match self.letta.agents().list(None).await {
    Ok(agents) => {
        for agent in agents {
            if agent.name == agent_name {
                return Ok(agent.id);
            }
        }
    }
}
```

## Discord Long-Running Operations

Handle long-running operations with Discord's interaction model:

```rust
use serenity::builder::CreateInteractionResponseMessage;
use serenity::model::application::CommandInteraction;

async fn handle_analysis_command(
    ctx: &Context,
    interaction: &CommandInteraction,
) -> Result<()> {
    // Defer response for long operations
    interaction.create_response(&ctx.http, |r| {
        r.kind(InteractionResponseType::DeferredChannelMessageWithSource)
    }).await?;

    // Perform analysis
    let result = perform_long_analysis().await?;

    // Send followup within 15min window
    interaction.create_followup(&ctx.http, |f| {
        f.content(format!("Analysis complete: {}", result))
    }).await?;

    Ok(())
}
```

## Agent Messaging Loops (Fixed 2025-07-04)

### The Name Prefix Bug
Agents would get stuck in infinite loops when asked to prefix their messages with their names:
- Agent would output: `*Flux:* *Flux:* *Flux:*` indefinitely
- Caused JSON parsing errors and message timeouts
- Root cause: Tool name conflicts and missing terminal rules

### Solution
- Removed generic `send_message` MCP tool that conflicted with Letta defaults
- Updated agent configurations to exclude conflicting tools
- Added proper terminal rules to Discord messaging tools

See [AGENT_LOOPS.md](./AGENT_LOOPS.md) for detailed information.

## Critical Issue: letta/letta-free Model Timeouts

**IMPORTANT**: The default `letta/letta-free` model has severe timeout issues that prevent agents from responding. Messages will timeout and never reach the agent.

### Quick Fix:
```bash
# Use a faster model like Groq (free with API key)
export LETTA_MODEL="groq/llama-3.1-70b-versatile"
export LETTA_EMBEDDING_MODEL="letta/letta-free"
cargo run --features full
```

See [LETTA_MODEL_CONFIG.md](../LETTA_MODEL_CONFIG.md) for detailed setup instructions.