# Pattern v2: Remote Presence Connector

## The Problem

Pattern agents often run on a server (for reliability, always-on presence), but need to interact with the partner's local environment:

- Read/write files on partner's machine
- Connect to local editor (LSP integration)
- Execute commands in partner's terminal
- Access local development tools

This is especially important for:
1. **Coding harness** - Agent needs to see code, run tests, use dev tools
2. **ACP (Agent Communication Protocol)** - Agents coordinating across environments
3. **ADHD support** - Agent helping with tasks that involve local files/apps

## Current State

`realtime.rs` has event sink traits for streaming agent responses:

```rust
#[async_trait]
pub trait AgentEventSink: Send + Sync {
    async fn on_event(&self, event: ResponseEvent, ctx: AgentEventContext);
}
```

This is one-way (agent → observer). We need bidirectional communication.

## Proposed Architecture

### Connector Trait

```rust
/// A connector provides access to an environment (local or remote)
#[async_trait]
pub trait EnvironmentConnector: Send + Sync {
    /// Read a file from the environment
    async fn read_file(&self, path: &Path) -> Result<String>;
    
    /// Write a file to the environment
    async fn write_file(&self, path: &Path, content: &str) -> Result<()>;
    
    /// List directory contents
    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>>;
    
    /// Execute a command
    async fn exec(&self, command: &str, args: &[&str]) -> Result<ExecResult>;
    
    /// Check if path exists
    async fn exists(&self, path: &Path) -> Result<bool>;
    
    /// Get environment info (working directory, OS, etc.)
    async fn env_info(&self) -> Result<EnvInfo>;
    
    /// Open a bidirectional stream (for LSP, etc.)
    async fn open_stream(&self, target: &str) -> Result<Box<dyn AsyncStream>>;
}

pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct EnvInfo {
    pub working_dir: PathBuf,
    pub os: String,
    pub hostname: String,
    /// Trust level of this connector
    pub trust_level: TrustLevel,
}
```

### Trust Levels

Different input sources have different trust levels:

```rust
pub enum TrustLevel {
    /// Partner's own environment - full trust
    /// CLI, remote connector authenticated as partner
    Partner,
    
    /// Known friend/collaborator - high trust
    /// Could allow some file operations
    Friend,
    
    /// Public conversant - limited trust
    /// Discord/Bluesky strangers
    Conversant,
    
    /// Untrusted - read-only, sandboxed
    Untrusted,
}
```

Trust level affects what tools the agent can use:

```rust
impl Agent {
    fn allowed_tools(&self, trust: TrustLevel) -> Vec<Tool> {
        match trust {
            TrustLevel::Partner => self.all_tools(),
            TrustLevel::Friend => self.tools_except(&["shell", "file_write"]),
            TrustLevel::Conversant => self.safe_tools_only(),
            TrustLevel::Untrusted => vec![],
        }
    }
}
```

### Implementations

#### LocalConnector

For when agent runs on same machine:

```rust
pub struct LocalConnector {
    working_dir: PathBuf,
    trust_level: TrustLevel,
}

#[async_trait]
impl EnvironmentConnector for LocalConnector {
    async fn read_file(&self, path: &Path) -> Result<String> {
        tokio::fs::read_to_string(path).await.map_err(Into::into)
    }
    
    async fn exec(&self, command: &str, args: &[&str]) -> Result<ExecResult> {
        let output = tokio::process::Command::new(command)
            .args(args)
            .current_dir(&self.working_dir)
            .output()
            .await?;
        
        Ok(ExecResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        })
    }
    
    // ...
}
```

#### RemoteConnector

For when agent runs on server, partner's machine is remote:

```rust
pub struct RemoteConnector {
    /// WebSocket or other transport to partner's client
    transport: Box<dyn Transport>,
    trust_level: TrustLevel,
}

#[async_trait]
impl EnvironmentConnector for RemoteConnector {
    async fn read_file(&self, path: &Path) -> Result<String> {
        let request = ConnectorRequest::ReadFile { path: path.to_owned() };
        let response = self.transport.request(request).await?;
        match response {
            ConnectorResponse::FileContent(content) => Ok(content),
            ConnectorResponse::Error(e) => Err(e.into()),
            _ => Err(anyhow!("unexpected response")),
        }
    }
    
    // ...
}
```

### Wire Protocol

JSON-RPC style messages over WebSocket:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ConnectorRequest {
    ReadFile { path: PathBuf },
    WriteFile { path: PathBuf, content: String },
    ListDir { path: PathBuf },
    Exec { command: String, args: Vec<String> },
    Exists { path: PathBuf },
    EnvInfo,
    OpenStream { target: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ConnectorResponse {
    FileContent(String),
    DirEntries(Vec<DirEntry>),
    ExecResult(ExecResult),
    Bool(bool),
    EnvInfo(EnvInfo),
    StreamOpened { stream_id: String },
    Error(String),
}
```

### Client-Side Daemon

Partner runs a small daemon that:
1. Connects to Pattern server via WebSocket
2. Authenticates (proves they're the partner)
3. Handles `ConnectorRequest`s from their agent
4. Enforces local security policies

```rust
// pattern-connector daemon
async fn main() {
    let config = load_config()?;
    
    let ws = connect_to_server(&config.server_url).await?;
    authenticate(&ws, &config.credentials).await?;
    
    loop {
        let request: ConnectorRequest = ws.recv().await?;
        
        // Check local policy
        if !policy_allows(&request) {
            ws.send(ConnectorResponse::Error("denied by policy")).await?;
            continue;
        }
        
        let response = handle_request(request).await;
        ws.send(response).await?;
    }
}
```

### Integration with Tools

Agent tools can use the connector:

```rust
pub struct FileReadTool {
    connector: Arc<dyn EnvironmentConnector>,
}

#[async_trait]
impl Tool for FileReadTool {
    async fn execute(&self, args: ToolArgs) -> Result<ToolResult> {
        let path = args.get::<PathBuf>("path")?;
        let content = self.connector.read_file(&path).await?;
        Ok(ToolResult::text(content))
    }
}
```

### LSP Integration

For editor integration, the connector can proxy LSP:

```rust
impl RemoteConnector {
    async fn connect_lsp(&self, language: &str) -> Result<LspClient> {
        let stream = self.open_stream(&format!("lsp:{}", language)).await?;
        Ok(LspClient::new(stream))
    }
}
```

The client-side daemon would spawn the appropriate LSP server and bridge the connection.

## Security Considerations

1. **Authentication** - Connector must prove it's the partner
   - Could use same auth as API (JWT)
   - Or separate long-lived connector tokens
   
2. **Authorization** - Even partner may want limits
   - Configurable allowed paths
   - Command allowlist/denylist
   - Confirmation prompts for destructive operations

3. **Sandboxing** - For non-partner connectors
   - Read-only access
   - No command execution
   - Limited to specific directories

4. **Audit logging** - Track all connector operations
   - Who, what, when
   - Useful for debugging and trust building

## Relationship to ACP

Agent Communication Protocol would use similar transport:
- Agent A on server X wants to coordinate with Agent B on server Y
- Each agent has connector access to their partner's environment
- ACP messages flow server-to-server
- File/environment access flows through connectors

```
┌─────────────────┐         ┌─────────────────┐
│  Partner A's    │         │  Partner B's    │
│  Machine        │         │  Machine        │
│  ┌───────────┐  │         │  ┌───────────┐  │
│  │ Connector │◄─┼─────────┼──┤ Connector │  │
│  │  Daemon   │  │   WS    │  │  Daemon   │  │
│  └─────┬─────┘  │         │  └─────┬─────┘  │
│        │        │         │        │        │
│        ▼        │         │        ▼        │
│   Local Files   │         │   Local Files   │
└─────────────────┘         └─────────────────┘
         ▲                           ▲
         │                           │
         │    ┌─────────────────┐    │
         │    │  Pattern Server │    │
         │    │  ┌───────────┐  │    │
         └────┼──┤  Agent A  │  │    │
              │  └─────┬─────┘  │    │
              │        │ ACP    │    │
              │        ▼        │    │
              │  ┌───────────┐  │    │
              │  │  Agent B  ├──┼────┘
              │  └───────────┘  │
              └─────────────────┘
```

## Transport: iroh

**Decision**: Use [iroh](https://iroh.computer/) for connector transport.

### Why iroh

- **Peer-to-peer** - No central relay required (though uses relays for NAT traversal)
- **Encryption built-in** - QUIC-based, TLS 1.3
- **Cryptographic identity** - Stable node IDs (public keys) for auth
- **Rust-native** - `iroh-net`, `iroh-rpc`, `iroh-bytes` crates, async, well-maintained
- **Handles reconnection** - Connection state management built in
- **NAT traversal** - STUN/TURN equivalent via n0 discovery

### iroh Protocol Stack

| Layer | iroh Crate | Use in Pattern |
|-------|------------|----------------|
| Transport | `iroh-net` | QUIC connections, node identity, NAT traversal |
| RPC | `iroh-rpc` | Request/response for connector operations |
| Streaming | `iroh-bytes` | Large file transfers, ACP message streams |

**Primary primitive**: `iroh-rpc` for most connector operations (file read/write, exec, etc.)

**Streaming**: `iroh-bytes` or raw QUIC streams for:
- Large file transfers (chunked)
- ACP message proxying
- LSP bidirectional streams

### Architecture with iroh

```
┌─────────────────────────────────────────────────────────────┐
│                  Partner's Machine                          │
│  ┌─────────────────────────────────────────────────────┐   │
│  │           pattern-connector daemon                   │   │
│  │                                                      │   │
│  │  iroh Endpoint                                       │   │
│  │  - persistent node ID (keypair)                      │   │
│  │  - connects to Pattern server                        │   │
│  │  - handles bidirectional streams                     │   │
│  │                                                      │   │
│  │  Request handlers:                                   │   │
│  │  - file read/write (within allowed paths)            │   │
│  │  - command execution (within allowed commands)       │   │
│  │  - LSP proxy (spawn local LSP, bridge streams)       │   │
│  │  - file watching (push notifications)                │   │
│  └─────────────────────────────────────────────────────┘   │
└───────────────────────────┬─────────────────────────────────┘
                            │ QUIC (iroh)
                            │ encrypted, NAT-traversing
                            │
┌───────────────────────────▼─────────────────────────────────┐
│                  Pattern Server                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  iroh Endpoint                                       │   │
│  │  - accepts connector connections                     │   │
│  │  - validates node ID against registered partners     │   │
│  │                                                      │   │
│  │  RemoteConnector                                     │   │
│  │  - implements EnvironmentConnector trait             │   │
│  │  - sends requests over iroh connection               │   │
│  │  - used by agents for file/env access                │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### Authentication via Node ID

iroh peers have cryptographic node IDs (public keys). We use this for auth:

```rust
pub struct ConnectorAuth {
    /// Partner's iroh node ID (Ed25519 public key)
    pub partner_node_id: iroh_net::NodeId,
    
    /// Constellation this connector is authorized for
    pub constellation_id: ConstellationId,
    
    /// When this auth was established
    pub paired_at: DateTime<Utc>,
    
    /// Optional: human-readable name for this connector
    pub name: Option<String>,
}

impl PatternServer {
    async fn on_connector_connect(&self, conn: iroh_net::Connection) -> Result<()> {
        let peer_id = conn.remote_node_id();
        
        // Check if this node ID is registered
        match self.db.get_connector_auth(&peer_id).await? {
            Some(auth) => {
                // Known partner - create connector
                let connector = RemoteConnector::new(conn, auth.constellation_id);
                self.register_connector(auth.constellation_id, connector).await;
                tracing::info!("Connector {} connected for constellation {}", 
                    peer_id, auth.constellation_id);
            }
            None => {
                // Unknown node - reject
                tracing::warn!("Unknown connector attempted connection: {}", peer_id);
                conn.close(0u8.into(), b"unknown node ID");
            }
        }
        
        Ok(())
    }
}
```

### Pairing Flow

First-time connector setup:

```
┌──────────────────────────────────────────────────────────────────┐
│ 1. Partner authenticates via web UI or CLI                       │
│    $ pattern auth login                                          │
│                                                                  │
│ 2. Partner requests pairing code                                 │
│    $ pattern connector pair --generate                           │
│    Pairing code: ABC-123-XYZ (expires in 10 minutes)            │
│                                                                  │
│ 3. On partner's machine, run connector with code                 │
│    $ pattern-connector pair --code ABC-123-XYZ                   │
│                                                                  │
│ 4. Connector generates keypair, sends node ID to server          │
│    POST /api/v1/connector/pair { code: "...", node_id: "..." }  │
│                                                                  │
│ 5. Server associates node ID with partner's constellation        │
│    Connector auth saved to DB                                    │
│                                                                  │
│ 6. Future connections: node ID is sufficient for auth            │
│    No tokens, no passwords - just cryptographic identity         │
└──────────────────────────────────────────────────────────────────┘
```

### Connector Daemon Implementation

```rust
// pattern-connector binary
use iroh_net::{Endpoint, NodeAddr, SecretKey};
use std::path::PathBuf;

const PATTERN_ALPN: &[u8] = b"pattern-connector/1";

#[tokio::main]
async fn main() -> Result<()> {
    let config = ConnectorConfig::load()?;
    
    // Load or generate persistent identity
    let secret_key = match std::fs::read(&config.key_path) {
        Ok(bytes) => SecretKey::try_from_bytes(&bytes)?,
        Err(_) => {
            let key = SecretKey::generate();
            std::fs::write(&config.key_path, key.to_bytes())?;
            key
        }
    };
    
    // Create iroh endpoint
    let endpoint = Endpoint::builder()
        .secret_key(secret_key)
        .discovery_n0()  // NAT traversal via n0 infrastructure
        .bind()
        .await?;
    
    println!("Connector node ID: {}", endpoint.node_id());
    
    // Connect to Pattern server
    let server_addr: NodeAddr = config.server_addr.parse()?;
    let conn = endpoint.connect(server_addr, PATTERN_ALPN).await?;
    
    println!("Connected to Pattern server");
    
    // Set up file watcher for push notifications
    let (watch_tx, watch_rx) = tokio::sync::mpsc::channel(100);
    if let Some(watch_paths) = &config.watch_paths {
        spawn_file_watcher(watch_paths.clone(), watch_tx);
    }
    
    // Handle incoming requests and outgoing notifications
    tokio::select! {
        r = handle_requests(&conn, &config) => r?,
        r = push_file_changes(&conn, watch_rx) => r?,
    }
    
    Ok(())
}

async fn handle_requests(
    conn: &iroh_net::Connection, 
    config: &ConnectorConfig
) -> Result<()> {
    loop {
        let (mut send, mut recv) = conn.accept_bi().await?;
        let config = config.clone();
        
        tokio::spawn(async move {
            let request: ConnectorRequest = read_json(&mut recv).await?;
            
            // Check against local policy
            if !config.policy.allows(&request) {
                let response = ConnectorResponse::Error {
                    code: "POLICY_DENIED".into(),
                    message: format!("Local policy denies: {:?}", request),
                };
                write_json(&mut send, &response).await?;
                return Ok(());
            }
            
            // Handle request
            let response = match request {
                ConnectorRequest::ReadFile { path } => {
                    match tokio::fs::read_to_string(&path).await {
                        Ok(content) => ConnectorResponse::FileContent(content),
                        Err(e) => ConnectorResponse::Error {
                            code: "READ_ERROR".into(),
                            message: e.to_string(),
                        },
                    }
                }
                ConnectorRequest::WriteFile { path, content } => {
                    match tokio::fs::write(&path, &content).await {
                        Ok(()) => ConnectorResponse::Ok,
                        Err(e) => ConnectorResponse::Error {
                            code: "WRITE_ERROR".into(),
                            message: e.to_string(),
                        },
                    }
                }
                ConnectorRequest::Exec { command, args, cwd } => {
                    let mut cmd = tokio::process::Command::new(&command);
                    cmd.args(&args);
                    if let Some(cwd) = cwd {
                        cmd.current_dir(cwd);
                    }
                    
                    match cmd.output().await {
                        Ok(output) => ConnectorResponse::ExecResult(ExecResult {
                            exit_code: output.status.code().unwrap_or(-1),
                            stdout: String::from_utf8_lossy(&output.stdout).into(),
                            stderr: String::from_utf8_lossy(&output.stderr).into(),
                        }),
                        Err(e) => ConnectorResponse::Error {
                            code: "EXEC_ERROR".into(),
                            message: e.to_string(),
                        },
                    }
                }
                ConnectorRequest::ListDir { path } => {
                    // ... similar pattern
                }
                ConnectorRequest::OpenLsp { language } => {
                    // Spawn LSP server, return stream ID for bidirectional comms
                }
                // ... other requests
            };
            
            write_json(&mut send, &response).await?;
            Ok::<_, anyhow::Error>(())
        });
    }
}
```

### Local Policy Configuration

Partner controls what the connector allows:

```toml
# ~/.config/pattern-connector/config.toml

[server]
address = "pattern.example.com"
node_id = "abc123..."  # Server's iroh node ID

[policy]
# Allowed paths for file operations (glob patterns)
allowed_paths = [
    "~/Projects/**",
    "~/Documents/pattern/**",
]

# Explicitly denied paths
denied_paths = [
    "~/.ssh/**",
    "~/.gnupg/**",
    "**/.env",
    "**/secrets/**",
]

# Allowed commands
allowed_commands = [
    "cargo",
    "npm",
    "git",
    "rg",
    "fd",
    "cat",
    "ls",
]

# Denied commands (takes precedence)
denied_commands = [
    "rm",
    "sudo",
    "chmod",
]

# Require confirmation for these operations
confirm_operations = [
    "write_file",
    "exec",
]

[watch]
# Paths to watch for changes (push to server)
paths = [
    "~/Projects/current/**/*.rs",
    "~/Projects/current/**/*.toml",
]
```

### File Change Notifications

Connector can push file change events to the server:

```rust
#[derive(Serialize, Deserialize)]
pub enum ConnectorNotification {
    FileChanged {
        path: PathBuf,
        change_type: FileChangeType,
    },
    FileCreated {
        path: PathBuf,
    },
    FileDeleted {
        path: PathBuf,
    },
    ConnectorStatus {
        status: ConnectorStatus,
    },
}

#[derive(Serialize, Deserialize)]
pub enum FileChangeType {
    Modified,
    Renamed { from: PathBuf },
}

async fn push_file_changes(
    conn: &iroh_net::Connection,
    mut watch_rx: mpsc::Receiver<notify::Event>,
) -> Result<()> {
    while let Some(event) = watch_rx.recv().await {
        let notification = match event.kind {
            notify::EventKind::Modify(_) => {
                ConnectorNotification::FileChanged {
                    path: event.paths[0].clone(),
                    change_type: FileChangeType::Modified,
                }
            }
            notify::EventKind::Create(_) => {
                ConnectorNotification::FileCreated {
                    path: event.paths[0].clone(),
                }
            }
            // ... etc
        };
        
        // Open unidirectional stream for notification
        let mut send = conn.open_uni().await?;
        write_json(&mut send, &notification).await?;
        send.finish().await?;
    }
    
    Ok(())
}
```

### Reconnection Handling

iroh handles most reconnection automatically, but we add application-level resilience:

```rust
impl RemoteConnector {
    async fn request<T: DeserializeOwned>(
        &self, 
        req: ConnectorRequest
    ) -> Result<T> {
        // Retry with backoff on connection errors
        let mut attempts = 0;
        loop {
            match self.try_request(&req).await {
                Ok(response) => return Ok(response),
                Err(e) if e.is_connection_error() && attempts < 3 => {
                    attempts += 1;
                    let backoff = Duration::from_millis(100 * 2u64.pow(attempts));
                    tokio::time::sleep(backoff).await;
                    
                    // iroh may have reconnected automatically
                    // if not, this will fail again
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
    
    /// Check if connector is currently connected
    pub fn is_connected(&self) -> bool {
        // iroh connection state
        !self.conn.close_reason().is_some()
    }
    
    /// Wait for reconnection (iroh handles this)
    pub async fn wait_connected(&self) -> Result<()> {
        // iroh will attempt to reconnect automatically
        // we just need to wait for it
        tokio::time::timeout(
            Duration::from_secs(30),
            self.conn.closed()
        ).await??;
        
        Ok(())
    }
}
```

### Integration with ACP

When Pattern runs as an ACP agent locally, it uses `LocalConnector`. When it runs on a server but the editor is local, we can tunnel ACP over iroh:

```
┌─────────────────────────────────────────────────────────────────┐
│                    Partner's Machine                            │
│                                                                 │
│  ┌─────────────┐         ┌─────────────────────────────────┐   │
│  │    Zed      │◄──ACP──►│  pattern-connector              │   │
│  │  (Editor)   │  stdio  │  - proxies ACP over iroh        │   │
│  └─────────────┘         │  - handles file/env requests    │   │
│                          └──────────────┬──────────────────┘   │
└─────────────────────────────────────────┼───────────────────────┘
                                          │ iroh
┌─────────────────────────────────────────▼───────────────────────┐
│                    Pattern Server                               │
│  ┌────────────────────────────────────────────────────────┐    │
│  │  Pattern Agent                                          │    │
│  │  - receives ACP messages via iroh                       │    │
│  │  - uses RemoteConnector for file access                 │    │
│  │  - full constellation persistence                       │    │
│  └────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

The connector daemon can spawn a local ACP shim that the editor connects to:

```rust
// In pattern-connector, optional ACP proxy mode
async fn run_acp_proxy(
    conn: &iroh_net::Connection,
) -> Result<()> {
    // Spawn local process that speaks ACP over stdio
    // Forward messages to Pattern server over iroh
    // Return responses back to editor
    
    let mut child = tokio::process::Command::new("pattern-connector")
        .arg("acp-shim")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    
    // Bridge stdio <-> iroh
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    
    tokio::select! {
        // Editor -> Server
        r = forward_stdin_to_iroh(stdout, conn) => r?,
        // Server -> Editor
        r = forward_iroh_to_stdout(conn, stdin) => r?,
    }
    
    Ok(())
}
```

## Open Questions

1. **LSP multiplexing** - Multiple LSP servers over single iroh connection? Probably separate streams per LSP.

2. **Offline operation** - Cache file contents on server for when connector disconnects?
   - Could cache recently-accessed files
   - Mark cached data as potentially stale
   - Agent sees "connector offline, using cached data from X minutes ago"

3. **Multiple connectors** - Partner with multiple machines. Options:
   - Route to most-recently-active connector
   - Route based on request (file path → which machine has it)
   - Allow agent to specify target connector
   - Probably: default to most-recent, allow explicit targeting

4. **Connector discovery** - Use iroh's discovery vs explicit config?
   - Explicit config is simpler and more secure
   - Discovery could help with "find my connectors" UX
   - Leaning toward: explicit config required, discovery as optional convenience

5. **iroh-rpc schema** - Define connector RPC interface formally?
   - Could generate from protobuf/schema
   - Or just use serde JSON over iroh-rpc
   - Leaning toward: start with serde, formalize later if needed
