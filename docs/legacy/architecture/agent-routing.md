# Agent Message Routing Implementation

## What We Built

We've successfully implemented actual agent message routing in Pattern! Messages are now sent to real Letta agents based on the routing rules.

### Key Components

1. **MultiAgentSystem::send_message_to_agent()**
   - Takes user ID, optional agent ID, and message
   - Creates or retrieves the appropriate Letta agent for that user
   - Sends the message and returns the agent's response
   - Each user gets their own instance of each agent type

2. **Agent Creation**
   - Agents are created with format: `{agent_id}_u{user_id}` (e.g., `entropy_u123`)
   - Each agent is initialized with:
     - Its specific system prompt from AgentConfig
     - Shared memory blocks as context
     - User-specific persona

3. **Message Flow**
   ```
   Discord/MCP → parse_agent_routing() → send_message_to_agent() → Letta API → Agent Response
   ```

### Usage Examples

**Discord DM:**
```
User: @entropy help me break down writing a report
Entropy: I'll help you break down that report into manageable chunks...

User: flux: when should I schedule this?
Flux: Looking at typical ADHD time patterns, a "2 hour" report will likely need 4-6 hours...

User: hey pattern
Pattern: Hello! I'm here to help coordinate your cognitive support. What do you need today?
```

**MCP Tools:**
```json
{
  "tool": "chat_with_agent",
  "arguments": {
    "user_id": 123,
    "agent_id": "momentum",
    "message": "How's my focus looking today?"
  }
}
```

### Agent Routing Rules

1. **Discord formats:**
   - `@agent_name message` - Route to specific agent
   - `agent_name: message` - Route to specific agent
   - `/agent agent_name message` - Route to specific agent
   - Plain message - Routes to default agent (usually Pattern)

2. **Agent names are dynamic** - based on what's configured in MultiAgentSystemBuilder

### Next Steps: Reimplement in pattern

1. **Migrate to Groups API**
   - Replace individual routing with group-based coordination
   - Create flexible group configurations
   - Leverage native multi-agent conversation support
   - See [Memory and Groups Architecture](./MEMORY_AND_GROUPS.md)

2. **Enhanced Memory System**
   - Implement three-tier memory hierarchy
   - Add passive knowledge sharing via sources
   - Build cost-optimized sleeptime processing

3. **Specialized Agent Behaviors**
   - Entropy: Task breakdown with ADHD awareness
   - Flux: Time translation and reality checks
   - Archive: Memory consolidation and pattern detection
   - Momentum: Energy state tracking
   - Anchor: Gentle habit support

4. **Flexible Coordination Patterns**
   - Experiment with different group managers
   - Create context-specific agent groups
   - Build overlapping groups for different needs

The foundation is now in place - next step is leveraging Letta's native multi-agent support!
