# Pattern v2: API Surface

## Overview

Pattern v2 exposes multiple interfaces:

1. **HTTP API** - REST endpoints for web clients, external integrations
2. **ACP (Agent Client Protocol)** - For editor integration (Zed, JetBrains, etc.)
3. **CLI** - Direct local interaction (remains important as trusted interface)

## HTTP API

Built on Axum, serving the pattern_server crate.

### Authentication

```
POST /api/v1/auth/login
POST /api/v1/auth/refresh
POST /api/v1/auth/logout
```

Existing v1 JWT-based auth can be preserved.

### Constellation Management

```
GET    /api/v1/constellations              # List user's constellations
POST   /api/v1/constellations              # Create new constellation
GET    /api/v1/constellations/:id          # Get constellation details
DELETE /api/v1/constellations/:id          # Delete constellation
```

### Agent Management

```
GET    /api/v1/constellations/:cid/agents           # List agents
POST   /api/v1/constellations/:cid/agents           # Create agent
GET    /api/v1/constellations/:cid/agents/:aid      # Get agent
PATCH  /api/v1/constellations/:cid/agents/:aid      # Update agent config
DELETE /api/v1/constellations/:cid/agents/:aid      # Delete agent
```

### Sessions (Conversations)

```
POST   /api/v1/constellations/:cid/agents/:aid/sessions     # Create session
GET    /api/v1/constellations/:cid/agents/:aid/sessions/:sid # Get session
DELETE /api/v1/constellations/:cid/agents/:aid/sessions/:sid # End session

# Send message and get streaming response
POST   /api/v1/constellations/:cid/agents/:aid/sessions/:sid/messages
       Content-Type: application/json
       Accept: text/event-stream
       
       { "content": "Hello", "source": "api" }
       
       Response: SSE stream of ResponseEvents
```

### Memory Operations

```
# Memory blocks
GET    /api/v1/constellations/:cid/agents/:aid/memory          # List blocks
GET    /api/v1/constellations/:cid/agents/:aid/memory/:label   # Get block
PUT    /api/v1/constellations/:cid/agents/:aid/memory/:label   # Update block
DELETE /api/v1/constellations/:cid/agents/:aid/memory/:label   # Delete block

# Block history (via Loro)
GET    /api/v1/constellations/:cid/agents/:aid/memory/:label/history
POST   /api/v1/constellations/:cid/agents/:aid/memory/:label/rollback
       { "version": "frontiers_json" }

# Archival search
POST   /api/v1/constellations/:cid/agents/:aid/memory/search
       { "query": "search terms", "limit": 10 }

# Shared blocks
POST   /api/v1/constellations/:cid/agents/:aid/memory/:label/share
       { "target_agent_id": "...", "access": "read_only" }
```

### Groups

```
GET    /api/v1/constellations/:cid/groups           # List groups
POST   /api/v1/constellations/:cid/groups           # Create group
GET    /api/v1/constellations/:cid/groups/:gid      # Get group
DELETE /api/v1/constellations/:cid/groups/:gid      # Delete group

POST   /api/v1/constellations/:cid/groups/:gid/members
       { "agent_id": "...", "role": "worker" }
DELETE /api/v1/constellations/:cid/groups/:gid/members/:aid
```

### Export/Import

```
POST   /api/v1/constellations/:cid/export
       Response: application/octet-stream (CAR file)

POST   /api/v1/constellations/import
       Content-Type: application/octet-stream
       Body: CAR file
```

---

## ACP Integration

The Agent Client Protocol enables Pattern to work with any ACP-compatible editor (Zed, JetBrains, Neovim, etc.).

### What is ACP?

ACP is an open standard (from Zed) that decouples agents from editors:
- **Agents** implement the `Agent` trait and run as subprocesses
- **Clients** (editors) spawn agents and communicate via JSON-RPC over stdio
- Agents can request file access, terminal execution, permissions from the client

### Pattern as an ACP Agent

Pattern would implement the `agent-client-protocol` crate's `Agent` trait:

```rust
use agent_client_protocol::{
    Agent, AgentSideConnection,
    InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse,
    // ...
};

pub struct PatternAcpAgent {
    /// The Pattern constellation/agent this wraps
    db: Arc<ConstellationDb>,
    agent_id: AgentId,
    
    /// Active sessions
    sessions: DashMap<SessionId, PatternSession>,
}

#[async_trait]
impl Agent for PatternAcpAgent {
    async fn initialize(&self, req: InitializeRequest) -> Result<InitializeResponse> {
        Ok(InitializeResponse {
            protocol_version: V1,
            capabilities: AgentCapabilities {
                load_session: true,  // Pattern has persistent sessions
                mcp: Some(McpCapabilities { /* ... */ }),
                // ...
            },
            implementation: Implementation {
                name: "pattern".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: Some("Pattern ADHD Support Agent".into()),
            },
            // ...
        })
    }
    
    async fn new_session(&self, req: NewSessionRequest) -> Result<NewSessionResponse> {
        // Create or load Pattern agent session
        let session = PatternSession::new(
            &self.db,
            &self.agent_id,
            req.mcp_servers,  // Forward MCP server configs
        ).await?;
        
        let session_id = SessionId::new();
        self.sessions.insert(session_id.clone(), session);
        
        Ok(NewSessionResponse {
            session_id,
            available_modes: vec![
                SessionMode { id: "default".into(), name: "Default".into() },
                SessionMode { id: "adhd".into(), name: "ADHD Support".into() },
            ],
            // ...
        })
    }
    
    async fn prompt(&self, req: PromptRequest) -> Result<PromptResponse> {
        let session = self.sessions.get(&req.session_id)
            .ok_or_else(|| Error::session_not_found())?;
        
        // Convert ACP content blocks to Pattern message format
        let message = convert_acp_to_pattern(&req.messages);
        
        // Get connection for sending updates back to client
        let conn = /* ... */;
        
        // Process with Pattern agent, streaming updates via ACP notifications
        let result = session.agent
            .process_message_stream(message)
            .await;
        
        // Stream tool calls, content chunks, etc. via session/update notifications
        while let Some(event) = result.next().await {
            match event {
                ResponseEvent::Text(chunk) => {
                    conn.notify(SessionNotification {
                        session_id: req.session_id.clone(),
                        update: SessionUpdate::ContentChunk(ContentChunk {
                            role: Role::Assistant,
                            content: ContentBlock::Text(TextContent { text: chunk }),
                        }),
                    }).await?;
                }
                ResponseEvent::ToolCall { id, name, args } => {
                    conn.notify(SessionNotification {
                        session_id: req.session_id.clone(),
                        update: SessionUpdate::ToolCall(ToolCall {
                            id: ToolCallId::new(id),
                            name,
                            kind: ToolKind::Function,
                            // ...
                        }),
                    }).await?;
                }
                // ...
            }
        }
        
        Ok(PromptResponse {
            stop_reason: StopReason::EndTurn,
        })
    }
    
    async fn cancel(&self, req: CancelNotification) -> Result<()> {
        if let Some(session) = self.sessions.get(&req.session_id) {
            session.cancel().await;
        }
        Ok(())
    }
    
    async fn load_session(&self, req: LoadSessionRequest) -> Result<LoadSessionResponse> {
        // Pattern has full session persistence - can restore from DB
        let session = PatternSession::load(&self.db, &req.session_id).await?;
        
        // Stream history back to client via notifications
        for message in session.history().await? {
            // Send as ContentChunk notifications
        }
        
        self.sessions.insert(req.session_id.clone(), session);
        Ok(LoadSessionResponse { /* ... */ })
    }
}
```

### Running as ACP Agent

Pattern would provide a binary that speaks ACP over stdio:

```bash
# Invoked by editor (Zed, etc.)
pattern-acp --constellation my-constellation --agent my-agent
```

The binary:
1. Reads JSON-RPC from stdin
2. Dispatches to `PatternAcpAgent` implementation
3. Writes responses/notifications to stdout

### ACP Client Capabilities Pattern Can Use

When the client (editor) supports these, Pattern can:

```rust
// Read files from user's workspace
let content = conn.request(ReadTextFileRequest {
    path: "/path/to/file.rs".into(),
}).await?;

// Write files (with permission)
conn.request(WriteTextFileRequest {
    path: "/path/to/file.rs".into(),
    content: new_content,
}).await?;

// Execute terminal commands
let terminal = conn.request(CreateTerminalRequest {
    command: "cargo".into(),
    args: vec!["test".into()],
    cwd: Some("/project".into()),
    env: vec![],
}).await?;

// Wait for command and get output
let result = conn.request(WaitForTerminalExitRequest {
    terminal_id: terminal.id,
}).await?;

// Request permission for dangerous operations
let permission = conn.request(RequestPermissionRequest {
    tool_call_id: call_id,
    tool_name: "shell_execute".into(),
    description: "Run rm -rf on /tmp/build".into(),
    options: vec![
        PermissionOption { id: "allow".into(), label: "Allow".into(), kind: PermissionOptionKind::Allow },
        PermissionOption { id: "deny".into(), label: "Deny".into(), kind: PermissionOptionKind::Deny },
    ],
}).await?;
```

### ACP vs Remote Presence Connector

These solve different problems:

| Aspect | ACP | Remote Presence Connector |
|--------|-----|---------------------------|
| Direction | Editor → Agent | Agent (on server) → User's machine |
| Transport | stdio (local subprocess) | WebSocket (network) |
| Use case | Editor integration | Server-hosted agent accessing local files |
| Trust | Agent trusted by editor | Connector authenticated as partner |

They can coexist:
- Local Pattern: Runs as ACP agent, accessed by editor directly
- Remote Pattern: Runs on server, uses connector for file access, could still speak ACP to local editor via tunnel

---

## WebSocket API

For real-time bidirectional communication (Discord bot, live updates):

```
WS /api/v1/constellations/:cid/ws

# Client → Server messages
{ "type": "subscribe", "agent_id": "..." }
{ "type": "message", "agent_id": "...", "content": "...", "source": "discord" }
{ "type": "unsubscribe", "agent_id": "..." }

# Server → Client messages
{ "type": "response_event", "agent_id": "...", "event": { ... } }
{ "type": "memory_update", "agent_id": "...", "block": "...", "content": "..." }
{ "type": "error", "message": "..." }
```

---

## CLI Commands (Preserved/Enhanced)

The CLI remains the trusted local interface:

```bash
# Agent operations
pattern agent list
pattern agent create --name "MyAgent" --model anthropic/claude-3-5-sonnet
pattern agent status MyAgent
pattern agent export MyAgent -o agent.car

# Interactive chat
pattern chat --agent MyAgent
pattern chat --group MyGroup

# Memory inspection
pattern debug list-core --agent MyAgent
pattern debug search-archival --agent MyAgent --query "important"
pattern debug show-context --agent MyAgent

# Memory history (new in v2)
pattern memory history --agent MyAgent --block persona
pattern memory rollback --agent MyAgent --block persona --version <frontiers>

# Export/Import
pattern export constellation MyConstellation -o constellation.car
pattern import --from constellation.car

# ACP mode (new in v2)
pattern acp --agent MyAgent  # Run as ACP agent for editor integration
```

---

## Trust Levels and Tool Access

Different API surfaces have different trust levels:

| Interface | Trust Level | Default Tool Access |
|-----------|-------------|---------------------|
| CLI | Partner | All tools |
| ACP (local editor) | Partner | All tools (editor controls permissions) |
| HTTP API (authenticated) | Partner | All tools |
| Remote Connector | Partner | All tools (connector authenticated) |
| Discord | Conversant | Safe tools only |
| Bluesky | Conversant | Safe tools only |
| Unauthenticated | Untrusted | Read-only |

Tool access can be further restricted per-agent via config.

---

## MCP Integration

Pattern already has MCP client support. With ACP integration:

1. **Editor provides MCP servers** - Via `NewSessionRequest.mcp_servers`
2. **Pattern connects to them** - Uses existing `pattern_mcp` client
3. **Tools from MCP servers** - Registered in Pattern's tool registry
4. **Pattern can also expose MCP** - For other tools to call Pattern

```
┌─────────────┐     ACP      ┌─────────────┐     MCP     ┌─────────────┐
│    Zed      │◄────────────►│   Pattern   │◄───────────►│  MCP Server │
│   (Client)  │   stdio      │   (Agent)   │             │ (e.g. gh)   │
└─────────────┘              └─────────────┘             └─────────────┘
```

---

## ACP Connection Model

One ACP connection = one Pattern entity:

```bash
# Connect to a single agent
pattern acp --agent MyAgent

# Connect to a group (coordination pattern routes internally)
pattern acp --group MyGroup
```

When connecting to a **group**, the ACP session maps to the group's coordination pattern:
- Messages route through the pattern manager (round-robin, dynamic, supervisor, etc.)
- `session/update` notifications include which agent is responding
- The group appears as a single "agent" to the editor

This keeps the ACP interface simple while allowing Pattern's multi-agent coordination to work underneath.

### Pattern vs Typical ACP Session Semantics

**Important distinction:** Pattern's model differs from typical ACP agents.

| Aspect | Typical ACP Agent | Pattern |
|--------|-------------------|---------|
| Session | Ephemeral conversation | Connection to persistent constellation |
| History | Per-session, loaded on `session/load` | Always persistent in constellation DB |
| `session/new` | Creates fresh context | Connects to existing agent, maybe specifies memory blocks to surface |
| `session/load` | Restores saved conversation | Mostly no-op - constellation is always "loaded" |
| Identity | Session-scoped | Long-term relationship with partner |

Pattern would implement ACP sessions as "connection handles" rather than "conversation containers":

```rust
async fn new_session(&self, req: NewSessionRequest) -> Result<NewSessionResponse> {
    // "Session" is just a connection to the persistent constellation
    // No new conversation context created - agent already has full history
    
    let session = AcpSessionHandle {
        constellation_id: self.constellation_id.clone(),
        agent_id: self.agent_id.clone(),
        // Optional: which memory blocks to emphasize in this connection
        active_blocks: req.config.get("active_blocks").cloned(),
        // MCP servers from editor
        mcp_servers: req.mcp_servers,
    };
    
    // Don't replay history - it's already in the agent's context
    // Editor can request specific history via extension methods if needed
    
    Ok(NewSessionResponse {
        session_id: SessionId::generate(),
        // ...
    })
}

async fn load_session(&self, req: LoadSessionRequest) -> Result<LoadSessionResponse> {
    // Pattern doesn't really "load" sessions - constellation is persistent
    // This could be used to switch which agent/group we're connected to
    // or to specify a different memory configuration
    
    Ok(LoadSessionResponse { /* ... */ })
}
```

This means Pattern agents maintain continuity across ACP connections - reconnecting picks up where you left off, because the agent never "forgot" anything.

---

---

## Deployment Model

### Single Canonical Server

Pattern runs on a **single server** - this could be:
- A home server (low latency, always-on, your hardware)
- A cloud VPS (accessible from anywhere)
- Your local machine (simplest, but not always-on)

The key insight: **LLM API response latency dominates everything**. Network latency for file operations over iroh is negligible by comparison. So "local vs remote" for the server matters less than you'd think.

### Multiple Entry Points, One Truth

The server handles multiple concurrent connections:

```
┌─────────────────────────────────────────────────────────────────┐
│                    Pattern Server (e.g., home server)           │
│                                                                 │
│  ┌────────────────────────────────────────────────────────┐    │
│  │  Constellation                                          │    │
│  │  └── Agents (Pattern, Flux, Entropy, Anchor, Archive)   │    │
│  │  └── constellation.db (single source of truth)          │    │
│  │  └── Shared context, coordination state                 │    │
│  └────────────────────────────────────────────────────────┘    │
│         ▲            ▲            ▲            ▲                │
│         │            │            │            │                │
│    Connector    Connector     Discord      Bluesky              │
│    (laptop)    (desktop)       Bot        Firehose              │
│         │            │            │            │                │
└─────────┼────────────┼────────────┼────────────┼────────────────┘
          │            │            │            │
     ┌────▼────┐  ┌────▼────┐      │            │
     │ Laptop  │  │ Desktop │   Discord     Bluesky
     │  Zed    │  │  Zed    │   servers     servers
     └─────────┘  └─────────┘
```

All entry points hit the same server, same constellation, same agents. No sync problem because there's one database.

### Concurrent Message Handling

When messages arrive simultaneously from different sources, Pattern handles them with **batch isolation**:

```
Timeline (actual processing):
  t=0: Discord msg arrives, batch A starts
  t=1: Bluesky mention arrives, batch B starts  
  t=3: Connector request arrives, batch C starts
  t=5: batch B completes (Bluesky was a quick reply)
  t=8: batch C completes
  t=12: batch A completes (Discord needed more thought)

Agent's view during batch A:
  [history up to t=0]
  [batch A in progress]
  (batches B and C are invisible)

Final history (reconstructed by arrival order):
  [prior history]
  batch A (Discord) - arrived t=0
  batch B (Bluesky) - arrived t=1
  batch C (Connector) - arrived t=3
```

This is **implicit forking** - each batch gets isolated context, results merge back ordered by arrival time (via Snowflake IDs). The agent experiences it as fast sequential processing.

### Deployment Configurations

#### Home Server (Recommended for Power Users)

```
┌─────────────────────────────────────────────────────────────────┐
│                    Home Network                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │  Home Server (always-on)                                 │   │
│  │  └── Pattern server process                              │   │
│  │  └── constellation.db                                    │   │
│  │  └── iroh endpoint (accepts connectors)                  │   │
│  └─────────────────────────────────────────────────────────┘   │
│         ▲                                                       │
│         │ iroh (local network = very fast)                      │
│         │                                                       │
│  ┌──────┴──────────────────────────────────────────────────┐   │
│  │  Workstation                                             │   │
│  │  └── pattern-connector daemon                            │   │
│  │  └── Zed/Editor (ACP via connector)                      │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
          │
          │ iroh (NAT-traversed when away from home)
          ▼
     ┌─────────┐
     │ Laptop  │ (on the go, connector still works)
     └─────────┘
```

**Pros**: Always-on, low latency on home network, NAT traversal when remote
**Cons**: Requires home server setup

#### Cloud VPS

```
┌─────────────────────────────────────────────────────────────────┐
│                    Cloud VPS                                    │
│  └── Pattern server                                             │
│  └── constellation.db                                           │
│  └── Discord bot, Bluesky firehose                              │
└───────────────────────────┬─────────────────────────────────────┘
                            │ iroh
              ┌─────────────┼─────────────┐
              ▼             ▼             ▼
          Laptop       Desktop        Phone
        (connector)  (connector)    (web UI)
```

**Pros**: Accessible from anywhere, no home server needed
**Cons**: More latency for file ops, cloud costs

#### Local Only (Simplest)

```
┌─────────────────────────────────────────────────────────────────┐
│                    Your Machine                                 │
│  └── Pattern server (runs when you're working)                  │
│  └── constellation.db                                           │
│  └── Direct file access (LocalConnector)                        │
│  └── Zed connects via ACP directly                              │
└─────────────────────────────────────────────────────────────────┘
```

**Pros**: Simplest setup, no network
**Cons**: Not always-on, no Discord/Bluesky when machine is off

---

## ACP Modes

### Direct ACP (Local)

Pattern runs as subprocess, editor spawns it directly:

```bash
# In editor's agent config
{
  "command": "pattern-acp",
  "args": ["--constellation", "my-constellation", "--agent", "coder"]
}
```

Pattern binary speaks ACP over stdio, has direct file access.

### Proxied ACP (Remote via Connector)

Editor connects to local connector daemon, which proxies to remote Pattern:

```bash
# Connector runs in background
$ pattern-connector --daemon

# Editor connects to connector's ACP socket
{
  "command": "pattern-connector",
  "args": ["acp-proxy"]
}
```

The connector:
1. Receives ACP messages from editor via stdio
2. Forwards to Pattern server via iroh
3. Handles file/env requests from server
4. Returns responses to editor

### ACP Session Mapping

| ACP Concept | Pattern Mapping |
|-------------|-----------------|
| `session/new` | Connect to existing agent/group (no new context created) |
| `session/load` | Reconnect to same agent (mostly no-op) |
| `session_id` | Connection handle, not conversation container |
| `prompt` | Send message to agent, get streaming response |
| `cancel` | Cancel current agent processing |

Pattern's persistent memory means the agent maintains context across sessions. The "session" is just a connection, not a conversation boundary.

---

## Open Questions

1. **Local/server sync** - For hybrid mode, how to sync constellation state?
   - Shared database via network? 
   - CAR-based sync?
   - Conflict resolution for concurrent edits?

2. **Rate limiting** - Different limits for different trust levels?
   - Partner (CLI, ACP): no limits
   - Conversant (Discord, Bluesky): per-user rate limits
   - API: token bucket per API key

3. **Group mode in ACP** - Should `session/set_mode` switch coordination patterns?
   - Or expose as separate capability?

4. **Connector auth in hybrid mode** - Local Pattern talking to server - same iroh auth flow?
